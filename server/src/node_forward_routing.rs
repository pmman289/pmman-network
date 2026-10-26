//! Per-ingress return routing for DNAT connections. Never changes host/main routes.
use super::{kernel_output, NodeConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{env, fs, net::IpAddr, path::PathBuf, process::Command};

const PROTOCOL: &str = "242";
const SLOTS: u32 = 64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Options {
    mask: u32,
    base: u32,
    table_base: u32,
    priority: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mask: 0xffff0000,
            base: 0x504d0000,
            table_base: 52100,
            priority: 9000,
        }
    }
}

fn number(value: &str) -> Result<u32, String> {
    if let Some(hex) = value.strip_prefix("0x") {
        u32::from_str_radix(hex, 16)
    } else {
        value.parse()
    }
    .map_err(|_| format!("invalid routing number: {value}"))
}

impl Options {
    fn environment() -> Result<Self, String> {
        let mut value = Self::default();
        for (name, field) in [
            ("PMMAN_PORT_FORWARD_MARK_MASK", &mut value.mask),
            ("PMMAN_PORT_FORWARD_MARK_BASE", &mut value.base),
            ("PMMAN_PORT_FORWARD_TABLE_BASE", &mut value.table_base),
            ("PMMAN_PORT_FORWARD_RULE_PRIORITY", &mut value.priority),
        ] {
            if let Ok(raw) = env::var(name) {
                *field = number(&raw)?;
            }
        }
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), String> {
        if self.mask == 0
            || (!self.mask).checked_add(1).is_none()
            || ((!self.mask) & ((!self.mask).wrapping_add(1))) != 0
            || self.base == 0
            || self.base & !self.mask != 0
            || self.priority == 0
            || self.priority >= 32766
            || self.table_base < 256
            || self.table_base.checked_add(SLOTS).is_none()
            || self.mark(SLOTS - 1).is_err()
        {
            return Err("invalid port-forward routing mask/base/table/priority".into());
        }
        Ok(())
    }

    fn mark(&self, slot: u32) -> Result<u32, String> {
        let offset = slot
            .checked_mul((!self.mask).wrapping_add(1))
            .ok_or("routing mark range overflow")?;
        self.base
            .checked_add(offset)
            .filter(|v| v & !self.mask == 0)
            .ok_or_else(|| "routing mark range overflow".into())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Ingress {
    pub listen_address: String,
    pub local_address: String,
    pub interface: String,
    pub gateway: Option<String>,
}

impl Ingress {
    fn validate(&self, config: &NodeConfig) -> Result<(), String> {
        let listen = self
            .listen_address
            .parse::<IpAddr>()
            .map_err(|_| "invalid ingress listen address")?;
        let local = self
            .local_address
            .parse::<IpAddr>()
            .map_err(|_| "invalid ingress local address")?;
        if listen.is_ipv4() != local.is_ipv4()
            || local.is_unspecified()
            || local.is_multicast()
            || local.is_loopback()
            || !valid_interface(&self.interface)
            || self.interface == config.interface
        {
            return Err("invalid port-forward ingress interface/address".into());
        }
        if let Some(gateway) = &self.gateway {
            let address = gateway
                .parse::<IpAddr>()
                .map_err(|_| "invalid ingress gateway")?;
            if address.is_ipv4() != local.is_ipv4()
                || address.is_unspecified()
                || address.is_multicast()
                || address.is_loopback()
            {
                return Err("invalid ingress gateway family/address".into());
            }
        }
        Ok(())
    }
}

fn valid_interface(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 15
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Path {
    pub ingress: Ingress,
    slot: u32,
    #[serde(default)]
    retired_at: Option<i64>,
}

impl Path {
    fn family(&self) -> &'static str {
        if self.ingress.local_address.contains(':') {
            "-6"
        } else {
            "-4"
        }
    }
    fn nft_family(&self) -> &'static str {
        if self.family() == "-4" {
            "ip"
        } else {
            "ip6"
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct Routing {
    options: Options,
    paths: Vec<Path>,
    #[serde(skip)]
    file: PathBuf,
}

fn ip(config: &NodeConfig, args: &[String]) -> Result<String, String> {
    let output = kernel_output(
        Command::new(&config.ip_binary).args(args),
        config.command_deadline,
    )?;
    if !output.status.success() {
        return Err(format!(
            "port-forward ip {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).into()).collect()
}
fn json(config: &NodeConfig, args: &[&str]) -> Result<Vec<Value>, String> {
    serde_json::from_str(&ip(config, &strings(args))?).map_err(|e| format!("invalid ip JSON: {e}"))
}
fn numeric(value: Option<&Value>) -> Option<u32> {
    value.and_then(|v| {
        v.as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .or_else(|| v.as_str().and_then(|s| number(s).ok()))
    })
}

pub(super) fn discover(config: &NodeConfig, listen: &str) -> Result<Ingress, String> {
    let address = listen
        .parse::<IpAddr>()
        .map_err(|_| "invalid entry listen address")?;
    let family = if address.is_ipv4() { "-4" } else { "-6" };
    let addresses = json(config, &["-j", family, "address", "show"])?;
    if let Ok(raw) = env::var("PMMAN_PORT_FORWARD_INGRESS_ROUTES") {
        let overrides: Vec<Ingress> = serde_json::from_str(&raw)
            .map_err(|_| "invalid PMMAN_PORT_FORWARD_INGRESS_ROUTES JSON")?;
        let matching: Vec<_> = overrides
            .into_iter()
            .filter(|v| v.listen_address.parse::<IpAddr>().ok() == Some(address))
            .collect();
        if matching.len() > 1 {
            return Err("duplicate ingress overrides".into());
        }
        if let Some(value) = matching.into_iter().next() {
            value.validate(config)?;
            if !has_address(&addresses, &value.interface, &value.local_address) {
                return Err("configured ingress address is not assigned to the interface".into());
            }
            return Ok(value);
        }
    }
    let defaults = json(
        config,
        &["-j", family, "route", "show", "table", "main", "default"],
    )?;
    select_ingress(config, listen, &addresses, &defaults)
}

fn has_address(addresses: &[Value], interface: &str, address: &str) -> bool {
    let address = address.parse::<IpAddr>().ok();
    addresses.iter().any(|link| {
        link["ifname"].as_str() == Some(interface)
            && link["addr_info"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item["local"]
                        .as_str()
                        .and_then(|s| s.parse::<IpAddr>().ok())
                        == address
                })
            })
    })
}

fn select_ingress(
    config: &NodeConfig,
    listen: &str,
    addresses: &[Value],
    defaults: &[Value],
) -> Result<Ingress, String> {
    let listen_ip = listen
        .parse::<IpAddr>()
        .map_err(|_| "invalid ingress listen address")?;
    let local_interface = addresses.iter().find(|link| {
        link["ifname"]
            .as_str()
            .is_some_and(|dev| has_address(addresses, dev, listen))
    });
    let selected_interface = local_interface.and_then(|link| link["ifname"].as_str());
    let mut routes: Vec<_> = defaults
        .iter()
        .filter(|route| {
            let dev = route["dev"].as_str().unwrap_or("");
            valid_interface(dev)
                && dev != config.interface
                && dev != "lo"
                && selected_interface.is_none_or(|expected| expected == dev)
                && route.get("nexthops").is_none()
                && route.get("multipath").is_none()
                && route.get("type").is_none_or(|v| v == "unicast")
        })
        .collect();
    routes.sort_by_key(|route| route["metric"].as_u64().unwrap_or(0));
    // Do not guess which private interface receives an EIP on multihomed hosts.
    let interfaces: std::collections::HashSet<_> =
        routes.iter().map(|v| v["dev"].as_str()).collect();
    if routes.is_empty()
        || interfaces.len() != 1
        || (routes.len() > 1 && routes[0]["metric"] == routes[1]["metric"])
    {
        return Err("cannot uniquely determine ingress return path; configure PMMAN_PORT_FORWARD_INGRESS_ROUTES".into());
    }
    let route = routes[0];
    let interface = route["dev"].as_str().unwrap().to_owned();
    let local_address = if selected_interface.is_some() {
        listen.to_owned()
    } else {
        route["prefsrc"].as_str().filter(|s| has_address(addresses, &interface, s)).map(str::to_owned)
            .or_else(|| {
                let candidates: Vec<_> = addresses.iter().filter(|l| l["ifname"].as_str() == Some(&interface))
                    .filter_map(|l| l["addr_info"].as_array()).flatten()
                    .filter(|a| a["scope"] == "global")
                    .filter_map(|a| a["local"].as_str()).collect();
                (candidates.len() == 1).then(|| candidates[0].to_owned())
            }).ok_or("cannot uniquely identify translated ingress address; configure PMMAN_PORT_FORWARD_INGRESS_ROUTES")?
    };
    let value = Ingress {
        listen_address: listen.to_owned(),
        local_address,
        interface,
        gateway: route["gateway"].as_str().map(str::to_owned),
    };
    value.validate(config)?;
    if value.local_address.parse::<IpAddr>().unwrap().is_ipv4() != listen_ip.is_ipv4() {
        return Err("ingress family mismatch".into());
    }
    Ok(value)
}

impl Routing {
    pub fn load(config: &NodeConfig) -> Result<Self, String> {
        let options = Options::environment()?;
        let file = PathBuf::from(format!("{}.port-forward-routing.json", config.version_path));
        let mut value = match fs::read(&file) {
            Ok(bytes) => serde_json::from_slice::<Self>(&bytes)
                .map_err(|_| "invalid port-forward routing ownership journal")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self {
                options: options.clone(),
                paths: vec![],
                file: file.clone(),
            },
            Err(e) => return Err(format!("cannot read routing ownership journal: {e}")),
        };
        if value.options != options {
            return Err("routing options changed while ownership journal exists; drain and remove old managed routes first".into());
        }
        let mut slots = std::collections::HashSet::new();
        for path in &value.paths {
            path.ingress.validate(config)?;
            if path.slot >= SLOTS || !slots.insert(path.slot) {
                return Err("invalid routing ownership slots".into());
            }
        }
        value.file = file;
        Ok(value)
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.file.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let temp = self.file.with_extension("tmp");
        let bytes = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        use std::io::Write;
        let mut file = fs::File::create(&temp).map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&temp, &self.file).map_err(|e| e.to_string())
    }

    pub fn prepare(&mut self, config: &NodeConfig, ingress: Ingress) -> Result<Path, String> {
        let existing = self.paths.iter().find(|p| p.ingress == ingress).cloned();
        let path = existing.clone().unwrap_or_else(|| Path {
            ingress,
            slot: (0..SLOTS)
                .find(|slot| !self.paths.iter().any(|p| p.slot == *slot))
                .unwrap_or(SLOTS),
            retired_at: None,
        });
        if path.slot >= SLOTS {
            return Err("port-forward ingress routing capacity reached (64); drain old connections before reuse".into());
        }
        self.check(config, &path, existing.is_some())?;
        if existing.is_none() {
            // Journal ownership before kernel writes, so a crash can be retried safely.
            self.paths.push(path.clone());
            self.save()?;
        }
        self.install(config, &path)?;
        Ok(path)
    }

    fn table(&self, path: &Path) -> u32 {
        self.options.table_base + path.slot
    }
    pub fn mark(&self, path: &Path) -> u32 {
        self.options.mark(path.slot).unwrap()
    }

    fn check(&self, config: &NodeConfig, path: &Path, owned: bool) -> Result<(), String> {
        let family = path.family();
        let rules = json(config, &["-j", family, "rule", "show"])?;
        for rule in rules {
            let mark = numeric(rule.get("fwmark"));
            let mask = numeric(rule.get("fwmask")).unwrap_or(u32::MAX);
            let our = self
                .paths
                .iter()
                .any(|p| p.family() == family && self.matches_rule(p, &rule));
            if !our
                && (numeric(rule.get("priority")) == Some(self.options.priority)
                    || numeric(rule.get("table")) == Some(self.table(path))
                    || mark.is_some_and(|v| (v ^ self.mark(path)) & mask & self.options.mask == 0))
            {
                return Err(
                    "port-forward routing mark, table or priority conflicts with an existing rule"
                        .into(),
                );
            }
        }
        // An empty, previously unused routing table is required. Never flush host tables.
        let table = self.table(path).to_string();
        let output = kernel_output(
            Command::new(&config.ip_binary).args(["-j", family, "route", "show", "table", &table]),
            config.command_deadline,
        )?;
        let text = String::from_utf8_lossy(&output.stdout);
        let routes: Vec<Value> = if output.status.success() {
            serde_json::from_str(&text).map_err(|_| "invalid route table JSON")?
        } else if String::from_utf8_lossy(&output.stderr).contains("FIB table does not exist") {
            vec![]
        } else {
            return Err("cannot inspect port-forward routing table".into());
        };
        for route in routes {
            if !owned || !self.owned_route(path, &route) {
                return Err(format!(
                    "routing table {table} is occupied; refusing to replace host routes"
                ));
            }
        }
        Ok(())
    }

    fn owned_route(&self, path: &Path, route: &Value) -> bool {
        if numeric(route.get("protocol")) != Some(242) || route["dst"] != "default" {
            return false;
        }
        if route["type"] == "unreachable" {
            return numeric(route.get("metric")) == Some(32760);
        }
        numeric(route.get("metric")) == Some(10)
            && route["dev"].as_str() == Some(&path.ingress.interface)
            && route["gateway"].as_str() == path.ingress.gateway.as_deref()
    }

    fn matches_rule(&self, path: &Path, rule: &Value) -> bool {
        numeric(rule.get("priority")) == Some(self.options.priority)
            && numeric(rule.get("fwmark")) == Some(self.mark(path))
            && numeric(rule.get("fwmask")).unwrap_or(u32::MAX) == self.options.mask
            && numeric(rule.get("table")) == Some(self.table(path))
            && numeric(rule.get("protocol")) == Some(242)
            && rule["src"] == "all"
            && rule.get("dst").is_none()
            && rule.get("iif").is_none()
            && rule.get("oif").is_none()
            && rule.get("not").is_none()
            && rule.get("action").is_none_or(|v| v == "to_tbl")
    }

    fn rule_args(&self, path: &Path, action: &str) -> Vec<String> {
        vec![
            path.family().into(),
            "rule".into(),
            action.into(),
            "priority".into(),
            self.options.priority.to_string(),
            "fwmark".into(),
            format!("{:#x}/{:#x}", self.mark(path), self.options.mask),
            "table".into(),
            self.table(path).to_string(),
            "protocol".into(),
            PROTOCOL.into(),
        ]
    }

    fn default_args(&self, path: &Path, action: &str, unreachable: bool) -> Vec<String> {
        let mut args = strings(&[path.family(), "route", action]);
        if unreachable {
            args.push("unreachable".into());
        }
        args.push("default".into());
        if !unreachable {
            if let Some(gateway) = &path.ingress.gateway {
                args.extend(["via".into(), gateway.clone()]);
            }
            args.extend(["dev".into(), path.ingress.interface.clone()]);
            if path.ingress.gateway.is_some() {
                args.push("onlink".into());
            }
        }
        args.extend([
            "table".into(),
            self.table(path).to_string(),
            "proto".into(),
            PROTOCOL.into(),
            "metric".into(),
            if unreachable { "32760" } else { "10" }.into(),
        ]);
        args
    }

    fn install(&self, config: &NodeConfig, path: &Path) -> Result<(), String> {
        // If the egress vanishes, do not silently fall through to host source policies.
        ip(config, &self.default_args(path, "replace", true))?;
        ip(config, &self.default_args(path, "replace", false))?;
        let rules = json(config, &["-j", path.family(), "rule", "show"])?;
        if !rules.iter().any(|rule| self.matches_rule(path, rule)) {
            ip(config, &self.rule_args(path, "add"))?;
        }
        Ok(())
    }

    pub fn dnat_mark(&self, path: &Path) -> String {
        format!(
            "ct mark set (ct mark & {:#x}) | {:#x}",
            !self.options.mask,
            self.mark(path)
        )
    }

    pub fn reply_rules(&self, family: &str) -> String {
        let mut script = String::new();
        for path in self.paths.iter().filter(|p| p.nft_family() == family) {
            script.push_str(&format!("add rule {family} pmman_network_port_forward return_mark ct direction reply ct status dnat ct mark & {:#x} == {:#x} meta mark set (meta mark & {:#x}) | {:#x}\n",
                self.options.mask, self.mark(path), !self.options.mask, self.mark(path)));
        }
        script
    }

    pub fn masquerade_rule(&self, path: &Path) -> String {
        format!("add rule {} pmman_network_port_forward postrouting ct direction original ct status dnat ct mark & {:#x} == {:#x} masquerade\n",
            path.nft_family(), self.options.mask, self.mark(path))
    }

    pub fn retained_rules(&self) -> bool {
        !self.paths.is_empty()
    }

    pub fn collect_unused(&mut self, config: &NodeConfig, active: &[Path]) -> Result<(), String> {
        let now = super::unix_now();
        let mut changed = false;
        for path in &mut self.paths {
            if active.iter().any(|p| p.slot == path.slot) {
                changed |= path.retired_at.take().is_some();
            } else if path.retired_at.is_none() {
                path.retired_at = Some(now);
                changed = true;
            }
        }
        if changed {
            self.save()?;
        }
        if self
            .paths
            .iter()
            .all(|p| active.iter().any(|a| a.slot == p.slot))
        {
            return Ok(());
        }
        // No tool/package dependency. If the kernel does not expose conntrack,
        // retain bounded ownership slots rather than disrupt established sessions.
        let Ok(text) = fs::read_to_string("/proc/net/nf_conntrack") else {
            return Ok(());
        };
        let marks: Vec<u32> = text
            .lines()
            .filter_map(|line| {
                line.split_whitespace()
                    .find_map(|word| word.strip_prefix("mark=").and_then(|v| number(v).ok()))
            })
            .collect();
        let mut removed = vec![];
        for path in &self.paths {
            if active.iter().any(|p| p.slot == path.slot)
                || path
                    .retired_at
                    .is_none_or(|at| now.saturating_sub(at) < 120)
                || marks
                    .iter()
                    .any(|m| m & self.options.mask == self.mark(path))
            {
                continue;
            }
            self.check(config, path, true)?;
            let rules = json(config, &["-j", path.family(), "rule", "show"])?;
            if rules.iter().any(|r| self.matches_rule(path, r)) {
                ip(config, &self.rule_args(path, "del"))?;
            }
            // Delete only exact journaled routes, never a whole table.
            for unreachable in [false, true] {
                let args = self.default_args(path, "del", unreachable);
                let output = kernel_output(
                    Command::new(&config.ip_binary).args(&args),
                    config.command_deadline,
                )?;
                if !output.status.success()
                    && !String::from_utf8_lossy(&output.stderr).contains("No such process")
                {
                    return Err("cannot remove managed return route".into());
                }
            }
            removed.push(path.slot);
        }
        if !removed.is_empty() {
            self.paths.retain(|p| !removed.contains(&p.slot));
            self.save()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_namespace_is_masked_bounded_and_preserves_foreign_bits() {
        let options = Options::default();
        options.validate().unwrap();
        for slot in 0..SLOTS {
            let mark = options.mark(slot).unwrap();
            assert_eq!(mark & !options.mask, 0);
            assert_eq!(((0x1234u32 & !options.mask) | mark) & !options.mask, 0x1234);
        }
        for mask in [0, 0xff00ff00] {
            assert!(Options {
                mask,
                ..options.clone()
            }
            .validate()
            .is_err());
        }
        assert!(Options {
            base: 0xffff0000,
            ..options
        }
        .validate()
        .is_err());
    }

    #[test]
    fn reply_mark_is_not_applied_to_original_requests_or_other_connections() {
        let value = Routing {
            options: Options::default(),
            file: PathBuf::new(),
            paths: vec![Path {
                ingress: Ingress {
                    listen_address: "198.51.100.1".into(),
                    local_address: "192.0.2.2".into(),
                    interface: "eth0".into(),
                    gateway: Some("192.0.2.1".into()),
                },
                slot: 0,
                retired_at: None,
            }],
        };
        let script = value.reply_rules("ip");
        assert!(script.contains("ct direction reply ct status dnat"));
        assert!(script.contains("meta mark & 0xffff"));
        assert!(value.reply_rules("ip6").is_empty());
        assert!(value
            .masquerade_rule(&value.paths[0])
            .contains("ct direction original"));
    }

    #[cfg(unix)]
    #[test]
    fn ingress_discovery_rejects_ambiguous_cloud_nat_and_matches_local_interface() {
        let fixture = super::super::tests::KernelFixture::new();
        let config = fixture.state.config.read().unwrap();
        let addresses = serde_json::json!([
            {"ifname":"eth0","addr_info":[{"local":"192.0.2.2","scope":"global"}]},
            {"ifname":"eth1","addr_info":[{"local":"198.51.100.2","scope":"global"}]}]);
        let routes = serde_json::json!([
            {"dev":"eth0","gateway":"192.0.2.1","prefsrc":"192.0.2.2","metric":100},
            {"dev":"eth1","gateway":"198.51.100.1","prefsrc":"198.51.100.2","metric":200}]);
        assert!(select_ingress(
            &config,
            "203.0.113.2",
            addresses.as_array().unwrap(),
            routes.as_array().unwrap()
        )
        .is_err());
        let local = select_ingress(
            &config,
            "198.51.100.2",
            addresses.as_array().unwrap(),
            routes.as_array().unwrap(),
        )
        .unwrap();
        assert_eq!(local.interface, "eth1");
        assert_eq!(local.gateway.as_deref(), Some("198.51.100.1"));
    }
}

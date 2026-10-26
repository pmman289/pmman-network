//! Client-owned cnipgen route snapshot.
//!
//! The snapshot is compiled into the service and is never refreshed while a
//! connection is running. The generator validates the same invariants before
//! publishing a new snapshot, and this module validates the embedded bytes
//! again so a damaged release cannot silently install an incomplete route set.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::sync::OnceLock;

pub const SOURCE: &str = "client_builtin_v1";

const IPV4_TEXT: &str = include_str!("../resources/cnipgen/overseas-v4.txt");
const IPV6_TEXT: &str = include_str!("../resources/cnipgen/overseas-v6.txt");
const METADATA_TEXT: &str = include_str!("../resources/cnipgen/metadata.json");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub version: &'static str,
    pub generated_at: &'static str,
    pub ipv4_count: usize,
    pub ipv6_count: usize,
    pub ipv4_sha256: &'static str,
    pub ipv6_sha256: &'static str,
}

#[derive(Debug)]
pub struct Snapshot {
    pub metadata: Metadata,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
}

#[derive(Deserialize)]
struct RawMetadata {
    generator_version: String,
    generated_at: String,
    ipv4_count: usize,
    ipv6_count: usize,
    ipv4_sha256: String,
    ipv6_sha256: String,
}

static SNAPSHOT: OnceLock<Result<Snapshot, String>> = OnceLock::new();

pub fn snapshot() -> Result<&'static Snapshot, String> {
    SNAPSHOT
        .get_or_init(|| load_snapshot())
        .as_ref()
        .map_err(Clone::clone)
}

fn load_snapshot() -> Result<Snapshot, String> {
    let raw: RawMetadata = serde_json::from_str(METADATA_TEXT)
        .map_err(|error| format!("cnipgen 元数据无效：{error}"))?;
    if raw.generator_version != SOURCE {
        return Err(format!(
            "cnipgen 规则版本不受支持：{}",
            raw.generator_version
        ));
    }
    let ipv4 = parse_routes(IPV4_TEXT, 4)?;
    let ipv6 = parse_routes(IPV6_TEXT, 6)?;
    validate_family_exclusions(&ipv4, &ipv6)?;
    if ipv4.len() != raw.ipv4_count || ipv6.len() != raw.ipv6_count {
        return Err("cnipgen 规则数量与元数据不一致".into());
    }
    if sha256(IPV4_TEXT.as_bytes()) != raw.ipv4_sha256
        || sha256(IPV6_TEXT.as_bytes()) != raw.ipv6_sha256
    {
        return Err("cnipgen 规则 SHA-256 校验失败".into());
    }
    let generated_at = leak_string(raw.generated_at)?;
    let ipv4_sha256 = leak_string(raw.ipv4_sha256)?;
    let ipv6_sha256 = leak_string(raw.ipv6_sha256)?;
    Ok(Snapshot {
        metadata: Metadata {
            version: SOURCE,
            generated_at,
            ipv4_count: raw.ipv4_count,
            ipv6_count: raw.ipv6_count,
            ipv4_sha256,
            ipv6_sha256,
        },
        ipv4,
        ipv6,
    })
}

fn leak_string(value: String) -> Result<&'static str, String> {
    if value.is_empty() {
        return Err("cnipgen 元数据包含空值".into());
    }
    Ok(Box::leak(value.into_boxed_str()))
}

fn sha256(value: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(value);
    format!("{:x}", digest.finalize())
}

fn parse_routes(text: &str, family: u8) -> Result<Vec<String>, String> {
    let mut routes = Vec::new();
    let mut previous: Option<(u128, u8)> = None;
    for (line_number, line) in text.lines().enumerate() {
        let value = line.trim();
        if value.is_empty() {
            continue;
        }
        let (address, prefix) = value
            .split_once('/')
            .ok_or_else(|| format!("cnipgen 规则第 {} 行缺少前缀", line_number + 1))?;
        let address = address
            .parse::<IpAddr>()
            .map_err(|error| format!("cnipgen 规则第 {} 行地址无效：{error}", line_number + 1))?;
        let prefix = prefix
            .parse::<u8>()
            .map_err(|error| format!("cnipgen 规则第 {} 行前缀无效：{error}", line_number + 1))?;
        let (numeric, maximum) = match address {
            IpAddr::V4(address) if family == 4 => (u32::from(address) as u128, 32),
            IpAddr::V6(address) if family == 6 => (u128::from(address), 128),
            _ => return Err(format!("cnipgen 规则第 {} 行地址族错误", line_number + 1)),
        };
        if prefix == 0 || prefix > maximum || !is_network_address(numeric, prefix, maximum) {
            return Err(format!(
                "cnipgen 规则第 {} 行不是合法非默认网段",
                line_number + 1
            ));
        }
        let key = (numeric, prefix);
        if previous.is_some_and(|item| item >= key) {
            return Err("cnipgen 规则必须严格按地址和前缀排序且去重".into());
        }
        previous = Some(key);
        routes.push(value.to_string());
    }
    if routes.is_empty() {
        return Err("cnipgen 规则为空".into());
    }
    Ok(routes)
}

fn is_network_address(value: u128, prefix: u8, maximum: u8) -> bool {
    if prefix == maximum {
        return true;
    }
    let host_bits = maximum - prefix;
    value & ((1u128 << host_bits) - 1) == 0
}

fn validate_family_exclusions(ipv4: &[String], ipv6: &[String]) -> Result<(), String> {
    let v4_special = [
        "0.0.0.0/8",
        "10.0.0.0/8",
        "100.64.0.0/10",
        "127.0.0.0/8",
        "169.254.0.0/16",
        "172.16.0.0/12",
        "192.0.0.0/24",
        "192.0.2.0/24",
        "192.168.0.0/16",
        "198.18.0.0/15",
        "198.51.100.0/24",
        "203.0.113.0/24",
        "224.0.0.0/4",
        "240.0.0.0/4",
    ];
    let v6_special = [
        "::/128",
        "::1/128",
        "::ffff:0:0/96",
        "100::/64",
        "2001:db8::/32",
        "2001::/32",
        "2002::/16",
        "fc00::/7",
        "fe80::/10",
        "ff00::/8",
    ];
    for value in v4_special {
        if ipv4.iter().any(|route| overlaps(route, value)) {
            return Err(format!("IPv4 海外规则包含特殊用途地址：{value}"));
        }
    }
    for value in v6_special {
        if ipv6.iter().any(|route| overlaps(route, value)) {
            return Err(format!("IPv6 海外规则包含特殊用途地址：{value}"));
        }
    }
    if ipv6
        .iter()
        .any(|route| !contains_network("2000::/3", route))
    {
        return Err("IPv6 海外规则超出全球单播范围".into());
    }
    Ok(())
}

fn overlaps(left: &str, right: &str) -> bool {
    let Some((left_address, left_prefix)) = parse_network(left) else {
        return false;
    };
    let Some((right_address, right_prefix)) = parse_network(right) else {
        return false;
    };
    if left_address.is_ipv4() != right_address.is_ipv4() {
        return false;
    }
    let maximum = if left_address.is_ipv4() { 32 } else { 128 };
    let left_value = address_value(left_address);
    let right_value = address_value(right_address);
    let shared = left_prefix.min(right_prefix);
    if shared == 0 {
        return true;
    }
    let shift = maximum - shared;
    (left_value >> shift) == (right_value >> shift)
}

fn contains_network(container: &str, child: &str) -> bool {
    let Some((container_address, container_prefix)) = parse_network(container) else {
        return false;
    };
    let Some((child_address, child_prefix)) = parse_network(child) else {
        return false;
    };
    if container_address.is_ipv4() != child_address.is_ipv4() || child_prefix < container_prefix {
        return false;
    }
    let maximum = if container_address.is_ipv4() { 32 } else { 128 };
    let shift = maximum - container_prefix;
    (address_value(container_address) >> shift) == (address_value(child_address) >> shift)
}

fn parse_network(value: &str) -> Option<(IpAddr, u8)> {
    let (address, prefix) = value.split_once('/')?;
    Some((address.parse().ok()?, prefix.parse().ok()?))
}

fn address_value(address: IpAddr) -> u128 {
    match address {
        IpAddr::V4(value) => u32::from(value) as u128,
        IpAddr::V6(value) => u128::from(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_snapshot_is_valid_and_has_both_families() {
        let snapshot = snapshot().expect("embedded route snapshot should validate");
        assert_eq!(snapshot.metadata.version, SOURCE);
        assert!(snapshot.metadata.ipv4_count > 1000);
        assert!(snapshot.metadata.ipv6_count > 1000);
    }

    #[test]
    fn parser_rejects_default_route_and_wrong_family() {
        assert!(parse_routes("0.0.0.0/0\n", 4).is_err());
        assert!(parse_routes("2001:db8::/32\n", 4).is_err());
    }
}

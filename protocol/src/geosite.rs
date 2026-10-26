//! Reader for the V2Fly/Xray `geosite.dat` protobuf format.
//!
//! The master owns and distributes the binary file. Nodes parse only the
//! selected categories and turn them into matchers for the local resolver.
use prost::Message;
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
};

pub const MAX_FILE_SIZE: usize = 32 * 1024 * 1024;

#[derive(Clone, PartialEq, Message)]
struct GeoSiteList {
    #[prost(message, repeated, tag = "1")]
    entry: Vec<GeoSite>,
}

#[derive(Clone, PartialEq, Message)]
struct GeoSite {
    #[prost(string, tag = "1")]
    country_code: String,
    #[prost(message, repeated, tag = "2")]
    domain: Vec<Domain>,
}

#[derive(Clone, PartialEq, Message)]
struct Domain {
    #[prost(enumeration = "domain::Kind", tag = "1")]
    kind: i32,
    #[prost(string, tag = "2")]
    value: String,
    #[prost(message, repeated, tag = "3")]
    attribute: Vec<Attribute>,
}

#[derive(Clone, PartialEq, Message)]
struct Attribute {
    #[prost(string, tag = "1")]
    key: String,
    #[prost(oneof = "attribute::Value", tags = "2, 3")]
    value: Option<attribute::Value>,
}
mod attribute {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Value {
        #[prost(bool, tag = "2")]
        Bool(bool),
        #[prost(int64, tag = "3")]
        Int(i64),
    }
}

fn decode(data: &[u8]) -> Result<GeoSiteList, String> {
    if data.is_empty() || data.len() > MAX_FILE_SIZE {
        return Err("geosite 文件必须为 1–32 MiB 内的非空 protobuf 文件".into());
    }
    GeoSiteList::decode(data).map_err(|error| format!("geosite.dat 格式无效：{error}"))
}

pub fn categories(data: &[u8]) -> Result<Vec<String>, String> {
    let mut result: Vec<_> = decode(data)?
        .entry
        .into_iter()
        .map(|site| site.country_code.to_ascii_lowercase())
        .collect();
    result.sort();
    result.dedup();
    if result.is_empty() {
        return Err("geosite 文件没有分类".into());
    }
    Ok(result)
}

/// Compiled once on configuration change; shared between query workers.
/// Full/root-domain lookup does not scan the entire geosite list per query.
#[derive(Default)]
pub struct Router {
    full: HashMap<String, Vec<SocketAddr>>,
    suffix: HashMap<String, Vec<SocketAddr>>,
    keywords: Vec<(String, Vec<SocketAddr>)>,
    regexes: Vec<(Regex, Vec<SocketAddr>)>,
}
impl Router {
    pub fn compile(groups: &[(Vec<Rule>, Vec<SocketAddr>)]) -> Result<Self, String> {
        let mut result = Self::default();
        for (rules, upstreams) in groups {
            if !rules.is_empty() && upstreams.is_empty() {
                return Err("geosite 分类必须配置上游 DNS".into());
            }
            for rule in rules {
                match &rule.matcher {
                    Matcher::Full(value) => {
                        result
                            .full
                            .entry(value.clone())
                            .or_insert_with(|| upstreams.clone());
                    }
                    Matcher::Suffix(value) => {
                        result
                            .suffix
                            .entry(value.clone())
                            .or_insert_with(|| upstreams.clone());
                    }
                    Matcher::Keyword(value) => {
                        result.keywords.push((value.clone(), upstreams.clone()))
                    }
                    Matcher::Regex(value) => result.regexes.push((
                        Regex::new(value).map_err(|error| error.to_string())?,
                        upstreams.clone(),
                    )),
                }
            }
        }
        result
            .keywords
            .sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
        Ok(result)
    }
    pub fn select(&self, name: &str) -> Option<&Vec<SocketAddr>> {
        if let Some(values) = self.full.get(name) {
            return Some(values);
        }
        let mut suffix = name;
        loop {
            if let Some(values) = self.suffix.get(suffix) {
                return Some(values);
            }
            let Some((_, rest)) = suffix.split_once('.') else {
                break;
            };
            suffix = rest;
        }
        if let Some((_, values)) = self.keywords.iter().find(|(key, _)| name.contains(key)) {
            return Some(values);
        }
        self.regexes
            .iter()
            .find(|(re, _)| re.is_match(name))
            .map(|(_, values)| values)
    }
}

mod domain {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
    #[repr(i32)]
    pub enum Kind {
        Plain = 0,
        Regex = 1,
        RootDomain = 2,
        Full = 3,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Matcher {
    Keyword(String),
    Regex(String),
    Suffix(String),
    Full(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub matcher: Matcher,
}

/// Decode one or more geosite categories. Category names are case-insensitive.
/// Duplicate entries are removed while preserving the first occurrence.
pub fn rules(data: &[u8], categories: &[&str]) -> Result<Vec<Rule>, String> {
    let list = decode(data)?;
    let wanted: HashSet<String> = categories
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect();
    if wanted.is_empty() {
        return Err("geosite 分类不能为空".into());
    }
    let mut seen = HashSet::new();
    let mut found = HashSet::new();
    let mut output = Vec::new();
    for site in list.entry {
        let code = site.country_code.to_ascii_lowercase();
        let selectors: Vec<_> = wanted
            .iter()
            .filter(|selector| selector.split('@').next() == Some(code.as_str()))
            .collect();
        if selectors.is_empty() {
            continue;
        }
        found.extend(selectors.iter().map(|selector| (*selector).clone()));
        for domain in site.domain {
            if !selectors.iter().any(|selector| {
                selector.split_once('@').map_or(true, |(_, attr)| {
                    domain
                        .attribute
                        .iter()
                        .any(|item| item.key.eq_ignore_ascii_case(attr))
                })
            }) {
                continue;
            }
            let kind = domain::Kind::try_from(domain.kind).map_err(|_| "geosite 含未知规则类型")?;
            let value = if kind == domain::Kind::Regex {
                domain.value.clone()
            } else {
                domain.value.trim().trim_matches('.').to_ascii_lowercase()
            };
            if value.is_empty() || !seen.insert((domain.kind, value.clone())) {
                continue;
            }
            let matcher = match kind {
                domain::Kind::Plain => Matcher::Keyword(value),
                domain::Kind::Regex => {
                    Regex::new(&value)
                        .map_err(|error| format!("geosite 正则无效：{value}: {error}"))?;
                    Matcher::Regex(value)
                }
                domain::Kind::RootDomain => Matcher::Suffix(value),
                domain::Kind::Full => Matcher::Full(value),
            };
            output.push(Rule { matcher });
        }
    }
    if found != wanted {
        return Err(format!(
            "geosite 未找到分类：{}",
            wanted
                .difference(&found)
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if output.is_empty() {
        return Err(format!("geosite 未找到分类：{}", categories.join(",")));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_selected_category_and_rejects_bad_regex() {
        let data = GeoSiteList {
            entry: vec![GeoSite {
                country_code: "CN".into(),
                domain: vec![
                    Domain {
                        kind: domain::Kind::RootDomain as i32,
                        value: "example.cn".into(),
                        attribute: vec![],
                    },
                    Domain {
                        kind: domain::Kind::Full as i32,
                        value: "www.example.com".into(),
                        attribute: vec![],
                    },
                ],
            }],
        }
        .encode_to_vec();
        let rules = rules(&data, &["cn"]).unwrap();
        assert_eq!(rules.len(), 2);
        assert!(matches!(rules[0].matcher, Matcher::Suffix(_)));
    }

    #[test]
    fn attributes_are_protobuf_messages_and_all_requested_categories_must_exist() {
        let data = GeoSiteList {
            entry: vec![GeoSite {
                country_code: "CN".into(),
                domain: vec![Domain {
                    kind: domain::Kind::Full as i32,
                    value: "WWW.Example.CN".into(),
                    attribute: vec![Attribute {
                        key: "test".into(),
                        value: Some(attribute::Value::Bool(true)),
                    }],
                }],
            }],
        }
        .encode_to_vec();
        assert_eq!(
            rules(&data, &["cn@test"]).unwrap()[0].matcher,
            Matcher::Full("www.example.cn".into())
        );
        assert!(rules(&data, &["cn@missing"]).is_err());
        assert!(rules(&data, &["cn", "missing"]).is_err());
        assert!(rules(&[], &["cn"]).is_err());
        assert!(rules(&[0xff, 0x01], &["cn"]).is_err());
    }

    #[test]
    fn compiled_router_matches_boundaries_and_regex_without_recompiling() {
        let domestic: SocketAddr = "192.0.2.1:53".parse().unwrap();
        let foreign: SocketAddr = "192.0.2.2:53".parse().unwrap();
        let table = Router::compile(&[
            (
                vec![
                    Rule {
                        matcher: Matcher::Suffix("example.cn".into()),
                    },
                    Rule {
                        matcher: Matcher::Regex(r"^re\d+\.example$".into()),
                    },
                ],
                vec![domestic],
            ),
            (
                vec![Rule {
                    matcher: Matcher::Full("special.example.cn".into()),
                }],
                vec![foreign],
            ),
        ])
        .unwrap();
        assert_eq!(table.select("a.example.cn"), Some(&vec![domestic]));
        assert_eq!(table.select("special.example.cn"), Some(&vec![foreign]));
        assert_eq!(table.select("re12.example"), Some(&vec![domestic]));
        assert!(table.select("notexample.cn").is_none());
        assert!(table.select("example.cn.evil").is_none());
        let bad = GeoSiteList {
            entry: vec![GeoSite {
                country_code: "CN".into(),
                domain: vec![Domain {
                    kind: 1,
                    value: "[".into(),
                    attribute: vec![],
                }],
            }],
        }
        .encode_to_vec();
        assert!(rules(&bad, &["cn"]).is_err());
    }

    #[test]
    fn optional_real_geosite_fixture() {
        let Ok(path) = std::env::var("PMMAN_TEST_GEOSITE") else {
            return;
        };
        let data = std::fs::read(path).unwrap();
        let domestic: SocketAddr = "192.0.2.1:53".parse().unwrap();
        let foreign: SocketAddr = "192.0.2.2:53".parse().unwrap();
        let d = rules(&data, &["cn"]).unwrap();
        let f = rules(&data, &["geolocation-!cn"]).unwrap();
        assert!(d.len() > 100 && f.len() > 100);
        let table = Router::compile(&[(d, vec![domestic]), (f, vec![foreign])]).unwrap();
        assert_eq!(table.select("www.baidu.com"), Some(&vec![domestic]));
        assert_eq!(table.select("www.google.com"), Some(&vec![foreign]));
    }
}

use std::cmp::Reverse;
use std::fmt;
use std::net::Ipv6Addr;

use serde::{Deserialize, Serialize};

pub mod aliyun;
pub mod cloudflare;
mod config;
pub mod dns;

pub use config::{Config, Credentials, Provider};

pub const INTERVAL: u64 = 300;
pub const RETRY: u64 = 60;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AddressKind {
    Manual,
    Stable,
    Dhcp,
    Other,
    Temporary,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Ipv6Suffix(Vec<u16>);

impl Ipv6Suffix {
    pub fn parse(input: &str) -> Result<Self, &'static str> {
        const INVALID: &str = "请输入 IPv6 末尾的十六进制段，例如 ::8023 或 ::abcd:1234。";
        let input = input.trim();
        let suffix = input
            .strip_prefix("::")
            .or_else(|| input.strip_prefix(':'))
            .unwrap_or(input);
        let mut segments = Vec::new();
        for part in suffix.split(':') {
            if segments.len() == 8
                || part.is_empty()
                || part.len() > 4
                || !part.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(INVALID);
            }
            segments.push(u16::from_str_radix(part, 16).map_err(|_| INVALID)?);
        }
        Ok(Self(segments))
    }

    fn matches(&self, ip: Ipv6Addr) -> bool {
        ip.segments().ends_with(&self.0)
    }
}

impl TryFrom<String> for Ipv6Suffix {
    type Error = &'static str;

    fn try_from(input: String) -> Result<Self, Self::Error> {
        Self::parse(&input)
    }
}

impl From<Ipv6Suffix> for String {
    fn from(suffix: Ipv6Suffix) -> Self {
        suffix.to_string()
    }
}

impl fmt::Display for Ipv6Suffix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            let separator = if index == 0 { "::" } else { ":" };
            write!(f, "{separator}{segment:x}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum AddressPolicy {
    #[default]
    Stable,
    Suffix(Ipv6Suffix),
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub address: Ipv6Addr,
    pub kind: AddressKind,
    pub metric: u32,
    pub preferred_lifetime: u32,
}

pub fn public_v6(ip: Ipv6Addr) -> bool {
    let octets = ip.octets();
    // Global unicast space, excluding the IETF special-purpose block, 6to4 and documentation.
    octets[0] & 0xe0 == 0x20
        && !(octets[0] == 0x20 && octets[1] == 0x01 && octets[2] < 2)
        && octets[..4] != [0x20, 0x01, 0x0d, 0xb8]
        && octets[..2] != [0x20, 0x02]
        && !(octets[0] == 0x3f && octets[1] == 0xff && octets[2] < 0x10)
}

pub fn select_address(
    policy: &AddressPolicy,
    candidates: &[Candidate],
) -> Result<Ipv6Addr, &'static str> {
    candidates
        .iter()
        .filter(|candidate| candidate.preferred_lifetime != 0 && public_v6(candidate.address))
        .filter(|candidate| match policy {
            AddressPolicy::Stable => {
                matches!(candidate.kind, AddressKind::Manual | AddressKind::Stable)
            }
            AddressPolicy::Suffix(suffix) => {
                candidate.kind != AddressKind::Temporary && suffix.matches(candidate.address)
            }
        })
        .min_by_key(|c| (c.metric, c.kind, Reverse(c.preferred_lifetime), c.address))
        .map(|c| c.address)
        .ok_or(match policy {
            AddressPolicy::Stable => "当前没有可用的稳定公网 IPv6。",
            AddressPolicy::Suffix(_) => "当前没有符合后缀规则的非临时公网 IPv6。",
        })
}

pub fn dns_name(name: &str) -> Result<String, &'static str> {
    let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
    if name.len() > 253
        || !name.contains('.')
        || name.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        || name.parse::<std::net::IpAddr>().is_ok()
    {
        return Err("请填写完整域名，例如 home.example.com。");
    }
    Ok(name)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Settings {
    pub domain: String,
    #[serde(default)]
    pub provider: Provider,
    #[serde(default)]
    pub address: AddressPolicy,
    #[cfg(target_os = "macos")]
    #[serde(default)]
    pub prefer_ddns_source: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum Phase {
    #[default]
    Idle,
    Running,
    Success,
    Failed,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Snapshot {
    pub settings: Option<Settings>,
    pub automatic: bool,
    pub phase: Phase,
    pub message: String,
    pub address: Option<Ipv6Addr>,
    pub last_run: Option<u64>,
    pub next_run: Option<u64>,
    #[cfg(target_os = "macos")]
    #[serde(default)]
    pub source_preference_error: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum Request {
    Status,
    Run,
    Save {
        settings: Settings,
        credentials: Credentials,
        run: bool,
    },
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Reply {
    pub snapshot: Snapshot,
    pub error: Option<String>,
}

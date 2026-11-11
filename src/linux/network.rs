use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use ipv6_ddns::{AddressKind, Candidate, public_v6};
use serde_json::Value;

use super::{Result, error};

fn ip(arguments: &[&str]) -> Result<Value> {
    let executable = ["/usr/sbin/ip", "/usr/bin/ip", "/sbin/ip", "/bin/ip"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .ok_or("未找到 ip 命令；请安装 iproute2（例如 apt install iproute2）。")?;
    let output = Command::new(executable)
        .args(arguments)
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| error("运行 iproute2 失败", e))?;
    if !output.status.success() {
        return Err(format!(
            "读取 IPv6 网络信息失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "iproute2 未返回有效的 JSON 网络信息。".into())
}

fn flag(value: &Value, name: &str) -> bool {
    value.get(name).and_then(Value::as_bool).unwrap_or(false)
        || value
            .get("flags")
            .and_then(Value::as_array)
            .is_some_and(|flags| flags.iter().any(|flag| flag.as_str() == Some(name)))
}

fn lifetime(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .map(|n| n.min(u32::MAX as u64) as u32)
        .or_else(|| match value.as_str()? {
            "forever" => Some(u32::MAX),
            text => text.parse().ok(),
        })
}

pub fn candidates() -> Result<Vec<Candidate>> {
    let addresses = ip(&["-j", "-6", "address", "show", "up"])?;
    let routes = ip(&["-j", "-6", "route", "show", "default"])?;
    let interfaces = addresses
        .as_array()
        .ok_or("iproute2 IPv6 地址列表格式无效。")?;
    let routes = routes
        .as_array()
        .ok_or("iproute2 IPv6 路由列表格式无效。")?;
    let mut metrics = HashMap::<&str, u32>::new();
    for route in routes {
        if flag(route, "linkdown")
            || route
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind != "unicast")
        {
            continue;
        }
        let metric = route
            .get("metric")
            .and_then(Value::as_u64)
            .unwrap_or(1024)
            .min((u32::MAX - 1) as u64) as u32;
        if let Some(device) = route.get("dev").and_then(Value::as_str) {
            metrics
                .entry(device)
                .and_modify(|old| *old = (*old).min(metric))
                .or_insert(metric);
        }
        if let Some(nexthops) = route.get("nexthops").and_then(Value::as_array) {
            for hop in nexthops {
                if flag(hop, "dead") || flag(hop, "linkdown") {
                    continue;
                }
                if let Some(device) = hop.get("dev").and_then(Value::as_str) {
                    metrics
                        .entry(device)
                        .and_modify(|old| *old = (*old).min(metric))
                        .or_insert(metric);
                }
            }
        }
    }
    let mut result = Vec::new();
    for interface in interfaces {
        let Some(name) = interface.get("ifname").and_then(Value::as_str) else {
            continue;
        };
        if name == "lo" {
            continue;
        }
        let Some(addresses) = interface.get("addr_info").and_then(Value::as_array) else {
            continue;
        };
        for address in addresses {
            if address.get("family").and_then(Value::as_str) != Some("inet6")
                || address.get("scope").and_then(Value::as_str) != Some("global")
                || [
                    "temporary",
                    "secondary",
                    "tentative",
                    "optimistic",
                    "dadfailed",
                    "deprecated",
                ]
                .iter()
                .any(|name| flag(address, name))
            {
                continue;
            }
            let Some(local) = address.get("local").and_then(Value::as_str) else {
                continue;
            };
            let Ok(local) = local.parse() else {
                continue;
            };
            if !public_v6(local) {
                continue;
            }
            let preferred_lifetime = address
                .get("preferred_life_time")
                .and_then(lifetime)
                .unwrap_or(0);
            if preferred_lifetime == 0 {
                continue;
            }
            // Non-temporary SLAAC (including stable-privacy), DHCPv6 and static addresses
            // are stable local assignments. 'dynamic' alone does not imply temporary.
            let kind = if flag(address, "dynamic") || flag(address, "stable-privacy") {
                AddressKind::Stable
            } else {
                AddressKind::Manual
            };
            result.push(Candidate {
                address: local,
                kind,
                metric: metrics.get(name).copied().unwrap_or(u32::MAX),
                preferred_lifetime,
            });
        }
    }
    Ok(result)
}

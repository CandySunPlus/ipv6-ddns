use std::net::Ipv6Addr;

use serde_json::{Value, json};

const AUTO_TTL: u64 = 1;

fn id(value: &Value) -> Result<&str, String> {
    value["id"]
        .as_str()
        .filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| "Cloudflare 返回了无效的资源 ID。".into())
}

fn rows(result: &Value) -> Result<&[Value], String> {
    result
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| "Cloudflare 返回了无效的列表。".into())
}

pub fn update(
    domain: &str,
    address: Ipv6Addr,
    mut request: impl FnMut(&str, &str, Option<Value>) -> Result<Value, String>,
) -> Result<&'static str, String> {
    let mut suffix = domain;
    let zone = loop {
        let result = request("GET", &format!("/zones?name={suffix}&per_page=5"), None)?;
        match rows(&result)? {
            [zone] if zone["name"].as_str() == Some(suffix) => break id(zone)?.to_owned(),
            [] => (),
            _ => return Err("Cloudflare 返回了多个或不匹配的 Zone，请检查 Token 的范围。".into()),
        }
        suffix = suffix
            .split_once('.')
            .map(|(_, rest)| rest)
            .filter(|rest| rest.contains('.'))
            .ok_or("未找到对应的 Cloudflare Zone，请检查域名和 Token 的 Zone 读取权限。")?;
    };

    let base = format!("/zones/{zone}/dns_records");
    let path = format!("{base}?type=AAAA&name.exact={domain}&per_page=5");
    let found = request("GET", &path, None)?;
    let records = rows(&found)?;
    if records
        .iter()
        .any(|r| r["name"].as_str() != Some(domain) || r["type"].as_str() != Some("AAAA"))
    {
        return Err("Cloudflare 返回了不匹配的 DNS 记录。".into());
    }
    match records {
        [] => {
            let created = request(
                "POST",
                &base,
                Some(json!({
                    "type": "AAAA", "name": domain, "content": address.to_string(),
                    "ttl": AUTO_TTL, "proxied": false
                })),
            )?;
            if created["ttl"].as_u64() != Some(AUTO_TTL) {
                return Err("Cloudflare 未确认新记录的 TTL 为 Auto。".into());
            }
            Ok("已创建 AAAA 记录")
        }
        [record] => {
            let address_matches =
                record["content"].as_str().and_then(|s| s.parse().ok()) == Some(address);
            let ttl_matches = record["ttl"].as_u64() == Some(AUTO_TTL);
            let (body, message) = match (address_matches, ttl_matches) {
                (true, true) => return Ok("AAAA 地址已一致，TTL 为 Auto"),
                (true, false) => (json!({"ttl": AUTO_TTL}), "已将 AAAA 记录的 TTL 设为 Auto"),
                (false, true) => (json!({"content": address.to_string()}), "已更新 AAAA 记录"),
                (false, false) => (
                    json!({"content": address.to_string(), "ttl": AUTO_TTL}),
                    "已更新 AAAA 地址并将 TTL 设为 Auto",
                ),
            };
            let updated = request("PATCH", &format!("{base}/{}", id(record)?), Some(body))?;
            if updated["ttl"].as_u64() != Some(AUTO_TTL) {
                return Err("Cloudflare 未确认记录的 TTL 为 Auto。".into());
            }
            Ok(message)
        }
        _ => Err("此域名有多条 AAAA 记录。请在 Cloudflare 确认要保留的一条后再运行。".into()),
    }
}

use std::net::Ipv6Addr;
use std::time::Duration;

use serde_json::Value;

use super::{Result, error};

fn failure(status: u16, value: &Value) -> String {
    let code = value["errors"][0]["code"].as_i64();
    match (status, code) {
        (401 | 403, _) => "Cloudflare 拒绝了请求。请检查 Token、Zone 范围和 DNS 编辑权限。".into(),
        (429, _) => "Cloudflare 暂时限制了请求频率。".into(),
        (_, Some(81053 | 81054 | 81056)) => {
            "Cloudflare 中已有冲突的 DNS 记录，请检查同名记录。".into()
        }
        (_, Some(code)) => format!("Cloudflare 请求失败（HTTP {status}，API {code}）。"),
        _ => format!("Cloudflare 响应无效（HTTP {status}）。"),
    }
}

fn decode(status: u16, body: &str) -> Result<Value> {
    if body.len() > 512 * 1024 {
        return Err("Cloudflare 响应超出大小限制。".into());
    }
    let mut value = serde_json::from_str::<Value>(body).unwrap_or(Value::Null);
    if (200..300).contains(&status) && value["success"].as_bool() == Some(true) {
        return Ok(value["result"].take());
    }
    Err(failure(status, &value))
}

pub fn update(domain: &str, address: Ipv6Addr, token: &str) -> Result<&'static str> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .timeout_write(Duration::from_secs(7))
        .build();
    ipv6_ddns::cloudflare::update(domain, address, |method, path, body| {
        let url = format!("https://api.cloudflare.com/client/v4{path}");
        let request = agent
            .request(method, &url)
            .set("Authorization", &format!("Bearer {token}"))
            .set("Content-Type", "application/json");
        let result = match body {
            Some(body) => request.send_string(&body.to_string()),
            None => request.call(),
        };
        match result {
            Ok(response) => {
                let status = response.status();
                let body = response
                    .into_string()
                    .map_err(|e| error("读取 Cloudflare 响应失败", e))?;
                decode(status, &body)
            }
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                decode(status, &body)
            }
            Err(ureq::Error::Transport(e)) => Err(error("连接 Cloudflare 失败", e)),
        }
    })
}

use std::fmt::Write;
use std::io::Read;
use std::net::Ipv6Addr;
use std::time::Duration;

use serde_json::Value;

use crate::{Config, Credentials, aliyun, cloudflare};

const MAX_BODY: u64 = 512 * 1024;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .timeout_write(Duration::from_secs(7))
        .timeout(Duration::from_secs(20))
        .build()
}

fn response(
    result: Result<ureq::Response, ureq::Error>,
    provider: &str,
) -> Result<(u16, Value), String> {
    let response = match result {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        // ureq errors may include the URL, headers, or signed request. Never
        // format them into a user-visible error (nor into logs).
        Err(ureq::Error::Transport(_)) => {
            return Err(format!(
                "连接 {provider} 失败。请检查网络、代理和 HTTPS 连接（含超时）。"
            ));
        }
    };
    let status = response.status();
    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut body)
        .map_err(|_| format!("读取 {provider} 响应失败，请检查网络后重试。"))?;
    if body.len() as u64 > MAX_BODY {
        return Err(format!("{provider} 响应超出大小限制。"));
    }
    // A malformed body is treated as a failed response without echoing bytes.
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    Ok((status, value))
}

fn cloudflare_response(status: u16, mut value: Value) -> Result<Value, String> {
    if (200..300).contains(&status) && value["success"].as_bool() == Some(true) {
        return Ok(value["result"].take());
    }
    let code = value["errors"][0]["code"].as_i64();
    Err(match (status, code) {
        (401 | 403, _) => "Cloudflare 拒绝了请求。请检查 Token、Zone 范围和 DNS 编辑权限。".into(),
        (429, _) => "Cloudflare 暂时限制了请求频率。".into(),
        (_, Some(81053 | 81054 | 81056)) => {
            "Cloudflare 中已有冲突的 DNS 记录，请检查同名记录。".into()
        }
        (_, Some(code)) => format!("Cloudflare 请求失败（HTTP {status}，API {code}）。"),
        _ => format!("Cloudflare 响应无效（HTTP {status}）。"),
    })
}

fn nonce() -> Result<String, String> {
    let mut random = [0_u8; 32];
    getrandom::getrandom(&mut random)
        .map_err(|_| "无法获取系统安全随机数，未发送阿里云请求。".to_owned())?;
    let mut nonce = String::with_capacity(random.len() * 2);
    for byte in random {
        let _ = write!(nonce, "{byte:02x}");
    }
    Ok(nonce)
}

fn aliyun_request(
    agent: &ureq::Agent,
    action: &str,
    parameters: &aliyun::Params,
    access_key_id: &str,
    secret: &str,
) -> Result<Value, String> {
    let timestamp = time::OffsetDateTime::now_utc()
        .format(time::macros::format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second]Z"
        ))
        .map_err(|_| "无法生成阿里云请求时间，请检查系统时间。".to_owned())?;
    let mut parameters = parameters.clone();
    for (key, value) in [
        ("Action", action),
        ("Version", "2015-01-09"),
        ("Format", "JSON"),
        ("AccessKeyId", access_key_id),
        ("SignatureMethod", "HMAC-SHA1"),
        ("SignatureVersion", "1.0"),
        ("Timestamp", timestamp.as_str()),
    ] {
        parameters.insert(key.into(), value.into());
    }
    parameters.insert("SignatureNonce".into(), nonce()?);
    let signature = aliyun::rpc_signature("POST", &parameters, secret)?;
    parameters.insert("Signature".into(), signature);
    // Keep all credentials and signatures in the form body, never in a URL.
    let result = agent
        .post("https://alidns.aliyuncs.com/")
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&aliyun::encoded_params(&parameters));
    let (status, value) = response(result, "阿里云")?;
    aliyun::response(status, value)
}

/// Perform a single DNS synchronization through the common rustls HTTPS stack.
/// Provider-specific business logic also exposes callback seams for offline use.
pub fn update(config: &Config, address: Ipv6Addr) -> Result<&'static str, String> {
    if config.settings.provider != config.credentials.provider() {
        return Err("DNS 服务商与凭据类型不匹配，请重新保存设置。".into());
    }
    let agent = agent();
    let domain = &config.settings.domain;
    match &config.credentials {
        Credentials::Cloudflare { token } => {
            cloudflare::update(domain, address, |method, path, body| {
                let request = agent
                    .request(
                        method,
                        &format!("https://api.cloudflare.com/client/v4{path}"),
                    )
                    .set("Authorization", &format!("Bearer {token}"))
                    .set("Content-Type", "application/json");
                let result = match body {
                    Some(body) => request.send_string(&body.to_string()),
                    None => request.call(),
                };
                let (status, value) = response(result, "Cloudflare")?;
                cloudflare_response(status, value)
            })
        }
        Credentials::Aliyun {
            access_key_id,
            access_key_secret,
        } => aliyun::update(domain, address, |action, parameters| {
            aliyun_request(&agent, action, parameters, access_key_id, access_key_secret)
        }),
    }
}

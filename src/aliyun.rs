use std::collections::BTreeMap;
use std::fmt::Write;
use std::net::Ipv6Addr;

use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha1::Sha1;

/// RPC action parameters, before the common authentication parameters are added.
pub type Params = BTreeMap<String, String>;

fn params(values: &[(&str, &str)]) -> Params {
    values
        .iter()
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect()
}

/// Alibaba Cloud RPC percent encoding: UTF-8 bytes, RFC 3986 unreserved characters.
pub fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            // Writing into a String cannot fail.
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

pub(crate) fn encoded_params(parameters: &Params) -> String {
    let mut result = String::new();
    for (key, value) in parameters {
        if !result.is_empty() {
            result.push('&');
        }
        result.push_str(&percent_encode(key));
        result.push('=');
        result.push_str(&percent_encode(value));
    }
    result
}

/// RPC v1 HMAC-SHA1 signature. `parameters` includes all common parameters,
/// including Action, but Signature itself is excluded from the canonical query.
/// The method must match the HTTP method used to send the request.
pub fn rpc_signature(method: &str, parameters: &Params, secret: &str) -> Result<String, String> {
    if !matches!(method, "GET" | "POST") {
        return Err("阿里云 RPC 签名使用了不支持的 HTTP 方法。".into());
    }
    let mut canonical = String::new();
    for (key, value) in parameters
        .iter()
        .filter(|(key, _)| key.as_str() != "Signature")
    {
        if !canonical.is_empty() {
            canonical.push('&');
        }
        canonical.push_str(&percent_encode(key));
        canonical.push('=');
        canonical.push_str(&percent_encode(value));
    }
    let string_to_sign = format!("{method}&%2F&{}", percent_encode(&canonical));
    let mut key = String::with_capacity(secret.len() + 1);
    key.push_str(secret);
    key.push('&');
    let mut mac = Hmac::<Sha1>::new_from_slice(key.as_bytes())
        .map_err(|_| "无法生成阿里云 RPC 签名。".to_owned())?;
    mac.update(string_to_sign.as_bytes());
    Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
}

/// Never relay Message, URLs, request IDs, or unrecognized Code text: API errors
/// and transport errors can contain the signed request and therefore credentials.
pub(crate) fn response(status: u16, value: Value) -> Result<Value, String> {
    if (200..300).contains(&status) && value.is_object() && value.get("Code").is_none() {
        return Ok(value);
    }
    let code = value["Code"].as_str().unwrap_or("");
    let explanation = match code {
        "InvalidAccessKeyId.NotFound" | "InvalidAccessKeyId" | "InvalidAccessKeyId.Inactive" => {
            "AccessKey ID 无效或已停用，请检查阿里云凭据。"
        }
        "SignatureDoesNotMatch"
        | "InvalidSecurityToken.Expired"
        | "InvalidSecurityToken.MismatchWithAccessKey" => {
            "签名验证失败，请检查 AccessKey Secret 和凭据状态。"
        }
        "Forbidden"
        | "Forbidden.RAM"
        | "Forbidden.RAMUser"
        | "Forbidden.RAMPolicy"
        | "Forbidden.RAMDomain"
        | "AccessDenied"
        | "Unauthorized" => {
            "权限不足，请检查 RAM 的域名读取、记录读取、新增和修改权限及域名授权范围。"
        }
        "Throttling" | "Throttling.User" | "Throttling.Api" | "Throttling.System" => {
            "请求过于频繁，请等待自动重试。"
        }
        "InvalidTimeStamp.Expired" | "InvalidTimeStamp.Format" | "SignatureNonceUsed" => {
            "请求时间或随机数被拒绝，请同步系统时间后重试。"
        }
        "DomainRecordDuplicate"
        | "DomainRecordConflict"
        | "DomainRecordConflict.A"
        | "DomainRecordConflict.CNAME"
        | "DomainRecordConflict.NS"
        | "RRConflict" => "已有冲突的 DNS 记录，请在阿里云检查同名记录后重试。",
        "DomainRecordLocked"
        | "DomainIsLocked"
        | "DomainExpired"
        | "DomainNotExist"
        | "InvalidDomainName.NoExist"
        | "InvalidRecordId.NotFound" => {
            "域名或记录不存在、已锁定或已过期，请在阿里云检查域名及记录状态。"
        }
        _ => {
            let explanation = match status {
                401 | 403 => "请求被拒绝，请检查 AccessKey 和 RAM 域名授权范围。",
                429 => "请求过于频繁，请等待自动重试。",
                _ => "请检查域名、RAM 权限和服务状态。",
            };
            return Err(format!("阿里云请求失败（HTTP {status}）。{explanation}"));
        }
    };
    // Only the fixed, recognized API codes above may be displayed.
    Err(format!("阿里云请求失败（API {code}）。{explanation}"))
}

fn record_id(value: &Value) -> Result<&str, String> {
    value["RecordId"]
        .as_str()
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        .ok_or_else(|| "阿里云返回了无效的 DNS 记录 ID。".into())
}

fn records(value: &Value) -> Result<&[Value], String> {
    value["DomainRecords"]["Record"]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| "阿里云返回了无效的 DNS 记录列表。".into())
}

/// Update one default-line AAAA record. The callback receives an RPC Action and
/// its unsigned action parameters, and returns the complete decoded RPC object.
/// Longest exact account-hosted suffix wins, including separately hosted child
/// zones. No public-suffix assumptions or account-wide record enumeration.
pub fn update(
    domain: &str,
    address: Ipv6Addr,
    mut request: impl FnMut(&str, &Params) -> Result<Value, String>,
) -> Result<&'static str, String> {
    let normalized = crate::dns_name(domain)?;
    let domain = normalized.as_str();
    let mut call = |action: &str, parameters: &Params| response(200, request(action, parameters)?);
    let mut suffix = domain;
    let zone = loop {
        let found = call(
            "DescribeDomains",
            &params(&[
                ("KeyWord", suffix),
                ("SearchMode", "EXACT"),
                ("PageNumber", "1"),
                ("PageSize", "100"),
            ]),
        )?;
        let zones = found["Domains"]["Domain"]
            .as_array()
            .ok_or("阿里云返回了无效的域名列表。")?;
        let total = found["TotalCount"]
            .as_u64()
            .ok_or("阿里云未返回域名列表的总数。")?;
        if total != zones.len() as u64 || total > 1 {
            return Err("阿里云返回的精确域名列表不完整或有冲突，已停止修改。".into());
        }
        match zones.as_slice() {
            [zone] if zone["DomainName"].as_str() == Some(suffix) => break suffix,
            [] => (),
            _ => return Err("阿里云返回了不匹配的域名，已停止修改。".into()),
        }
        suffix = suffix
            .split_once('.')
            .map(|(_, rest)| rest)
            .filter(|rest| rest.contains('.'))
            .ok_or("未找到阿里云账户托管的对应域名，请检查域名和 RAM 域名读取权限。")?;
    };
    let rr = if domain == zone {
        "@"
    } else {
        domain
            .strip_suffix(zone)
            .and_then(|prefix| prefix.strip_suffix('.'))
            .filter(|prefix| !prefix.is_empty())
            .ok_or("阿里云域名与主机记录不匹配。")?
    };
    let found = call(
        "DescribeSubDomainRecords",
        &params(&[
            ("DomainName", zone),
            ("SubDomain", domain),
            ("Type", "AAAA"),
            ("Line", "default"),
            ("PageNumber", "1"),
            ("PageSize", "100"),
        ]),
    )?;
    let listed = records(&found)?;
    let total = found["TotalCount"]
        .as_u64()
        .ok_or("阿里云未返回 DNS 记录列表的总数，已停止修改。")?;
    if total > 1 || listed.len() > 1 {
        return Err("此域名有多条默认线路 AAAA 记录。请在阿里云确认只保留一条后再运行。".into());
    }
    if total != listed.len() as u64 {
        return Err("阿里云 DNS 记录列表不完整，已停止修改。".into());
    }
    let address_text = address.to_string();
    let mut mutation = params(&[
        ("RR", rr),
        ("Type", "AAAA"),
        ("Value", &address_text),
        ("Line", "default"),
    ]);
    match listed {
        [] => {
            mutation.insert("DomainName".into(), zone.into());
            mutation.insert("TTL".into(), "600".into());
            let created = call("AddDomainRecord", &mutation)?;
            record_id(&created)?;
            Ok("已创建 AAAA 记录")
        }
        [record] => {
            if record["DomainName"].as_str() != Some(zone)
                || record["RR"].as_str() != Some(rr)
                || record["Type"].as_str() != Some("AAAA")
                || record["Line"].as_str() != Some("default")
            {
                return Err("阿里云返回了不匹配的 DNS 记录，已停止修改。".into());
            }
            let id = record_id(record)?;
            match record["Locked"].as_bool() {
                Some(false) => (),
                Some(true) => return Err("阿里云 AAAA 记录已锁定，请先在控制台解除锁定。".into()),
                None => return Err("阿里云未返回 DNS 记录锁定状态，已停止修改。".into()),
            }
            match record["Status"].as_str() {
                Some(status) if status.eq_ignore_ascii_case("Enable") => (),
                Some(status) if status.eq_ignore_ascii_case("Disable") => {
                    return Err("阿里云 AAAA 记录已停用，请先在控制台启用该记录。".into());
                }
                _ => return Err("阿里云返回了未知的 DNS 记录启用状态，已停止修改。".into()),
            }
            let ttl = record["TTL"]
                .as_u64()
                .filter(|ttl| *ttl > 0 && *ttl <= u32::MAX as u64)
                .ok_or("阿里云返回了无效的 DNS 记录 TTL，已停止修改。")?;
            let current = record["Value"]
                .as_str()
                .and_then(|value| value.parse::<Ipv6Addr>().ok())
                .ok_or("阿里云返回了无效的 AAAA 地址，已停止修改。")?;
            if current == address {
                return Ok("AAAA 地址已一致");
            }
            // UpdateDomainRecord defaults omitted TTL/Line to 600/default, so
            // send both explicitly. Status, weight and remarks are not changed.
            mutation.insert("RecordId".into(), id.into());
            mutation.insert("TTL".into(), ttl.to_string());
            let updated = call("UpdateDomainRecord", &mutation)?;
            if record_id(&updated)? != id {
                return Err("阿里云未确认修改的是原 DNS 记录，需在控制台检查后重试。".into());
            }
            Ok("已更新 AAAA 记录")
        }
        _ => unreachable!(),
    }
}

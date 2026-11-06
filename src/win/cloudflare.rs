use std::ffi::c_void;
use std::net::Ipv6Addr;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use serde_json::Value;
use windows_sys::Win32::Networking::WinHttp::*;
use windows_sys::w;

use super::{Result, last_error, wide};

struct Http(*mut c_void);

impl Http {
    fn new(raw: *mut c_void, action: &str) -> Result<Self> {
        if raw.is_null() {
            Err(last_error(action))
        } else {
            Ok(Self(raw))
        }
    }
}

impl Drop for Http {
    fn drop(&mut self) {
        unsafe {
            WinHttpCloseHandle(self.0);
        }
    }
}

fn check(result: i32, action: &str) -> Result<()> {
    if result == 0 {
        Err(last_error(action))
    } else {
        Ok(())
    }
}

pub fn update(domain: &str, address: Ipv6Addr, token: &str) -> Result<&'static str> {
    let session = Http::new(
        unsafe {
            WinHttpOpen(
                w!("IPv6-DDNS/0.1"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                null(),
                null(),
                0,
            )
        },
        "打开 HTTPS 会话失败",
    )?;
    check(
        unsafe { WinHttpSetTimeouts(session.0, 5_000, 5_000, 7_000, 10_000) },
        "设置 HTTPS 超时失败",
    )?;
    let connection = Http::new(
        unsafe { WinHttpConnect(session.0, w!("api.cloudflare.com"), 443, 0) },
        "连接 Cloudflare 失败",
    )?;
    let headers = wide(format!(
        "Authorization: Bearer {token}\r\nContent-Type: application/json\r\n"
    ));
    let deadline = Instant::now() + Duration::from_secs(45);
    ipv6_ddns::cloudflare::update(domain, address, |method, path, body| {
        if Instant::now() >= deadline {
            return Err("等待 Cloudflare 响应超时。".into());
        }
        request(&connection, &headers, method, path, body)
    })
}

fn request(
    connection: &Http,
    headers: &[u16],
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let request = Http::new(
        unsafe {
            WinHttpOpenRequest(
                connection.0,
                wide(method).as_ptr(),
                wide(format!("/client/v4{path}")).as_ptr(),
                null(),
                null(),
                null(),
                WINHTTP_FLAG_SECURE,
            )
        },
        "创建 Cloudflare 请求失败",
    )?;
    let redirects = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
    check(
        unsafe {
            WinHttpSetOption(
                request.0,
                WINHTTP_OPTION_REDIRECT_POLICY,
                (&raw const redirects).cast(),
                4,
            )
        },
        "设置 HTTPS 重定向策略失败",
    )?;
    let data = body.map(|value| value.to_string()).unwrap_or_default();
    let payload = if data.is_empty() {
        null()
    } else {
        data.as_ptr().cast()
    };
    check(
        unsafe {
            WinHttpSendRequest(
                request.0,
                headers.as_ptr(),
                u32::MAX,
                payload,
                data.len() as u32,
                data.len() as u32,
                0,
            )
        },
        "发送 Cloudflare 请求失败",
    )?;
    check(
        unsafe { WinHttpReceiveResponse(request.0, null_mut()) },
        "接收 Cloudflare 响应失败",
    )?;
    let mut status = 0u32;
    check(
        unsafe {
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&raw mut status).cast(),
                &mut 4,
                null_mut(),
            )
        },
        "读取 Cloudflare 状态失败",
    )?;

    let mut response = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let mut received = 0;
        check(
            unsafe {
                WinHttpReadData(
                    request.0,
                    chunk.as_mut_ptr().cast(),
                    chunk.len() as u32,
                    &mut received,
                )
            },
            "读取 Cloudflare 响应失败",
        )?;
        if received == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..received as usize]);
        if response.len() > 512 * 1024 {
            return Err("Cloudflare 响应超出大小限制。".into());
        }
    }
    let value = serde_json::from_slice::<Value>(&response).unwrap_or(Value::Null);
    if (200..300).contains(&status) && value["success"].as_bool() == Some(true) {
        let mut value = value;
        return Ok(value["result"].take());
    }
    let code = value["errors"][0]["code"].as_i64();
    match (status, code) {
        (401 | 403, _) => {
            Err("Cloudflare 拒绝了请求。请检查 Token、Zone 范围和 DNS 编辑权限。".into())
        }
        (429, _) => Err("Cloudflare 暂时限制了请求频率。".into()),
        (_, Some(81053 | 81054 | 81056)) => {
            Err("Cloudflare 中已有冲突的 DNS 记录，请检查同名记录。".into())
        }
        (_, Some(code)) => Err(format!(
            "Cloudflare 请求失败（HTTP {status}，API {code}）。"
        )),
        _ => Err(format!("Cloudflare 响应无效（HTTP {status}）。")),
    }
}

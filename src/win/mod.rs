mod diagnostics;
mod network;
mod platform;
mod service;
mod ui;

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr::null_mut;

use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MessageBoxW};

const NAME: &str = "IPv6DDNS";

type Result<T> = std::result::Result<T, String>;

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain([0]).collect()
}

unsafe fn from_wide(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let length = (0..).take_while(|i| unsafe { *ptr.add(*i) != 0 }).count();
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, length) })
}

fn error(action: &str, err: impl std::fmt::Display) -> String {
    format!("{action}（{err}）")
}

fn last_error(action: &str) -> String {
    error(action, std::io::Error::last_os_error())
}

fn win_code(code: u32, action: &str) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(error(
            action,
            std::io::Error::from_raw_os_error(code as i32),
        ))
    }
}

pub fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("--service") => {
            if service::dispatch().is_err() {
                std::process::exit(1);
            }
            return;
        }
        Some("--install-admin") => platform::install(args.get(1).map_or("", String::as_str)),
        Some("--auto-on-admin") => platform::set_auto(true),
        Some("--auto-off-admin") => platform::set_auto(false),
        Some("--uninstall-admin") => platform::uninstall(),
        Some("--uninstall") => platform::elevate("--uninstall-admin", None),
        _ => ui::launch(args.iter().any(|arg| arg == "--tray")),
    };
    if let Err(message) = result {
        unsafe {
            MessageBoxW(
                null_mut(),
                wide(message).as_ptr(),
                wide("IPv6 DDNS").as_ptr(),
                MB_ICONERROR,
            )
        };
        std::process::exit(1);
    }
}

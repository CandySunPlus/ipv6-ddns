#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
#[path = "win/mod.rs"]
mod win;

#[cfg(target_os = "macos")]
#[path = "mac/mod.rs"]
mod mac;

#[cfg(windows)]
fn main() {
    win::main();
}

#[cfg(target_os = "macos")]
fn main() {
    mac::main();
}

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    eprintln!("IPv6 DDNS runs on Windows and macOS.");
}

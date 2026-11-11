#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
#[path = "win/mod.rs"]
mod win;

#[cfg(target_os = "macos")]
#[path = "mac/mod.rs"]
mod mac;

#[cfg(target_os = "linux")]
#[path = "linux/mod.rs"]
mod linux;

#[cfg(windows)]
fn main() {
    win::main();
}

#[cfg(target_os = "macos")]
fn main() {
    mac::main();
}

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn main() {
    eprintln!("IPv6 DDNS supports Linux, Windows and macOS.");
    std::process::exit(1);
}

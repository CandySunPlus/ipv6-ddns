#![windows_subsystem = "windows"]

#[cfg(windows)]
#[path = "win/mod.rs"]
mod win;

#[cfg(windows)]
fn main() {
    win::main();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("IPv6 DDNS runs on Windows.");
}

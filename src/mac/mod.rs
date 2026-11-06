mod cloudflare;
mod network;
mod platform;
mod service;
mod ui;

use std::fmt::Display;

type Result<T, E = String> = std::result::Result<T, E>;

fn error(action: &str, err: impl Display) -> String {
    format!("{action}（{err}）")
}

pub fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("--service") => service::run(),
        Some("--install-admin") => platform::install(args.get(1).map_or("", String::as_str)),
        Some("--auto-on-admin") => platform::set_auto(true),
        Some("--auto-off-admin") => platform::set_auto(false),
        Some("--uninstall-admin") => platform::uninstall(),
        Some("--uninstall") => platform::elevate(&["--uninstall-admin"]),
        _ => ui::launch(args.iter().any(|arg| arg == "--tray")),
    };
    if let Err(message) = result {
        eprintln!("IPv6 DDNS：{message}");
        std::process::exit(1);
    }
}

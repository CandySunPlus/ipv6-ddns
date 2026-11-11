mod network;
mod platform;
mod service;
mod tui;

use std::fmt::Display;

pub type Result<T, E = String> = std::result::Result<T, E>;

fn error(action: &str, err: impl Display) -> String {
    format!("{action}（{err}）")
}

fn command() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        platform::require_root()?;
        return tui::launch();
    }
    if args.len() != 1 {
        return Err("参数无效，请使用 --help 查看用法。".into());
    }
    match args[0].to_str() {
        Some("tui") => {
            platform::require_root()?;
            tui::launch()
        }
        Some("--service") => service::run(),
        Some("install") => platform::install(),
        Some("uninstall") => platform::uninstall(),
        Some("start") => platform::start(),
        Some("stop") => platform::stop(),
        Some("status") => {
            let reply = service::client(ipv6_ddns::Request::Status)?;
            if let Some(message) = reply.error {
                return Err(message);
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&reply.snapshot)
                    .map_err(|e| error("编码服务状态失败", e))?
            );
            Ok(())
        }
        Some("run") => {
            let reply = service::client(ipv6_ddns::Request::Run)?;
            if let Some(message) = reply.error {
                return Err(message);
            }
            println!("已开始更新；使用 status 查看结果。");
            Ok(())
        }
        Some("--version") => {
            println!("IPv6 DDNS {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h" | "help") => {
            println!(
                "IPv6 DDNS\n\n用法：ipv6-ddns [tui|install|uninstall|start|stop|status|run]\n  无参数 / tui   打开终端界面（需要 root）\n  install       安装系统服务、启用开机启动并立即启动\n  uninstall     停止服务并删除本程序安装的文件及配置\n  start / stop  显式启动 / 停止系统服务\n  status        输出不含凭据的服务状态\n  run           立即执行一次更新（不要求开启自动更新）\n  --service     前台运行守护进程（不要求 systemd）\n  --version     显示版本\n\n除帮助及版本外，操作需要 root。安装依赖 systemd；IPv6 检测依赖 iproute2。\n关闭自动启动会同时关闭周期更新，但不会停止正在运行的服务。"
            );
            Ok(())
        }
        _ => Err("参数无效，请使用 --help 查看用法。".into()),
    }
}

pub fn main() {
    if let Err(message) = command() {
        eprintln!("IPv6 DDNS：{message}");
        std::process::exit(1);
    }
}

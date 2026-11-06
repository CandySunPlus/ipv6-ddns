use std::ffi::{CStr, CString, c_char, c_int};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::FromRawFd;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{Result, error};

pub const LABEL: &str = "com.ipv6ddns.daemon";
pub const TRAY_LABEL: &str = "com.ipv6ddns.tray";
pub const SOCKET: &str = "/var/run/ipv6ddns.sock";
pub const EXECUTABLE: &str = "/Library/PrivilegedHelperTools/IPv6-DDNS";
pub const PLIST: &str = "/Library/LaunchDaemons/com.ipv6ddns.daemon.plist";
const DIRECTORY: &str = "/Library/Application Support/IPv6DDNS";
const CONFIG: &str = "/Library/Application Support/IPv6DDNS/config.json";
const OWNER: &str = "/Library/Application Support/IPv6DDNS/owner";

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub settings: ipv6_ddns::Settings,
    pub token: String,
}

pub fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

pub fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

pub fn installed_path() -> PathBuf {
    PathBuf::from(EXECUTABLE)
}

pub fn installed() -> bool {
    Path::new(EXECUTABLE).is_file() && Path::new(PLIST).is_file()
}

pub fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().map_err(|e| error("定位程序文件失败", e))
}

fn user(uid: u32) -> Result<(u32, PathBuf)> {
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0u8; 4096];
    let mut result = null_mut();
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() || entry.pw_dir.is_null() {
        return Err("安装账户无效。".into());
    }
    let home = unsafe { CStr::from_ptr(entry.pw_dir) }
        .to_string_lossy()
        .into_owned();
    Ok((entry.pw_gid, PathBuf::from(home)))
}

pub fn home(uid: u32) -> Result<PathBuf> {
    user(uid).map(|(_, home)| home)
}

fn parse_owner(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split_whitespace();
    let uid = parts.next()?.parse().ok()?;
    let gid = parts.next()?.parse().ok()?;
    (parts.next().is_none() && uid != 0).then_some((uid, gid))
}

pub fn owner() -> Result<(u32, u32)> {
    let text = fs::read_to_string(OWNER).map_err(|e| error("读取服务安装账户失败", e))?;
    parse_owner(&text).ok_or_else(|| "服务安装账户无效。".into())
}

fn chown(path: &Path, uid: u32, gid: u32) -> Result<()> {
    if !is_root() {
        return Ok(());
    }
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| "安装路径无效。")?;
    if unsafe { libc::chown(path.as_ptr(), uid, gid) } != 0 {
        return Err(error(
            "设置安装文件所有者失败",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

fn chmod(path: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| error("设置安装文件权限失败", e))
}

fn write_atomic(path: &Path, body: &[u8], uid: u32, gid: u32, mode: u32) -> Result<()> {
    let name = path.file_name().ok_or("安装路径无效。")?.to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temporary)
            .map_err(|e| error("创建安装文件失败", e))?;
        file.write_all(body)
            .map_err(|e| error("写入安装文件失败", e))?;
        file.sync_all().map_err(|e| error("保存安装文件失败", e))?;
        drop(file);
        chmod(&temporary, mode)?;
        chown(&temporary, uid, gid)?;
        fs::rename(&temporary, path).map_err(|e| error("替换安装文件失败", e))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn create_directory(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path).map_err(|e| error("创建安装目录失败", e))?;
        chown(path, uid, gid)?;
        chmod(path, mode)?;
    }
    Ok(())
}

pub fn load() -> Result<Option<Config>> {
    match fs::read(CONFIG) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "已保存的配置无法读取。".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(error("读取配置失败", e)),
    }
}

pub fn store(config: &Config) -> Result<()> {
    create_directory(Path::new(DIRECTORY), 0, 0, 0o755)?;
    let bytes = serde_json::to_vec(config).map_err(|_| "无法编码配置。")?;
    write_atomic(Path::new(CONFIG), &bytes, 0, 0, 0o600)
}

fn run_at_load(text: &str) -> Option<bool> {
    let rest = text.split("<key>RunAtLoad</key>").nth(1)?.trim_start();
    if rest.starts_with("<true/>") {
        Some(true)
    } else if rest.starts_with("<false/>") {
        Some(false)
    } else {
        None
    }
}

pub fn automatic() -> Result<bool> {
    match fs::read_to_string(PLIST) {
        Ok(text) => Ok(run_at_load(&text).unwrap_or(true)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(error("读取开机启动设置失败", e)),
    }
}

pub fn daemon_plist(run_at_load: bool, uid: u32, gid: u32) -> String {
    let keep_alive = if run_at_load {
        "<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>"
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{EXECUTABLE}</string>
		<string>--service</string>
	</array>
	<key>RunAtLoad</key>
	<{run_at_load}/>
	{keep_alive}
	<key>ThrottleInterval</key>
	<integer>30</integer>
	<key>Sockets</key>
	<dict>
		<key>Listener</key>
		<dict>
			<key>SockFamily</key>
			<string>Unix</string>
			<key>SockPathName</key>
			<string>{SOCKET}</string>
			<key>SockPathOwner</key>
			<integer>{uid}</integer>
			<key>SockPathGroup</key>
			<integer>{gid}</integer>
			<key>SockPathMode</key>
			<integer>384</integer>
			<key>SockPassive</key>
			<true/>
			<key>SockType</key>
			<string>stream</string>
		</dict>
	</dict>
</dict>
</plist>
"#,
        run_at_load = if run_at_load { "true" } else { "false" },
        keep_alive = keep_alive,
    )
}

pub fn tray_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{TRAY_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{EXECUTABLE}</string>
		<string>--tray</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
</dict>
</plist>
"#
    )
}

fn tray_path(uid: u32) -> Result<PathBuf> {
    let (_, home) = user(uid)?;
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{TRAY_LABEL}.plist")))
}

fn launchctl(arguments: &[&str]) -> Result<std::process::Output> {
    Command::new("/bin/launchctl")
        .args(arguments)
        .output()
        .map_err(|e| error("运行 launchctl 失败", e))
}

fn launchctl_ok(arguments: &[&str]) -> Result<bool> {
    let output = launchctl(arguments)?;
    Ok(output.status.success())
}

fn launchctl_message(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let message = format!("{}{}", stdout.trim(), stderr.trim());
    if message.is_empty() {
        "launchctl 未返回详细信息。".into()
    } else {
        message
    }
}

fn bootout() {
    let _ = launchctl(&["bootout", &format!("system/{LABEL}")]);
}

fn bootstrap() -> Result<()> {
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        let output = launchctl(&["bootstrap", "system", PLIST])?;
        if output.status.success() {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(error("启动后台服务失败", launchctl_message(&output)));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn kickstart() -> Result<()> {
    let output = launchctl(&["kickstart", &format!("system/{LABEL}")])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(error("启动后台服务失败", launchctl_message(&output)))
    }
}

fn enable() -> Result<()> {
    let output = launchctl(&["enable", &format!("system/{LABEL}")])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(error("启用后台服务失败", launchctl_message(&output)))
    }
}

pub fn set_auto(enabled: bool) -> Result<()> {
    if !is_root() {
        return Err("需要管理员权限。".into());
    }
    let (uid, gid) = owner()?;
    write_atomic(
        Path::new(PLIST),
        daemon_plist(enabled, uid, gid).as_bytes(),
        0,
        0,
        0o644,
    )?;
    let loaded = launchctl_ok(&["print", &format!("system/{LABEL}")])?;
    if enabled {
        let _ = enable();
        if !loaded {
            bootstrap()?;
        }
        enable()?;
        kickstart()?;
    } else if !loaded {
        let _ = bootstrap();
    }
    Ok(())
}

pub fn install(owner_argument: &str) -> Result<()> {
    if !is_root() {
        return Err("需要管理员权限。".into());
    }
    let uid: u32 = owner_argument
        .trim()
        .parse()
        .map_err(|_| "安装账户无效。")?;
    if uid == 0 {
        return Err("安装账户无效。".into());
    }
    let (gid, home) = user(uid)?;
    let source = current_exe()?;
    let copied = source.as_path() != Path::new(EXECUTABLE);
    let previous = owner().ok();
    bootout();
    let _ = fs::remove_file(SOCKET);

    let result = (|| {
        create_directory(Path::new("/Library/PrivilegedHelperTools"), 0, 0, 0o755)?;
        if copied {
            let _ = fs::remove_file(EXECUTABLE);
            fs::copy(&source, EXECUTABLE).map_err(|e| error("复制应用程序失败", e))?;
        }
        chmod(Path::new(EXECUTABLE), 0o755)?;
        chown(Path::new(EXECUTABLE), 0, 0)?;

        create_directory(Path::new(DIRECTORY), 0, 0, 0o755)?;
        write_atomic(
            Path::new(OWNER),
            format!("{uid} {gid}\n").as_bytes(),
            0,
            0,
            0o600,
        )?;
        write_atomic(
            Path::new(PLIST),
            daemon_plist(true, uid, gid).as_bytes(),
            0,
            0,
            0o644,
        )?;

        let agents = home.join("Library/LaunchAgents");
        create_directory(&agents, uid, gid, 0o755)?;
        write_atomic(&tray_path(uid)?, tray_plist().as_bytes(), uid, gid, 0o644)?;
        if let Some((old_uid, _)) = previous
            && old_uid != uid
            && let Ok(path) = tray_path(old_uid)
        {
            let _ = fs::remove_file(path);
        }

        let _ = enable();
        bootstrap()?;
        enable()?;
        kickstart()
    })();

    if result.is_err() {
        let _ = fs::remove_file(PLIST);
        if copied {
            let _ = fs::remove_file(EXECUTABLE);
        }
        bootout();
    }
    result
}

fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(error("删除安装文件失败", e)),
    }
}

pub fn uninstall() -> Result<()> {
    if !is_root() {
        return Err("需要管理员权限。".into());
    }
    let owner = owner().ok();
    bootout();
    let _ = fs::remove_file(SOCKET);
    remove(Path::new(PLIST))?;
    remove(Path::new(EXECUTABLE))?;
    match fs::remove_dir_all(DIRECTORY) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(error("删除配置失败", e)),
    }
    if let Some((uid, _)) = owner {
        remove(&tray_path(uid)?)?;
    }
    Ok(())
}

pub fn elevate(arguments: &[&str]) -> Result<()> {
    let executable = if arguments.first() == Some(&"--install-admin") || !installed_path().is_file()
    {
        current_exe()?
    } else {
        installed_path()
    };
    let script = r#"on run argv
set target to quoted form of (item 1 of argv)
set extra to ""
repeat with i from 2 to (count of argv)
set extra to extra & " " & quoted form of (item i of argv)
end repeat
do shell script (target & extra) with prompt "IPv6 DDNS 需要管理员权限。" with administrator privileges
end run"#;
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .arg(&executable)
        .args(arguments)
        .output()
        .map_err(|e| error("请求管理员授权失败", e))?;
    if output.status.success() {
        return Ok(());
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if message.contains("User canceled") || message.contains("-128") {
        Err("已取消管理员授权。".into())
    } else {
        Err(format!("管理员操作未完成（{message}）"))
    }
}

unsafe extern "C" {
    fn launch_activate_socket(
        name: *const c_char,
        fds: *mut *mut c_int,
        count: *mut usize,
    ) -> c_int;
}

fn activated() -> Option<UnixListener> {
    let mut fds: *mut c_int = null_mut();
    let mut count = 0usize;
    let status = unsafe { launch_activate_socket(c"Listener".as_ptr(), &mut fds, &mut count) };
    if status != 0 || count == 0 || fds.is_null() {
        if !fds.is_null() {
            unsafe {
                libc::free(fds.cast());
            }
        }
        return None;
    }
    let descriptor = unsafe { *fds };
    for index in 1..count {
        unsafe {
            libc::close(*fds.add(index));
        }
    }
    unsafe {
        libc::free(fds.cast());
    }
    Some(unsafe { UnixListener::from_raw_fd(descriptor) })
}

fn restrict(path: &Path, uid: u32, gid: u32) -> Result<()> {
    chmod(path, 0o600)?;
    chown(path, uid, gid)
}

pub fn listener() -> Result<UnixListener> {
    let (uid, gid) = owner()?;
    let path = Path::new(SOCKET);
    if let Some(listener) = activated() {
        restrict(path, uid, gid)?;
        return Ok(listener);
    }
    remove(path)?;
    let listener = UnixListener::bind(path).map_err(|e| error("创建本地服务通道失败", e))?;
    restrict(path, uid, gid)?;
    Ok(listener)
}

pub fn local_time(unix: u64) -> String {
    let mut time: libc::tm = unsafe { std::mem::zeroed() };
    let seconds = unix as libc::time_t;
    if unsafe { libc::localtime_r(&seconds, &mut time) }.is_null() {
        return "—".into();
    }
    format!("{:02}:{:02}", time.tm_hour, time.tm_min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_owner_and_run_at_load_settings() {
        assert_eq!(parse_owner("501 20\n"), Some((501, 20)));
        for invalid in ["", "0 0", "501", "501 20 1", "a b"] {
            assert_eq!(parse_owner(invalid), None, "{invalid}");
        }
        assert_eq!(run_at_load(&daemon_plist(true, 501, 20)), Some(true));
        assert_eq!(run_at_load(&daemon_plist(false, 501, 20)), Some(false));
    }

    #[test]
    fn produces_valid_launchd_plists() {
        for (run_at_load, uid, gid) in [(true, 501, 20), (false, 502, 20)] {
            let path = std::env::temp_dir().join(format!(
                "ipv6ddns-{run_at_load}-{}.plist",
                std::process::id()
            ));
            fs::write(&path, daemon_plist(run_at_load, uid, gid)).unwrap();
            let output = Command::new("/usr/bin/plutil")
                .args(["-lint", path.to_str().unwrap()])
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output);
            let text = fs::read_to_string(&path).unwrap();
            assert!(text.contains(&format!("<integer>{uid}</integer>")));
            let _ = fs::remove_file(path);
        }
        let path = std::env::temp_dir().join(format!("ipv6ddns-tray-{}.plist", std::process::id()));
        fs::write(&path, tray_plist()).unwrap();
        let output = Command::new("/usr/bin/plutil")
            .args(["-lint", path.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let _ = fs::remove_file(path);
    }
}

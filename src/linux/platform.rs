use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use ipv6_ddns::Config;

use super::{Result, error};

pub const EXECUTABLE: &str = "/usr/local/libexec/ipv6-ddns";
pub const UNIT: &str = "/etc/systemd/system/ipv6-ddns.service";
pub const CONFIG: &str = "/etc/ipv6-ddns/config.json";
pub const RUNTIME: &str = "/run/ipv6-ddns";
pub const SOCKET: &str = "/run/ipv6-ddns/control.sock";
const ENABLED: &str = "/etc/systemd/system/multi-user.target.wants/ipv6-ddns.service";
const UNIT_NAME: &str = "ipv6-ddns.service";
const CONFIG_LIMIT: u64 = 16 * 1024;
static TEMPORARY: AtomicU64 = AtomicU64::new(0);

// Fixed paths and a complete, recognizable unit keep uninstall scoped to this application.
const UNIT_BODY: &str = "# Managed by IPv6 DDNS\n[Unit]\nDescription=IPv6 DDNS system service\nWants=network-online.target\nAfter=network-online.target\n\n[Service]\nType=simple\nUser=root\nGroup=root\nExecStart=/usr/local/libexec/ipv6-ddns --service\nRestart=on-failure\nRestartSec=5\nUMask=0077\nRuntimeDirectory=ipv6-ddns\nRuntimeDirectoryMode=0700\nTimeoutStopSec=10\nNoNewPrivileges=true\nProtectHome=true\nProtectSystem=full\nReadWritePaths=/etc/ipv6-ddns /run/ipv6-ddns\n\n[Install]\nWantedBy=multi-user.target\n";

pub fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

pub fn require_root() -> Result<()> {
    if is_root() {
        Ok(())
    } else {
        Err("此操作需要 root，请使用 sudo 运行。".into())
    }
}

fn safe_metadata(path: &Path) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path).map_err(|e| error("读取安全路径失败", e))?;
    if metadata.file_type().is_symlink() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "拒绝不安全的路径：{}（必须由 root 拥有，且不可由组或其他用户写入）。",
            path.display()
        ));
    }
    Ok(metadata)
}

// Walk every component: create_dir_all and chmod on a symlink could otherwise write elsewhere as root.
pub fn directory(path: &Path, mode: u32) -> Result<()> {
    require_root()?;
    let mut current = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => current.push("/"),
            Component::Normal(name) => current.push(name),
            _ => return Err("安全目录必须是绝对路径。".into()),
        }
        match fs::symlink_metadata(&current) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|e| error("创建安全目录失败", e))?;
                fs::set_permissions(&current, fs::Permissions::from_mode(mode))
                    .map_err(|e| error("设置目录权限失败", e))?;
            }
            Err(e) => return Err(error("读取安全目录失败", e)),
        }
        if !safe_metadata(&current)?.is_dir() {
            return Err(format!("安全目录不是目录：{}", current.display()));
        }
    }
    let metadata = safe_metadata(path)?;
    if mode == 0o700 && metadata.mode() & 0o7777 != mode {
        return Err(format!("目录权限必须为 0700：{}", path.display()));
    }
    Ok(())
}

fn existing_file(path: &Path, mode: u32) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(error("检查安装文件失败", e)),
        Ok(_) => {
            let metadata = safe_metadata(path)?;
            if !metadata.is_file() || metadata.nlink() != 1 || metadata.mode() & 0o7777 != mode {
                return Err(format!("拒绝不安全的文件或权限：{}", path.display()));
            }
            Ok(true)
        }
    }
}

fn open_file(path: &Path, mode: u32) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| error("打开安全文件失败", e))?;
    let metadata = file.metadata().map_err(|e| error("检查文件权限失败", e))?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != mode
    {
        return Err(format!(
            "文件必须由 root 拥有且权限为 {mode:04o}：{}",
            path.display()
        ));
    }
    Ok(file)
}

fn atomic_write(path: &Path, mode: u32, write: impl FnOnce(&mut File) -> Result<()>) -> Result<()> {
    let parent = path.parent().ok_or("安装路径无效。")?;
    existing_file(path, mode)?;
    let sequence = TEMPORARY.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".ipv6-ddns.{}.{sequence}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temporary)
        .map_err(|e| error("创建原子写入文件失败", e))?;
    let result = (|| {
        file.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|e| error("设置文件权限失败", e))?;
        // A setgid parent must not affect installed ownership.
        if unsafe { libc::fchown(std::os::fd::AsRawFd::as_raw_fd(&file), 0, 0) } != 0 {
            return Err(error("设置文件所有者失败", std::io::Error::last_os_error()));
        }
        write(&mut file)?;
        file.sync_all().map_err(|e| error("保存文件失败", e))?;
        existing_file(path, mode)?;
        fs::rename(&temporary, path).map_err(|e| error("原子替换文件失败", e))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| error("保存目录失败", e))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn load() -> Result<Option<Config>> {
    require_root()?;
    if !Path::new("/etc/ipv6-ddns").exists() {
        return Ok(None);
    }
    directory(Path::new("/etc/ipv6-ddns"), 0o700)?;
    if !existing_file(Path::new(CONFIG), 0o600)? {
        return Ok(None);
    }
    let file = open_file(Path::new(CONFIG), 0o600)?;
    if file
        .metadata()
        .map_err(|e| error("读取配置大小失败", e))?
        .len()
        > CONFIG_LIMIT
    {
        return Err("已保存的配置超出大小限制。".into());
    }
    serde_json::from_reader(file.take(CONFIG_LIMIT + 1))
        .map(Some)
        .map_err(|_| "已保存的配置无法读取。".into())
}

pub fn store(config: &Config) -> Result<()> {
    directory(Path::new("/etc/ipv6-ddns"), 0o700)?;
    let bytes = serde_json::to_vec(config).map_err(|_| "无法编码配置。")?;
    if bytes.len() as u64 > CONFIG_LIMIT {
        return Err("配置超出大小限制。".into());
    }
    atomic_write(Path::new(CONFIG), 0o600, |file| {
        file.write_all(&bytes).map_err(|e| error("写入配置失败", e))
    })
}

fn owned_unit() -> Result<bool> {
    if !existing_file(Path::new(UNIT), 0o644)? {
        return Ok(false);
    }
    let mut text = String::new();
    open_file(Path::new(UNIT), 0o644)?
        .take(8192)
        .read_to_string(&mut text)
        .map_err(|e| error("读取服务单元失败", e))?;
    if text != UNIT_BODY {
        return Err("已有同名 systemd 单元不属于本程序，拒绝覆盖或删除。".into());
    }
    Ok(true)
}

pub fn installed() -> bool {
    owned_unit().unwrap_or(false) && existing_file(Path::new(EXECUTABLE), 0o755).unwrap_or(false)
}

fn systemctl(arguments: &[&str]) -> Result<Output> {
    // Do not inherit a privileged caller's PATH or shell configuration.
    let binary = ["/usr/bin/systemctl", "/bin/systemctl"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .ok_or("未找到 systemctl；安装和服务管理需要 systemd。")?;
    Command::new(binary)
        .args(arguments)
        .env("SYSTEMD_PAGER", "")
        .env("SYSTEMD_COLORS", "0")
        .output()
        .map_err(|e| error("运行 systemctl 失败", e))
}

fn systemctl_ok(arguments: &[&str]) -> Result<()> {
    let output = systemctl(arguments)?;
    if output.status.success() {
        return Ok(());
    }
    let message = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "systemctl {} 失败：{}",
        arguments.join(" "),
        message.trim()
    ))
}

pub fn automatic() -> Result<bool> {
    match fs::symlink_metadata(ENABLED) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(error("读取自动启动状态失败", e)),
        Ok(metadata) if metadata.file_type().is_symlink() && metadata.uid() == 0 => {
            let target = fs::read_link(ENABLED).map_err(|e| error("读取自动启动链接失败", e))?;
            if target == Path::new(UNIT) || target == Path::new("../ipv6-ddns.service") {
                Ok(true)
            } else {
                Err("自动启动链接指向其他服务，拒绝使用。".into())
            }
        }
        Ok(_) => Err("自动启动链接不安全。".into()),
    }
}

pub fn running() -> Result<bool> {
    require_root()?;
    match super::service::client(ipv6_ddns::Request::Status) {
        Ok(_) => Ok(true),
        Err(message) => {
            if !Path::new(SOCKET).exists() {
                Ok(false)
            } else {
                let output = systemctl(&["is-active", "--quiet", UNIT_NAME])?;
                if output.status.success() {
                    Err(message)
                } else {
                    Ok(false)
                }
            }
        }
    }
}

pub fn install() -> Result<()> {
    require_root()?;
    let ours = owned_unit()?;
    if !ours && fs::symlink_metadata(EXECUTABLE).is_ok() {
        return Err("安装位置已有不属于本程序的文件，拒绝覆盖。".into());
    }
    directory(Path::new("/usr/local/libexec"), 0o755)?;
    directory(Path::new("/etc/systemd/system"), 0o755)?;
    directory(
        Path::new("/etc/systemd/system/multi-user.target.wants"),
        0o755,
    )?;
    directory(Path::new("/etc/ipv6-ddns"), 0o700)?;
    let source = std::env::current_exe().map_err(|e| error("定位程序失败", e))?;
    let mut input = File::open(source).map_err(|e| error("打开当前程序失败", e))?;
    atomic_write(Path::new(EXECUTABLE), 0o755, |file| {
        std::io::copy(&mut input, file)
            .map(|_| ())
            .map_err(|e| error("复制程序失败", e))
    })?;
    atomic_write(Path::new(UNIT), 0o644, |file| {
        file.write_all(UNIT_BODY.as_bytes())
            .map_err(|e| error("写入服务单元失败", e))
    })?;
    systemctl_ok(&["daemon-reload"])?;
    automatic()?; // Refuse unrelated enable links before systemctl can replace them.
    systemctl_ok(&["enable", UNIT_NAME])?;
    systemctl_ok(&["restart", UNIT_NAME])
}

pub fn set_auto(enabled: bool) -> Result<()> {
    require_root()?;
    if !installed() {
        return Err("请先安装 IPv6 DDNS 系统服务。".into());
    }
    directory(
        Path::new("/etc/systemd/system/multi-user.target.wants"),
        0o755,
    )?;
    automatic()?;
    if enabled {
        systemctl_ok(&["enable", UNIT_NAME])?;
        start()
    } else {
        // Disable boot/periodic operation, never stop the manual IPC service here.
        systemctl_ok(&["disable", UNIT_NAME])
    }
}

pub fn start() -> Result<()> {
    require_root()?;
    if !installed() {
        return Err("请先安装 IPv6 DDNS 系统服务。".into());
    }
    directory(Path::new("/etc/systemd/system"), 0o755)?;
    directory(Path::new("/usr/local/libexec"), 0o755)?;
    systemctl_ok(&["start", UNIT_NAME])
}

pub fn stop() -> Result<()> {
    require_root()?;
    if !owned_unit()? {
        return Err("未安装 IPv6 DDNS 系统服务。".into());
    }
    directory(Path::new("/etc/systemd/system"), 0o755)?;
    systemctl_ok(&["stop", UNIT_NAME])
}

pub fn uninstall() -> Result<()> {
    require_root()?;
    if !owned_unit()? {
        return Err("未找到本程序安装的 systemd 单元，拒绝删除其他文件。".into());
    }
    directory(Path::new("/etc/systemd/system"), 0o755)?;
    directory(
        Path::new("/etc/systemd/system/multi-user.target.wants"),
        0o755,
    )?;
    directory(Path::new("/usr/local/libexec"), 0o755)?;
    automatic()?;
    existing_file(Path::new(EXECUTABLE), 0o755)?;
    if fs::symlink_metadata("/etc/ipv6-ddns").is_ok() {
        directory(Path::new("/etc/ipv6-ddns"), 0o700)?;
        existing_file(Path::new(CONFIG), 0o600)?;
    }
    systemctl_ok(&["disable", "--now", UNIT_NAME])?;
    for path in [UNIT, EXECUTABLE, CONFIG] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(error("删除安装文件失败", e)),
        }
    }
    // Never recursively remove directories or unrelated files.
    if let Err(e) = fs::remove_dir("/etc/ipv6-ddns") {
        if !matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
        ) {
            return Err(error("删除配置目录失败", e));
        }
    }
    systemctl_ok(&["daemon-reload"])
}

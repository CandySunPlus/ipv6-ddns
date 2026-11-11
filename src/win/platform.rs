use std::ffi::OsStr;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use ipv6_ddns::Config;
use windows_service::service::{
    Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
    ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceSidType,
    ServiceStartType, ServiceState, ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::Cryptography::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW, WRITE_DAC,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Services::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows_sys::w;

use super::{NAME, Result, error, from_wide, last_error, wide, win_code};

const PARAMETERS: &str = r"SYSTEM\CurrentControlSet\Services\IPv6DDNS\Parameters";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

pub struct Handle(pub HANDLE);
// Kernel handles belong to the process and can be closed on another thread.
unsafe impl Send for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

impl Key {
    fn create(
        root: HKEY,
        path: &str,
        access: REG_SAM_FLAGS,
        sa: *const SECURITY_ATTRIBUTES,
    ) -> Result<Self> {
        let mut key = null_mut();
        win_code(
            unsafe {
                RegCreateKeyExW(
                    root,
                    wide(path).as_ptr(),
                    0,
                    null(),
                    0,
                    access,
                    sa,
                    &mut key,
                    null_mut(),
                )
            },
            "创建注册表项失败",
        )?;
        Ok(Self(key))
    }

    fn open(root: HKEY, path: &str, access: REG_SAM_FLAGS) -> Result<Self> {
        let mut key = null_mut();
        win_code(
            unsafe { RegOpenKeyExW(root, wide(path).as_ptr(), 0, access, &mut key) },
            "打开注册表项失败",
        )?;
        Ok(Self(key))
    }

    fn read(&self, name: &str, kind: REG_ROUTINE_FLAGS) -> Result<Option<Vec<u8>>> {
        let mut size = 0;
        let name = wide(name);
        let read = |data, size| unsafe {
            RegGetValueW(self.0, null(), name.as_ptr(), kind, null_mut(), data, size)
        };
        let status = read(null_mut(), &mut size);
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        win_code(status, "读取配置大小失败")?;
        if size > 32 * 1024 {
            return Err("注册表配置超出大小限制。".into());
        }
        let mut data = vec![0; size as usize];
        win_code(
            read(data.as_mut_ptr().cast(), &mut size),
            "读取注册表配置失败",
        )?;
        data.truncate(size as usize);
        Ok(Some(data))
    }

    fn set(&self, name: &str, kind: REG_VALUE_TYPE, data: &[u8]) -> Result<()> {
        win_code(
            unsafe {
                RegSetValueExW(
                    self.0,
                    wide(name).as_ptr(),
                    0,
                    kind,
                    data.as_ptr(),
                    data.len() as u32,
                )
            },
            "保存注册表配置失败",
        )
    }

    fn set_string(&self, name: &str, value: impl AsRef<OsStr>) -> Result<()> {
        let data = wide(value);
        self.set(name, REG_SZ, unsafe {
            std::slice::from_raw_parts(data.as_ptr().cast(), data.len() * 2)
        })
    }
}

pub struct Local(pub *mut std::ffi::c_void);
impl Drop for Local {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

pub fn descriptor(sddl: &str) -> Result<Local> {
    let mut sd = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide(sddl).as_ptr(),
            1,
            &mut sd,
            null_mut(),
        )
    } == 0
    {
        return Err(last_error("创建安全描述符失败"));
    }
    Ok(Local(sd))
}

fn sid_text(sid: PSID) -> Result<String> {
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(last_error("读取账户 SID 失败"));
    }
    let _allocation = Local(text.cast());
    Ok(unsafe { from_wide(text) })
}

pub fn current_sid() -> Result<String> {
    let mut raw = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(last_error("打开当前账户令牌失败"));
    }
    let token = Handle(raw);
    let mut size = 0;
    unsafe {
        GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut size);
    }
    let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    } == 0
    {
        return Err(last_error("读取当前账户令牌失败"));
    }
    sid_text(unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid })
}

fn service_sid() -> Result<String> {
    let mut sid = [0usize; 9];
    let mut size = size_of_val(&sid) as u32;
    let mut domain = [0u16; 64];
    let mut domain_size = domain.len() as u32;
    let mut kind = 0;
    if unsafe {
        LookupAccountNameW(
            null(),
            w!(r"NT SERVICE\IPv6DDNS"),
            sid.as_mut_ptr().cast(),
            &mut size,
            domain.as_mut_ptr(),
            &mut domain_size,
            &mut kind,
        )
    } == 0
    {
        return Err(last_error("读取服务账户 SID 失败"));
    }
    sid_text(sid.as_mut_ptr().cast())
}

fn is_sid(sid: &str) -> bool {
    sid.starts_with("S-1-")
        && sid.len() <= 184
        && sid[4..]
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

pub fn owner_sid() -> Result<String> {
    let key = Key::open(HKEY_LOCAL_MACHINE, PARAMETERS, KEY_QUERY_VALUE)?;
    let bytes = key
        .read("Owner", RRF_RT_REG_SZ)?
        .ok_or("服务缺少安装账户。")?;
    let words: Vec<_> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let sid = String::from_utf16_lossy(&words)
        .trim_end_matches('\0')
        .to_owned();
    if !is_sid(&sid) {
        return Err("服务安装账户无效。".into());
    }
    Ok(sid)
}

fn crypt(bytes: &[u8], protect: bool) -> Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let status = unsafe {
        if protect {
            CryptProtectData(
                &input,
                null(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN | CRYPTPROTECT_LOCAL_MACHINE,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                null_mut(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if status == 0 {
        return Err(last_error("处理加密配置失败"));
    }
    let _allocation = Local(output.pbData.cast());
    Ok(unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec())
}

pub fn load() -> Result<Option<Config>> {
    let key = Key::open(HKEY_LOCAL_MACHINE, PARAMETERS, KEY_QUERY_VALUE)?;
    key.read("Config", RRF_RT_REG_BINARY)?
        .map(|data| {
            serde_json::from_slice(&crypt(&data, false)?)
                .map_err(|_| "已保存的配置无法读取。".into())
        })
        .transpose()
}

pub fn store(config: &Config) -> Result<()> {
    let key = Key::open(HKEY_LOCAL_MACHINE, PARAMETERS, KEY_SET_VALUE)?;
    let bytes = serde_json::to_vec(config).map_err(|_| "无法编码配置。")?;
    key.set("Config", REG_BINARY, &crypt(&bytes, true)?)
}

pub fn open_service(access: ServiceAccess) -> windows_service::Result<Service> {
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?
        .open_service(NAME, access)
}

pub fn automatic(service: &Service) -> Result<bool> {
    service
        .query_config()
        .map(|config| config.start_type == ServiceStartType::AutoStart)
        .map_err(|e| error("读取开机启动设置失败", e))
}

pub fn installed_path() -> Result<PathBuf> {
    let mut path = null_mut();
    let hr = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, null_mut(), &mut path) };
    if hr < 0 {
        return Err(format!("无法定位 Program Files（{hr:#x}）。"));
    }
    let folder = unsafe { from_wide(path) };
    unsafe {
        CoTaskMemFree(path.cast());
    }
    Ok(PathBuf::from(folder).join(NAME).join("IPv6-DDNS.exe"))
}

fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().map_err(|e| error("定位程序文件失败", e))
}

pub fn elevate(command: &str, owner: Option<&str>) -> Result<()> {
    let path = if command == "--install-admin" {
        current_exe()?
    } else {
        installed_path()?
    };
    let file = wide(path);
    let parameters =
        wide(owner.map_or_else(|| command.to_owned(), |sid| format!("{command} {sid}")));
    let mut launch = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("runas"),
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    if unsafe { ShellExecuteExW(&mut launch) } == 0 {
        let code = unsafe { GetLastError() };
        return if code == ERROR_CANCELLED {
            Err("已取消管理员授权。".into())
        } else {
            win_code(code, "请求管理员授权失败")
        };
    }
    let child = Handle(launch.hProcess);
    let mut code = 1;
    unsafe {
        WaitForSingleObject(child.0, INFINITE);
        GetExitCodeProcess(child.0, &mut code);
    }
    if code == 0 {
        Ok(())
    } else {
        Err("管理员操作未完成。".into())
    }
}

pub fn install(owner: &str) -> Result<()> {
    if !is_sid(owner) {
        return Err("安装账户无效。".into());
    }
    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE)
            .map_err(|e| error("打开服务管理器失败", e))?;
    let path = installed_path()?;
    let directory = path.parent().unwrap();
    std::fs::create_dir(directory).map_err(|e| error("创建安装目录失败", e))?;
    let result = (|| {
        std::fs::copy(current_exe()?, &path).map_err(|e| error("复制应用程序失败", e))?;
        let info = ServiceInfo {
            name: NAME.into(),
            display_name: "IPv6 DDNS".into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::OnDemand,
            error_control: ServiceErrorControl::Normal,
            executable_path: path.clone(),
            launch_arguments: vec!["--service".into()],
            dependencies: vec![],
            account_name: Some(r"NT AUTHORITY\LocalService".into()),
            account_password: None,
        };
        let service = manager
            .create_service(&info, ServiceAccess::all())
            .map_err(|e| error("创建 Windows 服务失败", e))?;
        let result = (|| {
            configure_install(&service, owner)?;
            change_auto(&service, true)?;
            ensure_running(&service)?;
            let startup = Key::create(
                HKEY_USERS,
                &format!(r"{owner}\{RUN_KEY}"),
                KEY_SET_VALUE,
                null(),
            )?;
            startup.set_string(NAME, format!(r#""{}" --tray"#, path.display()))
        })();
        if result.is_err() {
            let _ = stop_service(&service);
            let _ = service.delete();
        }
        result
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(directory);
    }
    result
}

fn configure_install(service: &Service, owner: &str) -> Result<()> {
    service
        .set_config_service_sid_info(ServiceSidType::Unrestricted)
        .map_err(|e| error("配置服务身份失败", e))?;
    let sid = service_sid()?;
    let sd = descriptor(&format!("D:P(A;;KA;;;SY)(A;;KA;;;BA)(A;;0x3;;;{sid})"))?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let key = Key::create(
        HKEY_LOCAL_MACHINE,
        PARAMETERS,
        KEY_SET_VALUE | WRITE_DAC,
        &sa,
    )?;
    win_code(
        unsafe {
            RegSetKeySecurity(
                key.0,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                sd.0,
            )
        },
        "保护服务配置失败",
    )?;
    key.set_string("Owner", owner)?;

    let access = descriptor(&format!(
        "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x5;;;{sid})(A;;0x15;;;{owner})"
    ))?;
    if unsafe {
        SetServiceObjectSecurity(service.raw_handle(), DACL_SECURITY_INFORMATION, access.0)
    } == 0
    {
        return Err(last_error("设置服务访问权限失败"));
    }
    service
        .set_description("Keeps a DNS AAAA record synchronized with a Windows IPv6 address.")
        .map_err(|e| error("设置服务说明失败", e))?;
    service
        .update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86_400)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: Duration::from_secs(30),
            }]),
        })
        .map_err(|e| error("设置服务恢复策略失败", e))
}

fn change_auto(service: &Service, enabled: bool) -> Result<()> {
    let kind = if enabled {
        SERVICE_AUTO_START
    } else {
        SERVICE_DEMAND_START
    };
    if unsafe {
        ChangeServiceConfigW(
            service.raw_handle(),
            SERVICE_NO_CHANGE,
            kind,
            SERVICE_NO_CHANGE,
            null(),
            null(),
            null_mut(),
            null(),
            null(),
            null(),
            null(),
        )
    } == 0
    {
        return Err(last_error("更改开机启动失败"));
    }
    Ok(())
}

pub fn set_auto(enabled: bool) -> Result<()> {
    let service = open_service(
        ServiceAccess::CHANGE_CONFIG
            | ServiceAccess::START
            | ServiceAccess::QUERY_STATUS
            | ServiceAccess::QUERY_CONFIG,
    )
    .map_err(|e| error("打开 Windows 服务失败", e))?;
    if enabled {
        ensure_running(&service)?;
    }
    change_auto(&service, enabled)
}

pub fn ensure_running(service: &Service) -> Result<u32> {
    let query = || {
        service
            .query_status()
            .map_err(|e| error("读取服务状态失败", e))
    };
    let mut status = query()?;
    if status.current_state == ServiceState::Stopped {
        if let Err(e) = service.start(&[] as &[&OsStr]) {
            if !matches!(&e, windows_service::Error::Winapi(e) if e.raw_os_error() == Some(ERROR_SERVICE_ALREADY_RUNNING as i32))
            {
                return Err(error("启动 Windows 服务失败", e));
            }
        }
    }
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        if status.current_state == ServiceState::Running {
            return status.process_id.ok_or("服务进程不可用。".into());
        }
        if Instant::now() >= end {
            return Err("Windows 服务未能启动。".into());
        }
        std::thread::sleep(Duration::from_millis(100));
        status = query()?;
        if status.current_state == ServiceState::Stopped
            && status.exit_code != windows_service::service::ServiceExitCode::NO_ERROR
        {
            return Err(format!("Windows 服务启动失败（{:?}）。", status.exit_code));
        }
    }
}

fn stop_service(service: &Service) -> Result<()> {
    let query = || {
        service
            .query_status()
            .map_err(|e| error("读取服务状态失败", e))
    };
    let state = query()?.current_state;
    if state == ServiceState::Stopped {
        return Ok(());
    }
    if state != ServiceState::StopPending {
        service.stop().map_err(|e| error("停止服务失败", e))?;
    }
    let end = Instant::now() + Duration::from_secs(8);
    while query()?.current_state != ServiceState::Stopped {
        if Instant::now() >= end {
            return Err("等待 Windows 服务停止超时。".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let owner = owner_sid()?;
    let service =
        open_service(ServiceAccess::STOP | ServiceAccess::QUERY_STATUS | ServiceAccess::DELETE)
            .map_err(|e| error("打开 Windows 服务失败", e))?;
    stop_service(&service)?;
    if let Ok(key) = Key::open(HKEY_USERS, &format!(r"{owner}\{RUN_KEY}"), KEY_SET_VALUE) {
        let status = unsafe { RegDeleteValueW(key.0, wide(NAME).as_ptr()) };
        if status != ERROR_FILE_NOT_FOUND {
            win_code(status, "删除登录启动项失败")?;
        }
    }
    service.delete().map_err(|e| error("删除服务失败", e))?;
    drop(service);
    let path = installed_path()?;
    let directory = path.parent().unwrap();
    for item in [&path, directory] {
        if unsafe { MoveFileExW(wide(item).as_ptr(), null(), MOVEFILE_DELAY_UNTIL_REBOOT) } == 0 {
            return Err(last_error("安排重启后删除安装文件失败"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypts_and_restores_configuration_bytes() {
        let bytes = b"example config with a placeholder token";
        let encrypted = crypt(bytes, true).unwrap();
        assert!(!encrypted.windows(bytes.len()).any(|part| part == bytes));
        assert_eq!(crypt(&encrypted, false).unwrap(), bytes);
    }
}

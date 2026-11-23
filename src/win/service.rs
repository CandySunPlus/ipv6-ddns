use std::net::Ipv6Addr;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ipv6_ddns::{
    Config, Credentials, INTERVAL, Phase, RETRY, Reply, Request, Settings, Snapshot, dns,
    select_address,
};
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState,
    ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::w;

use super::platform::{self, Handle};
use super::source_preference::SourcePreference;
use super::{NAME, Result, error, last_error, network};

const PIPE: *const u16 = w!(r"\\.\pipe\IPv6DDNS.v2");
// File read/write permissions without FILE_CREATE_PIPE_INSTANCE (FILE_APPEND_DATA).
const CLIENT_ACCESS: u32 = 0x0012_019b;
const LIMIT: usize = 16 * 1024;
const SOURCE_INTERVAL: Duration = Duration::from_secs(15);
type Shared = Arc<Mutex<Engine>>;

#[derive(Default)]
struct Engine {
    config: Option<Config>,
    snapshot: Snapshot,
    due: Option<Instant>,
    published: Option<Ipv6Addr>,
    source: SourcePreference,
    source_due: Option<Instant>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Engine {
    fn set_automatic(&mut self, enabled: bool) {
        if enabled == self.snapshot.automatic {
            return;
        }
        self.snapshot.automatic = enabled;
        self.due = (enabled && self.config.is_some()).then(Instant::now);
    }

    fn reply(&self, error: Option<String>) -> Reply {
        let mut snapshot = self.snapshot.clone();
        snapshot.settings = self.config.as_ref().map(|c| c.settings.clone());
        snapshot.next_run = self
            .due
            .map(|deadline| now() + deadline.saturating_duration_since(Instant::now()).as_secs());
        Reply { snapshot, error }
    }

    fn reconcile_source(&mut self) {
        let target = self.published.filter(|_| {
            self.config
                .as_ref()
                .is_some_and(|config| config.settings.prefer_ddns_source)
        });
        let result = self.source.reconcile(target);
        self.source_due =
            (target.is_some() || result.is_err()).then(|| Instant::now() + SOURCE_INTERVAL);
        self.snapshot.source_preference_error = result.err();
    }

    fn save(&mut self, settings: Settings, credentials: Credentials) -> Result<()> {
        if self.snapshot.phase == Phase::Running {
            return Err("正在更新，请稍后再试。".into());
        }
        let config = Config::updated(settings, credentials, self.config.as_ref())?;
        platform::store(&config)?;
        if self.config.as_ref() == Some(&config) {
            return Ok(());
        }
        let only_source_changed = self.config.as_ref().is_some_and(|old| {
            old.settings.domain == config.settings.domain
                && old.settings.provider == config.settings.provider
                && old.settings.address == config.settings.address
                && old.credentials == config.credentials
        });
        let enabled = config.settings.prefer_ddns_source;
        self.config = Some(config);
        if only_source_changed && enabled {
            self.source_due = Some(Instant::now());
            return Ok(());
        }
        if !only_source_changed {
            self.published = None;
        }
        // Restore before a run can select candidates under the new settings.
        self.reconcile_source();
        if only_source_changed {
            return Ok(());
        }
        self.snapshot.phase = Phase::Idle;
        self.snapshot.message.clear();
        self.snapshot.address = None;
        self.snapshot.last_run = None;
        self.due = self
            .snapshot
            .automatic
            .then(|| Instant::now() + Duration::from_secs(INTERVAL));
        Ok(())
    }
}

fn start_update(engine: &Shared, state: &mut Engine) {
    let Some(config) = state.config.clone() else {
        return;
    };
    if state.snapshot.phase == Phase::Running {
        return;
    }
    state.snapshot.phase = Phase::Running;
    state.snapshot.message.clear();
    state.due = None;
    let engine = Arc::clone(engine);
    thread::spawn(move || {
        let current = network::candidates().and_then(|candidates| {
            select_address(&config.settings.address, &candidates).map_err(str::to_owned)
        });
        let address = current.as_ref().ok().copied();
        let outcome = current.and_then(|address| dns::update(&config, address));
        let mut state = engine.lock().unwrap();
        state.snapshot.address = address;
        state.snapshot.last_run = Some(now());
        if outcome.is_ok() {
            state.published = address;
            state.source_due = Some(Instant::now());
        }
        let (phase, message, seconds) = match outcome {
            Ok(message) => (Phase::Success, message.to_owned(), INTERVAL),
            Err(message) => (Phase::Failed, message, RETRY),
        };
        state.snapshot.phase = phase;
        state.snapshot.message = message;
        state.due = state
            .snapshot
            .automatic
            .then(|| Instant::now() + Duration::from_secs(seconds));
    });
}

fn read_message(pipe: HANDLE) -> Result<Vec<u8>> {
    let mut buffer = vec![0u8; LIMIT];
    let mut count = 0;
    if unsafe {
        ReadFile(
            pipe,
            buffer.as_mut_ptr(),
            LIMIT as u32,
            &mut count,
            null_mut(),
        )
    } == 0
    {
        return Err(last_error("读取服务消息失败"));
    }
    buffer.truncate(count as usize);
    Ok(buffer)
}

fn write_message(pipe: HANDLE, message: &[u8]) -> Result<()> {
    let mut written = 0;
    if message.len() > LIMIT {
        return Err("服务消息超出大小限制。".into());
    }
    if unsafe {
        WriteFile(
            pipe,
            message.as_ptr(),
            message.len() as u32,
            &mut written,
            null_mut(),
        )
    } == 0
    {
        return Err(last_error("发送服务消息失败"));
    }
    if written as usize != message.len() {
        return Err("服务消息未完整发送。".into());
    }
    Ok(())
}

fn open_pipe(name: *const u16, owner: &str) -> Result<Handle> {
    let sd = platform::descriptor(&format!("D:P(A;;GA;;;SY)(A;;{CLIENT_ACCESS:#x};;;{owner})"))?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let pipe = Handle(unsafe {
        CreateNamedPipeW(
            name,
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            LIMIT as u32,
            LIMIT as u32,
            1000,
            &sa,
        )
    });
    if pipe.0 == INVALID_HANDLE_VALUE {
        return Err(last_error("创建本地服务通道失败"));
    }
    Ok(pipe)
}

fn serve(pipe: Handle, engine: Shared) {
    loop {
        let connected = unsafe { ConnectNamedPipe(pipe.0, null_mut()) } != 0
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if connected {
            if let Ok(data) = read_message(pipe.0) {
                let reply = {
                    let mut state = engine.lock().unwrap();
                    let error = match serde_json::from_slice::<Request>(&data) {
                        Ok(Request::Status) => None,
                        Ok(Request::Run) if state.config.is_none() => {
                            Some("请先保存 DDNS 设置。".into())
                        }
                        Ok(Request::Run) if state.snapshot.phase == Phase::Running => {
                            Some("正在更新，请稍后再试。".into())
                        }
                        Ok(Request::Run) => {
                            start_update(&engine, &mut state);
                            None
                        }
                        Ok(Request::Save {
                            settings,
                            credentials,
                            run,
                        }) => match state.save(settings, credentials) {
                            Ok(()) => {
                                if run {
                                    start_update(&engine, &mut state);
                                }
                                None
                            }
                            Err(e) => Some(e),
                        },
                        Err(_) => Some("服务无法识别该请求。".into()),
                    };
                    state.reply(error)
                };
                if let Ok(data) = serde_json::to_vec(&reply)
                    && write_message(pipe.0, &data).is_ok()
                {
                    unsafe {
                        FlushFileBuffers(pipe.0);
                    }
                }
            }
        } else {
            thread::sleep(Duration::from_millis(20));
        }
        unsafe {
            DisconnectNamedPipe(pipe.0);
        }
    }
}

pub fn client(request: Request) -> Result<Reply> {
    let service = platform::open_service(ServiceAccess::QUERY_STATUS | ServiceAccess::START)
        .map_err(|e| error("打开 Windows 服务失败", e))?;
    let expected_pid = platform::ensure_running(&service)?;
    exchange(PIPE, expected_pid, request)
}

fn exchange(name: *const u16, expected_pid: u32, request: Request) -> Result<Reply> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let pipe = loop {
        let pipe = Handle(unsafe {
            CreateFileW(
                name,
                CLIENT_ACCESS,
                0,
                null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_ANONYMOUS,
                null_mut(),
            )
        });
        if pipe.0 != INVALID_HANDLE_VALUE {
            break pipe;
        }
        let code = unsafe { GetLastError() };
        if !matches!(code, ERROR_PIPE_BUSY | ERROR_FILE_NOT_FOUND) || Instant::now() >= deadline {
            return Err(error(
                "连接 DDNS 服务失败",
                std::io::Error::from_raw_os_error(code as i32),
            ));
        }
        thread::sleep(Duration::from_millis(30));
    };
    let mut actual_pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe.0, &mut actual_pid) } == 0
        || actual_pid != expected_pid
    {
        return Err("本地通道的进程与 DDNS 服务不匹配。".into());
    }
    if unsafe { SetNamedPipeHandleState(pipe.0, &PIPE_READMODE_MESSAGE, null(), null()) } == 0 {
        return Err(last_error("配置服务通道失败"));
    }
    let bytes = serde_json::to_vec(&request).map_err(|_| "无法编码服务请求。")?;
    write_message(pipe.0, &bytes)?;

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                pipe.0,
                null_mut(),
                0,
                null_mut(),
                &mut available,
                null_mut(),
            )
        } == 0
        {
            return Err(last_error("等待服务响应失败"));
        }
        if available != 0 {
            break;
        }
        if Instant::now() >= deadline {
            return Err("等待 DDNS 服务响应超时。".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    serde_json::from_slice(&read_message(pipe.0)?).map_err(|_| "无法读取服务响应。".into())
}

define_windows_service!(service_entry, service_main);

pub fn dispatch() -> Result<()> {
    service_dispatcher::start(NAME, service_entry).map_err(|e| error("启动服务入口失败", e))
}

fn service_main(_arguments: Vec<std::ffi::OsString>) {
    let (stop, signal) = mpsc::channel();
    let Ok(handle) = service_control_handler::register(NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    }) else {
        return;
    };

    let status = |state, exit, accepted| {
        handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accepted,
            exit_code: exit,
            checkpoint: u32::from(matches!(
                state,
                ServiceState::StartPending | ServiceState::StopPending
            )),
            wait_hint: if matches!(
                state,
                ServiceState::StartPending | ServiceState::StopPending
            ) {
                Duration::from_secs(5)
            } else {
                Duration::ZERO
            },
            process_id: None,
        })
    };
    let _ = status(
        ServiceState::StartPending,
        ServiceExitCode::NO_ERROR,
        ServiceControlAccept::empty(),
    );
    let result = (|| -> Result<()> {
        let owner = platform::owner_sid()?;
        let pipe = open_pipe(PIPE, &owner)?;
        let service = platform::open_service(ServiceAccess::QUERY_CONFIG)
            .map_err(|e| error("打开服务配置失败", e))?;
        let mut state = Engine::default();
        match platform::load() {
            Ok(config) => state.config = config,
            Err(message) => {
                state.snapshot.phase = Phase::Failed;
                state.snapshot.message = message;
            }
        }
        state.set_automatic(platform::automatic(&service)?);
        let engine = Arc::new(Mutex::new(state));
        let shared = Arc::clone(&engine);
        thread::spawn(move || serve(pipe, shared));
        status(
            ServiceState::Running,
            ServiceExitCode::NO_ERROR,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        )
        .map_err(|e| error("报告服务状态失败", e))?;
        loop {
            {
                let mut state = engine.lock().unwrap();
                if let Ok(automatic) = platform::automatic(&service) {
                    state.set_automatic(automatic);
                }
                if state.source_due.is_some_and(|due| Instant::now() >= due) {
                    state.reconcile_source();
                }
                if state.due.is_some_and(|due| Instant::now() >= due) {
                    start_update(&engine, &mut state);
                }
            }
            if signal.recv_timeout(Duration::from_secs(1)) != Err(mpsc::RecvTimeoutError::Timeout) {
                break;
            }
        }
        let _ = status(
            ServiceState::StopPending,
            ServiceExitCode::NO_ERROR,
            ServiceControlAccept::empty(),
        );
        engine.lock().unwrap().source.reconcile(None)
    })();
    let exit = if result.is_ok() {
        ServiceExitCode::NO_ERROR
    } else {
        ServiceExitCode::Win32(1)
    };
    let _ = status(ServiceState::Stopped, exit, ServiceControlAccept::empty());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchanges_status_over_a_restricted_windows_pipe() {
        let pid = std::process::id();
        let name = super::super::wide(format!(r"\\.\pipe\IPv6DDNS.Test.{pid}"));
        let pipe = open_pipe(name.as_ptr(), &platform::current_sid().unwrap()).unwrap();
        let state = Arc::new(Mutex::new(Engine::default()));
        thread::spawn(move || serve(pipe, state));
        let error = exchange(name.as_ptr(), pid + 1, Request::Status).unwrap_err();
        assert!(error.contains("不匹配"), "{error}");
        let reply = exchange(name.as_ptr(), pid, Request::Status).unwrap();
        assert!(reply.error.is_none());
        assert_eq!(reply.snapshot.phase, Phase::Idle);
        assert!(!reply.snapshot.automatic);
        let reply = exchange(name.as_ptr(), pid, Request::Run).unwrap();
        assert_eq!(reply.error.as_deref(), Some("请先保存 DDNS 设置。"));
    }
}

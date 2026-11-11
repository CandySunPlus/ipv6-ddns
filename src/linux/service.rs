use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ipv6_ddns::{
    Config, Credentials, INTERVAL, Phase, RETRY, Reply, Request, Settings, Snapshot, dns,
    select_address,
};

use super::{Result, error, network, platform};

const LIMIT: usize = 16 * 1024;
const TIMEOUT: Duration = Duration::from_secs(3);
static STOP: AtomicBool = AtomicBool::new(false);
type Shared = Arc<Mutex<Engine>>;

extern "C" fn stop_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Default)]
struct Engine {
    config: Option<Config>,
    snapshot: Snapshot,
    due: Option<Instant>,
}

impl Engine {
    fn automatic(&mut self, enabled: bool) {
        if self.snapshot.automatic == enabled {
            return;
        }
        self.snapshot.automatic = enabled;
        self.due = (enabled && self.config.is_some() && self.snapshot.phase != Phase::Running)
            .then(Instant::now);
    }

    fn reply(&self, error: Option<String>) -> Reply {
        let mut snapshot = self.snapshot.clone();
        snapshot.settings = self.config.as_ref().map(|config| config.settings.clone());
        snapshot.next_run = self
            .due
            .map(|deadline| now() + deadline.saturating_duration_since(Instant::now()).as_secs());
        Reply { snapshot, error }
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
        self.config = Some(config);
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

fn start_update(engine: &Shared, state: &mut Engine) -> Result<()> {
    if state.snapshot.phase == Phase::Running {
        return Err("正在更新，请稍后再试。".into());
    }
    let config = state.config.clone().ok_or("请先保存 DDNS 设置。")?;
    let shared = Arc::clone(engine);
    // The worker cannot acquire the engine until this request releases its lock.
    thread::Builder::new()
        .name("ddns-update".into())
        .spawn(move || {
            let current = network::candidates().and_then(|candidates| {
                select_address(&config.settings.address, &candidates).map_err(str::to_owned)
            });
            let address = current.as_ref().ok().copied();
            let result = current.and_then(|address| dns::update(&config, address));
            let mut state = shared.lock().unwrap();
            state.snapshot.address = address;
            state.snapshot.last_run = Some(now());
            let (phase, message, delay) = match result {
                Ok(message) => (Phase::Success, message.to_owned(), INTERVAL),
                Err(message) => (Phase::Failed, message, RETRY),
            };
            state.snapshot.phase = phase;
            state.snapshot.message = message;
            state.due = state
                .snapshot
                .automatic
                .then(|| Instant::now() + Duration::from_secs(delay));
        })
        .map_err(|e| error("启动 DNS 更新失败", e))?;
    state.snapshot.phase = Phase::Running;
    state.snapshot.message.clear();
    state.due = None;
    Ok(())
}

fn peer(stream: &UnixStream) -> Result<libc::ucred> {
    let mut credentials: libc::ucred = unsafe { zeroed() };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    } != 0
        || length as usize != size_of::<libc::ucred>()
    {
        return Err(error(
            "验证本地服务身份失败",
            std::io::Error::last_os_error(),
        ));
    }
    if credentials.uid != 0 {
        return Err("拒绝非 root 的本地通道进程。".into());
    }
    Ok(credentials)
}

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| "本地服务请求超时。".into())
}

fn read_exact(stream: &mut UnixStream, mut bytes: &mut [u8], deadline: Instant) -> Result<()> {
    while !bytes.is_empty() {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(|e| error("设置通道超时失败", e))?;
        match stream.read(bytes) {
            Ok(0) => return Err("本地服务连接提前关闭。".into()),
            Ok(length) => bytes = &mut bytes[length..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                if STOP.load(Ordering::SeqCst) {
                    return Err("服务正在退出。".into());
                }
            }
            Err(e) => return Err(error("读取本地服务消息失败", e)),
        }
    }
    Ok(())
}

fn write_all(stream: &mut UnixStream, mut bytes: &[u8], deadline: Instant) -> Result<()> {
    while !bytes.is_empty() {
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(|e| error("设置通道超时失败", e))?;
        match stream.write(bytes) {
            Ok(0) => return Err("本地服务连接无法写入。".into()),
            Ok(length) => bytes = &bytes[length..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                if STOP.load(Ordering::SeqCst) {
                    return Err("服务正在退出。".into());
                }
            }
            Err(e) => return Err(error("写入本地服务消息失败", e)),
        }
    }
    Ok(())
}

fn read_message(stream: &mut UnixStream, deadline: Instant) -> Result<Vec<u8>> {
    let mut header = [0; 4];
    read_exact(stream, &mut header, deadline)?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > LIMIT {
        return Err("本地服务消息超出大小限制。".into());
    }
    let mut bytes = vec![0; length];
    read_exact(stream, &mut bytes, deadline)?;
    Ok(bytes)
}

fn write_message(stream: &mut UnixStream, bytes: &[u8], deadline: Instant) -> Result<()> {
    if bytes.is_empty() || bytes.len() > LIMIT {
        return Err("本地服务消息超出大小限制。".into());
    }
    write_all(stream, &(bytes.len() as u32).to_be_bytes(), deadline)?;
    write_all(stream, bytes, deadline)
}

// AF_UNIX connect can block when a listener's backlog is full; bound that phase too.
fn connect(deadline: Instant) -> std::io::Result<UnixStream> {
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    let mut address: libc::sockaddr_un = unsafe { zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, byte) in address.sun_path.iter_mut().zip(platform::SOCKET.bytes()) {
        *target = byte as libc::c_char;
    }
    let result = unsafe {
        libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if result != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(err);
        }
        loop {
            let timeout = deadline.saturating_duration_since(Instant::now());
            if timeout.is_zero() {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            let result = unsafe {
                libc::poll(
                    &mut poll,
                    1,
                    timeout.as_millis().max(1).min(i32::MAX as u128) as i32,
                )
            };
            if result < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if result == 0 {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            if let Some(err) = stream.take_error()? {
                return Err(err);
            }
            break;
        }
    }
    stream.set_nonblocking(false)?;
    Ok(stream)
}

fn handle(stream: &mut UnixStream, engine: &Shared) -> Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    peer(stream)?;
    let bytes = read_message(stream, deadline)?;
    let reply = {
        let mut state = engine.lock().unwrap();
        let result = match platform::automatic() {
            Ok(automatic) => {
                state.automatic(automatic);
                match serde_json::from_slice::<Request>(&bytes) {
                    Ok(Request::Status) => Ok(()),
                    Ok(Request::Run) => start_update(engine, &mut state),
                    Ok(Request::Save {
                        settings,
                        credentials,
                        run,
                    }) => state.save(settings, credentials).and_then(|()| {
                        if run {
                            start_update(engine, &mut state)
                        } else {
                            Ok(())
                        }
                    }),
                    Err(_) => Err("服务无法识别该请求。".into()),
                }
            }
            Err(message) => Err(message),
        };
        state.reply(result.err())
    };
    let bytes = serde_json::to_vec(&reply).map_err(|_| "无法编码服务响应。")?;
    write_message(stream, &bytes, deadline)
}

struct Listener {
    socket: UnixListener,
    _lock: File,
    device: u64,
    inode: u64,
}

impl Drop for Listener {
    fn drop(&mut self) {
        if fs::symlink_metadata(platform::SOCKET)
            .is_ok_and(|metadata| metadata.dev() == self.device && metadata.ino() == self.inode)
        {
            let _ = fs::remove_file(platform::SOCKET);
        }
        // Keep the lock inode. Unlinking it would permit two simultaneous flock owners.
    }
}

fn listener() -> Result<Listener> {
    platform::directory(Path::new(platform::RUNTIME), 0o700)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/run/ipv6-ddns/service.lock")
        .map_err(|e| error("打开服务锁失败", e))?;
    let metadata = lock.metadata().map_err(|e| error("检查服务锁失败", e))?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
    {
        return Err("服务锁文件的所有者或权限不安全。".into());
    }
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(error(
            "DDNS 服务已在运行，无法取得独占锁",
            std::io::Error::last_os_error(),
        ));
    }
    match fs::symlink_metadata(platform::SOCKET) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket()
                || metadata.uid() != 0
                || metadata.mode() & 0o777 != 0o600
            {
                return Err("已有控制通道不安全，拒绝替换。".into());
            }
            match connect(Instant::now() + TIMEOUT) {
                Ok(_) => return Err("已有 DDNS 服务正在监听，拒绝替换其通道。".into()),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) =>
                {
                    fs::remove_file(platform::SOCKET)
                        .map_err(|e| error("删除过期控制通道失败", e))?;
                }
                Err(e) => return Err(error("检查已有控制通道失败，未删除通道", e)),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(error("检查控制通道失败", e)),
    }
    let socket = UnixListener::bind(platform::SOCKET).map_err(|e| error("创建控制通道失败", e))?;
    fs::set_permissions(platform::SOCKET, fs::Permissions::from_mode(0o600))
        .map_err(|e| error("设置控制通道权限失败", e))?;
    let metadata =
        fs::symlink_metadata(platform::SOCKET).map_err(|e| error("检查控制通道失败", e))?;
    let listener = Listener {
        socket,
        _lock: lock,
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    listener
        .socket
        .set_nonblocking(true)
        .map_err(|e| error("设置控制通道失败", e))?;
    Ok(listener)
}

pub fn run() -> Result<()> {
    platform::require_root()?;
    STOP.store(false, Ordering::SeqCst);
    unsafe {
        libc::signal(
            libc::SIGTERM,
            stop_signal as *const () as libc::sighandler_t,
        );
        libc::signal(libc::SIGINT, stop_signal as *const () as libc::sighandler_t);
    }
    let listener = listener()?;
    let mut state = Engine::default();
    match platform::load() {
        Ok(config) => state.config = config,
        Err(message) => {
            state.snapshot.phase = Phase::Failed;
            state.snapshot.message = message;
        }
    }
    state.automatic(platform::automatic()?);
    let engine = Arc::new(Mutex::new(state));
    while !STOP.load(Ordering::SeqCst) {
        {
            let mut state = engine.lock().unwrap();
            match platform::automatic() {
                Ok(automatic) => state.automatic(automatic),
                Err(message) => {
                    state.automatic(false);
                    state.snapshot.message = message;
                    if state.snapshot.phase != Phase::Running {
                        state.snapshot.phase = Phase::Failed;
                    }
                }
            }
            if state.due.is_some_and(|deadline| Instant::now() >= deadline) {
                if let Err(message) = start_update(&engine, &mut state) {
                    state.snapshot.phase = Phase::Failed;
                    state.snapshot.message = message;
                    state.due = Some(Instant::now() + Duration::from_secs(RETRY));
                }
            }
        }
        match listener.socket.accept() {
            Ok((mut stream, _)) => {
                let _ = handle(&mut stream, &engine);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(error("接受本地服务连接失败", e)),
        }
    }
    Ok(())
}

pub fn client(request: Request) -> Result<Reply> {
    platform::require_root()?;
    let deadline = Instant::now() + TIMEOUT;
    let mut stream = connect(deadline).map_err(|e| error("连接 DDNS 服务失败；请先启动服务", e))?;
    peer(&stream)?;
    let bytes = serde_json::to_vec(&request).map_err(|_| "无法编码服务请求。")?;
    write_message(&mut stream, &bytes, deadline)?;
    let bytes = read_message(&mut stream, deadline)?;
    serde_json::from_slice(&bytes).map_err(|_| "无法读取服务响应。".into())
}

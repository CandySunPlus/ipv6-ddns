use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ipv6_ddns::{
    Config, Credentials, INTERVAL, Phase, RETRY, Reply, Request, Settings, Snapshot, select_address,
};

use super::platform;
use super::{Result, error, network};

const LIMIT: usize = 16 * 1024;
type Shared = Arc<Mutex<Engine>>;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_signal: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

#[derive(Default)]
struct Engine {
    config: Option<Config>,
    snapshot: Snapshot,
    due: Option<Instant>,
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
        let outcome = current.and_then(|address| ipv6_ddns::dns::update(&config, address));
        let mut state = engine.lock().unwrap();
        state.snapshot.address = address;
        state.snapshot.last_run = Some(now());
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

fn read_message(stream: &mut UnixStream) -> Result<Vec<u8>> {
    let mut header = [0u8; 4];
    stream
        .read_exact(&mut header)
        .map_err(|e| error("读取服务消息失败", e))?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > LIMIT {
        return Err("服务消息超出大小限制。".into());
    }
    let mut buffer = vec![0; length];
    stream
        .read_exact(&mut buffer)
        .map_err(|e| error("读取服务消息失败", e))?;
    Ok(buffer)
}

fn write_message(stream: &mut UnixStream, message: &[u8]) -> Result<()> {
    if message.len() > LIMIT {
        return Err("服务消息超出大小限制。".into());
    }
    stream
        .write_all(&(message.len() as u32).to_be_bytes())
        .and_then(|()| stream.write_all(message))
        .map_err(|e| error("发送服务消息失败", e))
}

fn peer(stream: &UnixStream) -> Option<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    (unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } == 0).then_some(uid)
}

fn handle(stream: &mut UnixStream, engine: &Shared, owner: u32) -> Result<()> {
    if peer(stream) != Some(owner) {
        return Err("本地通道的访问者与安装账户不匹配。".into());
    }
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| error("配置服务通道失败", e))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| error("配置服务通道失败", e))?;
    let data = read_message(stream)?;
    let reply = {
        let mut state = engine.lock().unwrap();
        let error = match serde_json::from_slice::<Request>(&data) {
            Ok(Request::Status) => None,
            Ok(Request::Run) if state.config.is_none() => Some("请先保存 DDNS 设置。".into()),
            Ok(Request::Run) if state.snapshot.phase == Phase::Running => {
                Some("正在更新，请稍后再试。".into())
            }
            Ok(Request::Run) => {
                start_update(engine, &mut state);
                None
            }
            Ok(Request::Save {
                settings,
                credentials,
                run,
            }) => match state.save(settings, credentials) {
                Ok(()) => {
                    if run {
                        start_update(engine, &mut state);
                    }
                    None
                }
                Err(e) => Some(e),
            },
            Err(_) => Some("服务无法识别该请求。".into()),
        };
        state.reply(error)
    };
    let data = serde_json::to_vec(&reply).map_err(|_| "无法编码服务响应。".to_string())?;
    write_message(stream, &data)?;
    Ok(())
}

fn serve(listener: UnixListener, engine: Shared, owner: u32) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        let _ = handle(&mut stream, &engine, owner);
    }
}

pub fn run() -> Result<()> {
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let owner = platform::owner()?.0;
    let engine = Arc::new(Mutex::new(Engine::default()));
    {
        let mut state = engine.lock().unwrap();
        match platform::load() {
            Ok(config) => state.config = config,
            Err(message) => {
                state.snapshot.phase = Phase::Failed;
                state.snapshot.message = message;
            }
        }
        state.set_automatic(platform::automatic()?);
    }
    let listener = platform::listener()?;
    let shared = Arc::clone(&engine);
    thread::spawn(move || serve(listener, shared, owner));
    while !STOP.load(Ordering::SeqCst) {
        {
            let mut state = engine.lock().unwrap();
            if let Ok(automatic) = platform::automatic() {
                state.set_automatic(automatic);
            }
            if state.due.is_some_and(|due| Instant::now() >= due) {
                start_update(&engine, &mut state);
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

fn connect(path: &Path) -> Result<UnixStream> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionReset
                ) && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(30));
            }
            Err(e) => return Err(error("连接 DDNS 服务失败", e)),
        }
    }
}

fn exchange(path: &Path, expected_uid: u32, request: Request) -> Result<Reply> {
    let mut stream = connect(path)?;
    if peer(&stream) != Some(expected_uid) {
        return Err("本地通道的进程与 DDNS 服务不匹配。".into());
    }
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| error("配置服务通道失败", e))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| error("配置服务通道失败", e))?;
    let data = serde_json::to_vec(&request).map_err(|_| "无法编码服务请求。")?;
    write_message(&mut stream, &data)?;
    let data = read_message(&mut stream)?;
    serde_json::from_slice(&data).map_err(|_| "无法读取服务响应。".into())
}

pub fn client(request: Request) -> Result<Reply> {
    exchange(Path::new(platform::SOCKET), 0, request)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "ipv6ddns-{name}-{}-{}.sock",
            std::process::id(),
            now()
        ))
    }

    fn roundtrip(
        path: &Path,
        listener: &UnixListener,
        engine: &Shared,
        owner: u32,
        request: Request,
    ) -> Result<Reply> {
        let client_path = path.to_owned();
        let client = thread::spawn(move || exchange(&client_path, owner, request));
        let (mut server, _) = listener.accept().unwrap();
        let _ = self::handle(&mut server, engine, owner);
        client.join().unwrap()
    }

    #[test]
    fn exchanges_requests_over_a_local_socket() {
        let path = temporary_path("service");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let uid = unsafe { libc::geteuid() };
        let engine = Arc::new(Mutex::new(Engine::default()));

        let reply = roundtrip(&path, &listener, &engine, uid, Request::Status).unwrap();
        assert!(reply.error.is_none());
        assert_eq!(reply.snapshot.phase, Phase::Idle);
        assert!(!reply.snapshot.automatic);

        let reply = roundtrip(&path, &listener, &engine, uid, Request::Run).unwrap();
        assert_eq!(reply.error.as_deref(), Some("请先保存 DDNS 设置。"));

        let client_path = path.clone();
        let client = thread::spawn(move || {
            let mut rejected = UnixStream::connect(&client_path).unwrap();
            let mut byte = [0u8; 1];
            rejected.read(&mut byte).unwrap()
        });
        let (mut server, _) = listener.accept().unwrap();
        let _ = self::handle(&mut server, &engine, uid + 1);
        drop(server);
        assert_eq!(client.join().unwrap(), 0);
        let _ = std::fs::remove_file(path);
    }
}

//! Local tray diagnostics. Never pass configuration, request bodies or panic payloads here.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{GetLastError, HWND, SYSTEMTIME};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW, WM_NULL,
};

const MAX_BYTES: u64 = 1024 * 1024;
const STAGES: &[&str] = &[
    "idle",
    "dispatch",
    "keyboard",
    "tray-add",
    "tray-menu",
    "show",
    "hide",
    "layout",
    "render",
    "destroy",
    "receive",
];
static STAGE: AtomicUsize = AtomicUsize::new(0);
static MESSAGE: AtomicUsize = AtomicUsize::new(0);
static LOGGER: OnceLock<Option<Logger>> = OnceLock::new();

struct LogFile {
    path: PathBuf,
    file: Option<File>,
    length: u64,
    limit: u64,
}

impl LogFile {
    fn open(path: PathBuf, limit: u64) -> io::Result<Self> {
        fs::create_dir_all(
            path.parent()
                .ok_or_else(|| io::Error::other("no log directory"))?,
        )?;
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let length = file.metadata()?.len();
        Ok(Self {
            path,
            file: Some(file),
            length,
            limit,
        })
    }

    fn append(&mut self, line: &str) -> io::Result<()> {
        if self.length > 0 && self.length + line.len() as u64 + 1 > self.limit {
            // Close before rotating: Windows cannot always rename an open file.
            self.file.take();
            let previous = self.path.with_extension("log.1");
            if previous.exists() {
                fs::copy(&previous, self.path.with_extension("log.2"))?;
            }
            fs::copy(&self.path, &previous)?;
            fs::write(&self.path, [])?;
            self.length = 0;
        }
        if self.file.is_none() {
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
        }
        let file = self.file.as_mut().unwrap();
        writeln!(file, "{line}")?;
        file.flush()?;
        self.length += line.len() as u64 + 1;
        Ok(())
    }
}

enum Entry {
    Line(String),
    Flush(mpsc::SyncSender<()>),
}

struct Logger {
    sender: mpsc::SyncSender<Entry>,
    file: Arc<Mutex<LogFile>>,
}

fn line(message: &str) -> String {
    let mut time = SYSTEMTIME::default();
    unsafe { GetLocalTime(&mut time) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} pid={} tid={} {}",
        time.wYear,
        time.wMonth,
        time.wDay,
        time.wHour,
        time.wMinute,
        time.wSecond,
        time.wMilliseconds,
        std::process::id(),
        unsafe { GetCurrentThreadId() },
        message
    )
}

pub fn init() {
    LOGGER.get_or_init(|| {
        let path = PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
            .join("IPv6DDNS")
            .join("Logs")
            .join("tray.log");
        let file = Arc::new(Mutex::new(LogFile::open(path, MAX_BYTES).ok()?));
        let writer = Arc::clone(&file);
        let (sender, receiver) = mpsc::sync_channel(128);
        std::thread::Builder::new()
            .name("tray-log".into())
            .spawn(move || {
                while let Ok(entry) = receiver.recv() {
                    match entry {
                        Entry::Line(line) => {
                            if let Ok(mut file) = writer.lock() {
                                let _ = file.append(&line);
                            }
                        }
                        Entry::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })
            .ok()?;
        Some(Logger { sender, file })
    });
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(Some(logger)) = LOGGER.get() {
            // Release builds abort after this hook: write directly, without waiting for the logger.
            // Omit the panic payload, which could contain configuration or HTTP response text.
            if let Ok(mut file) = logger.file.try_lock() {
                let _ = file.append(&line(&format!(
                    "panic location={} stage={} backtrace={}",
                    info.location().map(|p| p.to_string()).unwrap_or_default(),
                    stage(),
                    std::backtrace::Backtrace::force_capture()
                )));
            }
        }
        previous(info);
    }));
}

pub fn record(message: impl AsRef<str>) {
    if let Some(Some(logger)) = LOGGER.get() {
        // Disk stalls and a full queue must never block the window thread.
        let _ = logger.sender.try_send(Entry::Line(line(message.as_ref())));
    }
}

pub fn flush() {
    if let Some(Some(logger)) = LOGGER.get() {
        let (sender, receiver) = mpsc::sync_channel(1);
        if logger.sender.try_send(Entry::Flush(sender)).is_ok() {
            let _ = receiver.recv_timeout(Duration::from_secs(1));
        }
    }
}

fn stage() -> &'static str {
    STAGES
        .get(STAGE.load(Ordering::Relaxed))
        .copied()
        .unwrap_or("unknown")
}

pub struct Stage(usize);

impl Stage {
    pub fn enter(name: &'static str) -> Self {
        let next = STAGES.iter().position(|item| *item == name).unwrap_or(0);
        Self(STAGE.swap(next, Ordering::Relaxed))
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        STAGE.store(self.0, Ordering::Relaxed);
    }
}

pub struct Message(usize);

impl Message {
    pub fn enter(message: u32) -> Self {
        Self(MESSAGE.swap(message as usize, Ordering::Relaxed))
    }
}

impl Drop for Message {
    fn drop(&mut self) {
        MESSAGE.store(self.0, Ordering::Relaxed);
    }
}

fn probe(hwnd: usize, timeout_ms: u32) -> Result<(), u32> {
    let mut result = 0;
    let ok = unsafe {
        SendMessageTimeoutW(
            hwnd as HWND,
            WM_NULL,
            0,
            0,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            timeout_ms,
            &mut result,
        )
    };
    if ok == 0 {
        Err(unsafe { GetLastError() })
    } else {
        Ok(())
    }
}

pub struct Watchdog(mpsc::Sender<()>);

impl Watchdog {
    pub fn start(hwnd: HWND) -> Self {
        let (sender, receiver) = mpsc::channel();
        let window = hwnd as usize;
        if let Err(error) = std::thread::Builder::new()
            .name("tray-watchdog".into())
            .spawn(move || {
                let mut failed = false;
                let mut last_check = Instant::now();
                let mut last_heartbeat = Instant::now();
                let mut checked = false;
                loop {
                    if receiver.recv_timeout(Duration::from_secs(15))
                        != Err(mpsc::RecvTimeoutError::Timeout)
                    {
                        break;
                    }
                    let gap = last_check.elapsed();
                    if gap > Duration::from_secs(45) {
                        record(format!(
                            "watchdog.gap seconds={} possible_suspend=true",
                            gap.as_secs()
                        ));
                    }
                    last_check = Instant::now();
                    match probe(window, 3000) {
                        Err(code) => {
                            record(format!(
                                "ui.unresponsive timeout_ms=3000 stage={} message=0x{:04x} win32_error={code}",
                                stage(), MESSAGE.load(Ordering::Relaxed)
                            ));
                            failed = true;
                        }
                        Ok(()) => {
                            if failed {
                                record("ui.recovered");
                                failed = false;
                            }
                            if !checked || last_heartbeat.elapsed() >= Duration::from_secs(300) {
                                record("ui.heartbeat responsive=true");
                                last_heartbeat = Instant::now();
                                checked = true;
                            }
                        }
                    }
                }
            })
        {
            record(format!("watchdog.start_failed kind={:?}", error.kind()));
        }
        Self(sender)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{CloseHandle, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    use windows_sys::w;

    #[test]
    fn rotates_logs_with_a_fixed_size_and_two_backups() {
        let directory = std::env::temp_dir().join(format!("ddns-log-test-{}", std::process::id()));
        let path = directory.join("tray.log");
        let mut log = LogFile::open(path.clone(), 8).unwrap();
        for entry in ["first", "second", "third", "fourth"] {
            log.append(entry).unwrap();
        }
        drop(log);
        assert_eq!(fs::read_to_string(&path).unwrap(), "fourth\n");
        assert_eq!(
            fs::read_to_string(path.with_extension("log.1")).unwrap(),
            "third\n"
        );
        assert_eq!(
            fs::read_to_string(path.with_extension("log.2")).unwrap(),
            "second\n"
        );
        for name in ["tray.log", "tray.log.1", "tray.log.2"] {
            fs::remove_file(directory.join(name)).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }

    unsafe extern "system" fn test_window(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_APP {
            unsafe {
                SetEvent(wp as _);
                WaitForSingleObject(lp as _, 2000);
            }
            return 0;
        }
        if msg == WM_DESTROY {
            unsafe { PostQuitMessage(0) };
            return 0;
        }
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }

    #[test]
    fn detects_a_blocked_window_and_its_recovery() {
        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || unsafe {
            let instance = GetModuleHandleW(null());
            let class = w!("IPv6DDNS.DiagnosticsTest");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(test_window),
                hInstance: instance,
                lpszClassName: class,
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&wc), 0);
            let hwnd = CreateWindowExW(
                0,
                class,
                null(),
                WS_POPUP,
                0,
                0,
                10,
                10,
                null_mut(),
                null_mut(),
                instance,
                null(),
            );
            assert!(!hwnd.is_null());
            sender.send(hwnd as usize).unwrap();
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                DispatchMessageW(&msg);
            }
            UnregisterClassW(class, instance);
        });
        let hwnd = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(probe(hwnd, 1000).is_ok());
        unsafe {
            let blocked = CreateEventW(null(), 1, 0, null());
            let release = CreateEventW(null(), 1, 0, null());
            assert!(!blocked.is_null() && !release.is_null());
            PostMessageW(hwnd as HWND, WM_APP, blocked as usize, release as isize);
            assert_eq!(WaitForSingleObject(blocked, 1000), 0);
            let result = probe(hwnd, 50);
            SetEvent(release);
            assert!(
                result.is_err(),
                "a blocked window must fail the watchdog probe"
            );
            assert!(
                probe(hwnd, 1000).is_ok(),
                "a recovered window must answer the probe"
            );
            PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0);
            CloseHandle(blocked);
            CloseHandle(release);
        }
        thread.join().unwrap();
    }
}

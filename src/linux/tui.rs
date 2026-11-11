use std::io::{self, IsTerminal, Write};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, ResetColor, SetAttribute};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use ipv6_ddns::{
    AddressPolicy, Credentials, Ipv6Suffix, Phase, Provider, Request, Settings, Snapshot,
};
use unicode_width::UnicodeWidthChar;

use super::{Result, platform, service};

/// Own terminal modes before doing any fallible drawing, so every return restores them.
struct Terminal;

impl Terminal {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode().map_err(|error| error.to_string())?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen, ResetColor, Hide)
            .map_err(|error| error.to_string())?;
        Ok(guard)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            SetAttribute(Attribute::Reset),
            ResetColor,
            Show,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Draft {
    provider: Provider,
    domain: String,
    token: String,
    access_key_id: String,
    access_key_secret: String,
    suffix_mode: bool,
    suffix: String,
    automatic: bool,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            provider: Provider::Aliyun,
            domain: String::new(),
            token: String::new(),
            access_key_id: String::new(),
            access_key_secret: String::new(),
            suffix_mode: false,
            suffix: String::new(),
            automatic: false,
        }
    }
}

impl Draft {
    fn settings(&self) -> Result<Settings> {
        Ok(Settings {
            domain: self.domain.trim().to_owned(),
            provider: self.provider,
            address: if self.suffix_mode {
                AddressPolicy::Suffix(Ipv6Suffix::parse(&self.suffix).map_err(str::to_owned)?)
            } else {
                AddressPolicy::Stable
            },
        })
    }

    fn credentials(&self) -> Credentials {
        match self.provider {
            Provider::Cloudflare => Credentials::Cloudflare {
                token: self.token.clone(),
            },
            Provider::Aliyun => Credentials::Aliyun {
                access_key_id: self.access_key_id.clone(),
                access_key_secret: self.access_key_secret.clone(),
            },
        }
    }

    fn load_settings(&mut self, settings: &Settings) {
        self.provider = settings.provider;
        self.domain.clone_from(&settings.domain);
        self.suffix_mode = matches!(settings.address, AddressPolicy::Suffix(_));
        if let AddressPolicy::Suffix(suffix) = &settings.address {
            self.suffix = suffix.to_string();
        }
    }

    fn has_credentials(&self) -> bool {
        match self.provider {
            Provider::Cloudflare => !self.token.is_empty(),
            Provider::Aliyun => {
                !self.access_key_id.is_empty() || !self.access_key_secret.is_empty()
            }
        }
    }

    fn text(&self, field: Field) -> &str {
        match field {
            Field::Domain => &self.domain,
            Field::Token => &self.token,
            Field::Id => &self.access_key_id,
            Field::Secret => &self.access_key_secret,
            Field::Suffix => &self.suffix,
            _ => "",
        }
    }

    fn set_text(&mut self, field: Field, value: String) {
        match field {
            Field::Domain => self.domain = value,
            Field::Token => self.token = value,
            Field::Id => self.access_key_id = value,
            Field::Secret => self.access_key_secret = value,
            Field::Suffix => self.suffix = value,
            _ => {}
        }
    }

    fn redact(&self, mut message: String) -> String {
        for secret in [&self.token, &self.access_key_secret] {
            if !secret.is_empty() {
                message = message.replace(secret.as_str(), "[已隐藏]");
            }
        }
        message
    }
}

enum Job {
    Save(Draft),
    Run,
    Install,
    Start,
    Stop,
}

struct Status {
    installed: bool,
    running: Result<bool>,
    automatic: Result<bool>,
    snapshot: Result<Snapshot>,
}

enum Completion {
    Save {
        submitted: Draft,
        snapshot: Option<Snapshot>,
        config_saved: bool,
        error: Option<String>,
    },
    Action {
        result: Result<&'static str>,
        snapshot: Option<Snapshot>,
    },
}

enum Message {
    Status(Status),
    Done(Completion),
}

fn status() -> Status {
    Status {
        installed: platform::installed(),
        running: platform::running(),
        automatic: platform::automatic(),
        snapshot: service::client(Request::Status).and_then(|reply| match reply.error {
            Some(error) => Err(error),
            None => Ok(reply.snapshot),
        }),
    }
}

fn perform(job: Job) -> Completion {
    match job {
        Job::Save(submitted) => {
            let result = submitted.settings().and_then(|settings| {
                service::client(Request::Save {
                    settings,
                    credentials: submitted.credentials(),
                    run: false,
                })
            });
            let (snapshot, config_saved, error) = match result {
                Err(error) => (None, false, Some(error)),
                Ok(reply) => {
                    match reply.error {
                        Some(error) => (Some(reply.snapshot), false, Some(error)),
                        None => {
                            let error = platform::set_auto(submitted.automatic).err().map(|error| {
                            format!("DNS 配置已保存，但自动启动保存失败：{error}；草稿未标记为保存。")
                        });
                            (Some(reply.snapshot), true, error)
                        }
                    }
                }
            };
            let error = error.map(|error| submitted.redact(error));
            Completion::Save {
                submitted,
                snapshot,
                config_saved,
                error,
            }
        }
        Job::Run => match service::client(Request::Run) {
            Ok(reply) => Completion::Action {
                result: match reply.error {
                    Some(error) => Err(error),
                    None => Ok("已请求同步已保存配置；草稿未参与。"),
                },
                snapshot: Some(reply.snapshot),
            },
            Err(error) => Completion::Action {
                result: Err(error),
                snapshot: None,
            },
        },
        Job::Install => Completion::Action {
            result: platform::install()
                .map(|()| "服务已安装并启动，默认启用自动启动；配置草稿未保存。"),
            snapshot: None,
        },
        Job::Start => Completion::Action {
            result: platform::start().map(|()| "服务已启动；配置草稿未保存。"),
            snapshot: None,
        },
        Job::Stop => Completion::Action {
            result: platform::stop().map(|()| "服务已停止；退出 TUI 不会改变服务状态。"),
            snapshot: None,
        },
    }
}

// All socket/systemd work stays off the input thread. A disconnected UI ends the
// worker; no-service polling sleeps for two seconds rather than retrying in a loop.
fn worker(jobs: Receiver<Job>, messages: mpsc::Sender<Message>) {
    if messages.send(Message::Status(status())).is_err() {
        return;
    }
    loop {
        match jobs.recv_timeout(Duration::from_secs(2)) {
            Ok(job) => {
                if messages.send(Message::Done(perform(job))).is_err() {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if messages.send(Message::Status(status())).is_err() {
            return;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Run,
    Provider,
    Domain,
    Id,
    Secret,
    Token,
    Strategy,
    Suffix,
    Automatic,
    Install,
    Service,
}

impl Field {
    fn editable(self) -> bool {
        matches!(
            self,
            Self::Domain | Self::Id | Self::Secret | Self::Token | Self::Suffix
        )
    }

    fn secret(self) -> bool {
        matches!(self, Self::Secret | Self::Token)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Run => "立即同步已保存配置",
            Self::Provider => "DNS 服务商",
            Self::Domain => "AAAA 域名",
            Self::Id => "AccessKey ID",
            Self::Secret => "AccessKey Secret",
            Self::Token => "API Token",
            Self::Strategy => "地址选择",
            Self::Suffix => "IPv6 后缀",
            Self::Automatic => "开机自动启动",
            Self::Install => "安装 systemd 服务",
            Self::Service => "后台服务",
        }
    }
}

struct Editor {
    field: Field,
    text: String,
    cursor: usize,
}

impl Editor {
    fn byte_cursor(&self) -> usize {
        self.text
            .char_indices()
            .nth(self.cursor)
            .map_or(self.text.len(), |(index, _)| index)
    }

    fn key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.text.chars().count()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.chars().count(),
            KeyCode::Backspace if self.cursor > 0 => {
                let end = self.byte_cursor();
                self.cursor -= 1;
                let start = self.byte_cursor();
                self.text.replace_range(start..end, "");
            }
            KeyCode::Delete if self.cursor < self.text.chars().count() => {
                let start = self.byte_cursor();
                let end = start + self.text[start..].chars().next().map_or(0, char::len_utf8);
                self.text.replace_range(start..end, "");
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !character.is_control() =>
            {
                self.text.insert(self.byte_cursor(), character);
                self.cursor += 1;
            }
            _ => {}
        }
    }
}

struct Ui {
    tab: usize,
    focus: usize,
    draft: Draft,
    snapshot: Option<Snapshot>,
    installed: bool,
    running: Option<bool>,
    automatic: Option<bool>,
    connection_error: Option<String>,
    platform_error: Option<String>,
    notice: String,
    edited: bool,
    initialized: bool,
    partial_save: bool,
    busy: bool,
    editor: Option<Editor>,
}

impl Ui {
    fn new() -> Self {
        Self {
            tab: 0,
            focus: 0,
            draft: Draft::default(),
            snapshot: None,
            installed: false,
            running: None,
            automatic: None,
            connection_error: Some("正在连接后台服务…".to_owned()),
            platform_error: None,
            notice: "保存不会同步 DNS；F5 只使用后台已保存配置。".to_owned(),
            edited: false,
            initialized: false,
            partial_save: false,
            busy: false,
            editor: None,
        }
    }

    fn fields(&self) -> &'static [Field] {
        match self.tab {
            0 => &[Field::Run],
            1 => match self.draft.provider {
                Provider::Aliyun => &[Field::Provider, Field::Domain, Field::Id, Field::Secret],
                Provider::Cloudflare => &[Field::Provider, Field::Domain, Field::Token],
            },
            2 if self.draft.suffix_mode => &[Field::Strategy, Field::Suffix],
            2 => &[Field::Strategy],
            _ => &[Field::Automatic, Field::Install, Field::Service],
        }
    }

    fn dirty(&self) -> bool {
        self.partial_save
            || self.draft.has_credentials()
            || self.automatic != Some(self.draft.automatic)
            || match self.draft.settings() {
                Ok(settings) => {
                    self.snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.settings.as_ref())
                        != Some(&settings)
                }
                Err(_) => true,
            }
            || self
                .editor
                .as_ref()
                .is_some_and(|editor| self.draft.text(editor.field) != editor.text)
    }

    fn accept_snapshot(&mut self, snapshot: Snapshot) {
        if !self.initialized {
            if !self.edited {
                if let Some(settings) = &snapshot.settings {
                    self.draft.load_settings(settings);
                }
                self.draft.automatic = self.automatic.unwrap_or(snapshot.automatic);
            }
            self.initialized = true;
        }
        self.snapshot = Some(snapshot);
    }

    fn message(&mut self, message: Message) {
        match message {
            Message::Status(status) => {
                self.installed = status.installed;
                self.running = status.running.as_ref().ok().copied();
                self.automatic = status.automatic.as_ref().ok().copied();
                self.platform_error = status.running.err().or_else(|| status.automatic.err());
                match status.snapshot {
                    Ok(snapshot) => {
                        self.connection_error = None;
                        self.accept_snapshot(snapshot);
                    }
                    Err(error) => self.connection_error = Some(error),
                }
            }
            Message::Done(completion) => {
                self.busy = false;
                match completion {
                    Completion::Save {
                        submitted,
                        snapshot,
                        config_saved,
                        error,
                    } => {
                        if let Some(snapshot) = snapshot {
                            self.accept_snapshot(snapshot);
                        }
                        if config_saved {
                            // Clear only the submitted provider's unchanged fields. Typing or
                            // switching provider while the worker saves never loses new edits.
                            match submitted.provider {
                                Provider::Cloudflare if self.draft.token == submitted.token => {
                                    self.draft.token.clear()
                                }
                                Provider::Aliyun => {
                                    if self.draft.access_key_id == submitted.access_key_id {
                                        self.draft.access_key_id.clear();
                                    }
                                    if self.draft.access_key_secret == submitted.access_key_secret {
                                        self.draft.access_key_secret.clear();
                                    }
                                }
                                _ => {}
                            }
                            if error.is_none() {
                                self.automatic = Some(submitted.automatic);
                                if self.draft.settings().ok() == submitted.settings().ok() {
                                    if let Some(settings) = self
                                        .snapshot
                                        .as_ref()
                                        .and_then(|snapshot| snapshot.settings.as_ref())
                                    {
                                        self.draft.load_settings(settings);
                                    }
                                }
                            }
                        }
                        if config_saved {
                            self.partial_save = error.is_some();
                        }
                        self.notice = error
                            .unwrap_or_else(|| "已保存配置及自动启动设置；未同步 DNS。".to_owned());
                    }
                    Completion::Action { result, snapshot } => {
                        if let Some(snapshot) = snapshot {
                            self.accept_snapshot(snapshot);
                        }
                        self.notice = match result {
                            Ok(message) => message.to_owned(),
                            Err(error) => format!("操作失败：{error}"),
                        };
                    }
                }
            }
        }
        self.focus = self.focus.min(self.fields().len() - 1);
    }

    fn submit(&mut self, job: Job, jobs: &SyncSender<Job>) {
        // SAFETY: geteuid has no preconditions and does not mutate process state.
        if unsafe { libc::geteuid() } != 0 {
            self.notice =
                "需要 root 权限；请退出后运行 sudo ipv6-ddns tui（不会自动提权）。".to_owned();
            return;
        }
        if self.busy {
            self.notice = "后台操作尚未完成，请稍候；可继续编辑草稿。".to_owned();
            return;
        }
        match jobs.try_send(job) {
            Ok(()) => {
                self.busy = true;
                self.notice = "后台正在处理；可继续编辑草稿。".to_owned();
            }
            Err(mpsc::TrySendError::Full(_)) => {
                self.notice = "后台操作正在排队，请稍候。".to_owned()
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.notice = "后台工作线程已退出；请重新打开 TUI。".to_owned()
            }
        }
    }

    fn commit_editor(&mut self) {
        if let Some(editor) = self.editor.take() {
            self.draft.set_text(editor.field, editor.text);
            self.edited = true;
            self.notice = "字段已更新为草稿；Ctrl+S 保存，F5 不使用草稿。".to_owned();
        }
    }

    fn key(&mut self, key: KeyEvent, jobs: &SyncSender<Job>) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            self.commit_editor();
            if let Err(error) = self.draft.settings() {
                self.notice = error;
            } else {
                self.submit(Job::Save(self.draft.clone()), jobs);
            }
            return true;
        }
        if key.code == KeyCode::F(5) {
            self.submit(Job::Run, jobs);
            return true;
        }
        if self.editor.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.editor = None;
                    self.notice = "已取消编辑，保留原草稿。".to_owned();
                }
                KeyCode::Enter => self.commit_editor(),
                _ => {
                    if let Some(editor) = &mut self.editor {
                        editor.key(key);
                    }
                }
            }
            return true;
        }
        let fields = self.fields();
        let field = fields[self.focus];
        match key.code {
            KeyCode::Char('q' | 'Q') => return false,
            KeyCode::Char(number @ '1'..='4') => {
                self.tab = (number as u8 - b'1') as usize;
                self.focus = 0;
            }
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % fields.len(),
            KeyCode::BackTab | KeyCode::Up => {
                self.focus = (self.focus + fields.len() - 1) % fields.len()
            }
            KeyCode::Esc => {
                self.tab = 0;
                self.focus = 0;
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Enter
                if matches!(field, Field::Provider | Field::Strategy) =>
            {
                if field == Field::Provider {
                    self.draft.provider = match self.draft.provider {
                        Provider::Aliyun => Provider::Cloudflare,
                        Provider::Cloudflare => Provider::Aliyun,
                    };
                } else {
                    self.draft.suffix_mode = !self.draft.suffix_mode;
                }
                self.edited = true;
            }
            KeyCode::Char(' ') | KeyCode::Enter if field == Field::Automatic => {
                self.draft.automatic = !self.draft.automatic;
                self.edited = true;
                self.notice = "自动启动已改为草稿；Ctrl+S 后生效，不会立即同步。".to_owned();
            }
            KeyCode::Enter if field.editable() => {
                let text = self.draft.text(field).to_owned();
                self.editor = Some(Editor {
                    field,
                    cursor: text.chars().count(),
                    text,
                });
                self.edited = true;
            }
            KeyCode::Enter if field == Field::Run => self.submit(Job::Run, jobs),
            KeyCode::Enter if field == Field::Install => {
                if self.installed {
                    self.notice = "服务已经安装；如需启动，请选择下方服务操作。".to_owned();
                } else {
                    self.submit(Job::Install, jobs);
                }
            }
            KeyCode::Enter if field == Field::Service => {
                if !self.installed {
                    self.notice = "服务尚未安装；请先显式选择安装服务。".to_owned();
                } else {
                    match self.running {
                        Some(true) => self.submit(Job::Stop, jobs),
                        Some(false) => self.submit(Job::Start, jobs),
                        None => {
                            self.notice =
                                "服务状态未知；请先检查下方 systemd 错误，未执行操作。".to_owned()
                        }
                    }
                }
            }
            _ => {}
        }
        self.focus = self.focus.min(self.fields().len() - 1);
        true
    }

    fn field_value(&self, field: Field, width: usize) -> String {
        match field {
            Field::Provider => format!(
                "< {} >",
                match self.draft.provider {
                    Provider::Aliyun => "阿里云 DNS",
                    Provider::Cloudflare => "Cloudflare",
                }
            ),
            Field::Strategy => format!(
                "< {} >",
                if self.draft.suffix_mode {
                    "按 IPv6 后缀筛选"
                } else {
                    "稳定公网 IPv6"
                }
            ),
            Field::Automatic => if self.draft.automatic {
                "[x] 开启"
            } else {
                "[ ] 关闭"
            }
            .to_owned(),
            Field::Run => "[ Enter 同步 / F5 ]".to_owned(),
            Field::Install => if self.installed {
                "[ 已安装 ]"
            } else {
                "[ Enter 安装并启动 ]"
            }
            .to_owned(),
            Field::Service => match self.running {
                Some(true) => "[ Enter 停止服务 ]",
                Some(false) => "[ Enter 启动服务 ]",
                None => "[ 状态未知，暂不可操作 ]",
            }
            .to_owned(),
            _ => {
                let editor = self.editor.as_ref().filter(|editor| editor.field == field);
                let raw = editor.map_or(self.draft.text(field), |editor| editor.text.as_str());
                let saved_provider = self
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.settings.as_ref())
                    .map(|settings| settings.provider);
                let value = if let Some(editor) = editor {
                    let (before, after) = raw.split_at(editor.byte_cursor());
                    let before = if field.secret() {
                        "*".repeat(editor.cursor)
                    } else {
                        before.to_owned()
                    };
                    let after = if field.secret() {
                        "*".repeat(after.chars().count())
                    } else {
                        after.to_owned()
                    };
                    format!("{}│{after}", tail_columns(&before, width.saturating_sub(5)))
                } else if raw.is_empty()
                    && matches!(field, Field::Id | Field::Secret | Field::Token)
                    && saved_provider == Some(self.draft.provider)
                {
                    "已保存；留空保持不变".to_owned()
                } else if field.secret() {
                    "*".repeat(raw.chars().count().min(width.saturating_sub(4)))
                } else {
                    raw.to_owned()
                };
                format!("[ {value} ]")
            }
        }
    }

    fn draw(&self, output: &mut impl Write) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        queue!(
            output,
            SetAttribute(Attribute::Reset),
            ResetColor,
            Clear(ClearType::All)
        )?;
        let mut screen = Screen {
            output,
            width,
            height,
        };
        if width < 76 || height < 24 {
            screen.line(
                0,
                0,
                "请将终端调整到至少 76 列 × 24 行；q / Ctrl+C 退出。",
                true,
            )?;
            return screen.output.flush();
        }
        screen.line(1, 0, "IPv6 DDNS / Linux TUI", true)?;
        screen.line(1, 1, "AAAA 动态解析 · 配置保存与立即同步相互独立", false)?;
        let running = match self.running {
            Some(true) => "运行中",
            Some(false) => "已停止",
            None => "状态未知",
        };
        screen.line(
            1,
            3,
            &format!(
                "后台服务：{} / {}    配置：{}{}",
                if self.installed {
                    "已安装"
                } else {
                    "未安装"
                },
                running,
                if self.dirty() {
                    "* 未保存"
                } else {
                    "已保存"
                },
                if self.busy { "    后台处理中" } else { "" }
            ),
            false,
        )?;
        let mut x = 1;
        for (tab, title) in ["1 状态", "2 DNS 配置", "3 IPv6", "4 服务"]
            .iter()
            .enumerate()
        {
            let label = format!("[ {title} ]");
            screen.line(x, 5, &label, self.tab == tab)?;
            x += columns(&label) as u16 + 2;
        }
        screen.line(1, 6, &"─".repeat(width.saturating_sub(3) as usize), false)?;
        if self.tab == 0 {
            let snapshot = self.snapshot.as_ref();
            let stale = if self.connection_error.is_some() && snapshot.is_some() {
                "（上次已知，连接已断开）"
            } else {
                ""
            };
            let address = snapshot
                .and_then(|snapshot| snapshot.address)
                .map_or_else(|| "尚未选择".to_owned(), |address| address.to_string());
            screen.line(3, 8, &format!("选定 IPv6：{address}{stale}"), false)?;
            let phase = snapshot.map_or("不可用", |snapshot| match snapshot.phase {
                Phase::Idle => "等待",
                Phase::Running => "同步中",
                Phase::Success => "成功",
                Phase::Failed => "失败",
            });
            let message = snapshot.map_or("", |snapshot| snapshot.message.as_str());
            screen.line(
                3,
                10,
                &self.draft.redact(format!("同步状态：{phase}  {message}")),
                false,
            )?;
            screen.line(
                3,
                12,
                &format!(
                    "上次同步：{}",
                    timestamp(snapshot.and_then(|snapshot| snapshot.last_run))
                ),
                false,
            )?;
            screen.line(
                3,
                14,
                &format!(
                    "下次同步：{}",
                    timestamp(snapshot.and_then(|snapshot| snapshot.next_run))
                ),
                false,
            )?;
            screen.line(
                3,
                17,
                "周期 5 分钟，失败重试 1 分钟；关闭自动启动后仅按需同步。",
                false,
            )?;
        }
        for (index, field) in self.fields().iter().copied().enumerate() {
            let y = if self.tab == 0 {
                16
            } else {
                8 + index as u16 * 2
            };
            let active = index == self.focus;
            screen.line(1, y, if active { ">" } else { " " }, active)?;
            screen.line(3, y, field.label(), active)?;
            screen.line(
                25,
                y,
                &self.field_value(field, width.saturating_sub(27) as usize),
                active,
            )?;
        }
        match self.tab {
            1 => screen.line(
                3,
                16,
                "← / → 切换服务商；同一已保存服务商的空凭据保持原值。",
                false,
            )?,
            2 => {
                screen.line(3, 13, "稳定模式跳过临时隐私地址；后缀例：::8023。", false)?;
                let address = self
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.address)
                    .map_or_else(
                        || "尚无后台检测结果".to_owned(),
                        |address| address.to_string(),
                    );
                screen.line(
                    3,
                    15,
                    &format!("后台选定：{address}；修改策略后请先保存。"),
                    false,
                )?;
            }
            3 => {
                screen.line(
                    3,
                    15,
                    "安装会启动服务并默认启用自动启动；不会保存当前草稿。",
                    false,
                )?;
                screen.line(
                    3,
                    16,
                    "退出不停止服务；卸载请使用 sudo ipv6-ddns uninstall。",
                    false,
                )?;
            }
            _ => {}
        }
        let saved = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.settings.as_ref())
            .map_or_else(
                || "已保存：无法读取 / 尚无配置".to_owned(),
                |settings| {
                    format!(
                        "已保存：{} / {} / AAAA",
                        settings.provider.name(),
                        settings.domain
                    )
                },
            );
        screen.line(1, height - 6, &saved, false)?;
        let error = self
            .connection_error
            .as_ref()
            .or(self.platform_error.as_ref());
        if let Some(error) = error {
            let suffix = if unsafe { libc::geteuid() } != 0 {
                "；管理请用 sudo ipv6-ddns tui"
            } else {
                ""
            };
            screen.line(
                1,
                height - 5,
                &self.draft.redact(format!("后台不可用：{error}{suffix}")),
                false,
            )?;
        }
        screen.line(1, height - 4, &self.draft.redact(self.notice.clone()), true)?;
        if self.editor.is_some() {
            screen.line(
                1,
                height - 3,
                "编辑：Enter 确认草稿 · Esc 取消 · ←→ 移动 · Ctrl+U 清空；凭据不会显示。",
                false,
            )?;
        }
        screen.line(
            1,
            height - 2,
            "1–4 切页 Tab/↑↓ 焦点 ←→ 选择 Enter 编辑 Space 勾选 Ctrl+S 保存 F5 同步 q 退出",
            false,
        )?;
        screen.output.flush()
    }
}

fn columns(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0))
        .sum()
}

fn tail_columns(text: &str, width: usize) -> String {
    let mut used = 0;
    let mut start = text.len();
    for (index, character) in text.char_indices().rev() {
        let next = character.width().unwrap_or(0);
        if used + next > width {
            break;
        }
        used += next;
        start = index;
    }
    text[start..].to_owned()
}

struct Screen<'a, W> {
    output: &'a mut W,
    width: u16,
    height: u16,
}

impl<W: Write> Screen<'_, W> {
    fn line(&mut self, x: u16, y: u16, text: &str, active: bool) -> io::Result<()> {
        if x >= self.width || y >= self.height {
            return Ok(());
        }
        let mut clipped = String::new();
        let mut used = 0;
        // Terminal control bytes from daemon messages or pasted text must never
        // become escape sequences. Reserve the last column to avoid wrapping.
        let available = self.width.saturating_sub(x + 1) as usize;
        for character in text.chars().filter(|character| !character.is_control()) {
            let width = character.width().unwrap_or(0);
            if used + width > available {
                break;
            }
            clipped.push(character);
            used += width;
        }
        queue!(self.output, MoveTo(x, y), SetAttribute(Attribute::Reset))?;
        if active {
            queue!(
                self.output,
                SetAttribute(Attribute::Bold),
                SetAttribute(Attribute::Underlined)
            )?;
        }
        queue!(self.output, Print(clipped), SetAttribute(Attribute::Reset))
    }
}

fn timestamp(value: Option<u64>) -> String {
    let Some(value) = value else {
        return "—".to_owned();
    };
    let Ok(value) = i64::try_from(value) else {
        return "时间超出范围".to_owned();
    };
    let Ok(time) = time::OffsetDateTime::from_unix_timestamp(value) else {
        return "时间超出范围".to_owned();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let relative = if value >= 0 && value as u64 > now {
        format!("（{} 秒后）", value as u64 - now)
    } else {
        String::new()
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC{relative}",
        time.year(),
        time.month() as u8,
        time.day(),
        time.hour(),
        time.minute(),
        time.second()
    )
}

pub fn launch() -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("TUI 需要交互终端；请使用 ipv6-ddns status 查看后台状态。".to_owned());
    }
    let _terminal = Terminal::enter()?;
    let (job_tx, job_rx) = mpsc::sync_channel(1);
    let (message_tx, message_rx) = mpsc::channel();
    thread::Builder::new()
        .name("ddns-tui-status".to_owned())
        .spawn(move || worker(job_rx, message_tx))
        .map_err(|error| error.to_string())?;
    let mut ui = Ui::new();
    let mut output = io::stdout();
    let mut redraw = true;
    loop {
        while let Ok(message) = message_rx.try_recv() {
            ui.message(message);
            redraw = true;
        }
        if redraw {
            ui.draw(&mut output).map_err(|error| error.to_string())?;
            redraw = false;
        }
        if event::poll(Duration::from_millis(100)).map_err(|error| error.to_string())? {
            match event::read().map_err(|error| error.to_string())? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if !ui.key(key, &job_tx) {
                        return Ok(());
                    }
                    redraw = true;
                }
                Event::Resize(_, _) => redraw = true,
                _ => {}
            }
        }
    }
}

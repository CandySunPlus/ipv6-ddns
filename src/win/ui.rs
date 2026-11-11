use std::cell::RefCell;
use std::ptr::{null, null_mut};
use std::sync::mpsc;
use std::thread;

use ipv6_ddns::{
    AddressPolicy, Credentials, Ipv6Suffix, Phase, Provider, Reply, Request, Settings, Snapshot,
};
use windows_service::service::ServiceAccess;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemServices::{SS_LEFT, SS_RIGHT};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTimeEx};
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, IsWindowEnabled, SetFocus, VK_ESCAPE,
};
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::w;

use super::{Result, diagnostics, error, last_error, network, platform, service, wide};

const CLASS: *const u16 = w!("IPv6DDNS.NativeWindow.1");
const TRAY: u32 = WM_APP + 1;
const SHOW: u32 = WM_APP + 2;
const EVENT: u32 = WM_APP + 3;
const EXIT: u32 = WM_APP + 4;
const CONFIRM_UNINSTALL: u32 = WM_APP + 5;
const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;

const fn rgb(red: u32, green: u32, blue: u32) -> u32 {
    red | green << 8 | blue << 16
}
const PAPER: u32 = rgb(243, 243, 243);
const CARD: u32 = rgb(255, 255, 255);
const INK: u32 = rgb(26, 26, 26);
const MUTED: u32 = rgb(97, 97, 97);
const ACCENT: u32 = rgb(0, 103, 192);
const LINE: u32 = rgb(222, 222, 222);
const SUCCESS: u32 = rgb(15, 114, 59);
const ERROR: u32 = rgb(171, 42, 50);
const WIDTH: i32 = 486;
const COMPACT_HEIGHT: i32 = 171;

const STATE: usize = 0;
const BADGE: usize = 1;
const DETAIL: usize = 2;
const TIMES: usize = 3;
const TOGGLE: usize = 4;
const RUN: usize = 5;
const SETTINGS: usize = 6;
const PROVIDER_LABEL: usize = SETTINGS + 1;
const PROVIDER: usize = PROVIDER_LABEL + 1;
const HOST_LABEL: usize = PROVIDER + 1;
const HOST: usize = HOST_LABEL + 1;
const TOKEN_LABEL: usize = HOST + 1;
const TOKEN: usize = TOKEN_LABEL + 1;
const ACCESS_ID_LABEL: usize = TOKEN + 1;
const ACCESS_ID: usize = ACCESS_ID_LABEL + 1;
const ACCESS_SECRET_LABEL: usize = ACCESS_ID + 1;
const ACCESS_SECRET: usize = ACCESS_SECRET_LABEL + 1;
const FILTER: usize = ACCESS_SECRET + 1;
const SUFFIX_LABEL: usize = FILTER + 1;
const SUFFIX: usize = SUFFIX_LABEL + 1;
const HINT: usize = SUFFIX + 1;
const AUTO: usize = HINT + 1;
const UNINSTALL: usize = AUTO + 1;
const SAVE: usize = UNINSTALL + 1;
const WIDGET_COUNT: usize = SAVE + 1;
const ID: usize = 100;

#[derive(Clone, Copy)]
enum Kind {
    Label,
    Edit,
    Secret,
    Button,
    Combo,
    Check,
    Primary,
}
use Kind::*;

const WIDGETS: &[(usize, Kind, &str, [i32; 4])] = &[
    (STATE, Label, "", [75, 27, 250, 26]),
    (BADGE, Label, "", [325, 31, 127, 19]),
    (DETAIL, Label, "", [75, 56, 377, 35]),
    (TIMES, Label, "", [75, 92, 377, 17]),
    (TOGGLE, Button, "展开设置", [16, 125, 108, 32]),
    (RUN, Primary, "立即运行", [358, 125, 112, 32]),
    (SETTINGS, Label, "同步设置", [32, 184, 422, 24]),
    (PROVIDER_LABEL, Label, "DNS 服务商", [32, 217, 422, 20]),
    (PROVIDER, Combo, "", [32, 238, 422, 180]),
    (HOST_LABEL, Label, "域名", [32, 283, 422, 20]),
    (HOST, Edit, "", [32, 304, 422, 32]),
    (
        TOKEN_LABEL,
        Label,
        "Cloudflare API Token",
        [32, 349, 422, 20],
    ),
    (TOKEN, Secret, "", [32, 370, 422, 32]),
    (
        ACCESS_ID_LABEL,
        Label,
        "阿里云 AccessKey ID",
        [32, 349, 422, 20],
    ),
    (ACCESS_ID, Edit, "", [32, 370, 422, 32]),
    (
        ACCESS_SECRET_LABEL,
        Label,
        "阿里云 AccessKey Secret",
        [32, 415, 422, 20],
    ),
    (ACCESS_SECRET, Secret, "", [32, 436, 422, 32]),
    (FILTER, Check, "按 IPv6 后缀筛选", [32, 412, 422, 25]),
    (SUFFIX_LABEL, Label, "IPv6 后缀", [32, 441, 422, 19]),
    (SUFFIX, Edit, "", [32, 462, 422, 31]),
    (HINT, Label, "默认使用稳定的公网 IPv6。", [32, 501, 422, 21]),
    (AUTO, Check, "开机自动更新", [32, 527, 174, 32]),
    (UNINSTALL, Button, "卸载服务…", [231, 527, 102, 32]),
    (SAVE, Button, "保存设置", [355, 527, 99, 32]),
];

#[derive(Clone, Copy)]
enum Work {
    Poll,
    Save,
    Run,
    Install,
    Uninstall,
    Automatic(bool),
}

impl Work {
    fn name(self) -> &'static str {
        match self {
            Self::Poll => "poll",
            Self::Save => "save",
            Self::Run => "run",
            Self::Install => "install",
            Self::Uninstall => "uninstall",
            Self::Automatic(_) => "automatic",
        }
    }
}

struct Event(u64, Work, Result<Option<Reply>>);

struct Ui {
    hwnd: HWND,
    icon: HICON,
    widgets: [HWND; WIDGET_COUNT],
    fonts: Vec<HFONT>,
    brushes: [HBRUSH; 2],
    dpi: u32,
    taskbar_created: u32,
    tray_added: bool,
    installed: bool,
    initialized: bool,
    expanded: bool,
    filter: bool,
    provider: Provider,
    draft_changed: bool,
    pending: Option<Work>,
    sequence: u64,
    snapshot: Snapshot,
    notice: Option<(bool, String)>,
    connection_issue: Option<String>,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
}

thread_local! { static UI: RefCell<Option<Ui>> = const { RefCell::new(None) }; }

fn text(hwnd: HWND, value: &str) {
    if get_text(hwnd) != value {
        unsafe {
            SetWindowTextW(hwnd, wide(value).as_ptr());
        }
    }
}

fn get_text(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    let mut buffer = vec![0; len as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), len + 1) };
    String::from_utf16_lossy(&buffer[..copied as usize])
}

fn cue(hwnd: HWND, value: &str) {
    unsafe {
        SendMessageW(hwnd, EM_SETCUEBANNER, 0, wide(value).as_ptr() as isize);
    }
}

fn scale(pixels: i32, dpi: u32) -> i32 {
    (pixels * dpi as i32 + 48) / 96
}

fn settings_height(provider: Provider) -> i32 {
    578 + if provider == Provider::Aliyun { 66 } else { 0 }
}

fn window_size(dpi: u32, expanded: bool, provider: Provider) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scale(WIDTH, dpi),
        bottom: scale(
            if expanded {
                settings_height(provider)
            } else {
                COMPACT_HEIGHT
            },
            dpi,
        ),
    };
    unsafe {
        AdjustWindowRectExForDpi(&mut rect, STYLE, 0, 0, dpi);
    }
    (rect.right - rect.left, rect.bottom - rect.top)
}

fn font(face: &str, size: i32, bold: bool, dpi: u32) -> HFONT {
    unsafe {
        CreateFontW(
            -scale(size, dpi),
            0,
            0,
            0,
            if bold { 600 } else { 400 },
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            DEFAULT_PITCH as u32,
            wide(face).as_ptr(),
        )
    }
}

fn edit_line_height(hwnd: HWND) -> Option<i32> {
    unsafe {
        let dc = GetDC(hwnd);
        if dc.is_null() {
            return None;
        }
        let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HFONT;
        let old_font = if font.is_null() {
            null_mut()
        } else {
            SelectObject(dc, font)
        };
        let mut metrics = TEXTMETRICW::default();
        let measured = GetTextMetricsW(dc, &mut metrics) != 0;
        if !old_font.is_null() {
            SelectObject(dc, old_font);
        }
        ReleaseDC(hwnd, dc);
        measured.then_some(metrics.tmHeight)
    }
}

fn paint_edit_frame(hwnd: HWND) {
    let mut info = WINDOWINFO {
        cbSize: size_of::<WINDOWINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetWindowInfo(hwnd, &mut info) == 0 {
            return;
        }
        let dc = GetWindowDC(hwnd);
        if dc.is_null() {
            return;
        }
        let outer = RECT {
            left: 0,
            top: 0,
            right: info.rcWindow.right - info.rcWindow.left,
            bottom: info.rcWindow.bottom - info.rcWindow.top,
        };
        ExcludeClipRect(
            dc,
            info.rcClient.left - info.rcWindow.left,
            info.rcClient.top - info.rcWindow.top,
            info.rcClient.right - info.rcWindow.left,
            info.rcClient.bottom - info.rcWindow.top,
        );
        FillRect(dc, &outer, GetSysColorBrush(COLOR_WINDOW));
        let frame = RECT {
            bottom: outer.bottom - 1,
            ..outer
        };
        let outline = CreateSolidBrush(LINE);
        FrameRect(dc, &frame, outline);
        DeleteObject(outline);

        if IsWindowEnabled(hwnd) != 0 {
            let focused = GetFocus() == hwnd;
            let thickness = if focused {
                scale(2, GetDpiForWindow(hwnd)).max(1)
            } else {
                1
            };
            let underline = RECT {
                top: frame.bottom - thickness,
                ..frame
            };
            let brush = CreateSolidBrush(if focused { ACCENT } else { rgb(138, 138, 138) });
            FillRect(dc, &underline, brush);
            DeleteObject(brush);
        }
        ReleaseDC(hwnd, dc);
    }
}

unsafe extern "system" fn centered_edit(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    unsafe {
        match msg {
            WM_NCCALCSIZE => {
                DefSubclassProc(hwnd, msg, wp, lp);
                // Both forms of WM_NCCALCSIZE start with the new client rectangle.
                let rect = &mut *(lp as *mut RECT);
                if let Some(height) = edit_line_height(hwnd) {
                    let padding = (rect.bottom - rect.top - height).max(0);
                    rect.top += padding / 2;
                    rect.bottom -= padding - padding / 2;
                }
                0
            }
            WM_NCPAINT => {
                paint_edit_frame(hwnd);
                0
            }
            // Keep the padding clickable as part of the original edit control.
            WM_NCHITTEST => HTCLIENT as LRESULT,
            WM_SETFOCUS | WM_KILLFOCUS | WM_ENABLE => {
                let result = DefSubclassProc(hwnd, msg, wp, lp);
                RedrawWindow(hwnd, null(), null_mut(), RDW_INVALIDATE | RDW_FRAME);
                result
            }
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(centered_edit), id);
                DefSubclassProc(hwnd, msg, wp, lp)
            }
            _ => DefSubclassProc(hwnd, msg, wp, lp),
        }
    }
}

impl Ui {
    fn new(hwnd: HWND, instance: HINSTANCE, icon: HICON, installed: bool) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut app = Self {
            hwnd,
            icon,
            widgets: [null_mut(); WIDGET_COUNT],
            fonts: Vec::new(),
            brushes: [unsafe { CreateSolidBrush(PAPER) }, unsafe {
                CreateSolidBrush(CARD)
            }],
            dpi: unsafe { GetDpiForWindow(hwnd) }.max(96),
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            tray_added: false,
            installed,
            initialized: false,
            expanded: false,
            filter: false,
            provider: Provider::default(),
            draft_changed: false,
            pending: None,
            sequence: 0,
            snapshot: Snapshot::default(),
            notice: None,
            connection_issue: None,
            sender,
            receiver,
        };
        for &(id, kind, title, _) in WIDGETS {
            let (class, ex_style, style) = match kind {
                Label => (
                    w!("STATIC"),
                    0,
                    if id == BADGE { SS_RIGHT } else { SS_LEFT },
                ),
                Edit | Secret => (
                    w!("EDIT"),
                    WS_EX_CLIENTEDGE,
                    WS_TABSTOP
                        | ES_AUTOHSCROLL as u32
                        | if matches!(kind, Secret) {
                            ES_PASSWORD as u32
                        } else {
                            0
                        },
                ),
                Combo => (
                    w!("COMBOBOX"),
                    0,
                    WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32 | CBS_HASSTRINGS as u32,
                ),
                Button | Primary => (w!("BUTTON"), 0, WS_TABSTOP | BS_OWNERDRAW as u32),
                Check => (w!("BUTTON"), 0, WS_TABSTOP | BS_AUTOCHECKBOX as u32),
            };
            let child = unsafe {
                CreateWindowExW(
                    ex_style,
                    class,
                    wide(title).as_ptr(),
                    WS_CHILD | if id < SETTINGS { WS_VISIBLE } else { 0 } | style,
                    0,
                    0,
                    0,
                    0,
                    hwnd,
                    (id + ID) as HMENU,
                    instance,
                    null(),
                )
            };
            if child.is_null() {
                return Err(last_error("创建界面控件失败"));
            }
            if matches!(kind, Edit | Secret)
                && unsafe { SetWindowSubclass(child, Some(centered_edit), 0, 0) } == 0
            {
                return Err("无法设置输入框的垂直对齐。".into());
            }
            app.widgets[id] = child;
        }
        unsafe {
            SendMessageW(app.widgets[HOST], EM_LIMITTEXT, 253, 0);
            SendMessageW(app.widgets[TOKEN], EM_LIMITTEXT, 512, 0);
            SendMessageW(app.widgets[ACCESS_ID], EM_LIMITTEXT, 512, 0);
            SendMessageW(app.widgets[ACCESS_SECRET], EM_LIMITTEXT, 512, 0);
            for provider in [Provider::Cloudflare, Provider::Aliyun] {
                SendMessageW(
                    app.widgets[PROVIDER],
                    CB_ADDSTRING,
                    0,
                    wide(provider.name()).as_ptr() as isize,
                );
            }
            SendMessageW(app.widgets[PROVIDER], CB_SETCURSEL, 0, 0);
            SendMessageW(app.widgets[SUFFIX], EM_LIMITTEXT, 64, 0);
        }
        cue(app.widgets[HOST], "home.example.com");
        cue(app.widgets[SUFFIX], "例如 ::8023");
        app.layout();
        app.render();
        Ok(app)
    }

    fn layout(&mut self) {
        let _stage = diagnostics::Stage::enter("layout");
        let dpi = self.dpi;
        let fonts = vec![
            font("Microsoft YaHei UI", 14, false, dpi),
            font("Microsoft YaHei UI", 20, true, dpi),
            font("Microsoft YaHei UI", 16, true, dpi),
            font("Microsoft YaHei UI", 13, false, dpi),
            font("Segoe UI Variable Text", 13, false, dpi),
            font("Consolas", 15, false, dpi),
        ];
        for &(id, _, _, [x, mut y, width, height]) in WIDGETS {
            if self.provider == Provider::Aliyun && id >= FILTER {
                y += 66;
            }
            let font_id = match id {
                STATE => 1,
                SETTINGS => 2,
                TIMES | HINT => 3,
                BADGE => 4,
                SUFFIX => 5,
                _ => 0,
            };
            unsafe {
                SendMessageW(self.widgets[id], WM_SETFONT, fonts[font_id] as usize, 0);
                MoveWindow(
                    self.widgets[id],
                    scale(x, dpi),
                    scale(y, dpi),
                    scale(width, dpi),
                    scale(height, dpi),
                    1,
                );
            }
        }
        let margin = scale(8, dpi) as isize;
        for id in [HOST, TOKEN, ACCESS_ID, ACCESS_SECRET, SUFFIX] {
            unsafe {
                SendMessageW(
                    self.widgets[id],
                    EM_SETMARGINS,
                    (EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize,
                    margin | (margin << 16),
                );
            }
        }
        for old in std::mem::replace(&mut self.fonts, fonts) {
            unsafe {
                DeleteObject(old);
            }
        }
    }

    fn icon_data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            ..Default::default()
        }
    }

    fn add_tray(&mut self) {
        let _stage = diagnostics::Stage::enter("tray-add");
        diagnostics::record("tray.add.begin");
        let mut data = self.icon_data();
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = TRAY;
        data.hIcon = self.icon;
        let title = wide("IPv6 DDNS");
        data.szTip[..title.len()].copy_from_slice(&title);
        self.tray_added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) } != 0;
        if self.tray_added {
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            unsafe {
                Shell_NotifyIconW(NIM_SETVERSION, &data);
            }
        }
        diagnostics::record(format!("tray.add.end success={}", self.tray_added));
    }

    fn show(&mut self) {
        let _stage = diagnostics::Stage::enter("show");
        diagnostics::record("window.show.begin");
        unsafe {
            ShowWindow(self.hwnd, SW_RESTORE);
            SetForegroundWindow(self.hwnd);
            SetTimer(self.hwnd, 1, 1000, None);
        }
        if self.installed && self.pending.is_none() {
            self.work(Work::Poll, None);
        }
        diagnostics::record("window.show.end");
    }

    fn hide(&mut self) {
        let _stage = diagnostics::Stage::enter("hide");
        diagnostics::record(format!(
            "window.hide.begin tray_available={}",
            self.tray_added
        ));
        unsafe {
            if self.tray_added {
                KillTimer(self.hwnd, 1);
                ShowWindow(self.hwnd, SW_HIDE);
                self.set_expanded(false);
            } else {
                DestroyWindow(self.hwnd);
            }
        }
        diagnostics::record("window.hide.end");
    }

    fn tray_menu(&mut self) {
        let _stage = diagnostics::Stage::enter("tray-menu");
        diagnostics::record("tray.menu.begin");
        let mut point = POINT::default();
        let menu = unsafe { CreatePopupMenu() };
        if menu.is_null() {
            diagnostics::record("tray.menu.create_failed");
            return;
        }
        unsafe {
            GetCursorPos(&mut point);
            AppendMenuW(menu, MF_STRING, 1, w!("打开 IPv6 DDNS"));
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            AppendMenuW(menu, MF_STRING, 2, w!("退出托盘"));
            SetMenuDefaultItem(menu, 1, 0);
            SetForegroundWindow(self.hwnd);
        }
        let clicked = unsafe {
            TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                self.hwnd,
                null(),
            )
        };
        unsafe {
            DestroyMenu(menu);
            PostMessageW(self.hwnd, WM_NULL, 0, 0);
        }
        if clicked == 1 {
            self.show();
        }
        if clicked == 2 {
            diagnostics::record("exit.request reason=tray-menu");
            unsafe {
                PostMessageW(self.hwnd, EXIT, 0, 0);
            }
        }
        diagnostics::record(format!("tray.menu.end selection={clicked}"));
    }

    fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
        diagnostics::record(format!("window.settings expanded={expanded}"));
        let (width, height) = window_size(self.dpi, expanded, self.provider);
        let mut position = RECT::default();
        let mut monitor = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            GetWindowRect(self.hwnd, &mut position);
            if expanded
                && GetMonitorInfoW(
                    MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST),
                    &mut monitor,
                ) != 0
            {
                position.top = position
                    .top
                    .min(monitor.rcWork.bottom - height)
                    .max(monitor.rcWork.top);
            }
            SetWindowPos(
                self.hwnd,
                null_mut(),
                position.left,
                position.top,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.render();
    }

    fn command(&mut self, id: usize) {
        match id {
            TOGGLE => self.set_expanded(!self.expanded),
            PROVIDER => {
                let selected = unsafe { SendMessageW(self.widgets[PROVIDER], CB_GETCURSEL, 0, 0) };
                self.provider = if selected == 1 {
                    Provider::Aliyun
                } else {
                    Provider::Cloudflare
                };
                self.draft_changed = true;
                self.layout();
                self.set_expanded(self.expanded);
            }
            FILTER => {
                self.filter = !self.filter;
                self.draft_changed = true;
                self.render();
                if self.filter {
                    unsafe {
                        SetFocus(self.widgets[SUFFIX]);
                    }
                }
            }
            RUN if !self.installed => self.work(Work::Install, None),
            RUN => {
                if self.snapshot.settings.is_some() {
                    self.work(Work::Run, Some(Request::Run));
                } else {
                    self.set_expanded(true);
                    unsafe {
                        SetFocus(self.widgets[HOST]);
                    }
                }
            }
            SAVE => {
                let domain = match network::domain(&get_text(self.widgets[HOST])) {
                    Ok(value) => value,
                    Err(e) => {
                        self.reject(e, HOST);
                        return;
                    }
                };
                let address = if self.filter {
                    match Ipv6Suffix::parse(&get_text(self.widgets[SUFFIX])) {
                        Ok(suffix) => AddressPolicy::Suffix(suffix),
                        Err(e) => {
                            self.reject(e.into(), SUFFIX);
                            return;
                        }
                    }
                } else {
                    AddressPolicy::Stable
                };
                let credentials = match self.provider {
                    Provider::Cloudflare => Credentials::Cloudflare {
                        token: get_text(self.widgets[TOKEN]),
                    },
                    Provider::Aliyun => Credentials::Aliyun {
                        access_key_id: get_text(self.widgets[ACCESS_ID]),
                        access_key_secret: get_text(self.widgets[ACCESS_SECRET]),
                    },
                };
                let request = Request::Save {
                    settings: Settings {
                        domain,
                        provider: self.provider,
                        address,
                    },
                    credentials,
                    run: false,
                };
                self.work(Work::Save, Some(request));
            }
            AUTO if self.installed => self.work(Work::Automatic(!self.snapshot.automatic), None),
            UNINSTALL if self.installed => unsafe {
                PostMessageW(self.hwnd, CONFIRM_UNINSTALL, 0, 0);
            },
            _ => (),
        }
    }

    fn reject(&mut self, message: String, target: usize) {
        self.notice = Some((true, message));
        unsafe {
            SetFocus(self.widgets[target]);
        }
        self.render();
    }

    fn work(&mut self, work: Work, request: Option<Request>) {
        if self
            .pending
            .is_some_and(|item| !matches!(item, Work::Poll) || matches!(work, Work::Poll))
        {
            return;
        }
        self.pending = Some(work);
        self.sequence += 1;
        let sequence = self.sequence;
        if !matches!(work, Work::Poll) {
            diagnostics::record(format!(
                "work.begin kind={} sequence={sequence}",
                work.name()
            ));
        }
        if !matches!(work, Work::Poll) {
            self.notice = None;
            self.connection_issue = None;
            self.render();
        }
        let sender = self.sender.clone();
        let window = self.hwnd as usize;
        thread::spawn(move || {
            let started = std::time::Instant::now();
            let result = match work {
                Work::Poll | Work::Save | Work::Run => {
                    service::client(request.unwrap_or(Request::Status)).map(Some)
                }
                Work::Install => platform::current_sid()
                    .and_then(|sid| platform::elevate("--install-admin", Some(&sid)))
                    .map(|()| None),
                Work::Uninstall => platform::elevate("--uninstall-admin", None).map(|()| None),
                Work::Automatic(enabled) => platform::elevate(
                    if enabled {
                        "--auto-on-admin"
                    } else {
                        "--auto-off-admin"
                    },
                    None,
                )
                .map(|()| None),
            };
            if result.is_err() || started.elapsed().as_secs() >= 5 || !matches!(work, Work::Poll) {
                diagnostics::record(format!(
                    "work.end kind={} sequence={sequence} success={} elapsed_ms={}",
                    work.name(),
                    result.is_ok(),
                    started.elapsed().as_millis()
                ));
            }
            if sender.send(Event(sequence, work, result)).is_ok() {
                unsafe {
                    PostMessageW(window as HWND, EVENT, 0, 0);
                }
            }
        });
    }

    fn receive(&mut self) {
        let _stage = diagnostics::Stage::enter("receive");
        while let Ok(Event(sequence, work, outcome)) = self.receiver.try_recv() {
            if sequence != self.sequence {
                continue;
            }
            self.pending = None;
            match outcome {
                Ok(Some(reply)) => {
                    self.connection_issue = None;
                    self.receive_reply(work, reply);
                }
                Ok(None) => match work {
                    Work::Install => {
                        self.installed = true;
                        if self.expanded {
                            unsafe {
                                ShowWindow(self.widgets[UNINSTALL], SW_SHOWNA);
                            }
                        }
                        self.work(Work::Poll, None);
                    }
                    Work::Uninstall => {
                        diagnostics::record("exit.request reason=uninstalled");
                        self.installed = false;
                        unsafe {
                            MessageBoxW(
                                self.hwnd,
                                w!("服务已卸载。安装文件将在下次重启时删除。"),
                                w!("IPv6 DDNS"),
                                MB_OK | MB_ICONINFORMATION,
                            );
                            PostMessageW(self.hwnd, EXIT, 0, 0);
                        }
                        return;
                    }
                    Work::Automatic(enabled) => self.snapshot.automatic = enabled,
                    _ => (),
                },
                Err(e) if matches!(work, Work::Poll) => self.connection_issue = Some(e),
                Err(e) => self.notice = Some((true, e)),
            }
            self.render();
        }
    }

    fn receive_reply(&mut self, work: Work, reply: Reply) {
        if !self.initialized {
            self.initialized = true;
            if !self.draft_changed {
                if let Some(settings) = &reply.snapshot.settings {
                    self.provider = settings.provider;
                    unsafe {
                        SendMessageW(
                            self.widgets[PROVIDER],
                            CB_SETCURSEL,
                            usize::from(self.provider == Provider::Aliyun),
                            0,
                        );
                    }
                    text(self.widgets[HOST], &settings.domain);
                    if let AddressPolicy::Suffix(suffix) = &settings.address {
                        self.filter = true;
                        text(self.widgets[SUFFIX], &suffix.to_string());
                    }
                    self.layout();
                    self.set_expanded(self.expanded);
                }
            }
        }
        if let Some(error) = reply.error {
            self.notice = Some((true, error));
        } else if matches!(work, Work::Save) {
            let fields: &[usize] = match self.provider {
                Provider::Cloudflare => &[TOKEN],
                Provider::Aliyun => &[ACCESS_ID, ACCESS_SECRET],
            };
            for &id in fields {
                unsafe {
                    SetWindowTextW(self.widgets[id], w!(""));
                }
            }
            self.draft_changed = false;
            self.notice = Some((false, "已保存配置。点击「立即运行」更新。".into()));
        } else if self.snapshot.phase != reply.snapshot.phase {
            self.notice = None;
        }
        self.snapshot = reply.snapshot;
    }

    fn state(&self) -> (&'static str, &str, u32) {
        if let Some(message) = &self.connection_issue {
            return ("服务连接失败", message, ERROR);
        }
        if let Some((is_error, message)) = &self.notice {
            return if *is_error {
                ("需要处理", message, ERROR)
            } else {
                ("已保存", message, SUCCESS)
            };
        }
        if matches!(
            self.pending,
            Some(Work::Install | Work::Uninstall | Work::Automatic(_))
        ) {
            return (
                "等待管理员授权",
                "请在 Windows 授权窗口中确认操作。",
                ACCENT,
            );
        }
        if !self.installed {
            return ("等待安装", "安装服务后，即可在登录前自动同步 IPv6。", MUTED);
        }
        if !self.initialized {
            return ("正在连接服务", "正在读取本机的 DDNS 状态。", ACCENT);
        }
        match self.snapshot.phase {
            Phase::Idle if self.snapshot.settings.is_none() => {
                ("等待配置", "展开设置，选择服务商并填写域名和凭据。", MUTED)
            }
            Phase::Idle => (
                "等待运行",
                "配置已保存。点击「立即运行」可手动更新。",
                MUTED,
            ),
            Phase::Running => ("正在同步", "正在检查 DNS 服务商中的 AAAA 记录。", ACCENT),
            Phase::Success => ("同步完成", &self.snapshot.message, SUCCESS),
            Phase::Failed => ("同步失败", &self.snapshot.message, ERROR),
        }
    }

    fn render(&self) {
        let _stage = diagnostics::Stage::enter("render");
        let (heading, detail, _) = self.state();
        text(self.widgets[STATE], heading);
        let detail = match self.snapshot.address {
            Some(ip) if heading == "同步完成" => format!("{detail} · {ip}"),
            _ => detail.to_owned(),
        };
        text(self.widgets[DETAIL], &detail);
        let times = match (self.snapshot.last_run, self.snapshot.next_run) {
            (Some(last), Some(next)) => format!(
                "最近运行  {}     下次检查  {}",
                local_time(last),
                local_time(next)
            ),
            (Some(last), None) => format!("最近运行  {}", local_time(last)),
            (None, Some(next)) => format!("下次检查  {}", local_time(next)),
            (None, None) => String::new(),
        };
        text(self.widgets[TIMES], &times);
        let active = !matches!(
            self.pending,
            Some(Work::Install | Work::Uninstall | Work::Automatic(_) | Work::Save | Work::Run)
        );
        let ready = active && self.snapshot.phase != Phase::Running;
        let saved_provider = self
            .snapshot
            .settings
            .as_ref()
            .map(|settings| settings.provider);
        let configured = saved_provider == Some(self.provider);
        text(
            self.widgets[BADGE],
            &format!(
                "{}  ·  AAAA",
                saved_provider.unwrap_or(self.provider).name()
            ),
        );
        for (id, prompt) in [
            (TOKEN, "输入 Cloudflare API Token"),
            (ACCESS_ID, "输入阿里云 AccessKey ID"),
            (ACCESS_SECRET, "输入阿里云 AccessKey Secret"),
        ] {
            cue(
                self.widgets[id],
                if configured {
                    "已保存，留空继续使用"
                } else {
                    prompt
                },
            );
        }
        text(
            self.widgets[RUN],
            if !self.installed {
                "安装服务"
            } else if self.snapshot.phase == Phase::Running {
                "运行中…"
            } else {
                "立即运行"
            },
        );
        text(
            self.widgets[TOGGLE],
            if self.expanded {
                "收起设置"
            } else {
                "展开设置"
            },
        );
        text(
            self.widgets[HINT],
            if self.filter {
                "匹配末尾的十六进制段，例如 ::abcd:1234。"
            } else {
                "默认使用稳定的公网 IPv6，跳过临时地址。"
            },
        );
        unsafe {
            for (id, child) in self.widgets.iter().enumerate().skip(SETTINGS) {
                let provider_visible = match id {
                    TOKEN_LABEL | TOKEN => self.provider == Provider::Cloudflare,
                    ACCESS_ID_LABEL | ACCESS_ID | ACCESS_SECRET_LABEL | ACCESS_SECRET => {
                        self.provider == Provider::Aliyun
                    }
                    UNINSTALL => self.installed,
                    _ => true,
                };
                ShowWindow(
                    *child,
                    if self.expanded && provider_visible {
                        SW_SHOWNA
                    } else {
                        SW_HIDE
                    },
                );
            }
            for id in [PROVIDER, HOST, TOKEN, ACCESS_ID, ACCESS_SECRET, FILTER] {
                EnableWindow(self.widgets[id], i32::from(active));
            }
            EnableWindow(self.widgets[SUFFIX], i32::from(active && self.filter));
            EnableWindow(
                self.widgets[RUN],
                i32::from(ready && (!self.installed || self.initialized)),
            );
            EnableWindow(
                self.widgets[SAVE],
                i32::from(self.installed && self.initialized && ready),
            );
            EnableWindow(self.widgets[UNINSTALL], i32::from(self.installed && active));
            EnableWindow(
                self.widgets[AUTO],
                i32::from(self.installed && self.initialized && active),
            );
            SendMessageW(
                self.widgets[AUTO],
                BM_SETCHECK,
                if self.snapshot.automatic {
                    BST_CHECKED
                } else {
                    BST_UNCHECKED
                } as usize,
                0,
            );
            SendMessageW(
                self.widgets[FILTER],
                BM_SETCHECK,
                if self.filter {
                    BST_CHECKED
                } else {
                    BST_UNCHECKED
                } as usize,
                0,
            );
            for id in [TOGGLE, RUN, SAVE, UNINSTALL, SUFFIX_LABEL] {
                InvalidateRect(self.widgets[id], null(), 0);
            }
            InvalidateRect(self.hwnd, null(), 0);
        }
    }

    fn paint(&self) {
        let mut paint = PAINTSTRUCT::default();
        let dc = unsafe { BeginPaint(self.hwnd, &mut paint) };
        let bounds = |left, top, right, bottom| RECT {
            left: scale(left, self.dpi),
            top: scale(top, self.dpi),
            right: scale(right, self.dpi),
            bottom: scale(bottom, self.dpi),
        };
        unsafe {
            FillRect(dc, &paint.rcPaint, self.brushes[0]);
        }
        rounded(dc, bounds(16, 16, 470, 114), scale(9, self.dpi), CARD, LINE);
        let color = self.state().2;
        let mark = bounds(40, 41, 56, 57);
        rounded(
            dc,
            bounds(31, 32, 65, 66),
            scale(10, self.dpi),
            rgb(238, 245, 251),
            rgb(238, 245, 251),
        );
        rounded(dc, mark, scale(16, self.dpi), CARD, color);
        rounded(dc, bounds(44, 45, 52, 53), scale(8, self.dpi), color, color);
        if self.expanded {
            rounded(
                dc,
                bounds(16, 170, 470, settings_height(self.provider) - 9),
                scale(9, self.dpi),
                CARD,
                LINE,
            );
        }
        unsafe {
            EndPaint(self.hwnd, &paint);
        }
    }

    fn draw_button(&self, item: &DRAWITEMSTRUCT) {
        let id = item.CtlID as usize - ID;
        let disabled = item.itemState & ODS_DISABLED != 0;
        let pressed = item.itemState & ODS_SELECTED != 0;
        let primary = id == RUN && !disabled;
        let background = if matches!(id, SAVE | UNINSTALL) {
            CARD
        } else {
            PAPER
        };
        let (fill, outline, foreground) = if disabled {
            (rgb(246, 246, 246), LINE, rgb(145, 145, 145))
        } else if primary {
            let blue = if pressed { rgb(0, 82, 154) } else { ACCENT };
            (blue, blue, CARD)
        } else if id == UNINSTALL {
            (if pressed { PAPER } else { CARD }, CARD, ERROR)
        } else {
            (if pressed { PAPER } else { CARD }, LINE, INK)
        };
        unsafe {
            FillRect(
                item.hDC,
                &item.rcItem,
                self.brushes[usize::from(background == CARD)],
            );
        }
        rounded(item.hDC, item.rcItem, scale(7, self.dpi), fill, outline);
        let mut rect = item.rcItem;
        let mut title = wide(get_text(item.hwndItem));
        unsafe {
            let previous = SelectObject(item.hDC, self.fonts[0]);
            SetBkMode(item.hDC, TRANSPARENT as i32);
            SetTextColor(item.hDC, foreground);
            DrawTextW(
                item.hDC,
                title.as_mut_ptr(),
                -1,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            SelectObject(item.hDC, previous);
            if item.itemState & ODS_FOCUS != 0 {
                InflateRect(&mut rect, -scale(4, self.dpi), -scale(4, self.dpi));
                DrawFocusRect(item.hDC, &rect);
            }
        }
    }
}

fn rounded(dc: HDC, rect: RECT, diameter: i32, fill: u32, outline: u32) {
    unsafe {
        let brush = CreateSolidBrush(fill);
        let pen = CreatePen(PS_SOLID, 1, outline);
        let old_brush = SelectObject(dc, brush);
        let old_pen = SelectObject(dc, pen);
        RoundRect(
            dc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            diameter,
            diameter,
        );
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        DeleteObject(pen);
        DeleteObject(brush);
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        let _stage = diagnostics::Stage::enter("destroy");
        diagnostics::record("tray.delete.begin");
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &self.icon_data());
        }
        diagnostics::record("tray.delete.end");
        for font in &self.fonts {
            unsafe {
                DeleteObject(*font);
            }
        }
        for brush in self.brushes {
            unsafe {
                DeleteObject(brush);
            }
        }
    }
}

fn local_time(unix: u64) -> String {
    let ticks = (unix + 11_644_473_600) * 10_000_000;
    let time = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    if unsafe { FileTimeToSystemTime(&time, &mut utc) } == 0
        || unsafe { SystemTimeToTzSpecificLocalTimeEx(null(), &utc, &mut local) } == 0
    {
        return "—".into();
    }
    format!("{:02}:{:02}", local.wHour, local.wMinute)
}

unsafe extern "system" fn window(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let _message = diagnostics::Message::enter(msg);
    // These notifications can arrive inside a nested menu/message loop, while Ui is borrowed.
    match msg {
        WM_POWERBROADCAST => {
            let event = match wp as u32 {
                PBT_APMSUSPEND => "suspend",
                PBT_APMRESUMEAUTOMATIC => "resume-automatic",
                PBT_APMRESUMESUSPEND => "resume-user",
                PBT_APMRESUMECRITICAL => "resume-critical",
                _ => "other",
            };
            diagnostics::record(format!("power.broadcast event={event} code={wp}"));
            return 1;
        }
        WM_QUERYENDSESSION => diagnostics::record("session.query_end"),
        WM_ENDSESSION => diagnostics::record(format!("session.end ending={}", wp != 0)),
        WM_DESTROY => diagnostics::record("window.destroy"),
        _ => (),
    }
    if msg == WM_DESTROY {
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }
    if msg == CONFIRM_UNINSTALL {
        let answer = unsafe {
            MessageBoxW(
                hwnd,
                w!(
                    "卸载会停止后台同步，删除本机保存的设置和登录启动项。\n安装文件将在下次重启时删除。\n\n要卸载 IPv6 DDNS 服务吗？"
                ),
                w!("卸载服务"),
                MB_YESNO | MB_DEFBUTTON2 | MB_ICONWARNING,
            )
        };
        if answer == IDYES {
            UI.with(|state| {
                if let Some(ui) = state.borrow_mut().as_mut() {
                    ui.work(Work::Uninstall, None);
                }
            });
        }
        return 0;
    }
    // Setting a label's text can ask for its colors before the state update returns.
    if matches!(msg, WM_CTLCOLORSTATIC | WM_CTLCOLORBTN) {
        let id = (unsafe { GetDlgCtrlID(lp as HWND) } as usize).saturating_sub(ID);
        let color = match id {
            BADGE | DETAIL | TIMES | HINT => MUTED,
            SUFFIX_LABEL
                if unsafe { IsWindowEnabled(GetDlgItem(hwnd, (SUFFIX + ID) as i32)) } == 0 =>
            {
                MUTED
            }
            _ => INK,
        };
        return unsafe {
            SetTextColor(wp as HDC, color);
            SetBkColor(wp as HDC, CARD);
            GetStockObject(WHITE_BRUSH) as LRESULT
        };
    }
    let result = UI.with(|state| {
        let mut state = state.try_borrow_mut().ok()?;
        let ui = state.as_mut()?;
        match msg {
            WM_COMMAND if wp as u16 as usize >= ID => {
                let id = wp as u16 as usize - ID;
                let notification = (wp >> 16) as u32;
                if notification == 0 || (id == PROVIDER && notification == CBN_SELCHANGE) {
                    ui.command(id);
                } else if notification == EN_CHANGE
                    && matches!(id, HOST | TOKEN | ACCESS_ID | ACCESS_SECRET | SUFFIX)
                {
                    ui.draft_changed = true;
                }
            }
            WM_PAINT => ui.paint(),
            WM_ERASEBKGND => return Some(1),
            WM_DRAWITEM if wp >= ID && matches!(wp - ID, TOGGLE | RUN | SAVE | UNINSTALL) => {
                ui.draw_button(unsafe { &*(lp as *const DRAWITEMSTRUCT) });
                return Some(1);
            }
            WM_CLOSE => ui.hide(),
            WM_TIMER if wp == 1 && ui.installed => ui.work(Work::Poll, None),
            WM_DPICHANGED => {
                ui.dpi = wp as u16 as u32;
                let position = unsafe { &*(lp as *const RECT) };
                let (width, height) = window_size(ui.dpi, ui.expanded, ui.provider);
                unsafe {
                    SetWindowPos(
                        hwnd,
                        null_mut(),
                        position.left,
                        position.top,
                        width,
                        height,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                ui.layout();
            }
            SHOW => ui.show(),
            EVENT => ui.receive(),
            EXIT => unsafe {
                DestroyWindow(hwnd);
            },
            TRAY => match lp as u16 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK | 0x400 | 0x401 => ui.show(),
                WM_CONTEXTMENU | WM_RBUTTONUP => ui.tray_menu(),
                _ => (),
            },
            value if value == ui.taskbar_created => {
                diagnostics::record("shell.taskbar_created");
                ui.add_tray();
            }
            _ => return None,
        }
        Some(0)
    });
    result.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
}

pub fn launch(hidden: bool) -> Result<()> {
    let result = launch_inner(hidden);
    // Error text can include user input, so record only the outcome.
    diagnostics::record(format!("process.exit success={}", result.is_ok()));
    diagnostics::flush();
    result
}

fn launch_inner(hidden: bool) -> Result<()> {
    let mutex = platform::Handle(unsafe { CreateMutexW(null(), 0, w!(r"Local\IPv6DDNS.Tray.1")) });
    let existing = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if mutex.0.is_null() {
        return Err(last_error("创建托盘实例失败"));
    }
    if existing {
        if !hidden {
            for _ in 0..20 {
                let hwnd = unsafe { FindWindowW(CLASS, null()) };
                if !hwnd.is_null() {
                    let mut pid = 0;
                    unsafe {
                        GetWindowThreadProcessId(hwnd, &mut pid);
                        AllowSetForegroundWindow(pid);
                        PostMessageW(hwnd, SHOW, 0, 0);
                    }
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        return Ok(());
    }
    // Only the owning tray instance writes/rotates the log, including during repeated launches.
    diagnostics::init();
    diagnostics::record(format!(
        "process.start version={} hidden={hidden}",
        env!("CARGO_PKG_VERSION")
    ));
    let instance = unsafe { GetModuleHandleW(null()) };
    let icon = unsafe { LoadIconW(instance, 101usize as *const u16) };
    let icon = if icon.is_null() {
        unsafe { LoadIconW(null_mut(), IDI_APPLICATION) }
    } else {
        icon
    };
    let wc = WNDCLASSW {
        lpfnWndProc: Some(window),
        hInstance: instance,
        lpszClassName: CLASS,
        hIcon: icon,
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        ..Default::default()
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        return Err(last_error("注册窗口失败"));
    }
    let (width, height) = window_size(
        unsafe { GetDpiForSystem() }.max(96),
        false,
        Provider::default(),
    );
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            CLASS,
            w!("IPv6 DDNS"),
            STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width,
            height,
            null_mut(),
            null_mut(),
            instance,
            null(),
        )
    };
    if hwnd.is_null() {
        return Err(last_error("创建主窗口失败"));
    }
    let _watchdog = diagnostics::Watchdog::start(hwnd);
    diagnostics::record("window.created watchdog_started=true");
    for (attribute, color) in [(DWMWA_CAPTION_COLOR, PAPER), (DWMWA_TEXT_COLOR, INK)] {
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                attribute as u32,
                (&raw const color).cast(),
                size_of::<u32>() as u32,
            );
        }
    }
    let installed = match platform::open_service(ServiceAccess::QUERY_STATUS) {
        Ok(_) => true,
        Err(windows_service::Error::Winapi(e))
            if e.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) =>
        {
            false
        }
        Err(e) => return Err(error("检查 DDNS 服务失败", e)),
    };
    let mut ui = Ui::new(hwnd, instance, icon, installed)?;
    ui.add_tray();
    let show = !hidden || !ui.tray_added;
    UI.with(|state| *state.borrow_mut() = Some(ui));
    if show {
        unsafe {
            PostMessageW(hwnd, SHOW, 0, 0);
        }
    }

    let mut msg = MSG::default();
    loop {
        let count = unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) };
        if count < 0 {
            return Err(last_error("读取窗口消息失败"));
        }
        if count == 0 {
            break;
        }
        if msg.message == WM_KEYDOWN && msg.wParam == VK_ESCAPE as usize {
            let expanded = UI.with(|state| state.borrow().as_ref().is_some_and(|ui| ui.expanded));
            unsafe {
                if expanded {
                    PostMessageW(hwnd, WM_COMMAND, TOGGLE + ID, 0);
                } else {
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                }
            }
            continue;
        }
        let handled = {
            let _stage = diagnostics::Stage::enter("keyboard");
            unsafe { IsDialogMessageW(hwnd, &msg) }
        };
        if handled == 0 {
            let _stage = diagnostics::Stage::enter("dispatch");
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    UI.with(|state| {
        state.borrow_mut().take();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_edit_lines_are_vertically_centered() {
        let instance = unsafe { GetModuleHandleW(null()) };
        let parent = unsafe {
            CreateWindowExW(
                0,
                w!("STATIC"),
                null(),
                WS_POPUP,
                0,
                0,
                1024,
                1024,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        assert!(!parent.is_null());
        let mut ui = Ui::new(parent, instance, null_mut(), false).unwrap();
        let mut offsets = Vec::new();
        for dpi in [96, 120, 144, 192] {
            ui.dpi = dpi;
            ui.layout();
            for id in [HOST, TOKEN, ACCESS_ID, ACCESS_SECRET, SUFFIX] {
                let edit = ui.widgets[id];
                text(edit, "Ag");
                let mut info = WINDOWINFO {
                    cbSize: size_of::<WINDOWINFO>() as u32,
                    ..Default::default()
                };
                assert_ne!(unsafe { GetWindowInfo(edit, &mut info) }, 0);
                let position = unsafe { SendMessageW(edit, EM_POSFROMCHAR, 0, 0) };
                assert_ne!(position, -1);
                let text_top = info.rcClient.top + (position >> 16) as i16 as i32;
                let top = text_top - info.rcWindow.top;
                let bottom = info.rcWindow.bottom - text_top - edit_line_height(edit).unwrap();
                if top < 0 || bottom < 0 || (top - bottom).abs() > 1 {
                    offsets.push((dpi, id, top, bottom));
                }
            }
        }
        unsafe {
            DestroyWindow(parent);
        }
        assert!(
            offsets.is_empty(),
            "(DPI, control, top, bottom) = {offsets:?}"
        );
    }
}

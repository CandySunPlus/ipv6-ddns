use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use ipv6_ddns::{
    AddressPolicy, Credentials, Ipv6Suffix, Phase, Provider, Reply, Request, Settings, Snapshot,
};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertSecondButtonReturn, NSAlertStyle, NSApplication,
    NSApplicationActivationOptions, NSApplicationActivationPolicy, NSBackingStoreType, NSButton,
    NSButtonType, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSFont, NSImage,
    NSLineBreakMode, NSMenu, NSMenuItem, NSPopUpButton, NSRunningApplication, NSSecureTextField,
    NSStatusBar, NSStatusItem, NSTextField, NSVariableStatusItemLength, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer};

use super::{Result, error, network, platform, service};

const WIDTH: f64 = 486.0;
const COMPACT_HEIGHT: f64 = 171.0;
const SETTINGS_HEIGHT: f64 = 578.0;

static SHOW: AtomicBool = AtomicBool::new(false);

extern "C" fn show(_signal: libc::c_int) {
    SHOW.store(true, Ordering::SeqCst);
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    struct FlippedView;

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

struct Widgets {
    state: Retained<NSTextField>,
    badge: Retained<NSTextField>,
    detail: Retained<NSTextField>,
    times: Retained<NSTextField>,
    toggle: Retained<NSButton>,
    run: Retained<NSButton>,
    settings_title: Retained<NSTextField>,
    provider_label: Retained<NSTextField>,
    provider: Retained<NSPopUpButton>,
    access_key_label: Retained<NSTextField>,
    access_key: Retained<NSTextField>,
    secret_label: Retained<NSTextField>,
    secret: Retained<NSSecureTextField>,
    domain_label: Retained<NSTextField>,
    host: Retained<NSTextField>,
    token_label: Retained<NSTextField>,
    token: Retained<NSSecureTextField>,
    filter: Retained<NSButton>,
    suffix_label: Retained<NSTextField>,
    suffix: Retained<NSTextField>,
    hint: Retained<NSTextField>,
    prefer_source: Retained<NSButton>,
    source_hint: Retained<NSTextField>,
    automatic: Retained<NSButton>,
    uninstall: Retained<NSButton>,
    save: Retained<NSButton>,
}

#[derive(Clone, Copy)]
enum Work {
    Poll,
    Save,
    Run,
    Install,
    Uninstall,
    Automatic(bool),
}

struct Event(u64, Work, Result<Option<Reply>>);

#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Muted,
    Accent,
    Success,
    Error,
}

fn tone_color(tone: Tone) -> Retained<NSColor> {
    match tone {
        Tone::Muted => NSColor::secondaryLabelColor(),
        Tone::Accent => NSColor::controlAccentColor(),
        Tone::Success => NSColor::systemGreenColor(),
        Tone::Error => NSColor::systemRedColor(),
    }
}

struct Ui {
    snapshot: Snapshot,
    installed: bool,
    initialized: bool,
    expanded: bool,
    filter: bool,
    pending: Option<Work>,
    sequence: u64,
    notice: Option<(bool, String)>,
    connection_issue: Option<String>,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
    ticks: u64,
}

struct State {
    window: Retained<NSWindow>,
    widgets: Widgets,
    _status: Retained<NSStatusItem>,
    _lock: File,
    ui: RefCell<Ui>,
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

fn label(
    mtm: MainThreadMarker,
    value: &str,
    frame: NSRect,
    size: f64,
    bold: bool,
    color: &NSColor,
) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(value), mtm);
    label.setFrame(frame);
    let font = if bold {
        NSFont::boldSystemFontOfSize(size)
    } else {
        NSFont::systemFontOfSize(size)
    };
    label.setFont(Some(&font));
    label.setTextColor(Some(color));
    label
}

fn field(mtm: MainThreadMarker, frame: NSRect, placeholder: &str) -> Retained<NSTextField> {
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), frame);
    field.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
    field
}

fn button(mtm: MainThreadMarker, title: &str, frame: NSRect) -> Retained<NSButton> {
    let button = NSButton::initWithFrame(NSButton::alloc(mtm), frame);
    button.setTitle(&NSString::from_str(title));
    button.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    button
}

fn checkbox(mtm: MainThreadMarker, title: &str, frame: NSRect) -> Retained<NSButton> {
    let button = button(mtm, title, frame);
    button.setButtonType(NSButtonType::Switch);
    button
}

fn hide_settings(widgets: &Widgets, expanded: bool, installed: bool) {
    let views: [&NSView; 19] = [
        &widgets.settings_title,
        &widgets.provider_label,
        &widgets.provider,
        &widgets.access_key_label,
        &widgets.access_key,
        &widgets.secret_label,
        &widgets.secret,
        &widgets.domain_label,
        &widgets.host,
        &widgets.token_label,
        &widgets.token,
        &widgets.filter,
        &widgets.suffix_label,
        &widgets.suffix,
        &widgets.hint,
        &widgets.prefer_source,
        &widgets.source_hint,
        &widgets.automatic,
        &widgets.save,
    ];
    for view in views {
        view.setHidden(!expanded);
    }
    widgets.uninstall.setHidden(!expanded || !installed);
    let aliyun = selected_provider(widgets) == Provider::Aliyun;
    widgets.token_label.setHidden(!expanded || aliyun);
    widgets.token.setHidden(!expanded || aliyun);
    widgets.access_key_label.setHidden(!expanded || !aliyun);
    widgets.access_key.setHidden(!expanded || !aliyun);
    widgets.secret_label.setHidden(!expanded || !aliyun);
    widgets.secret.setHidden(!expanded || !aliyun);
}

fn selected_provider(widgets: &Widgets) -> Provider {
    if widgets.provider.indexOfSelectedItem() == 1 {
        Provider::Aliyun
    } else {
        Provider::Cloudflare
    }
}

fn secure_field(
    mtm: MainThreadMarker,
    frame: NSRect,
    placeholder: &str,
) -> Retained<NSSecureTextField> {
    let field = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), frame);
    field.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
    field
}
fn build_widgets(mtm: MainThreadMarker, view: &NSView) -> Widgets {
    let muted = NSColor::secondaryLabelColor();
    let ink = NSColor::labelColor();
    let widgets = Widgets {
        state: label(mtm, "", rect(75.0, 27.0, 250.0, 26.0), 16.0, true, &ink),
        badge: label(
            mtm,
            "Cloudflare  ·  AAAA",
            rect(325.0, 31.0, 127.0, 19.0),
            11.0,
            false,
            &muted,
        ),
        detail: label(mtm, "", rect(75.0, 56.0, 377.0, 35.0), 13.0, false, &ink),
        times: label(mtm, "", rect(75.0, 92.0, 377.0, 17.0), 11.0, false, &muted),
        toggle: button(mtm, "展开设置", rect(16.0, 125.0, 108.0, 32.0)),
        run: button(mtm, "立即运行", rect(358.0, 125.0, 112.0, 32.0)),
        settings_title: label(
            mtm,
            "同步设置",
            rect(32.0, 184.0, 422.0, 24.0),
            13.0,
            true,
            &ink,
        ),
        provider_label: label(
            mtm,
            "DNS 服务商",
            rect(32.0, 216.0, 422.0, 18.0),
            11.0,
            false,
            &ink,
        ),
        provider: {
            let provider = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(mtm),
                rect(29.0, 237.0, 428.0, 28.0),
                false,
            );
            provider.addItemWithTitle(&NSString::from_str("Cloudflare"));
            provider.addItemWithTitle(&NSString::from_str("阿里云 DNS"));
            provider.selectItemAtIndex(0);
            provider
        },
        access_key_label: label(
            mtm,
            "AccessKey ID",
            rect(32.0, 330.0, 422.0, 18.0),
            11.0,
            false,
            &ink,
        ),
        access_key: field(
            mtm,
            rect(32.0, 351.0, 422.0, 24.0),
            "输入阿里云 AccessKey ID",
        ),
        secret_label: label(
            mtm,
            "AccessKey Secret",
            rect(32.0, 387.0, 422.0, 18.0),
            11.0,
            false,
            &ink,
        ),
        secret: secure_field(
            mtm,
            rect(32.0, 408.0, 422.0, 24.0),
            "输入阿里云 AccessKey Secret",
        ),
        domain_label: label(
            mtm,
            "域名",
            rect(32.0, 273.0, 422.0, 18.0),
            11.0,
            false,
            &ink,
        ),
        host: field(mtm, rect(32.0, 294.0, 422.0, 24.0), "home.example.com"),
        token_label: label(
            mtm,
            "Cloudflare API Token",
            rect(32.0, 330.0, 422.0, 18.0),
            11.0,
            false,
            &ink,
        ),
        token: secure_field(
            mtm,
            rect(32.0, 351.0, 422.0, 24.0),
            "输入 Cloudflare API Token",
        ),
        filter: checkbox(mtm, "按 IPv6 后缀筛选", rect(32.0, 392.0, 203.0, 25.0)),
        suffix_label: label(
            mtm,
            "IPv6 后缀",
            rect(32.0, 421.0, 422.0, 19.0),
            11.0,
            false,
            &ink,
        ),
        suffix: field(mtm, rect(32.0, 446.0, 422.0, 24.0), "例如 ::8023"),
        hint: label(
            mtm,
            "默认使用稳定的公网 IPv6，跳过临时地址。",
            rect(32.0, 481.0, 422.0, 21.0),
            11.0,
            false,
            &muted,
        ),
        prefer_source: checkbox(
            mtm,
            "优先使用 DDNS IPv6 作为源地址",
            rect(32.0, 451.0, 422.0, 25.0),
        ),
        source_hint: label(mtm, "", rect(32.0, 480.0, 422.0, 21.0), 11.0, false, &muted),
        automatic: checkbox(mtm, "开机自动更新", rect(32.0, 527.0, 174.0, 32.0)),
        uninstall: button(mtm, "卸载服务…", rect(231.0, 527.0, 102.0, 32.0)),
        save: button(mtm, "保存设置", rect(355.0, 527.0, 99.0, 32.0)),
    };
    if let Some(cell) = widgets.detail.cell() {
        cell.setWraps(true);
        cell.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    }
    widgets.detail.setMaximumNumberOfLines(2);
    for widget in [
        &widgets.state as &NSView,
        &widgets.badge,
        &widgets.detail,
        &widgets.times,
        &widgets.toggle,
        &widgets.run,
        &widgets.settings_title,
        &widgets.provider_label,
        &widgets.provider,
        &widgets.access_key_label,
        &widgets.access_key,
        &widgets.secret_label,
        &widgets.secret,
        &widgets.domain_label,
        &widgets.host,
        &widgets.token_label,
        &widgets.token,
        &widgets.filter,
        &widgets.suffix_label,
        &widgets.suffix,
        &widgets.hint,
        &widgets.prefer_source,
        &widgets.source_hint,
        &widgets.automatic,
        &widgets.uninstall,
        &widgets.save,
    ] {
        view.addSubview(widget);
    }
    hide_settings(&widgets, false, false);
    widgets.toggle.setKeyEquivalent(&NSString::from_str(""));
    widgets.run.setKeyEquivalent(&NSString::from_str("\r"));
    widgets
}

fn build_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, WIDTH, COMPACT_HEIGHT),
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str("IPv6 DDNS"));
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    let view: Retained<FlippedView> = unsafe {
        msg_send![
            FlippedView::alloc(mtm),
            initWithFrame: rect(0.0, 0.0, WIDTH, COMPACT_HEIGHT)
        ]
    };
    window.setContentView(Some(&view));
    window.center();
    window
}

fn build_status(mtm: MainThreadMarker) -> (Retained<NSStatusItem>, Retained<NSMenuItem>) {
    let status = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = status.button(mtm) {
        match NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("network"),
            Some(&NSString::from_str("IPv6 DDNS")),
        ) {
            Some(image) => {
                image.setTemplate(true);
                button.setImage(Some(&image));
            }
            None => button.setTitle(&NSString::from_str("DDNS")),
        }
        button.setToolTip(Some(&NSString::from_str("IPv6 DDNS")));
    }
    let menu = NSMenu::new(mtm);
    let open = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("打开 IPv6 DDNS"),
            Some(sel!(showWindow:)),
            &NSString::from_str(""),
        )
    };
    menu.addItem(&open);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let quit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("退出托盘"),
            Some(sel!(terminate:)),
            &NSString::from_str("q"),
        )
    };
    menu.addItem(&quit);
    status.setMenu(Some(&menu));
    (status, open)
}

fn build_main_menu(mtm: MainThreadMarker) {
    let main = NSMenu::new(mtm);
    let app_item = NSMenuItem::new(mtm);
    app_item.setTitle(&NSString::from_str("IPv6 DDNS"));
    let app_menu = NSMenu::new(mtm);
    let quit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("退出 IPv6 DDNS"),
            Some(sel!(terminate:)),
            &NSString::from_str("q"),
        )
    };
    app_menu.addItem(&quit);
    app_item.setSubmenu(Some(&app_menu));
    main.addItem(&app_item);

    let edit_item = NSMenuItem::new(mtm);
    edit_item.setTitle(&NSString::from_str("编辑"));
    let edit_menu = NSMenu::new(mtm);
    for (title, action, key) in [
        ("撤销", sel!(undo:), "z"),
        ("剪切", sel!(cut:), "x"),
        ("拷贝", sel!(copy:), "c"),
        ("粘贴", sel!(paste:), "v"),
        ("全选", sel!(selectAll:), "a"),
    ] {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        };
        edit_menu.addItem(&item);
    }
    edit_item.setSubmenu(Some(&edit_menu));
    main.addItem(&edit_item);
    NSApplication::sharedApplication(mtm).setMainMenu(Some(&main));
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = State]
    struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSWindowDelegate for Controller {}

    impl Controller {
        #[unsafe(method(showWindow:))]
        fn show_window(&self, _sender: &AnyObject) {
            self.show();
        }

        #[unsafe(method(tick:))]
        fn tick_timer(&self, _timer: &NSTimer) {
            self.tick();
        }

        #[unsafe(method(toggleSettings:))]
        fn toggle_settings(&self, _sender: &AnyObject) {
            let expanded = self.ui().expanded;
            self.set_expanded(!expanded);
        }

        #[unsafe(method(runNow:))]
        fn run_now(&self, _sender: &AnyObject) {
            let (installed, initialized, phase, configured) = {
                let ui = self.ui();
                (
                    ui.installed,
                    ui.initialized,
                    ui.snapshot.phase,
                    ui.snapshot.settings.is_some(),
                )
            };
            if !installed {
                self.work(Work::Install, None);
            } else if !initialized || phase == Phase::Running {
                return;
            } else if !configured {
                self.set_expanded(true);
            } else {
                self.work(Work::Run, Some(Request::Run));
            }
        }

        #[unsafe(method(saveSettings:))]
        fn save_settings(&self, _sender: &AnyObject) {
            self.save();
        }

        #[unsafe(method(providerChanged:))]
        fn provider_changed(&self, _sender: &AnyObject) {
            self.ui().notice = None;
            if self.ui().expanded {
                self.resize(true);
            }
            self.render();
        }

        #[unsafe(method(filterChanged:))]
        fn filter_changed(&self, _sender: &AnyObject) {
            let filter = self.ivars().widgets.filter.state() == NSControlStateValueOn;
            self.ui().filter = filter;
            self.render();
        }

        #[unsafe(method(autoChanged:))]
        fn auto_changed(&self, _sender: &AnyObject) {
            let enabled = self.ivars().widgets.automatic.state() == NSControlStateValueOn;
            let (installed, initialized, current, active) = {
                let ui = self.ui();
                (
                    ui.installed,
                    ui.initialized,
                    ui.snapshot.automatic,
                    !matches!(
                        ui.pending,
                        Some(
                            Work::Install
                                | Work::Uninstall
                                | Work::Automatic(_)
                                | Work::Save
                                | Work::Run
                        )
                    ),
                )
            };
            if !installed || !initialized || !active {
                self.render();
                return;
            }
            if enabled != current {
                self.work(Work::Automatic(enabled), None);
            }
        }

        #[unsafe(method(uninstallService:))]
        fn uninstall_service(&self, _sender: &AnyObject) {
            let alert = NSAlert::new(MainThreadMarker::new().expect("main thread"));
            alert.setMessageText(&NSString::from_str("卸载服务"));
            alert.setInformativeText(&NSString::from_str(
                "卸载会停止后台同步，删除本机保存的设置和登录启动项。\n\n要卸载 IPv6 DDNS 服务吗？",
            ));
            alert.setAlertStyle(NSAlertStyle::Warning);
            alert.addButtonWithTitle(&NSString::from_str("取消"));
            alert.addButtonWithTitle(&NSString::from_str("卸载"));
            if alert.runModal() == NSAlertSecondButtonReturn {
                self.work(Work::Uninstall, None);
            }
        }

        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            self.hide();
            false
        }
    }
);

impl Controller {
    fn ui(&self) -> std::cell::RefMut<'_, Ui> {
        self.ivars().ui.borrow_mut()
    }

    fn show(&self) {
        self.ivars().window.deminiaturize(None);
        self.ivars().window.makeKeyAndOrderFront(None);
        activate(MainThreadMarker::new().expect("main thread"));
        let poll = {
            let ui = self.ui();
            ui.installed && ui.pending.is_none()
        };
        if poll {
            self.work(Work::Poll, None);
        }
    }

    fn hide(&self) {
        self.set_expanded(false);
        self.ivars().window.orderOut(None);
    }

    fn set_expanded(&self, expanded: bool) {
        let installed = self.ui().installed;
        {
            let mut ui = self.ui();
            if ui.expanded == expanded {
                return;
            }
            ui.expanded = expanded;
        }
        hide_settings(&self.ivars().widgets, expanded, installed);
        self.resize(expanded);
        self.render();
    }

    fn resize(&self, expanded: bool) {
        let window = &self.ivars().window;
        let frame = window.frame();
        let top = frame.origin.y + frame.size.height;
        window.setContentSize(NSSize::new(
            WIDTH,
            if expanded {
                SETTINGS_HEIGHT
                    + if selected_provider(&self.ivars().widgets) == Provider::Aliyun {
                        46.0
                    } else {
                        0.0
                    }
            } else {
                COMPACT_HEIGHT
            },
        ));
        let mut frame = window.frame();
        frame.origin.y = top - frame.size.height;
        if let Some(screen) = window.screen() {
            let visible = screen.visibleFrame();
            if frame.origin.y < visible.origin.y {
                frame.origin.y = visible.origin.y;
            }
        }
        window.setFrame_display_animate(frame, true, true);
    }

    fn reject(&self, message: String) {
        self.ui().notice = Some((true, message));
        self.render();
    }

    fn save(&self) {
        let domain = match network::domain(&self.ivars().widgets.host.stringValue().to_string()) {
            Ok(domain) => domain,
            Err(message) => return self.reject(message),
        };
        let filter = self.ui().filter;
        let address = if filter {
            match Ipv6Suffix::parse(&self.ivars().widgets.suffix.stringValue().to_string()) {
                Ok(suffix) => AddressPolicy::Suffix(suffix),
                Err(message) => return self.reject(message.into()),
            }
        } else {
            AddressPolicy::Stable
        };
        let widgets = &self.ivars().widgets;
        let provider = selected_provider(widgets);
        let credentials = match provider {
            Provider::Cloudflare => Credentials::Cloudflare {
                token: widgets.token.stringValue().to_string(),
            },
            Provider::Aliyun => Credentials::Aliyun {
                access_key_id: widgets.access_key.stringValue().to_string(),
                access_key_secret: widgets.secret.stringValue().to_string(),
            },
        };
        let request = Request::Save {
            settings: Settings {
                domain,
                provider,
                address,
                prefer_ddns_source: widgets.prefer_source.state() == NSControlStateValueOn,
            },
            credentials,
            run: false,
        };
        self.work(Work::Save, Some(request));
    }

    fn work(&self, work: Work, request: Option<Request>) {
        let sequence;
        let sender;
        {
            let mut ui = self.ui();
            if ui
                .pending
                .is_some_and(|item| !matches!(item, Work::Poll) || matches!(work, Work::Poll))
            {
                return;
            }
            ui.pending = Some(work);
            ui.sequence += 1;
            sequence = ui.sequence;
            sender = ui.sender.clone();
            if !matches!(work, Work::Poll) {
                ui.notice = None;
                ui.connection_issue = None;
            }
        }
        if !matches!(work, Work::Poll) {
            self.render();
        }
        thread::spawn(move || {
            let result = match work {
                Work::Poll | Work::Save | Work::Run => {
                    service::client(request.unwrap_or(Request::Status)).map(Some)
                }
                Work::Install => {
                    let uid = platform::current_uid().to_string();
                    platform::elevate(&["--install-admin", &uid]).map(|()| None)
                }
                Work::Uninstall => platform::elevate(&["--uninstall-admin"]).map(|()| None),
                Work::Automatic(enabled) => platform::elevate(&[if enabled {
                    "--auto-on-admin"
                } else {
                    "--auto-off-admin"
                }])
                .map(|()| None),
            };
            let _ = sender.send(Event(sequence, work, result));
        });
    }

    fn receive(&self) {
        loop {
            let event = self.ui().receiver.try_recv();
            match event {
                Ok(event) => self.receive_event(event),
                Err(_) => break,
            }
        }
    }

    fn receive_event(&self, Event(sequence, work, outcome): Event) {
        {
            let mut ui = self.ui();
            if sequence != ui.sequence {
                return;
            }
            ui.pending = None;
        }
        match outcome {
            Ok(Some(reply)) => {
                self.ui().connection_issue = None;
                self.receive_reply(work, reply);
            }
            Ok(None) => match work {
                Work::Install => {
                    self.ui().installed = true;
                    let installed = self.ui().installed;
                    hide_settings(&self.ivars().widgets, self.ui().expanded, installed);
                    self.work(Work::Poll, None);
                }
                Work::Uninstall => {
                    self.ui().installed = false;
                    info(
                        "服务已卸载。",
                        "后台同步已停止，安装文件和登录启动项已删除。",
                    );
                    NSApplication::sharedApplication(MainThreadMarker::new().expect("main thread"))
                        .terminate(None);
                    return;
                }
                Work::Automatic(enabled) => self.ui().snapshot.automatic = enabled,
                _ => (),
            },
            Err(message) if matches!(work, Work::Poll) => {
                self.ui().connection_issue = Some(message);
            }
            Err(message) => self.ui().notice = Some((true, message)),
        }
        self.render();
    }

    fn receive_reply(&self, work: Work, reply: Reply) {
        let widgets = &self.ivars().widgets;
        let previous_provider = selected_provider(widgets);
        let mut ui = self.ui();
        if !ui.initialized {
            ui.initialized = true;
            if let Some(settings) = &reply.snapshot.settings {
                text(&widgets.host, &settings.domain);
                widgets.provider.selectItemAtIndex(match settings.provider {
                    Provider::Cloudflare => 0,
                    Provider::Aliyun => 1,
                });
                if let AddressPolicy::Suffix(suffix) = &settings.address {
                    ui.filter = true;
                    text(&widgets.suffix, &suffix.to_string());
                }
                widgets
                    .prefer_source
                    .setState(if settings.prefer_ddns_source {
                        NSControlStateValueOn
                    } else {
                        NSControlStateValueOff
                    });
            }
        }
        if let Some(error) = reply.error.clone() {
            ui.notice = Some((true, error));
        } else if matches!(work, Work::Save) {
            match selected_provider(widgets) {
                Provider::Cloudflare => widgets.token.setStringValue(&NSString::from_str("")),
                Provider::Aliyun => {
                    widgets.access_key.setStringValue(&NSString::from_str(""));
                    widgets.secret.setStringValue(&NSString::from_str(""));
                }
            }
            ui.notice = Some((false, "已保存配置。点击「立即运行」可同步。".into()));
        } else if ui.snapshot.phase != reply.snapshot.phase {
            ui.notice = None;
        }
        ui.snapshot = reply.snapshot;
        let resized = ui.expanded && previous_provider != selected_provider(widgets);
        drop(ui);
        if resized {
            self.resize(true);
        }
    }

    fn status(&self) -> (String, String, Tone) {
        let ui = self.ui();
        if let Some(message) = &ui.connection_issue {
            return ("服务连接失败".into(), message.clone(), Tone::Error);
        }
        if let Some((is_error, message)) = &ui.notice {
            return if *is_error {
                ("需要处理".into(), message.clone(), Tone::Error)
            } else {
                ("已保存".into(), message.clone(), Tone::Success)
            };
        }
        if matches!(
            ui.pending,
            Some(Work::Install | Work::Uninstall | Work::Automatic(_))
        ) {
            return (
                "等待管理员授权".into(),
                "请在系统授权窗口中确认操作。".into(),
                Tone::Accent,
            );
        }
        if !ui.installed {
            return (
                "等待安装".into(),
                "安装服务后，即可在登录前自动同步 IPv6。".into(),
                Tone::Muted,
            );
        }
        if !ui.initialized {
            return (
                "正在连接服务".into(),
                "正在读取本机的 DDNS 状态。".into(),
                Tone::Accent,
            );
        }
        match ui.snapshot.phase {
            Phase::Idle if ui.snapshot.settings.is_none() => (
                "等待配置".into(),
                format!(
                    "打开设置，填写域名和 {} 凭据。",
                    selected_provider(&self.ivars().widgets).name()
                ),
                Tone::Muted,
            ),
            Phase::Idle => (
                "等待运行".into(),
                "配置已保存。点击「立即运行」可手动更新。".into(),
                Tone::Muted,
            ),
            Phase::Running => (
                "正在同步".into(),
                format!(
                    "正在检查 {} 中的 AAAA 记录。",
                    ui.snapshot
                        .settings
                        .as_ref()
                        .map_or(Provider::default(), |settings| settings.provider)
                        .name()
                ),
                Tone::Accent,
            ),
            Phase::Success => (
                "同步完成".into(),
                ui.snapshot.message.clone(),
                Tone::Success,
            ),
            Phase::Failed => ("同步失败".into(), ui.snapshot.message.clone(), Tone::Error),
        }
    }

    fn render(&self) {
        let widgets = &self.ivars().widgets;
        let (heading, mut detail, tone) = self.status();
        let (
            address,
            last_run,
            next_run,
            automatic,
            filter,
            expanded,
            installed,
            initialized,
            phase,
            active,
        ) = {
            let ui = self.ui();
            let active = !matches!(
                ui.pending,
                Some(Work::Install | Work::Uninstall | Work::Automatic(_) | Work::Save | Work::Run)
            );
            (
                ui.snapshot.address,
                ui.snapshot.last_run,
                ui.snapshot.next_run,
                ui.snapshot.automatic,
                ui.filter,
                ui.expanded,
                ui.installed,
                ui.initialized,
                ui.snapshot.phase,
                active,
            )
        };
        if heading == "同步完成"
            && let Some(address) = address
        {
            detail = format!("{detail} · {address}");
        }
        text(&widgets.state, &heading);
        widgets.state.setTextColor(Some(&tone_color(tone)));
        text(&widgets.detail, &detail);
        let times = match (last_run, next_run) {
            (Some(last), Some(next)) => format!(
                "最近运行  {}     下次检查  {}",
                platform::local_time(last),
                platform::local_time(next)
            ),
            (Some(last), None) => format!("最近运行  {}", platform::local_time(last)),
            (None, Some(next)) => format!("下次检查  {}", platform::local_time(next)),
            (None, None) => String::new(),
        };
        text(&widgets.times, &times);
        let provider = selected_provider(widgets);
        let saved_provider = self
            .ui()
            .snapshot
            .settings
            .as_ref()
            .map(|settings| settings.provider);
        let same_provider = saved_provider == Some(provider);
        text(
            &widgets.badge,
            &format!("{}  ·  AAAA", saved_provider.unwrap_or(provider).name()),
        );
        widgets
            .token
            .setPlaceholderString(Some(&NSString::from_str(if same_provider {
                "已保存，留空继续使用"
            } else {
                "输入 Cloudflare API Token"
            })));
        widgets
            .access_key
            .setPlaceholderString(Some(&NSString::from_str(if same_provider {
                "已保存，留空继续使用"
            } else {
                "输入阿里云 AccessKey ID"
            })));
        widgets
            .secret
            .setPlaceholderString(Some(&NSString::from_str(if same_provider {
                "已保存，留空继续使用"
            } else {
                "输入阿里云 AccessKey Secret"
            })));
        let offset = if provider == Provider::Aliyun {
            46.0
        } else {
            0.0
        };
        widgets
            .filter
            .setFrame(rect(32.0, 392.0 + offset, 203.0, 25.0));
        let suffix_frame = rect(313.0, 391.0 + offset, 141.0, 24.0);
        let label_height = widgets.suffix_label.intrinsicContentSize().height;
        widgets.suffix.setFrame(suffix_frame);
        widgets.suffix_label.setFrame(rect(
            244.0,
            suffix_frame.origin.y + (suffix_frame.size.height - label_height) / 2.0,
            70.0,
            label_height,
        ));
        widgets
            .hint
            .setFrame(rect(32.0, 428.0 + offset, 422.0, 21.0));
        widgets
            .prefer_source
            .setFrame(rect(32.0, 451.0 + offset, 422.0, 25.0));
        widgets
            .source_hint
            .setFrame(rect(32.0, 480.0 + offset, 422.0, 21.0));
        widgets
            .automatic
            .setFrame(rect(32.0, 527.0 + offset, 174.0, 32.0));
        widgets
            .uninstall
            .setFrame(rect(231.0, 527.0 + offset, 102.0, 32.0));
        widgets
            .save
            .setFrame(rect(355.0, 527.0 + offset, 99.0, 32.0));
        title(
            &widgets.run,
            if !installed {
                "安装服务"
            } else if phase == Phase::Running {
                "运行中…"
            } else {
                "立即运行"
            },
        );
        title(
            &widgets.toggle,
            if expanded {
                "收起设置"
            } else {
                "展开设置"
            },
        );
        text(
            &widgets.hint,
            if filter {
                "匹配末尾的十六进制段，例如 ::abcd:1234。"
            } else {
                "默认使用稳定的公网 IPv6，跳过临时地址。"
            },
        );
        let ui = self.ui();
        let failure = &ui.snapshot.source_preference_error;
        text(
            &widgets.source_hint,
            failure
                .as_deref()
                .unwrap_or("影响本机各应用的新 IPv6 连接，首次同步成功后生效。"),
        );
        widgets
            .source_hint
            .setTextColor(Some(&tone_color(if failure.is_some() {
                Tone::Error
            } else {
                Tone::Muted
            })));
        drop(ui);
        let ready = active && phase != Phase::Running;
        let editable = active && (!installed || initialized);
        widgets.host.setEnabled(editable);
        widgets.token.setEnabled(editable);
        widgets.provider.setEnabled(editable);
        widgets.access_key.setEnabled(editable);
        widgets.secret.setEnabled(editable);
        widgets.filter.setEnabled(editable);
        widgets.suffix.setEnabled(editable && filter);
        widgets.prefer_source.setEnabled(editable);
        widgets.run.setEnabled(ready && (!installed || initialized));
        widgets.save.setEnabled(installed && initialized && ready);
        widgets.uninstall.setEnabled(installed && active);
        widgets
            .automatic
            .setEnabled(installed && initialized && active);
        widgets.automatic.setState(if automatic {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        widgets.filter.setState(if filter {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        hide_settings(widgets, expanded, installed);
    }

    fn tick(&self) {
        if SHOW.swap(false, Ordering::SeqCst) {
            self.show();
        }
        self.receive();
        let poll = {
            let mut ui = self.ui();
            ui.ticks += 1;
            self.ivars().window.isVisible()
                && ui.installed
                && ui.pending.is_none()
                && ui.ticks.is_multiple_of(2)
        };
        if poll {
            self.work(Work::Poll, None);
        }
    }
}

fn title(button: &NSButton, value: &str) {
    if button.title().to_string() != value {
        button.setTitle(&NSString::from_str(value));
    }
}

fn activate(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    if NSObjectProtocol::respondsToSelector(&*app, sel!(activate)) {
        app.activate();
    } else {
        activate_legacy();
    }
}

#[allow(deprecated)]
fn activate_legacy() {
    NSRunningApplication::currentApplication()
        .activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
}

fn text(field: &NSTextField, value: &str) {
    if field.stringValue().to_string() != value {
        field.setStringValue(&NSString::from_str(value));
    }
}

fn info(title: &str, body: &str) {
    let alert = NSAlert::new(MainThreadMarker::new().expect("main thread"));
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(body));
    alert.addButtonWithTitle(&NSString::from_str("好"));
    alert.runModal();
}

fn acquire() -> Result<(File, bool)> {
    let home = platform::home(platform::current_uid())?;
    let directory = home.join("Library/Application Support/IPv6DDNS");
    fs::create_dir_all(&directory).map_err(|e| error("创建托盘目录失败", e))?;
    let path = directory.join("tray.lock");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|e| error("创建托盘实例锁失败", e))?;
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !locked {
        let errno = std::io::Error::last_os_error();
        if errno.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(error("创建托盘实例锁失败", errno));
        }
        return Ok((file, true));
    }
    file.set_len(0)
        .map_err(|e| error("写入托盘实例锁失败", e))?;
    write!(file, "{}", std::process::id()).map_err(|e| error("写入托盘实例锁失败", e))?;
    Ok((file, false))
}

fn signal_running() {
    let Ok(home) = platform::home(platform::current_uid()) else {
        return;
    };
    let path = home.join("Library/Application Support/IPv6DDNS/tray.lock");
    for _ in 0..5 {
        if let Ok(text) = fs::read_to_string(&path)
            && let Ok(pid) = text.trim().parse::<libc::pid_t>()
        {
            unsafe {
                libc::kill(pid, libc::SIGUSR1);
            }
            return;
        }
        thread::sleep(std::time::Duration::from_millis(20));
    }
}

pub fn launch(hidden: bool) -> Result<()> {
    unsafe {
        libc::signal(libc::SIGUSR1, show as *const () as libc::sighandler_t);
    }
    let (lock, existing) = acquire()?;
    if existing {
        if !hidden {
            signal_running();
        }
        return Ok(());
    }
    let _controller = setup(hidden, lock)?;
    let mtm = MainThreadMarker::new().ok_or("界面必须在主线程运行。")?;
    NSApplication::sharedApplication(mtm).run();
    Ok(())
}

fn setup(hidden: bool, lock: File) -> Result<Retained<Controller>> {
    let mtm = MainThreadMarker::new().ok_or("界面必须在主线程运行。")?;
    let app = NSApplication::sharedApplication(mtm);
    let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    build_main_menu(mtm);
    let window = build_window(mtm);
    let content = window.contentView().ok_or("创建主窗口失败。")?;
    let widgets = build_widgets(mtm, &content);
    let (status, open) = build_status(mtm);
    let (sender, receiver) = mpsc::channel();
    let installed = platform::installed();
    let controller = Controller::alloc(mtm).set_ivars(State {
        window,
        widgets,
        _status: status,
        _lock: lock,
        ui: RefCell::new(Ui {
            snapshot: Snapshot::default(),
            installed,
            initialized: false,
            expanded: false,
            filter: false,
            pending: None,
            sequence: 0,
            notice: None,
            connection_issue: None,
            sender,
            receiver,
            ticks: 0,
        }),
    });
    let controller: Retained<Controller> = unsafe { msg_send![super(controller), init] };

    let widgets = &controller.ivars().widgets;
    unsafe {
        widgets.toggle.setTarget(Some(&controller));
        widgets.toggle.setAction(Some(sel!(toggleSettings:)));
        widgets.run.setTarget(Some(&controller));
        widgets.run.setAction(Some(sel!(runNow:)));
        widgets.save.setTarget(Some(&controller));
        widgets.save.setAction(Some(sel!(saveSettings:)));
        widgets.provider.setTarget(Some(&controller));
        widgets.provider.setAction(Some(sel!(providerChanged:)));
        widgets.filter.setTarget(Some(&controller));
        widgets.filter.setAction(Some(sel!(filterChanged:)));
        widgets.automatic.setTarget(Some(&controller));
        widgets.automatic.setAction(Some(sel!(autoChanged:)));
        widgets.uninstall.setTarget(Some(&controller));
        widgets.uninstall.setAction(Some(sel!(uninstallService:)));
        open.setTarget(Some(&controller));
    }
    controller
        .ivars()
        .window
        .setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            0.5,
            &controller,
            sel!(tick:),
            None,
            true,
        );
    }
    controller.render();
    if !hidden {
        controller.show();
    }
    Ok(controller)
}

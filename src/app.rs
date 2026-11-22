use crate::client::{Event, Helper};
use crate::config;
use crate::i18n::{EN, RU, Tr};
use crate::import;
use crate::proto::{IfaceStatus, Request};
use crate::store::{Store, Tunnel};
use crate::tray::{Tray, TrayState};
use crate::waker::{UiEvent, Waker};
use eframe::egui::{
    self, Align, Color32, FontId, Layout, RichText, Sense, TextFormat, Vec2, text::LayoutJob,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, channel},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const GREEN: Color32 = Color32::from_rgb(0x2e, 0xb8, 0x5c);
const YELLOW: Color32 = Color32::from_rgb(0xe5, 0xa5, 0x0a);
const AMBER_DARK: Color32 = Color32::from_rgb(0xb0, 0x6a, 0x00);

/// Warning color readable on both light and dark backgrounds.
fn warn(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode { YELLOW } else { AMBER_DARK }
}

/// Version of settings.json. Files older than 2 (or none at all) get the current
/// defaults of the options below: LAN bypass and kill-switch on.
const SETTINGS_VERSION: u32 = 2;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Settings {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    theme: Option<String>,
    #[serde(default)]
    lan_bypass: bool,
    #[serde(default)]
    kill_switch: bool,
    #[serde(default)]
    boot_connect: bool,
    /// Look for a newer release on GitHub once a day (off by default).
    #[serde(default)]
    check_updates: bool,
    /// Unix time of the last update check.
    #[serde(default)]
    update_checked_at: u64,
    /// Newer release found by the last check.
    #[serde(default)]
    known_update: Option<crate::update::Release>,
    /// Version already announced with a notification.
    #[serde(default)]
    notified_update: Option<String>,
}

fn load_settings(json: Option<&str>) -> Settings {
    let mut settings: Settings = json.and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();
    if settings.version < SETTINGS_VERSION {
        // Defaults: local devices reachable, nothing leaks if the tunnel dies.
        // (0.1.0-beta saved lan_bypass = false for everyone; turning on only the
        // kill-switch would cut those users off from their local network.)
        settings.lan_bypass = true;
        settings.kill_switch = true;
    }
    settings
}

fn theme_from(s: Option<&str>) -> egui::ThemePreference {
    match s {
        Some("light") => egui::ThemePreference::Light,
        Some("dark") => egui::ThemePreference::Dark,
        _ => egui::ThemePreference::System,
    }
}

fn theme_str(t: egui::ThemePreference) -> &'static str {
    match t {
        egui::ThemePreference::Light => "light",
        egui::ThemePreference::Dark => "dark",
        egui::ThemePreference::System => "system",
    }
}
const GREY: Color32 = Color32::from_rgb(0x90, 0x90, 0x90);
const RED: Color32 = Color32::from_rgb(0xe0, 0x4b, 0x4b);

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Tunnels,
    Log,
}

#[derive(PartialEq, Clone, Copy)]
enum Busy {
    Up,
    Down,
}

enum Dialog {
    Editor { original: Option<String>, name: String, text: String, error: Option<String> },
    Paste { text: String, error: Option<String> },
    ConfirmDelete(String),
    /// Imported configs contain PreUp/PostUp/... commands (run as root).
    ConfirmScripts { cands: Vec<import::Candidate>, scripts: Vec<String> },
    About,
}

/// (started by the user, newer release or error)
type UpdateResult = (bool, Result<Option<crate::update::Release>, String>);

enum FileResult {
    Import(Vec<PathBuf>),
    Export(String, PathBuf),
    ExportAll(PathBuf),
    SaveLog(PathBuf),
}

#[derive(Clone, Copy)]
enum Backend {
    Kernel,
    SystemGo,
    Embedded,
    None,
}

pub struct App {
    ru: bool,
    store: Store,
    tunnels: Vec<Tunnel>,
    selected: Option<String>,
    tab: Tab,
    helper: Option<Helper>,
    status: HashMap<String, IfaceStatus>,
    status_inflight: bool,
    last_status: Instant,
    busy: HashMap<String, Busy>,
    active: HashSet<String>,
    last_scan: Instant,
    log: Vec<String>,
    toast: Option<(String, bool, Instant)>,
    dialog: Option<Dialog>,
    file_rx: Option<Receiver<FileResult>>,
    backend: Backend,
    screenshot: Option<(PathBuf, u32)>,
    theme: egui::ThemePreference,
    logo: Option<(egui::TextureHandle, f32)>,
    waker: Waker,
    events: Receiver<UiEvent>,
    tray: Option<Tray>,
    /// The whole application should exit (not just hide the window).
    pub quit: bool,
    /// The window should be (re)opened.
    pub show_requested: bool,
    autostart: bool,
    debug: String,
    lan_bypass: bool,
    kill_switch: bool,
    boot_connect: bool,
    /// The kill-switch blocks traffic although no tunnel is up (from the helper).
    blocked: bool,
    /// Tunnel activated last (offered by "Reconnect" when the kill-switch blocks).
    last_active: Option<String>,
    /// A window is open (notifications are sent only while it is not).
    window_open: bool,
    /// Last status polling error (logged once, not every second).
    status_error: Option<String>,
    check_updates: bool,
    update_checked_at: u64,
    /// A newer release is available.
    update: Option<crate::update::Release>,
    notified_update: Option<String>,
    /// Running update check: (started by the user, result).
    update_rx: Option<Receiver<UpdateResult>>,
    awg_expanded: bool,
    sys_theme: crate::system_theme::SystemTheme,
    /// System theme last given to the current egui context.
    applied_sys_theme: Option<egui::Theme>,
}

impl App {
    pub fn new(events: Receiver<UiEvent>, waker: Waker, tray: Option<Tray>, sys_theme: crate::system_theme::SystemTheme) -> Self {
        let store = Store::open().expect("cannot create ~/.config/awg-gui");
        let settings = load_settings(std::fs::read_to_string(store.dir.parent().unwrap().join("settings.json")).ok().as_deref());
        let ru = settings.lang.as_deref().map(|l| l == "ru").unwrap_or_else(crate::i18n::detect_ru);
        let theme = match std::env::var("AWG_GUI_THEME").ok() {
            Some(t) => theme_from(Some(&t)),
            None => theme_from(settings.theme.as_deref()),
        };
        let mut app = App {
            ru,
            tunnels: store.list(),
            store,
            selected: None,
            tab: Tab::Tunnels,
            helper: None,
            status: HashMap::new(),
            status_inflight: false,
            last_status: Instant::now(),
            busy: HashMap::new(),
            active: HashSet::new(),
            last_scan: Instant::now() - Duration::from_secs(10),
            log: crate::logfile::load(),
            toast: None,
            dialog: None,
            file_rx: None,
            backend: detect_backend(),
            screenshot: None,
            theme,
            logo: None,
            waker,
            events,
            tray,
            quit: false,
            show_requested: false,
            autostart: crate::desktop::autostart_enabled(),
            lan_bypass: settings.lan_bypass,
            kill_switch: settings.kill_switch,
            boot_connect: settings.boot_connect,
            blocked: false,
            last_active: None,
            window_open: false,
            status_error: None,
            check_updates: settings.check_updates,
            update_checked_at: settings.update_checked_at,
            update: settings.known_update.clone().filter(|u| crate::update::is_newer(&u.version)),
            notified_update: settings.notified_update.clone(),
            update_rx: None,
            awg_expanded: false,
            sys_theme,
            applied_sys_theme: None,
            debug: if cfg!(debug_assertions) { std::env::var("AWG_GUI_DEBUG").unwrap_or_default() } else { String::new() },
        };
        app.selected = app.tunnels.first().map(|t| t.name.clone());
        app.scan_active();
        if let Some(a) = app.tunnels.iter().find(|t| app.active.contains(&t.name)) {
            app.selected = Some(a.name.clone());
        }
        app.update_tray();
        // Debug-only hooks used for automated screenshots.
        let dbg = app.debug.clone();
        if dbg.contains("helper") {
            app.send(Request::Status);
        }
        if dbg.contains("editor")
            && let Some(t) = app.selected.clone().and_then(|s| app.tunnel(&s).cloned()) {
                app.dialog = Some(Dialog::Editor { original: Some(t.name.clone()), name: t.name, text: t.text, error: None });
            }
        if dbg.contains("paste") {
            app.dialog = Some(Dialog::Paste { text: String::new(), error: None });
        }
        if dbg.contains("log") {
            app.tab = Tab::Log;
        }
        if dbg.contains("awg") {
            app.awg_expanded = true;
        }
        if dbg.contains("lan") {
            app.lan_bypass = true;
        }
        app
    }

    pub fn has_tray(&self) -> bool {
        self.tray.as_ref().map(|t| !t.is_closed()).unwrap_or(false)
    }

    /// Called whenever a new window (egui context) is created.
    pub fn attach(&mut self, ctx: &egui::Context) {
        ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = Vec2::new(8.0, 6.0);
            style.spacing.button_padding = Vec2::new(10.0, 5.0);
            for (ts, size) in [
                (egui::TextStyle::Body, 14.5),
                (egui::TextStyle::Button, 14.5),
                (egui::TextStyle::Monospace, 13.5),
                (egui::TextStyle::Heading, 19.0),
            ] {
                if let Some(f) = style.text_styles.get_mut(&ts) {
                    f.size = size;
                }
            }
        });
        ctx.style_mut_of(egui::Theme::Light, |style| {
            style.visuals.panel_fill = Color32::from_gray(246);
            style.visuals.window_fill = Color32::from_gray(240);
            style.visuals.faint_bg_color = Color32::from_gray(238);
            style.visuals.extreme_bg_color = Color32::WHITE;
            style.visuals.selection.bg_fill = Color32::from_rgb(0xf3, 0x8a, 0x45);
            style.visuals.selection.stroke.color = Color32::WHITE;
            style.visuals.hyperlink_color = Color32::from_rgb(0xc8, 0x55, 0x1c);
        });
        ctx.style_mut_of(egui::Theme::Dark, |style| {
            style.visuals.selection.bg_fill = Color32::from_rgb(0xb8, 0x52, 0x1f);
            style.visuals.selection.stroke.color = Color32::WHITE;
        });
        ctx.set_theme(self.theme);
        self.applied_sys_theme = None;
        self.follow_system_theme(ctx);
        self.logo = None;
        self.show_requested = false;
        self.screenshot = std::env::var_os("AWG_GUI_SCREENSHOT").map(|p| (PathBuf::from(p), 0));
        self.waker.set(Some(ctx.clone()));
        self.window_open = true;
    }

    /// "System" theme: egui falls back to `fallback_theme` because winit reports no
    /// system theme on Linux; feed it the desktop preference from the portal.
    fn follow_system_theme(&mut self, ctx: &egui::Context) {
        let t = self.sys_theme.get();
        if t.is_some() && t != self.applied_sys_theme {
            self.applied_sys_theme = t;
            ctx.options_mut(|o| o.fallback_theme = t.unwrap());
            ctx.request_repaint();
        }
    }

    /// Called after the window has been closed.
    pub fn detach(&mut self) {
        self.waker.set(None);
        self.window_open = false;
        self.logo = None;
        if self.debug.contains("reopened") && !self.debug.contains("reshow") {
            self.debug.push_str(",reshow");
            self.show_requested = true; // debug: simulate "Open" from the tray
        }
        self.file_rx = None;
    }

    /// Background work; runs every frame and also while the window is closed.
    pub fn tick(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            self.handle_event(ev);
        }
        self.poll_helper();
        self.poll_files();
        self.poll_update();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        if self.check_updates && self.update_rx.is_none() && now.saturating_sub(self.update_checked_at) >= 86400 {
            self.start_update_check(false);
        }
        if self.last_scan.elapsed() > Duration::from_millis(1000) {
            self.scan_active();
        }
        self.update_tray();
    }

    /// Block (with a timeout) until something happens while the window is closed.
    pub fn wait(&mut self, timeout: Duration) {
        if let Ok(ev) = self.events.recv_timeout(timeout) {
            self.handle_event(ev);
        }
        self.tick();
    }

    fn handle_event(&mut self, ev: UiEvent) {
        match ev {
            UiEvent::Show => self.show_requested = true,
            UiEvent::Quit => self.quit = true,
            UiEvent::Reload => self.reload(),
            UiEvent::Toggle(name) => self.toggle(&name),
            UiEvent::Import => {
                self.show_requested = true;
                self.tab = Tab::Tunnels;
                self.pick_import_files();
            }
            UiEvent::About => {
                self.show_requested = true;
                self.dialog = Some(Dialog::About);
            }
        }
    }

    fn toggle(&mut self, name: &str) {
        if self.active.contains(name) {
            self.deactivate(name);
        } else {
            self.activate(name);
        }
    }

    fn update_tray(&mut self) {
        let state = TrayState {
            ru: self.ru,
            tunnels: self
                .tunnels
                .iter()
                .map(|t| (t.name.clone(), self.active.contains(&t.name), self.busy.contains_key(&t.name)))
                .collect(),
            addresses: self
                .tunnels
                .iter()
                .filter(|t| self.active.contains(&t.name))
                .filter_map(|t| config::parse(&t.text).ok())
                .flat_map(|c| c.interface().map(|i| i.list("Address")).unwrap_or_default())
                .collect(),
            update: self.update.clone(),
        };
        if let Some(t) = &mut self.tray {
            t.set(state);
        }
    }

    fn set_autostart(&mut self, on: bool) {
        match crate::desktop::set_autostart(on) {
            Ok(()) => self.autostart = on,
            Err(e) => self.error(e.to_string()),
        }
    }

    fn tr(&self) -> &'static Tr {
        if self.ru { &RU } else { &EN }
    }

    fn logln(&mut self, s: impl AsRef<str>) {
        let stamp = crate::clock::stamp(crate::clock::now_local());
        let lines: Vec<String> =
            s.as_ref().lines().filter(|l| !l.trim().is_empty()).map(|l| format!("{stamp}  {l}")).collect();
        crate::logfile::append(&lines);
        self.log.extend(lines);
        if self.log.len() > crate::logfile::KEEP_LINES {
            self.log.drain(..1000);
        }
    }

    /// Desktop notification, only while the window is closed (unless `always`).
    fn notify(&self, summary: &str, body: String, urgent: bool, always: bool) {
        if (always || !self.window_open) && self.screenshot.is_none() {
            crate::notify::send(summary.to_string(), body, urgent);
        }
    }

    fn info(&mut self, s: impl Into<String>) {
        let s = s.into();
        self.logln(&s);
        self.toast = Some((s, false, Instant::now()));
    }

    fn error(&mut self, s: impl Into<String>) {
        let s = s.into();
        self.logln(format!("ERROR: {s}"));
        self.notify(self.tr().n_error_title, s.clone(), true, false);
        self.toast = Some((s, true, Instant::now()));
    }

    fn reload(&mut self) {
        self.tunnels = self.store.list();
        if let Some(s) = &self.selected
            && !self.tunnels.iter().any(|t| &t.name == s) {
                self.selected = self.tunnels.first().map(|t| t.name.clone());
            }
    }

    fn tunnel(&self, name: &str) -> Option<&Tunnel> {
        self.tunnels.iter().find(|t| t.name == name)
    }

    fn scan_active(&mut self) {
        // debug builds: AWG_GUI_FAKE_ACTIVE=name1,name2 pretends these are up (screenshots)
        let fake = if cfg!(debug_assertions) { std::env::var("AWG_GUI_FAKE_ACTIVE").unwrap_or_default() } else { String::new() };
        let now: HashSet<String> = self
            .tunnels
            .iter()
            .filter(|t| Path::new("/sys/class/net").join(&t.name).exists() || fake.split(',').any(|f| f == t.name))
            .map(|t| t.name.clone())
            .collect();
        // A tunnel that disappeared without being deactivated here went down by itself
        // (amneziawg-go crashed, interface removed, ...).
        let lost: Vec<String> =
            self.active.iter().filter(|n| !now.contains(*n) && !self.busy.contains_key(*n) && self.tunnel(n).is_some()).cloned().collect();
        self.active = now;
        self.last_scan = Instant::now();
        for name in lost {
            let ks = self.kill_switch_on()
                && self.tunnel(&name).and_then(|t| config::parse(&t.text).ok()).map(|c| c.uses_policy_routing()).unwrap_or(false);
            let tr = self.tr();
            let mut body = tr.n_lost.replace("{}", &name);
            if ks {
                body.push_str(tr.n_lost_ks);
                self.blocked = true;
            }
            self.logln(format!("ERROR: {body}"));
            self.toast = Some((body.clone(), true, Instant::now()));
            self.notify(tr.n_lost_title, body, true, true);
        }
    }

    // ---------- helper communication ----------

    fn send(&mut self, req: Request) {
        if self.helper.is_none() {
            let w = self.waker.clone();
            match Helper::spawn(move || w.wake()) {
                Ok(h) => {
                    self.logln("requesting administrator authorization (pkexec)…");
                    self.helper = Some(h);
                }
                Err(e) => return self.error(e),
            }
        }
        match &req {
            Request::Up { name, .. } => {
                self.busy.insert(name.clone(), Busy::Up);
            }
            Request::Down { name, .. } => {
                self.busy.insert(name.clone(), Busy::Down);
            }
            Request::Status => self.status_inflight = true,
            Request::SetBoot { .. } | Request::Unblock => {}
        }
        if !self.helper.as_ref().unwrap().send(req) {
            self.helper = None;
            self.busy.clear();
            self.status_inflight = false;
            self.error("helper is not running");
        }
    }

    fn poll_helper(&mut self) {
        let mut events = vec![];
        if let Some(h) = &self.helper {
            while let Ok(e) = h.rx.try_recv() {
                events.push(e);
            }
        }
        for e in events {
            match e {
                Event::Ready => {
                    if let Some(h) = &mut self.helper {
                        h.ready = true;
                    }
                    self.logln("helper started");
                }
                Event::Died(msg) => {
                    self.helper = None;
                    self.busy.clear();
                    self.status.clear();
                    self.status_inflight = false;
                    self.error(msg);
                }
                Event::Reply(Request::Status, resp) => {
                    self.status_inflight = false;
                    if resp.ok {
                        if !resp.output.trim().is_empty() {
                            self.logln(&resp.output);
                        }
                        self.status_error = None;
                        self.blocked = resp.blocked;
                        if let Some(s) = resp.status {
                            self.status = s.into_iter().map(|i| (i.name.clone(), i)).collect();
                        }
                    } else {
                        // polled every second: log a failure once, not every time
                        let e = resp.error.unwrap_or_default();
                        if self.status_error.as_deref() != Some(e.as_str()) {
                            self.logln(format!("ERROR: statistics: {e}"));
                            self.logln(&resp.output);
                            self.status_error = Some(e);
                        }
                    }
                }
                Event::Reply(req, resp) => {
                    if !resp.output.trim().is_empty() {
                        self.logln(&resp.output);
                    }
                    let tr = self.tr();
                    match req {
                        Request::Up { name, .. } => {
                            if self.busy.get(&name) == Some(&Busy::Up) {
                                self.busy.remove(&name);
                            }
                            if resp.ok {
                                self.info(format!("{name}: {}", tr.active));
                                self.notify(tr.n_activated_title, tr.n_activated.replace("{}", &name), false, false);
                                self.backend = detect_backend();
                                self.blocked = false;
                                self.last_active = Some(name);
                            }
                        }
                        Request::Down { name, .. } => {
                            if self.busy.get(&name) == Some(&Busy::Down) {
                                self.busy.remove(&name);
                            }
                            if resp.ok {
                                // gone on purpose: not "lost"
                                self.active.remove(&name);
                                self.blocked = false;
                                self.info(format!("{name}: {}", tr.inactive));
                                self.notify(tr.n_deactivated_title, tr.n_deactivated.replace("{}", &name), false, false);
                            }
                        }
                        Request::Unblock => {
                            if resp.ok {
                                self.blocked = false;
                            }
                        }
                        Request::SetBoot { .. } | Request::Status => {}
                    }
                    if !resp.ok {
                        self.error(resp.error.unwrap_or_else(|| "unknown error".into()));
                    }
                    self.scan_active();
                }
            }
        }
        let ready = self.helper.as_ref().map(|h| h.ready).unwrap_or(false);
        if ready && !self.status_inflight && self.last_status.elapsed() > Duration::from_secs(1) {
            self.last_status = Instant::now();
            self.send(Request::Status);
        }
    }

    /// The config actually given to awg-quick (the saved one is never changed).
    fn runtime_config(&self, text: &str) -> String {
        config::runtime_config(text, config::Runtime { lan_bypass: self.lan_bypass, kill_switch: self.kill_switch_on() })
    }

    /// The kill-switch is wanted and can work. Without nftables it is not applied
    /// (activation must not fail just because the option is on by default); the
    /// details view says so.
    fn kill_switch_on(&self) -> bool {
        self.kill_switch && nft_available()
    }

    fn set_lan_bypass(&mut self, on: bool) {
        self.lan_bypass = on;
        self.save_settings();
        self.reapply_options();
    }

    fn set_kill_switch(&mut self, on: bool) {
        self.kill_switch = on;
        if on && !nft_available() {
            self.error(self.tr().ks_no_nft);
        }
        self.save_settings();
        self.reapply_options();
    }

    /// Reconnect an active tunnel the runtime options apply to.
    fn reapply_options(&mut self) {
        let active: Vec<Tunnel> = self
            .tunnels
            .iter()
            .filter(|t| self.active.contains(&t.name) && !self.busy.contains_key(&t.name))
            .filter(|t| config::parse(&t.text).map(|c| c.uses_policy_routing()).unwrap_or(false))
            .cloned()
            .collect();
        for t in active {
            self.info(self.tr().reconnecting.replace("{}", &t.name));
            self.send(Request::Down { name: t.name.clone(), config: Some(t.text.clone()) });
            let cfg = self.runtime_config(&t.text);
            self.send(Request::Up { name: t.name, config: cfg, boot: self.boot_connect });
        }
    }

    fn set_check_updates(&mut self, on: bool) {
        self.check_updates = on;
        if on {
            self.update_checked_at = 0; // check right away
        } else {
            self.update = None;
        }
        self.save_settings();
    }

    /// Ask GitHub for releases in a thread. `manual`: the user pressed the button
    /// (allowed even with the daily check off — that click is the consent).
    fn start_update_check(&mut self, manual: bool) {
        if self.update_rx.is_some() {
            return;
        }
        self.update_checked_at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        self.save_settings();
        let (tx, rx) = channel();
        let w = self.waker.clone();
        std::thread::spawn(move || {
            let _ = tx.send((manual, crate::update::check()));
            w.wake();
        });
        self.update_rx = Some(rx);
    }

    fn poll_update(&mut self) {
        let Some(rx) = &self.update_rx else { return };
        let Ok((manual, res)) = rx.try_recv() else { return };
        self.update_rx = None;
        let tr = self.tr();
        match res {
            Ok(Some(r)) => {
                let msg = tr.update_available.replace("{}", &r.version);
                if manual {
                    self.info(&msg);
                } else {
                    self.logln(&msg);
                }
                if self.notified_update.as_deref() != Some(r.version.as_str()) {
                    self.notify(tr.n_update_title, tr.n_update.replace("{}", &r.version), false, true);
                    self.notified_update = Some(r.version.clone());
                }
                self.update = Some(r);
            }
            Ok(None) => {
                self.update = None;
                if manual {
                    self.info(tr.up_to_date);
                }
            }
            Err(e) => {
                let msg = tr.update_failed.replace("{}", &e);
                if manual {
                    self.error(msg);
                } else {
                    self.logln(format!("ERROR: {msg}"));
                    // try again in an hour instead of tomorrow
                    self.update_checked_at = self.update_checked_at.saturating_sub(86400 - 3600);
                }
            }
        }
        self.save_settings();
    }

    fn set_boot_connect(&mut self, on: bool) {
        self.boot_connect = on;
        self.save_settings();
        let active = self.tunnels.iter().find(|t| self.active.contains(&t.name)).cloned();
        let tunnel = match (on, active) {
            (true, Some(t)) => Some((t.name.clone(), self.runtime_config(&t.text))),
            (true, None) => return, // remembered on the next activation
            (false, _) => None,
        };
        self.send(Request::SetBoot { tunnel });
    }

    fn activate(&mut self, name: &str) {
        if self.busy.contains_key(name) {
            return;
        }
        let Some(t) = self.tunnel(name).cloned() else { return };
        if let Err(e) = config::validate(&t.text) {
            return self.error(format!("{name}: {e}"));
        }
        // Like the Windows client: only one tunnel at a time.
        let others: Vec<Tunnel> =
            self.tunnels.iter().filter(|o| o.name != name && self.active.contains(&o.name)).cloned().collect();
        for o in others {
            self.send(Request::Down { name: o.name, config: Some(o.text) });
        }
        let cfg = self.runtime_config(&t.text);
        self.send(Request::Up { name: t.name, config: cfg, boot: self.boot_connect });
    }

    fn deactivate(&mut self, name: &str) {
        if self.busy.contains_key(name) {
            return;
        }
        let config = self.tunnel(name).map(|t| t.text.clone());
        self.send(Request::Down { name: name.to_string(), config });
    }

    // ---------- import / export ----------

    /// Import, asking first if any config runs commands as root.
    fn import_candidates(&mut self, cands: Vec<import::Candidate>) {
        let scripts: Vec<String> = cands
            .iter()
            .filter_map(|c| config::parse(&c.text).ok().map(|conf| (c, conf)))
            .flat_map(|(c, conf)| conf.scripts().into_iter().map(move |(k, v)| format!("{}: {k} = {v}", c.name_hint)))
            .collect();
        if scripts.is_empty() {
            self.import_confirmed(cands);
        } else {
            self.dialog = Some(Dialog::ConfirmScripts { cands, scripts });
        }
    }

    fn import_confirmed(&mut self, cands: Vec<import::Candidate>) {
        let mut added: Vec<String> = vec![];
        for c in cands {
            if let Err(e) = config::validate(&c.text) {
                self.error(format!("{}: {e}", c.name_hint));
                continue;
            }
            let name = self.store.unique_name(&c.name_hint, &added);
            match self.store.save(&name, &c.text) {
                Ok(()) => added.push(name),
                Err(e) => self.error(format!("{name}: {e}")),
            }
        }
        if !added.is_empty() {
            self.reload();
            self.selected = added.last().cloned();
            self.tab = Tab::Tunnels;
            self.info(self.tr().imported.replace("{}", &added.join(", ")));
        }
    }

    fn import_paths(&mut self, paths: Vec<PathBuf>) {
        let mut all = vec![];
        for p in paths {
            match import::from_file(&p) {
                Ok(c) => all.extend(c),
                Err(e) => self.error(e),
            }
        }
        self.import_candidates(all);
    }

    fn pick_import_files(&mut self) {
        let (tx, rx) = channel();
        let title = self.tr().conf_files;
        std::thread::spawn(move || {
            if let Some(files) = rfd::FileDialog::new()
                .add_filter(title, &["conf", "zip", "vpn", "txt"])
                .add_filter("*", &["*"])
                .pick_files()
            {
                let _ = tx.send(FileResult::Import(files));
            }
        });
        self.file_rx = Some(rx);
    }

    fn pick_export_file(&mut self, name: String) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            if let Some(p) = rfd::FileDialog::new().set_file_name(format!("{name}.conf")).save_file() {
                let _ = tx.send(FileResult::Export(name, p));
            }
        });
        self.file_rx = Some(rx);
    }

    fn pick_export_all(&mut self) {
        let (tx, rx) = channel();
        let title = self.tr().zip_files;
        let name = format!("awg-tunnels-{}.zip", crate::clock::file_stamp(crate::clock::now_local()));
        std::thread::spawn(move || {
            if let Some(p) = rfd::FileDialog::new().add_filter(title, &["zip"]).set_file_name(name).save_file() {
                let p = if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) { p } else { p.with_extension("zip") };
                let _ = tx.send(FileResult::ExportAll(p));
            }
        });
        self.file_rx = Some(rx);
    }

    fn pick_save_log(&mut self) {
        let (tx, rx) = channel();
        let title = self.tr().text_files;
        let name = format!("awg-gui-log-{}.txt", crate::clock::file_stamp(crate::clock::now_local()));
        std::thread::spawn(move || {
            if let Some(p) = rfd::FileDialog::new().add_filter(title, &["txt"]).set_file_name(name).save_file() {
                let _ = tx.send(FileResult::SaveLog(p));
            }
        });
        self.file_rx = Some(rx);
    }

    /// Write a private file (configs contain keys): mode 0600.
    fn write_private(&mut self, path: &Path, data: &[u8]) -> bool {
        use std::os::unix::fs::OpenOptionsExt;
        let res = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, data));
        match res {
            Ok(()) => {
                self.info(self.tr().exported.replace("{}", &path.display().to_string()));
                true
            }
            Err(e) => {
                self.error(format!("{}: {e}", path.display()));
                false
            }
        }
    }

    fn poll_files(&mut self) {
        let Some(rx) = &self.file_rx else { return };
        let Ok(r) = rx.try_recv() else { return };
        self.file_rx = None;
        match r {
            FileResult::Import(paths) => self.import_paths(paths),
            FileResult::Export(name, path) => {
                let Some(t) = self.tunnel(&name).cloned() else { return };
                self.write_private(&path, t.text.as_bytes());
            }
            FileResult::ExportAll(path) => {
                let entries: Vec<(String, Vec<u8>)> =
                    self.tunnels.iter().map(|t| (format!("{}.conf", t.name), t.text.clone().into_bytes())).collect();
                match crate::zip::write(&entries, crate::clock::now_local()) {
                    Ok(z) => {
                        self.write_private(&path, &z);
                    }
                    Err(e) => self.error(e.to_string()),
                }
            }
            FileResult::SaveLog(path) => {
                let text = self.log.join("\n") + "\n";
                self.write_private(&path, text.as_bytes());
            }
        }
    }

    // ---------- editor ----------

    fn save_editor(&mut self) {
        let Some(Dialog::Editor { original, name, text, error }) = &mut self.dialog else { return };
        let tr = if self.ru { &RU } else { &EN };
        let name = name.trim().to_string();
        if !config::is_valid_name(&name) {
            *error = Some(tr.invalid_name.into());
            return;
        }
        if original.as_deref() != Some(name.as_str()) && self.store.exists(&name) {
            *error = Some(tr.name_taken.into());
            return;
        }
        if let Err(e) = config::validate(text) {
            *error = Some(e);
            return;
        }
        let text = if text.ends_with('\n') { text.clone() } else { format!("{text}\n") };
        let original = original.clone();
        let was_active = original.as_ref().map(|o| self.active.contains(o)).unwrap_or(false);
        if was_active && original.as_deref() != Some(name.as_str()) {
            if let Some(Dialog::Editor { error, .. }) = &mut self.dialog {
                *error = Some(tr.active_rename.into());
            }
            return;
        }
        let old_text = original.as_ref().and_then(|o| self.tunnel(o)).map(|t| t.text.clone());
        if let Some(o) = &original
            && o != &name
                && let Err(e) = self.store.rename(o, &name) {
                    return self.error(e.to_string());
                }
        if let Err(e) = self.store.save(&name, &text) {
            return self.error(e.to_string());
        }
        self.dialog = None;
        self.reload();
        self.selected = Some(name.clone());
        if was_active && old_text.as_deref() != Some(text.as_str()) {
            self.info(tr.reconnecting.replace("{}", &name));
            self.send(Request::Down { name: name.clone(), config: old_text });
            let cfg = self.runtime_config(&text);
            self.send(Request::Up { name, config: cfg, boot: self.boot_connect });
        } else {
            self.info(tr.saved);
        }
    }

    fn save_settings(&self) {
        let st = Settings {
            lang: Some(if self.ru { "ru" } else { "en" }.into()),
            theme: Some(theme_str(self.theme).into()),
            version: SETTINGS_VERSION,
            lan_bypass: self.lan_bypass,
            kill_switch: self.kill_switch,
            boot_connect: self.boot_connect,
            check_updates: self.check_updates,
            update_checked_at: self.update_checked_at,
            known_update: self.update.clone(),
            notified_update: self.notified_update.clone(),
        };
        let _ = std::fs::write(
            self.store.dir.parent().unwrap().join("settings.json"),
            serde_json::to_string_pretty(&st).unwrap(),
        );
    }

    fn set_theme(&mut self, ctx: &egui::Context, t: egui::ThemePreference) {
        self.theme = t;
        ctx.set_theme(t);
        self.save_settings();
    }

    /// Logo texture rasterized for the current pixel density (re-made if it changes).
    fn logo_texture(&mut self, ctx: &egui::Context, size: f32) -> egui::TextureHandle {
        let ppp = ctx.pixels_per_point();
        if self.logo.as_ref().map(|(_, p)| *p != ppp).unwrap_or(true) {
            let px = (size * ppp).round().max(16.0) as usize;
            let tex = ctx.load_texture("logo", crate::logo::color_image(px), egui::TextureOptions::LINEAR);
            self.logo = Some((tex, ppp));
        }
        self.logo.as_ref().unwrap().0.clone()
    }

    fn install_shortcut(&mut self) {
        match crate::desktop::install() {
            Ok(()) => self.info(self.tr().shortcut_done),
            Err(e) => self.error(e.to_string()),
        }
    }
}

/// Files copied in a file manager arrive as text: file:// URIs or absolute paths.
fn pasted_paths(text: &str) -> Option<Vec<PathBuf>> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    if lines.is_empty() {
        return None;
    }
    lines
        .iter()
        .map(|l| {
            let p = match l.strip_prefix("file://") {
                Some(rest) => PathBuf::from(percent_decode(rest.trim_start_matches("localhost"))),
                None => PathBuf::from(l),
            };
            (p.is_absolute() && p.is_file()).then_some(p)
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

impl App {
    /// Shortcuts of the Windows client (Ctrl+O, Ctrl+N, Ctrl+E, Delete) plus list
    /// navigation and Ctrl+V import (the way to bring files in on Wayland, where
    /// the window toolkit does not support drag and drop).
    fn keyboard(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        if self.dialog.is_some() || self.tab != Tab::Tunnels {
            return;
        }
        let no_focus = ctx.memory(|m| m.focused().is_none());
        let shortcut = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        if shortcut(Modifiers::COMMAND, Key::O) {
            self.pick_import_files();
        } else if shortcut(Modifiers::COMMAND, Key::N) {
            self.new_empty_tunnel();
        } else if shortcut(Modifiers::COMMAND, Key::E) {
            self.edit_selected();
        } else if no_focus && shortcut(Modifiers::NONE, Key::Delete) {
            self.delete_selected();
        } else if no_focus && (shortcut(Modifiers::NONE, Key::Enter) || shortcut(Modifiers::NONE, Key::Space)) {
            if let Some(n) = self.selected.clone() {
                self.toggle(&n);
            }
        } else if no_focus && shortcut(Modifiers::NONE, Key::ArrowDown) {
            self.move_selection(1);
        } else if no_focus && shortcut(Modifiers::NONE, Key::ArrowUp) {
            self.move_selection(-1);
        }
        if no_focus {
            let pasted = ctx.input(|i| {
                i.events.iter().find_map(|e| if let egui::Event::Paste(t) = e { Some(t.clone()) } else { None })
            });
            if let Some(text) = pasted.filter(|t| !t.trim().is_empty()) {
                match pasted_paths(&text) {
                    Some(paths) => self.import_paths(paths),
                    None => self.dialog = Some(Dialog::Paste { text, error: None }),
                }
            }
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.tunnels.is_empty() {
            return;
        }
        let cur = self.selected.as_ref().and_then(|s| self.tunnels.iter().position(|t| &t.name == s));
        let next = match cur {
            Some(i) => (i as i32 + delta).clamp(0, self.tunnels.len() as i32 - 1) as usize,
            None => 0,
        };
        self.selected = Some(self.tunnels[next].name.clone());
    }

    fn new_empty_tunnel(&mut self) {
        let key = crate::keys::generate_private();
        self.dialog = Some(Dialog::Editor {
            original: None,
            name: String::new(),
            text: format!("[Interface]\nPrivateKey = {key}\n"),
            error: None,
        });
    }

    fn edit_selected(&mut self) {
        if let Some(t) = self.selected.clone().and_then(|s| self.tunnel(&s).cloned()) {
            self.dialog = Some(Dialog::Editor { original: Some(t.name.clone()), name: t.name, text: t.text, error: None });
        }
    }

    fn delete_selected(&mut self) {
        let Some(n) = self.selected.clone() else { return };
        if self.busy.contains_key(&n) {
            self.error(self.tr().busy_wait);
        } else if self.active.contains(&n) {
            self.error(self.tr().active_cant_delete);
        } else {
            self.dialog = Some(Dialog::ConfirmDelete(n));
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick();
        self.follow_system_theme(ctx);
        if self.quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.show_requested {
            self.show_requested = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if self.debug.contains("reopen") && !self.debug.contains("reopened") {
            // debug: simulate closing to tray and reopening from it
            self.debug.push_str(",reopened");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.keyboard(ctx);
        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if !dropped.is_empty() {
            self.import_paths(dropped);
        }
        ctx.request_repaint_after(Duration::from_millis(1000));
        if let Some((path, frames)) = &mut self.screenshot {
            *frames += 1;
            if *frames == 8 {
                self.quit = true;
                let path = path.clone();
                let c = ctx.clone();
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
                    move |img| {
                        if let Err(e) = crate::desktop::write_png(&path, &img) {
                            eprintln!("screenshot: {e}");
                        }
                        c.send_viewport_cmd(egui::ViewportCommand::Close);
                    },
                )));
            }
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let tr = self.tr();

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let logo = self.logo_texture(&ctx, 30.0);
                ui.add(egui::Image::new(&logo).fit_to_exact_size(Vec2::splat(30.0)));
                ui.label(RichText::new("AmneziaWG").heading().strong());
                ui.add_space(14.0);
                ui.selectable_value(&mut self.tab, Tab::Tunnels, tr.tunnels);
                ui.selectable_value(&mut self.tab, Tab::Log, tr.log);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.menu_button("☰", |ui| {
                        if ui.button(tr.add_to_menu).clicked() {
                            self.install_shortcut();
                            ui.close();
                        }
                        let mut auto = self.autostart;
                        if ui.checkbox(&mut auto, tr.autostart).changed() {
                            self.set_autostart(auto);
                        }
                        let mut lan = self.lan_bypass;
                        if ui.checkbox(&mut lan, tr.lan_bypass).on_hover_text(tr.lan_bypass_hint).changed() {
                            self.set_lan_bypass(lan);
                        }
                        let mut ks = self.kill_switch;
                        if ui.checkbox(&mut ks, tr.kill_switch).on_hover_text(tr.kill_switch_hint).changed() {
                            self.set_kill_switch(ks);
                        }
                        let mut boot = self.boot_connect;
                        if ui.checkbox(&mut boot, tr.boot_connect).on_hover_text(tr.boot_connect_hint).changed() {
                            self.set_boot_connect(boot);
                        }
                        let mut upd = self.check_updates;
                        if ui.checkbox(&mut upd, tr.check_updates).on_hover_text(tr.check_updates_hint).changed() {
                            self.set_check_updates(upd);
                        }
                        ui.separator();
                        if ui.add_enabled(!self.tunnels.is_empty(), egui::Button::new(tr.export_all)).clicked() {
                            self.pick_export_all();
                            ui.close();
                        }
                        ui.separator();
                        ui.label(RichText::new(tr.theme).weak());
                        for (t, label) in [
                            (egui::ThemePreference::System, tr.theme_system),
                            (egui::ThemePreference::Light, tr.theme_light),
                            (egui::ThemePreference::Dark, tr.theme_dark),
                        ] {
                            if ui.radio(self.theme == t, label).clicked() {
                                self.set_theme(&ctx, t);
                            }
                        }
                        ui.separator();
                        ui.label(RichText::new(tr.language).weak());
                        if ui.radio(self.ru, "Русский").clicked() {
                            self.ru = true;
                            self.save_settings();
                        }
                        if ui.radio(!self.ru, "English").clicked() {
                            self.ru = false;
                            self.save_settings();
                        }
                        ui.separator();
                        if ui.button(tr.about).clicked() {
                            self.dialog = Some(Dialog::About);
                            ui.close();
                        }
                        if ui.button(tr.quit).clicked() {
                            self.quit = true;
                            ui.close();
                        }
                        ui.label(RichText::new(format!("awg-gui {}", env!("CARGO_PKG_VERSION"))).weak());
                    });
                    // Quick light/dark toggle.
                    let dark = ui.visuals().dark_mode;
                    let (icon, hint, next) = if dark {
                        ("☀", tr.theme_light, egui::ThemePreference::Light)
                    } else {
                        ("🌙", tr.theme_dark, egui::ThemePreference::Dark)
                    };
                    if ui.button(icon).on_hover_text(hint).clicked() {
                        self.set_theme(&ctx, next);
                    }
                    if let Some(u) = self.update.clone() {
                        let text = RichText::new(tr.update_btn.replace("{}", &u.version)).color(Color32::WHITE);
                        let btn = egui::Button::new(text).fill(ui.visuals().selection.bg_fill);
                        if ui.add(btn).on_hover_text(tr.update_hint).clicked() {
                            crate::desktop::open_url(&u.url);
                        }
                    }
                });
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("statusbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                let toast = self.toast.as_ref().filter(|(_, _, t)| t.elapsed() < Duration::from_secs(8));
                if let Some((msg, is_err, _)) = toast {
                    let c = if *is_err { RED } else { ui.visuals().text_color() };
                    ui.label(RichText::new(msg).color(c));
                } else {
                    let (b, c) = match self.backend {
                        Backend::Kernel => (tr.backend_kernel, GREY),
                        Backend::SystemGo => (tr.backend_system_go, GREY),
                        Backend::Embedded => (tr.backend_embedded, GREY),
                        Backend::None => (tr.backend_none, RED),
                    };
                    ui.label(RichText::new(format!("{}: {b}", tr.backend)).color(c).small());
                }
            });
        });

        match self.tab {
            Tab::Tunnels => self.tunnels_tab(ui),
            Tab::Log => self.log_tab(ui),
        }
        self.dialogs(&ctx);
    }
}

impl App {
    fn tunnels_tab(&mut self, ui: &mut egui::Ui) {
        let tr = self.tr();
        egui::Panel::left("list").resizable(true).default_size(270.0).min_size(250.0).show(ui, |ui| {
            egui::Panel::bottom("list_buttons").show_separator_line(false).show(ui, |ui| {
                ui.add_space(4.0);
                // "Add tunnel" spans the full width of the two buttons below it.
                let w = ui.available_width();
                let half = (w - ui.spacing().item_spacing.x) / 2.0;
                let h = 30.0;
                let add = ui.add_sized([w, h], egui::Button::new(tr.add_tunnel));
                egui::Popup::menu(&add).show(|ui| {
                    ui.set_min_width(w);
                    if ui.add(egui::Button::new(tr.import_file).shortcut_text("Ctrl+O")).clicked() {
                        self.pick_import_files();
                        ui.close();
                    }
                    if ui.add(egui::Button::new(tr.import_text).shortcut_text("Ctrl+V")).clicked() {
                        self.dialog = Some(Dialog::Paste { text: String::new(), error: None });
                        ui.close();
                    }
                    if ui.add(egui::Button::new(tr.new_empty).shortcut_text("Ctrl+N")).clicked() {
                        self.new_empty_tunnel();
                        ui.close();
                    }
                });
                ui.horizontal(|ui| {
                    let sel = self.selected.clone();
                    if ui.add_enabled_ui(sel.is_some(), |ui| ui.add_sized([half, h], egui::Button::new(tr.delete))).inner.clicked() {
                        self.delete_selected();
                    }
                    if ui.add_enabled_ui(sel.is_some(), |ui| ui.add_sized([half, h], egui::Button::new(tr.export))).inner.clicked() {
                        self.pick_export_file(sel.unwrap());
                    }
                });
                ui.add_space(4.0);
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(4.0);
                let names: Vec<String> = self.tunnels.iter().map(|t| t.name.clone()).collect();
                for name in names {
                    let selected = self.selected.as_deref() == Some(name.as_str());
                    let color = self.dot_color(&name);
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
                    let v = ui.visuals();
                    if selected {
                        ui.painter().rect_filled(rect, 6.0, v.selection.bg_fill);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, 6.0, v.widgets.hovered.weak_bg_fill);
                    }
                    let c = rect.left_center() + Vec2::new(16.0, 0.0);
                    ui.painter().circle_filled(c, 5.5, color);
                    let text_color = if selected { v.selection.stroke.color } else { v.text_color() };
                    ui.painter().text(
                        c + Vec2::new(14.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        &name,
                        FontId::proportional(15.0),
                        text_color,
                    );
                    if resp.clicked() || resp.secondary_clicked() {
                        self.selected = Some(name.clone());
                    }
                    if resp.double_clicked() {
                        self.toggle(&name);
                    }
                    resp.context_menu(|ui| {
                        let busy = self.busy.contains_key(&name);
                        let label = if self.active.contains(&name) { tr.deactivate } else { tr.activate };
                        if ui.add_enabled(!busy, egui::Button::new(label).shortcut_text("Enter")).clicked() {
                            self.toggle(&name);
                            ui.close();
                        }
                        ui.separator();
                        if ui.add(egui::Button::new(tr.edit).shortcut_text("Ctrl+E")).clicked() {
                            self.edit_selected();
                            ui.close();
                        }
                        if ui.button(tr.export).clicked() {
                            self.pick_export_file(name.clone());
                            ui.close();
                        }
                        if ui.add(egui::Button::new(tr.delete).shortcut_text("Del")).clicked() {
                            self.delete_selected();
                            ui.close();
                        }
                        ui.separator();
                        if ui.add(egui::Button::new(tr.import_file).shortcut_text("Ctrl+O")).clicked() {
                            self.pick_import_files();
                            ui.close();
                        }
                        if ui.add(egui::Button::new(tr.new_empty).shortcut_text("Ctrl+N")).clicked() {
                            self.new_empty_tunnel();
                            ui.close();
                        }
                        if ui.button(tr.export_all).clicked() {
                            self.pick_export_all();
                            ui.close();
                        }
                    });
                }
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            if self.blocked {
                self.blocked_banner(ui);
            }
            let sel = self.selected.clone().and_then(|s| self.tunnel(&s).cloned());
            match sel {
                Some(t) => {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        self.details(ui, &t);
                    });
                }
                None => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() / 3.0);
                        ui.label(RichText::new(tr.no_tunnels).heading());
                        ui.add_space(6.0);
                        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
                        ui.label(RichText::new(if wayland { tr.drop_hint_wayland } else { tr.drop_hint }).weak());
                        ui.add_space(12.0);
                        if ui.button(tr.import_file).clicked() {
                            self.pick_import_files();
                        }
                        if ui.button(tr.import_text).clicked() {
                            self.dialog = Some(Dialog::Paste { text: String::new(), error: None });
                        }
                    });
                }
            }
        });
    }

    /// The kill-switch outlived its tunnel: offer to reconnect or to unblock.
    fn blocked_banner(&mut self, ui: &mut egui::Ui) {
        let tr = self.tr();
        egui::Frame::new()
            .fill(warn(ui).gamma_multiply(0.18))
            .stroke(egui::Stroke::new(1.0, warn(ui)))
            .corner_radius(8.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(tr.blocked_banner).strong());
                ui.horizontal(|ui| {
                    let last = self.last_active.clone().or_else(|| self.selected.clone());
                    if let Some(n) = last.filter(|n| self.tunnel(n).is_some())
                        && ui.button(format!("{} «{n}»", tr.reconnect)).clicked()
                    {
                        self.activate(&n);
                    }
                    if ui.button(tr.unblock).clicked() {
                        self.send(Request::Unblock);
                    }
                });
            });
        ui.add_space(8.0);
    }

    fn dot_color(&self, name: &str) -> Color32 {
        if self.busy.contains_key(name) {
            YELLOW
        } else if self.active.contains(name) {
            GREEN
        } else {
            GREY
        }
    }

    fn details(&mut self, ui: &mut egui::Ui, t: &Tunnel) {
        let tr = self.tr();
        let conf = config::parse(&t.text).unwrap_or_default();
        let iface = conf.interface().cloned().unwrap_or_default();
        let active = self.active.contains(&t.name);
        let busy = self.busy.get(&t.name).copied();
        let st = self.status.get(&t.name).filter(|_| active).cloned();

        card(ui, &format!("{}: {}", tr.interface, t.name), |ui| {
            grid(ui, "iface", |ui| {
                row_label(ui, tr.status);
                ui.horizontal(|ui| {
                    let (txt, col) = match busy {
                        Some(Busy::Up) => (tr.activating, warn(ui)),
                        Some(Busy::Down) => (tr.deactivating, warn(ui)),
                        None if active => (tr.active, GREEN),
                        None => (tr.inactive, GREY),
                    };
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 5.5, col);
                    ui.label(txt);
                    if busy.is_some() {
                        ui.spinner();
                    }
                });
                ui.end_row();

                let pk = st.as_ref().map(|s| s.public_key.clone()).or_else(|| conf.public_key());
                if let Some(pk) = pk {
                    row_label(ui, tr.public_key);
                    key_value(ui, &pk, tr);
                    ui.end_row();
                }
                let port = st
                    .as_ref()
                    .map(|s| s.listen_port.to_string())
                    .or_else(|| iface.get("ListenPort").map(str::to_string));
                if let Some(p) = port {
                    row_label(ui, tr.listen_port);
                    ui.label(p);
                    ui.end_row();
                }
                for (label, key) in [(tr.addresses, "Address"), (tr.dns, "DNS")] {
                    let l = iface.list(key);
                    if !l.is_empty() {
                        row_label(ui, label);
                        ui.label(l.join(", "));
                        ui.end_row();
                    }
                }
                if let Some(m) = iface.get("MTU") {
                    row_label(ui, tr.mtu);
                    ui.label(m);
                    ui.end_row();
                }
                if let Some(t) = iface.get("Table") {
                    row_label(ui, tr.table);
                    ui.label(if t.eq_ignore_ascii_case("off") { tr.off } else { t });
                    ui.end_row();
                }
                let scripts = conf.scripts();
                if !scripts.is_empty() {
                    row_label(ui, tr.scripts);
                    let keys: Vec<&str> = scripts.iter().map(|(k, _)| k.as_str()).collect();
                    let list: Vec<String> = scripts.iter().map(|(k, v)| format!("{k} = {v}")).collect();
                    ui.label(RichText::new(keys.join(", ")).color(warn(ui)))
                        .on_hover_text(format!("{}\n\n{}", tr.scripts_hint, list.join("\n")));
                    ui.end_row();
                }
                if conf.uses_policy_routing() {
                    row_label(ui, tr.local_network);
                    let txt = match (self.lan_bypass, self.kill_switch_on()) {
                        (true, _) => tr.lan_direct,
                        (false, true) => tr.lan_blocked,
                        (false, false) => tr.lan_via_vpn,
                    };
                    ui.label(txt).on_hover_text(tr.lan_bypass_hint);
                    ui.end_row();
                }
                if self.kill_switch {
                    row_label(ui, tr.kill_switch_row);
                    let t = if !nft_available() {
                        RichText::new(tr.ks_no_nft).color(warn(ui))
                    } else if conf.uses_policy_routing() {
                        RichText::new(tr.ks_on)
                    } else {
                        RichText::new(tr.ks_na).weak()
                    };
                    ui.label(t).on_hover_text(tr.kill_switch_hint);
                    ui.end_row();
                }
                row_label(ui, tr.awg_params);
                match conf.awg_version() {
                    Some(v) => {
                        ui.horizontal(|ui| {
                            ui.label(v);
                            let t = if self.awg_expanded { tr.hide_details } else { tr.show_details };
                            if ui.small_button(t).clicked() {
                                self.awg_expanded = !self.awg_expanded;
                            }
                        });
                    }
                    None => {
                        ui.label(RichText::new(tr.plain_wg).weak());
                    }
                }
                ui.end_row();
            });
            if self.awg_expanded && conf.is_awg() {
                ui.add_space(4.0);
                awg_details(ui, &iface, tr);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let (label, up) = if active || busy == Some(Busy::Up) {
                    (tr.deactivate, false)
                } else {
                    (tr.activate, true)
                };
                let btn = egui::Button::new(RichText::new(label).strong()).min_size(Vec2::new(150.0, 30.0));
                if ui.add_enabled(busy.is_none(), btn).clicked() {
                    if up {
                        self.activate(&t.name);
                    } else {
                        self.deactivate(&t.name);
                    }
                }
                if self.helper.is_none() && !active {
                    ui.label(RichText::new(tr.auth_hint).weak().small());
                }
            });
        });

        if active && self.helper.is_none() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr.stats_need_auth).weak());
                if ui.button(tr.show_stats).clicked() {
                    self.last_status = Instant::now() - Duration::from_secs(5);
                    self.send(Request::Status);
                }
            });
        }

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        for (i, p) in conf.peers().enumerate() {
            ui.add_space(8.0);
            let pk = p.get("PublicKey").unwrap_or("").to_string();
            let ps = st.as_ref().and_then(|s| s.peers.iter().find(|x| x.public_key == pk));
            let title = if conf.peers().count() > 1 { format!("{} #{}", tr.peer, i + 1) } else { tr.peer.to_string() };
            card(ui, &title, |ui| {
                grid(ui, ("peer", i), |ui| {
                    row_label(ui, tr.public_key);
                    key_value(ui, &pk, tr);
                    ui.end_row();
                    if p.get("PresharedKey").is_some() {
                        row_label(ui, tr.preshared_key);
                        ui.label(tr.enabled);
                        ui.end_row();
                    }
                    let allowed = p.list("AllowedIPs");
                    if !allowed.is_empty() {
                        row_label(ui, tr.allowed_ips);
                        ui.add(egui::Label::new(allowed.join(", ")).wrap());
                        ui.end_row();
                    }
                    let ep = ps.and_then(|s| s.endpoint.clone()).or_else(|| p.get("Endpoint").map(str::to_string));
                    if let Some(ep) = ep {
                        row_label(ui, tr.endpoint);
                        ui.label(ep);
                        ui.end_row();
                    }
                    if let Some(k) = p.get("PersistentKeepalive").filter(|k| *k != "0" && *k != "off") {
                        row_label(ui, tr.keepalive);
                        ui.label(tr.every_n_sec.replace("{}", k));
                        ui.end_row();
                    }
                    if let Some(ps) = ps {
                        row_label(ui, tr.latest_handshake);
                        if ps.latest_handshake == 0 {
                            ui.label(RichText::new(tr.never).color(warn(ui)));
                        } else {
                            let age = now.saturating_sub(ps.latest_handshake);
                            let col = if age > 180 { warn(ui) } else { ui.visuals().text_color() };
                            ui.label(RichText::new(ago(tr, age)).color(col));
                        }
                        ui.end_row();
                        row_label(ui, tr.transfer);
                        ui.label(
                            tr.transfer_fmt.replacen("{}", &bytes(ps.rx), 1).replacen("{}", &bytes(ps.tx), 1),
                        );
                        ui.end_row();
                    }
                });
            });
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr.trusted_note).weak().small());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(tr.edit).on_hover_text("Ctrl+E").clicked() {
                    self.edit_selected();
                }
            });
        });
    }

    fn log_tab(&mut self, ui: &mut egui::Ui) {
        let tr = self.tr();
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button(tr.copy).clicked() {
                    ui.ctx().copy_text(self.log.join("\n"));
                    self.toast = Some((tr.copied.into(), false, Instant::now()));
                }
                if ui.button(tr.save_log).clicked() {
                    self.pick_save_log();
                }
                if ui.button(tr.clear).clicked() {
                    self.log.clear();
                    crate::logfile::clear();
                }
            });
            ui.separator();
            egui::ScrollArea::both().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                for l in &self.log {
                    let col = if l.contains("ERROR") { RED } else { ui.visuals().text_color() };
                    ui.label(RichText::new(l).monospace().color(col));
                }
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let tr = self.tr();
        let mut close = false;
        let mut save = false;
        let mut do_import: Option<String> = None;
        let mut do_delete: Option<String> = None;
        let mut do_import_confirmed = false;
        let mut do_check_update = false;
        match &mut self.dialog {
            None => {}
            Some(Dialog::Editor { original, name, text, error }) => {
                egui::Modal::new(egui::Id::unique("editor")).show(ctx, |ui| {
                    let screen = ctx.content_rect();
                    let w = (screen.width() - 80.0).clamp(400.0, 900.0);
                    ui.set_width(w);
                    ui.heading(if original.is_some() { tr.edit_tunnel } else { tr.new_tunnel });
                    ui.add_space(6.0);
                    egui::Grid::new("ed").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        ui.label(format!("{}:", tr.name));
                        let ok = config::is_valid_name(name.trim()) || name.is_empty();
                        let mut te = egui::TextEdit::singleline(name).desired_width(220.0).char_limit(15);
                        if !ok {
                            te = te.text_color(RED);
                        }
                        ui.add(te);
                        ui.end_row();
                        ui.label(format!("{}:", tr.public_key));
                        let pk = config::parse(text).ok().and_then(|c| c.public_key()).unwrap_or_default();
                        ui.add(egui::Label::new(RichText::new(pk).monospace()).selectable(true));
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                    let rows = ((screen.height() - 280.0) / 18.0).clamp(8.0, 30.0) as usize;
                    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                        let mut job = highlight(ui, buf.as_str());
                        job.wrap.max_width = wrap;
                        ui.fonts_mut(|f| f.layout_job(job))
                    };
                    egui::ScrollArea::vertical().max_height(rows as f32 * 18.0).show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .code_editor()
                                .desired_rows(rows)
                                .desired_width(f32::INFINITY)
                                .layouter(&mut layouter),
                        );
                    });
                    // checked while typing, like the Windows editor
                    let live = config::validate(text).err();
                    let name_ok = config::is_valid_name(name.trim());
                    if let Some(e) = error.as_ref().or(live.as_ref()) {
                        ui.add_space(4.0);
                        ui.label(RichText::new(format!("{}: {e}", tr.error)).color(RED));
                    }
                    ui.add_space(8.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        let btn = egui::Button::new(RichText::new(tr.save).strong());
                        let r = ui.add_enabled(live.is_none() && name_ok, btn);
                        let r = if name_ok { r } else { r.on_disabled_hover_text(tr.invalid_name) };
                        if r.clicked() || (r.enabled() && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S))) {
                            save = true;
                        }
                        if ui.button(tr.cancel).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            close = true;
                        }
                    });
                });
            }
            Some(Dialog::Paste { text, error }) => {
                egui::Modal::new(egui::Id::unique("paste")).show(ctx, |ui| {
                    ui.set_width((ctx.content_rect().width() - 80.0).clamp(400.0, 760.0));
                    ui.heading(tr.import_title);
                    ui.add_space(4.0);
                    ui.label(tr.paste_hint);
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        let r = ui.add(
                            egui::TextEdit::multiline(text)
                                .code_editor()
                                .desired_rows(12)
                                .desired_width(f32::INFINITY),
                        );
                        if text.is_empty() {
                            r.request_focus();
                        }
                    });
                    if let Some(e) = error {
                        ui.label(RichText::new(format!("{}: {e}", tr.error)).color(RED));
                    }
                    ui.add_space(8.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if ui.add_enabled(!text.trim().is_empty(), egui::Button::new(RichText::new(tr.import_btn).strong())).clicked() {
                            do_import = Some(text.clone());
                        }
                        if ui.button(tr.cancel).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            close = true;
                        }
                    });
                });
            }
            Some(Dialog::ConfirmScripts { scripts, .. }) => {
                egui::Modal::new(egui::Id::unique("scripts")).show(ctx, |ui| {
                    ui.set_width((ctx.content_rect().width() - 80.0).clamp(400.0, 760.0));
                    ui.heading(RichText::new(format!("⚠ {}", tr.scripts_title)).color(warn(ui)));
                    ui.add_space(4.0);
                    ui.label(tr.scripts_confirm);
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                        for l in scripts.iter() {
                            ui.add(egui::Label::new(RichText::new(l).monospace()).wrap());
                        }
                    });
                    ui.add_space(10.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if ui.button(RichText::new(tr.import_anyway).color(RED)).clicked() {
                            do_import_confirmed = true;
                        }
                        if ui.button(tr.cancel).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            close = true;
                        }
                    });
                });
            }
            Some(Dialog::About) => {
                let logo = self.logo.as_ref().map(|(t, _)| t.clone());
                let update_checking = self.update_rx.is_some();
                let update = self.update.clone();
                egui::Modal::new(egui::Id::unique("about")).show(ctx, |ui| {
                    ui.set_width(440.0);
                    ui.vertical_centered(|ui| {
                        if let Some(l) = logo {
                            ui.add(egui::Image::new(&l).fit_to_exact_size(Vec2::splat(64.0)));
                        }
                        ui.heading("AmneziaWG");
                        ui.label(format!("awg-gui {}", env!("CARGO_PKG_VERSION")));
                        ui.add_space(4.0);
                        ui.label(RichText::new(tr.about_text).weak());
                    });
                    ui.add_space(8.0);
                    grid(ui, "about", |ui| {
                        row_label(ui, tr.version);
                        ui.label(env!("CARGO_PKG_VERSION"));
                        ui.end_row();
                        row_label(ui, tr.backend);
                        ui.label(match detect_backend() {
                            Backend::Kernel => tr.backend_kernel,
                            Backend::SystemGo => tr.backend_system_go,
                            Backend::Embedded => tr.backend_embedded,
                            Backend::None => tr.backend_none,
                        });
                        ui.end_row();
                        if let Some(v) = crate::helper::backend_versions() {
                            row_label(ui, tr.components);
                            ui.label(RichText::new(v).monospace().small());
                            ui.end_row();
                        }
                        row_label(ui, tr.license);
                        ui.label("GPL-2.0-or-later");
                        ui.end_row();
                        row_label(ui, "GitHub");
                        if ui.link("coinman-dev/awg-linux-client").on_hover_text(env!("CARGO_PKG_REPOSITORY")).clicked() {
                            crate::desktop::open_url(env!("CARGO_PKG_REPOSITORY"));
                        }
                        ui.end_row();
                        row_label(ui, tr.updates);
                        ui.horizontal(|ui| {
                            let checking = update_checking;
                            if ui.add_enabled(!checking, egui::Button::new(if checking { tr.checking } else { tr.check_now }))
                                .on_hover_text(tr.check_updates_hint)
                                .clicked()
                            {
                                do_check_update = true;
                            }
                            if let Some(u) = &update
                                && ui.link(tr.update_available.replace("{}", &u.version)).clicked() {
                                    crate::desktop::open_url(&u.url);
                                }
                        });
                        ui.end_row();
                    });
                    ui.add_space(8.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if ui.button(tr.close).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            close = true;
                        }
                    });
                });
            }
            Some(Dialog::ConfirmDelete(name)) => {
                egui::Modal::new(egui::Id::unique("del")).show(ctx, |ui| {
                    ui.set_max_width(420.0);
                    ui.label(tr.delete_confirm.replace("{}", name));
                    ui.add_space(10.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if ui.button(RichText::new(tr.delete).color(RED)).clicked() {
                            do_delete = Some(name.clone());
                        }
                        if ui.button(tr.cancel).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            close = true;
                        }
                    });
                });
            }
        }
        if close {
            self.dialog = None;
        }
        if do_check_update {
            self.start_update_check(true);
        }
        if do_import_confirmed && let Some(Dialog::ConfirmScripts { cands, .. }) = self.dialog.take() {
            self.import_confirmed(cands);
        }
        if save {
            self.save_editor();
        }
        if let Some(text) = do_import {
            match import::from_text(&text, "tunnel") {
                Ok(c) => {
                    let errors: Vec<String> =
                        c.iter().filter_map(|c| config::validate(&c.text).err()).collect();
                    if errors.len() == c.len() {
                        if let Some(Dialog::Paste { error, .. }) = &mut self.dialog {
                            *error = Some(errors.join("; "));
                        }
                    } else {
                        self.dialog = None;
                        self.import_candidates(c);
                    }
                }
                Err(e) => {
                    if let Some(Dialog::Paste { error, .. }) = &mut self.dialog {
                        *error = Some(e);
                    }
                }
            }
        }
        if let Some(name) = do_delete {
            self.dialog = None;
            match self.store.delete(&name) {
                Ok(()) => {
                    self.reload();
                    self.info(tr.deleted.replace("{}", &name));
                }
                Err(e) => self.error(e.to_string()),
            }
        }
    }
}

// ---------- small UI helpers ----------

fn card(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style()).corner_radius(8.0).inner_margin(12.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).heading());
        ui.add_space(6.0);
        body(ui);
    });
}

fn grid(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, body: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([16.0, 6.0]).min_col_width(120.0).show(ui, body);
}

/// AmneziaWG obfuscation parameters, one readable row per group.
fn awg_details(ui: &mut egui::Ui, iface: &config::Section, tr: &Tr) {
    let get = |k: &str| iface.get(k).map(str::to_string);
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(6.0)
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            grid(ui, "awg-details", |ui| {
                if let Some(jc) = get("Jc") {
                    row_label(ui, tr.awg_junk);
                    let size = match (get("Jmin"), get("Jmax")) {
                        (Some(a), Some(b)) => tr.awg_junk_fmt.replacen("{}", &jc, 1).replacen("{}", &format!("{a}–{b}"), 1),
                        _ => jc,
                    };
                    ui.label(size);
                    ui.end_row();
                }
                let s: Vec<String> = ["S1", "S2", "S3", "S4"]
                    .iter()
                    .filter_map(|k| get(k).map(|v| format!("{k} = {v}")))
                    .collect();
                if !s.is_empty() {
                    row_label(ui, tr.awg_padding);
                    ui.label(RichText::new(s.join("    ")).monospace());
                    ui.end_row();
                }
                for (i, k) in ["H1", "H2", "H3", "H4"].iter().enumerate() {
                    if let Some(v) = get(k) {
                        row_label(ui, if i == 0 { tr.awg_headers } else { "" });
                        ui.label(RichText::new(format!("{k} = {}", v.replace('-', " – "))).monospace());
                        ui.end_row();
                    }
                }
                let mut first = true;
                for k in ["I1", "I2", "I3", "I4", "I5"] {
                    if let Some(v) = get(k) {
                        row_label(ui, if first { tr.awg_imitation } else { "" });
                        first = false;
                        let short = if v.chars().count() > 44 {
                            format!("{}…", v.chars().take(44).collect::<String>())
                        } else {
                            v.clone()
                        };
                        let r = ui
                            .add(egui::Label::new(RichText::new(format!("{k} = {short}")).monospace()).sense(Sense::click()))
                            .on_hover_text(format!("{v}\n\n{}", tr.click_to_copy));
                        if r.clicked() {
                            ui.ctx().copy_text(v.clone());
                        }
                        ui.end_row();
                    }
                }
                // AmneziaWG 3.0
                if get("HeaderProtectionKey").is_some() {
                    row_label(ui, tr.awg_header_protection);
                    ui.label(tr.awg_key_set); // a shared secret: not shown
                    ui.end_row();
                }
                if let Some(v) = get("ContentPaddingAddition") {
                    row_label(ui, tr.awg_content_padding);
                    ui.label(RichText::new(v.replace('-', " – ")).monospace());
                    ui.end_row();
                }
                let mut first = true;
                for k in ["RekeyAfterTime", "RekeyTimeout", "RejectAfterTime", "KeepaliveTimeout", "MaxHandshakeAttempts"] {
                    if let Some(v) = get(k) {
                        row_label(ui, if first { tr.awg_timers } else { "" });
                        first = false;
                        ui.label(RichText::new(format!("{k} = {}", v.replace('-', " – "))).monospace());
                        ui.end_row();
                    }
                }
                // AmneziaWG 3.1
                for (k, label) in [("RandomTrailers", tr.awg_random_trailers), ("DisableCookies", tr.awg_disable_cookies)] {
                    if let Some(v) = get(k) {
                        row_label(ui, label);
                        let on = matches!(v.to_ascii_lowercase().as_str(), "on" | "1" | "true" | "yes");
                        ui.label(format!("{} ({k} = {v})", if on { tr.on } else { tr.off }));
                        ui.end_row();
                    }
                }
            });
        });
}

fn row_label(ui: &mut egui::Ui, s: &str) {
    if s.is_empty() {
        ui.label("");
        return;
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.label(RichText::new(format!("{s}:")).weak());
    });
}

fn key_value(ui: &mut egui::Ui, key: &str, tr: &Tr) {
    let r = ui
        .add(egui::Label::new(RichText::new(key).monospace()).sense(Sense::click()))
        .on_hover_text(tr.click_to_copy);
    if r.clicked() {
        ui.ctx().copy_text(key.to_string());
    }
}

fn highlight(ui: &egui::Ui, text: &str) -> LayoutJob {
    let font = FontId::monospace(13.5);
    let dark = ui.visuals().dark_mode;
    let normal = ui.visuals().text_color();
    let section = if dark { Color32::from_rgb(0x6c, 0xb6, 0xff) } else { Color32::from_rgb(0x1a, 0x5f, 0xb4) };
    let key = if dark { Color32::from_rgb(0xe0, 0xa8, 0x5c) } else { Color32::from_rgb(0x9a, 0x5b, 0x00) };
    let comment = GREY;
    let mut job = LayoutJob::default();
    let fmt = |c: Color32| TextFormat { font_id: font.clone(), color: c, ..Default::default() };
    for line in text.split_inclusive('\n') {
        let (code, cmt) = match line.find('#') {
            Some(i) => line.split_at(i),
            None => (line, ""),
        };
        let trimmed = code.trim();
        if trimmed.starts_with('[') {
            job.append(code, 0.0, fmt(section));
        } else if let Some(eq) = code.find('=') {
            job.append(&code[..eq], 0.0, fmt(key));
            job.append(&code[eq..], 0.0, fmt(normal));
        } else {
            job.append(code, 0.0, fmt(normal));
        }
        if !cmt.is_empty() {
            job.append(cmt, 0.0, fmt(comment));
        }
    }
    job
}

fn bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.2} {}", U[i]) }
}

fn ago(tr: &Tr, s: u64) -> String {
    if s < 60 {
        tr.secs_ago.replace("{}", &s.to_string())
    } else if s < 3600 {
        tr.mins_ago.replace("{}", &(s / 60).to_string())
    } else if s < 86400 {
        tr.hours_ago.replace("{}", &(s / 3600).to_string())
    } else {
        tr.days_ago.replace("{}", &(s / 86400).to_string())
    }
}

/// `nft` (package nftables) is installed. Checked on use: it may be installed while
/// the program runs.
fn nft_available() -> bool {
    ["/usr/sbin/nft", "/sbin/nft", "/usr/bin/nft", "/usr/local/sbin/nft"].iter().any(|p| Path::new(p).is_file())
}

fn detect_backend() -> Backend {
    let exists = |p: &str| Path::new(p).exists();
    if exists("/sys/module/amneziawg") {
        return Backend::Kernel;
    }
    let in_path = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .chain(["/usr/sbin", "/usr/bin", "/usr/local/bin", "/usr/local/sbin"])
        .any(|d| Path::new(d).join("amneziawg-go").is_file());
    if in_path {
        return Backend::SystemGo;
    }
    // Module present on disk but not loaded yet?
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let mods = std::fs::read_to_string(format!("/lib/modules/{}/modules.dep", release.trim())).unwrap_or_default();
    if mods.contains("/amneziawg.ko") {
        return Backend::Kernel;
    }
    if crate::helper::has_embedded_backend() { Backend::Embedded } else { Backend::None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_defaults() {
        let fresh = load_settings(None);
        assert!(fresh.lan_bypass && fresh.kill_switch && !fresh.boot_connect && !fresh.check_updates);
        // saved by 0.1.0-beta: no version, lan_bypass written as false
        let old = load_settings(Some(r#"{"lang":"ru","theme":"dark","lan_bypass":false}"#));
        assert!(old.lan_bypass && old.kill_switch);
        assert_eq!(old.lang.as_deref(), Some("ru"));
        // the user's own choice in the current format is kept
        let mine = load_settings(Some(r#"{"version":2,"lan_bypass":false,"kill_switch":false}"#));
        assert!(!mine.lan_bypass && !mine.kill_switch);
        assert!(load_settings(Some("garbage")).kill_switch);
    }
}

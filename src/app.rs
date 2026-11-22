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

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Settings {
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    theme: Option<String>,
    #[serde(default)]
    lan_bypass: bool,
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
}

enum FileResult {
    Import(Vec<PathBuf>),
    Export(String, PathBuf),
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
    awg_expanded: bool,
}

impl App {
    pub fn new(events: Receiver<UiEvent>, waker: Waker, tray: Option<Tray>) -> Self {
        let store = Store::open().expect("cannot create ~/.config/awg-gui");
        let settings: Settings = std::fs::read_to_string(store.dir.parent().unwrap().join("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
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
            log: vec![],
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
            awg_expanded: false,
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
        if dbg.contains("editor") {
            if let Some(t) = app.selected.clone().and_then(|s| app.tunnel(&s).cloned()) {
                app.dialog = Some(Dialog::Editor { original: Some(t.name.clone()), name: t.name, text: t.text, error: None });
            }
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
        self.logo = None;
        self.show_requested = false;
        self.screenshot = std::env::var_os("AWG_GUI_SCREENSHOT").map(|p| (PathBuf::from(p), 0));
        self.waker.set(Some(ctx.clone()));
    }

    /// Called after the window has been closed.
    pub fn detach(&mut self) {
        self.waker.set(None);
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
            match ev {
                UiEvent::Show => self.show_requested = true,
                UiEvent::Quit => self.quit = true,
                UiEvent::Reload => self.reload(),
                UiEvent::Toggle(name) => {
                    if self.active.contains(&name) {
                        self.deactivate(&name);
                    } else {
                        self.activate(&name);
                    }
                }
            }
        }
        self.poll_helper();
        self.poll_files();
        if self.last_scan.elapsed() > Duration::from_millis(1000) {
            self.scan_active();
        }
        self.update_tray();
    }

    /// Block (with a timeout) until something happens while the window is closed.
    pub fn wait(&mut self, timeout: Duration) {
        if let Ok(ev) = self.events.recv_timeout(timeout) {
            // put it back in front of the queue by handling it right away
            match ev {
                UiEvent::Show => self.show_requested = true,
                UiEvent::Quit => self.quit = true,
                UiEvent::Reload => self.reload(),
                UiEvent::Toggle(name) => {
                    if self.active.contains(&name) {
                        self.deactivate(&name);
                    } else {
                        self.activate(&name);
                    }
                }
            }
        }
        self.tick();
    }

    fn update_tray(&mut self) {
        let state = TrayState {
            ru: self.ru,
            tunnels: self
                .tunnels
                .iter()
                .map(|t| (t.name.clone(), self.active.contains(&t.name), self.busy.contains_key(&t.name)))
                .collect(),
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
        let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 + local_offset();
        let t = t.rem_euclid(86400);
        let (h, m, sec) = (t / 3600, (t / 60) % 60, t % 60);
        for line in s.as_ref().lines().filter(|l| !l.trim().is_empty()) {
            self.log.push(format!("{h:02}:{m:02}:{sec:02}  {line}"));
        }
        if self.log.len() > 5000 {
            self.log.drain(..1000);
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
        self.toast = Some((s, true, Instant::now()));
    }

    fn reload(&mut self) {
        self.tunnels = self.store.list();
        if let Some(s) = &self.selected {
            if !self.tunnels.iter().any(|t| &t.name == s) {
                self.selected = self.tunnels.first().map(|t| t.name.clone());
            }
        }
    }

    fn tunnel(&self, name: &str) -> Option<&Tunnel> {
        self.tunnels.iter().find(|t| t.name == name)
    }

    fn scan_active(&mut self) {
        // debug builds: AWG_GUI_FAKE_ACTIVE=name1,name2 pretends these are up (screenshots)
        let fake = if cfg!(debug_assertions) { std::env::var("AWG_GUI_FAKE_ACTIVE").unwrap_or_default() } else { String::new() };
        self.active = self
            .tunnels
            .iter()
            .filter(|t| Path::new("/sys/class/net").join(&t.name).exists() || fake.split(',').any(|f| f == t.name))
            .map(|t| t.name.clone())
            .collect();
        self.last_scan = Instant::now();
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
        }
        if !self.helper.as_ref().unwrap().send(req) {
            self.helper = None;
            self.busy.clear();
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
                Event::Reply(req, resp) => {
                    if !resp.output.trim().is_empty() {
                        self.logln(&resp.output);
                    }
                    match req {
                        Request::Status => {
                            self.status_inflight = false;
                            if let Some(s) = resp.status {
                                self.status = s.into_iter().map(|i| (i.name.clone(), i)).collect();
                            }
                        }
                        Request::Up { name, .. } => {
                            if self.busy.get(&name) == Some(&Busy::Up) {
                                self.busy.remove(&name);
                            }
                            if resp.ok {
                                self.info(format!("{name}: {}", self.tr().active));
                                self.backend = detect_backend();
                            }
                        }
                        Request::Down { name, .. } => {
                            if self.busy.get(&name) == Some(&Busy::Down) {
                                self.busy.remove(&name);
                            }
                            if resp.ok {
                                self.info(format!("{name}: {}", self.tr().inactive));
                            }
                        }
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
        if self.lan_bypass { config::with_lan_bypass(text) } else { text.to_string() }
    }

    fn set_lan_bypass(&mut self, on: bool) {
        self.lan_bypass = on;
        self.save_settings();
        // Re-apply to an active full-tunnel connection.
        let active: Vec<Tunnel> = self
            .tunnels
            .iter()
            .filter(|t| self.active.contains(&t.name))
            .filter(|t| config::parse(&t.text).map(|c| c.is_full_tunnel()).unwrap_or(false))
            .cloned()
            .collect();
        for t in active {
            self.info(self.tr().reconnecting.replace("{}", &t.name));
            self.send(Request::Down { name: t.name.clone(), config: Some(t.text.clone()) });
            let cfg = self.runtime_config(&t.text);
            self.send(Request::Up { name: t.name, config: cfg });
        }
    }

    fn activate(&mut self, name: &str) {
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
        self.send(Request::Up { name: t.name, config: cfg });
    }

    fn deactivate(&mut self, name: &str) {
        let config = self.tunnel(name).map(|t| t.text.clone());
        self.send(Request::Down { name: name.to_string(), config });
    }

    // ---------- import / export ----------

    fn import_candidates(&mut self, cands: Vec<import::Candidate>) {
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
                .add_filter(title, &["conf", "vpn", "txt"])
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

    fn poll_files(&mut self) {
        let Some(rx) = &self.file_rx else { return };
        let Ok(r) = rx.try_recv() else { return };
        self.file_rx = None;
        match r {
            FileResult::Import(paths) => self.import_paths(paths),
            FileResult::Export(name, path) => {
                let Some(t) = self.tunnel(&name) else { return };
                use std::os::unix::fs::OpenOptionsExt;
                let res = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&path)
                    .and_then(|mut f| std::io::Write::write_all(&mut f, t.text.as_bytes()));
                match res {
                    Ok(()) => self.info(self.tr().exported.replace("{}", &path.display().to_string())),
                    Err(e) => self.error(format!("{}: {e}", path.display())),
                }
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
        if let Some(o) = &original {
            if o != &name {
                if let Err(e) = self.store.rename(o, &name) {
                    return self.error(e.to_string());
                }
            }
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
            self.send(Request::Up { name, config: cfg });
        } else {
            self.info(tr.saved);
        }
    }

    fn save_settings(&self) {
        let st = Settings {
            lang: Some(if self.ru { "ru" } else { "en" }.into()),
            theme: Some(theme_str(self.theme).into()),
            lan_bypass: self.lan_bypass,
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

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick();
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
                    if ui.button(tr.import_file).clicked() {
                        self.pick_import_files();
                        ui.close();
                    }
                    if ui.button(tr.import_text).clicked() {
                        self.dialog = Some(Dialog::Paste { text: String::new(), error: None });
                        ui.close();
                    }
                    if ui.button(tr.new_empty).clicked() {
                        let key = crate::keys::generate_private();
                        self.dialog = Some(Dialog::Editor {
                            original: None,
                            name: String::new(),
                            text: format!("[Interface]\nPrivateKey = {key}\n"),
                            error: None,
                        });
                        ui.close();
                    }
                });
                ui.horizontal(|ui| {
                    let sel = self.selected.clone();
                    if ui.add_enabled_ui(sel.is_some(), |ui| ui.add_sized([half, h], egui::Button::new(tr.delete))).inner.clicked() {
                        let n = sel.clone().unwrap();
                        if self.active.contains(&n) {
                            self.error(tr.active_cant_delete);
                        } else {
                            self.dialog = Some(Dialog::ConfirmDelete(n));
                        }
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
                    if resp.clicked() {
                        self.selected = Some(name.clone());
                    }
                    if resp.double_clicked() {
                        if self.active.contains(&name) {
                            self.deactivate(&name);
                        } else {
                            self.activate(&name);
                        }
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
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
                        ui.label(RichText::new(tr.drop_hint).weak());
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
                if conf.is_full_tunnel() {
                    row_label(ui, tr.local_network);
                    let txt = if self.lan_bypass { tr.lan_direct } else { tr.lan_via_vpn };
                    ui.label(txt).on_hover_text(tr.lan_bypass_hint);
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
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            if ui.button(tr.edit).clicked() {
                self.dialog = Some(Dialog::Editor {
                    original: Some(t.name.clone()),
                    name: t.name.clone(),
                    text: t.text.clone(),
                    error: None,
                });
            }
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
                if ui.button(tr.clear).clicked() {
                    self.log.clear();
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
                    let rows = ((screen.height() - 260.0) / 18.0).clamp(8.0, 30.0) as usize;
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
                    if let Some(e) = error {
                        ui.add_space(4.0);
                        ui.label(RichText::new(format!("{}: {e}", tr.error)).color(RED));
                    }
                    ui.add_space(8.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if ui.button(RichText::new(tr.save).strong()).clicked() {
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

/// Local UTC offset in seconds (`date +%z`, evaluated once).
fn local_offset() -> i64 {
    use std::sync::OnceLock;
    static OFF: OnceLock<i64> = OnceLock::new();
    *OFF.get_or_init(|| {
        let out = std::process::Command::new("date").arg("+%z").output().ok();
        let s = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        if s.len() != 5 {
            return 0;
        }
        let sign = if s.starts_with('-') { -1 } else { 1 };
        let h: i64 = s[1..3].parse().unwrap_or(0);
        let m: i64 = s[3..5].parse().unwrap_or(0);
        sign * (h * 3600 + m * 60)
    })
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

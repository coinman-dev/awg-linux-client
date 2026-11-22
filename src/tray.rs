//! System tray icon (StatusNotifierItem / AppIndicator) via ksni.

use crate::waker::{UiEvent, Waker};
use ksni::{
    Icon, MenuItem, ToolTip,
    blocking::{Handle, TrayMethods},
    menu::{CheckmarkItem, StandardItem},
};
use std::sync::mpsc::Sender;

#[derive(Clone, PartialEq, Default)]
pub struct TrayState {
    pub ru: bool,
    /// (name, active, busy)
    pub tunnels: Vec<(String, bool, bool)>,
}

pub struct AwgTray {
    tx: Sender<UiEvent>,
    waker: Waker,
    state: TrayState,
    icon_on: Vec<Icon>,
    icon_off: Vec<Icon>,
}

impl AwgTray {
    fn send(&self, e: UiEvent) {
        let _ = self.tx.send(e);
        self.waker.wake();
    }
}

fn icons(grey: bool) -> Vec<Icon> {
    [22usize, 32, 48, 64]
        .iter()
        .map(|&n| {
            let rgba = crate::logo::rasterize(n);
            let mut argb = Vec::with_capacity(n * n * 4);
            for px in rgba.chunks_exact(4) {
                let (mut r, mut g, mut b) = (px[0], px[1], px[2]);
                if grey {
                    let l = (0.3 * r as f32 + 0.59 * g as f32 + 0.11 * b as f32) as u8;
                    (r, g, b) = (l, l, l);
                }
                argb.extend_from_slice(&[px[3], r, g, b]);
            }
            Icon { width: n as i32, height: n as i32, data: argb }
        })
        .collect()
}

impl ksni::Tray for AwgTray {
    fn id(&self) -> String {
        "awg-gui".into()
    }
    fn title(&self) -> String {
        "AmneziaWG".into()
    }
    fn icon_pixmap(&self) -> Vec<Icon> {
        if self.state.tunnels.iter().any(|t| t.1) { self.icon_on.clone() } else { self.icon_off.clone() }
    }
    fn tool_tip(&self) -> ToolTip {
        let active: Vec<&str> = self.state.tunnels.iter().filter(|t| t.1).map(|t| t.0.as_str()).collect();
        let description = match (active.is_empty(), self.state.ru) {
            (true, true) => "Нет активных туннелей".to_string(),
            (true, false) => "No active tunnels".to_string(),
            (false, true) => format!("Подключено: {}", active.join(", ")),
            (false, false) => format!("Connected: {}", active.join(", ")),
        };
        ToolTip { title: "AmneziaWG".into(), description, ..Default::default() }
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(UiEvent::Show);
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let ru = self.state.ru;
        let mut m: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: if ru { "Открыть AmneziaWG" } else { "Open AmneziaWG" }.into(),
                activate: Box::new(|t: &mut Self| t.send(UiEvent::Show)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ];
        if self.state.tunnels.is_empty() {
            m.push(
                StandardItem {
                    label: if ru { "Нет туннелей" } else { "No tunnels" }.into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        for (name, active, busy) in &self.state.tunnels {
            let n = name.clone();
            m.push(
                CheckmarkItem {
                    label: name.replace('_', "__"),
                    checked: *active,
                    enabled: !*busy,
                    activate: Box::new(move |t: &mut Self| t.send(UiEvent::Toggle(n.clone()))),
                    ..Default::default()
                }
                .into(),
            );
        }
        m.push(MenuItem::Separator);
        m.push(
            StandardItem {
                label: if ru { "Выход" } else { "Quit" }.into(),
                activate: Box::new(|t: &mut Self| t.send(UiEvent::Quit)),
                ..Default::default()
            }
            .into(),
        );
        m
    }
}

pub struct Tray {
    handle: Handle<AwgTray>,
    last: TrayState,
}

impl Tray {
    /// `None` if there is no tray host (e.g. GNOME without the AppIndicator extension).
    pub fn spawn(tx: Sender<UiEvent>, waker: Waker) -> Option<Tray> {
        let t = AwgTray { tx, waker, state: TrayState::default(), icon_on: icons(false), icon_off: icons(true) };
        match t.spawn() {
            Ok(handle) => Some(Tray { handle, last: TrayState::default() }),
            Err(e) => {
                eprintln!("tray unavailable: {e}");
                None
            }
        }
    }

    pub fn set(&mut self, state: TrayState) {
        if state != self.last {
            self.last = state.clone();
            self.handle.update(move |t| t.state = state);
        }
    }

    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }
}

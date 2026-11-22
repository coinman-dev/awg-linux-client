//! Light/dark preference of the desktop from xdg-desktop-portal
//! (org.freedesktop.appearance color-scheme), followed live.
//! winit reports no system theme on Linux, so egui's "System" would always be dark.

use crate::waker::Waker;
use eframe::egui::Theme;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use zbus::{
    blocking::{Connection, Proxy},
    zvariant::{OwnedValue, Value},
};

const UNKNOWN: u8 = 0;
const DARK: u8 = 1;
const LIGHT: u8 = 2;

#[derive(Clone)]
pub struct SystemTheme(Arc<AtomicU8>);

impl SystemTheme {
    pub fn get(&self) -> Option<Theme> {
        match self.0.load(Ordering::Relaxed) {
            DARK => Some(Theme::Dark),
            LIGHT => Some(Theme::Light),
            _ => None,
        }
    }
}

/// Reads the current value synchronously, then watches for changes in a thread.
pub fn spawn(waker: Waker) -> SystemTheme {
    let state = SystemTheme(Arc::new(AtomicU8::new(UNKNOWN)));
    let Ok(conn) = Connection::session() else { return state };
    let Ok(proxy) = portal(&conn) else { return state };
    if let Some(v) = read(&proxy) {
        state.0.store(v, Ordering::Relaxed);
    }
    let st = state.clone();
    std::thread::spawn(move || {
        let Ok(signals) = proxy.receive_signal("SettingChanged") else { return };
        for msg in signals {
            let Ok((ns, key, value)) = msg.body().deserialize::<(String, String, OwnedValue)>() else {
                continue;
            };
            if ns == "org.freedesktop.appearance" && key == "color-scheme" {
                st.0.store(scheme(&value), Ordering::Relaxed);
                waker.wake();
            }
        }
        drop(conn);
    });
    state
}

fn portal(conn: &Connection) -> zbus::Result<Proxy<'static>> {
    Proxy::new(
        conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
    )
}

fn read(proxy: &Proxy) -> Option<u8> {
    let args = ("org.freedesktop.appearance", "color-scheme");
    // ReadOne (portal v2+); Read (older) wraps the value in one more variant.
    let v: OwnedValue = proxy.call("ReadOne", &args).or_else(|_| proxy.call("Read", &args)).ok()?;
    Some(scheme(&v))
}

/// 1 = prefer dark, 2 = prefer light, 0 = no preference (apps default to light).
fn scheme(v: &Value) -> u8 {
    match v {
        Value::U32(1) => DARK,
        Value::U32(_) => LIGHT,
        Value::Value(inner) => scheme(inner),
        _ => UNKNOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schemes() {
        assert_eq!(scheme(&Value::U32(1)), DARK);
        assert_eq!(scheme(&Value::U32(2)), LIGHT);
        assert_eq!(scheme(&Value::U32(0)), LIGHT);
        // old portals: Read returns the value wrapped in another variant
        assert_eq!(scheme(&Value::Value(Box::new(Value::U32(1)))), DARK);
        assert_eq!(scheme(&Value::from("x")), UNKNOWN);
    }
}

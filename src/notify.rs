//! Desktop notifications (org.freedesktop.Notifications), like the Windows client's
//! tray balloons. Sent from a thread; silently ignored without a notification server.

use std::collections::HashMap;
use zbus::{blocking::Connection, zvariant::Value};

pub fn send(summary: String, body: String, urgent: bool) {
    std::thread::spawn(move || {
        let Ok(conn) = Connection::session() else { return };
        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("urgency", Value::U8(if urgent { 2 } else { 1 }));
        hints.insert("desktop-entry", Value::from("awg-gui"));
        let _ = conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &("AmneziaWG", 0u32, "awg-gui", summary.as_str(), body.as_str(), Vec::<&str>::new(), hints, -1i32),
        );
    });
}

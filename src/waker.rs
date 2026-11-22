//! Events from the tray / other instances to the app, and a way to wake the UI.

use eframe::egui;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub enum UiEvent {
    Show,
    Quit,
    Toggle(String),
    Reload,
    /// Open the window with the import file dialog.
    Import,
    /// Open the window with the About dialog.
    About,
}

/// Holds the egui context of the currently open window (if any).
#[derive(Clone, Default)]
pub struct Waker(Arc<Mutex<Option<egui::Context>>>);

impl Waker {
    pub fn set(&self, ctx: Option<egui::Context>) {
        *self.0.lock().unwrap() = ctx;
    }
    pub fn wake(&self) {
        if let Some(c) = self.0.lock().unwrap().as_ref() {
            c.request_repaint();
        }
    }
}

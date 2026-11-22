//! Single instance: a unix socket in $XDG_RUNTIME_DIR. A second launch asks the
//! running one to show its window (or reload tunnels after `--import`) and exits.

use crate::waker::{UiEvent, Waker};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    sync::mpsc::Sender,
};

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("awg-gui.sock")
}

/// Send a command to a running instance. Returns false if none is running.
pub fn notify(cmd: &str) -> bool {
    match UnixStream::connect(socket_path()) {
        Ok(mut s) => writeln!(s, "{cmd}").is_ok(),
        Err(_) => false,
    }
}

/// Become the primary instance and listen for commands.
pub fn listen(tx: Sender<UiEvent>, waker: Waker) -> std::io::Result<()> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            if BufReader::new(stream).read_line(&mut line).is_ok() {
                let ev = match line.trim() {
                    "show" => UiEvent::Show,
                    "reload" => UiEvent::Reload,
                    _ => continue,
                };
                let _ = tx.send(ev);
                waker.wake();
            }
        }
    });
    Ok(())
}

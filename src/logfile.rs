//! The operations log, kept across restarts in
//! $XDG_STATE_HOME/awg-gui/awg-gui.log (~/.local/state/awg-gui/awg-gui.log).

use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::PathBuf};

/// Lines kept in memory / loaded at start; the file is trimmed beyond MAX_BYTES.
pub const KEEP_LINES: usize = 5000;
const MAX_BYTES: u64 = 2 << 20;

pub fn path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
        .join("awg-gui/awg-gui.log")
}

/// Last lines of the log; trims the file when it has grown too big.
pub fn load() -> Vec<String> {
    let p = path();
    let text = fs::read_to_string(&p).unwrap_or_default();
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let keep = lines[lines.len().saturating_sub(KEEP_LINES / 2)..].to_vec();
    if fs::metadata(&p).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = fs::write(&p, keep.join("\n") + "\n");
    }
    keep
}

pub fn append(lines: &[String]) {
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&p) {
        let _ = f.write_all((lines.join("\n") + "\n").as_bytes());
    }
}

pub fn clear() {
    let _ = fs::write(path(), "");
}

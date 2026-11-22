//! Tunnel configs stored per-user in ~/.config/awg-gui/tunnels/<name>.conf (mode 0600).

use crate::config::is_valid_name;
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};

#[derive(Clone, Debug)]
pub struct Tunnel {
    pub name: String,
    pub text: String,
}

pub struct Store {
    pub dir: PathBuf,
}

pub fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
}

impl Store {
    pub fn open() -> std::io::Result<Store> {
        let dir = config_home().join("awg-gui").join("tunnels");
        fs::create_dir_all(&dir)?;
        fs::set_permissions(dir.parent().unwrap(), fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        Ok(Store { dir })
    }

    pub fn list(&self) -> Vec<Tunnel> {
        let mut v: Vec<Tunnel> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension()? != "conf" {
                    return None;
                }
                let name = p.file_stem()?.to_str()?.to_string();
                if !is_valid_name(&name) {
                    return None;
                }
                Some(Tunnel { name, text: fs::read_to_string(&p).ok()? })
            })
            .collect();
        v.sort_by_key(|t| t.name.to_lowercase());
        v
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.conf"))
    }

    pub fn exists(&self, name: &str) -> bool {
        self.path(name).exists()
    }

    pub fn save(&self, name: &str, text: &str) -> std::io::Result<()> {
        assert!(is_valid_name(name));
        let tmp = self.dir.join(format!(".{name}.tmp"));
        let _ = fs::remove_file(&tmp);
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, self.path(name))
    }

    pub fn delete(&self, name: &str) -> std::io::Result<()> {
        fs::remove_file(self.path(name))
    }

    pub fn rename(&self, old: &str, new: &str) -> std::io::Result<()> {
        fs::rename(self.path(old), self.path(new))
    }

    /// `base` sanitized + numeric suffix until it's free.
    pub fn unique_name(&self, base: &str, taken: &[String]) -> String {
        let base = crate::config::sanitize_name(base);
        let free = |n: &str| !self.exists(n) && !taken.iter().any(|t| t == n);
        if free(&base) {
            return base;
        }
        for i in 2.. {
            let suffix = format!("-{i}");
            let mut b = base.clone();
            b.truncate(15 - suffix.len());
            let cand = format!("{b}{suffix}");
            if free(&cand) {
                return cand;
            }
        }
        unreachable!()
    }
}

//! Desktop integration: .desktop entry + icon, window icon, PNG writer (for screenshots).

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub const ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128">
<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
<stop offset="0" stop-color="#fbb26a"/><stop offset="1" stop-color="#e2582b"/></linearGradient></defs>
<rect x="6" y="6" width="116" height="116" rx="26" fill="url(#g)"/>
<path d="M64 24 L98 36 V62 C98 84 83 99 64 106 C45 99 30 84 30 62 V36 Z" fill="none" stroke="#fff" stroke-width="8" stroke-linejoin="round"/>
<path d="M50 80 L64 46 L78 80 M55 69 H73" fill="none" stroke="#fff" stroke-width="7" stroke-linecap="round" stroke-linejoin="round"/>
</svg>
"##;

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".local/share"))
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn exe_quoted() -> std::io::Result<String> {
    let exe = std::env::current_exe()?;
    Ok(format!("\"{}\"", exe.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"")))
}

/// Contents of the .desktop file. `exec` is already quoted; `extra` is appended to Exec.
pub fn desktop_entry(exec: &str, extra: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=AmneziaWG\nGenericName=VPN client\n\
         Comment=AmneziaWG / WireGuard tunnel manager\nComment[ru]=Управление туннелями AmneziaWG / WireGuard\n\
         Exec={exec}{extra}\nIcon=awg-gui\nTerminal=false\nCategories=Network;Security;\n\
         Keywords=vpn;wireguard;amnezia;awg;\nStartupWMClass=awg-gui\nStartupNotify=true\n"
    )
}

/// User-level menu entry + icon (for running the bare binary without the .deb).
pub fn install() -> std::io::Result<()> {
    let icon_dir = data_home().join("icons/hicolor/scalable/apps");
    fs::create_dir_all(&icon_dir)?;
    fs::write(icon_dir.join("awg-gui.svg"), ICON_SVG)?;
    let apps = data_home().join("applications");
    fs::create_dir_all(&apps)?;
    fs::write(apps.join("awg-gui.desktop"), desktop_entry(&exe_quoted()?, ""))?;
    let _ = std::process::Command::new("update-desktop-database").arg(&apps).output();
    Ok(())
}

/// GNOME/KDE on Wayland match windows to .desktop files by app id; without one the
/// dock shows no icon. Install a user entry if no system-wide one exists (or the
/// binary has moved).
pub fn ensure_installed() {
    let user = data_home().join("applications/awg-gui.desktop");
    if Path::new("/usr/share/applications/awg-gui.desktop").exists() {
        // Installed from the .deb: a stale user entry would shadow the system one.
        if fs::read_to_string(&user).map(|c| c.contains("StartupWMClass=awg-gui")).unwrap_or(false) {
            let _ = fs::remove_file(&user);
            let _ = fs::remove_file(data_home().join("icons/hicolor/scalable/apps/awg-gui.svg"));
        }
        return;
    }
    let want = match exe_quoted() {
        Ok(e) => desktop_entry(&e, ""),
        Err(_) => return,
    };
    if fs::read_to_string(&user).map(|c| c != want).unwrap_or(true) {
        let _ = install();
    }
}

fn autostart_file() -> PathBuf {
    crate::store::config_home().join("autostart/awg-gui.desktop")
}

pub fn autostart_enabled() -> bool {
    autostart_file().exists()
}

pub fn set_autostart(on: bool) -> std::io::Result<()> {
    let f = autostart_file();
    if on {
        fs::create_dir_all(f.parent().unwrap())?;
        fs::write(&f, desktop_entry(&exe_quoted()?, " --minimized"))
    } else {
        match fs::remove_file(&f) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Open a web page in the default browser (egui's own link opening is not built in).
pub fn open_url(url: &str) {
    match std::process::Command::new("xdg-open").arg(url).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("xdg-open: {e}"),
    }
}

/// Keep an enabled autostart entry pointing at the current executable
/// (the binary may have been moved or reinstalled elsewhere).
pub fn refresh_autostart() {
    let f = autostart_file();
    if let (Ok(cur), Ok(exe)) = (fs::read_to_string(&f), exe_quoted()) {
        let want = desktop_entry(&exe, " --minimized");
        if cur != want {
            let _ = fs::write(&f, want);
        }
    }
}

/// Minimal PNG encoder (RGBA8) used for `AWG_GUI_SCREENSHOT`.
pub fn write_png(path: &Path, img: &eframe::egui::ColorImage) -> std::io::Result<()> {
    let [w, h] = img.size;
    let mut raw = Vec::with_capacity((w * 4 + 1) * h);
    for y in 0..h {
        raw.push(0);
        for p in &img.pixels[y * w..(y + 1) * w] {
            raw.extend_from_slice(&p.to_array());
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(&raw)?;
    let idat = z.finish()?;
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend((w as u32).to_be_bytes());
    ihdr.extend((h as u32).to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    for (ty, data) in [(b"IHDR", &ihdr), (b"IDAT", &idat), (b"IEND", &Vec::new())] {
        out.extend((data.len() as u32).to_be_bytes());
        let mut c = ty.to_vec();
        c.extend_from_slice(data);
        out.extend(&c);
        out.extend(crate::zip::crc32(&c).to_be_bytes());
    }
    fs::write(path, out)
}

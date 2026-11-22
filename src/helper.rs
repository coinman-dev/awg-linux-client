//! Privileged helper: the same executable started as `pkexec awg-gui --helper`.
//! Reads JSON requests from stdin, runs `awg-quick` / `awg`, answers on stdout.
//! Exits when the GUI closes the pipe; tunnels stay up (like Windows services).
//!
//! `awg-gui --boot` (root, from the `awg-gui-boot.service` systemd unit) restores
//! the tunnel that was active when the system went down, like the Windows client's
//! tunnel services do.

use crate::config::{KILL_SWITCH_TABLE, has_kill_switch, is_valid_name};
use crate::proto::{READY, Request, Response, parse_dump};
use std::{
    fs,
    io::{BufRead, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

const RUN_DIR: &str = "/run/awg-gui";
const LIB_DIR: &str = "/var/lib/awg-gui";
/// Embedded tools are unpacked here and put at the START of PATH: they are built
/// together with this program and know every AmneziaWG parameter it shows, while
/// distribution packages may be older. The kernel module is still used when present.
const BIN_DIR: &str = "/var/lib/awg-gui/bin";
/// Config of the tunnel restored at boot (at most one file).
const BOOT_DIR: &str = "/var/lib/awg-gui/boot";
const BOOT_UNIT: &str = "awg-gui-boot.service";
const SYS_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

pub fn embedded() -> [(&'static str, Option<&'static [u8]>); 3] {
    [("amneziawg-go", AWG_GO), ("awg", AWG), ("awg-quick", AWG_QUICK)]
}

pub fn has_embedded_backend() -> bool {
    AWG_GO.is_some()
}

/// Versions of the embedded components (written by packaging/build-backend.sh).
pub fn backend_versions() -> Option<&'static str> {
    BACKEND_VERSIONS
}

fn write_atomic(path: &Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)
}

/// Copy embedded binaries to BIN_DIR when missing or outdated.
fn install_embedded() -> Result<String, String> {
    let mut log = String::new();
    for (name, data) in embedded() {
        let Some(data) = data else { continue };
        let path = Path::new(BIN_DIR).join(name);
        if fs::read(&path).map(|b| b == data).unwrap_or(false) {
            continue;
        }
        fs::create_dir_all(BIN_DIR)
            .and_then(|_| write_atomic(&path, data, 0o755))
            .map_err(|e| format!("cannot install {}: {e}", path.display()))?;
        log.push_str(&format!("[awg-gui] installed embedded {name} to {}\n", path.display()));
    }
    Ok(log)
}

fn is_root() -> bool {
    fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(1) == 0
}

/// Common setup of the root modes. Returns log lines to show in the GUI.
fn init() -> Result<String, String> {
    let path = if has_embedded_backend() { format!("{BIN_DIR}:{SYS_PATH}") } else { format!("{SYS_PATH}:{BIN_DIR}") };
    // SAFETY: called before any thread is started.
    unsafe {
        std::env::set_var("PATH", path);
    }
    let log = match install_embedded() {
        Ok(l) => l,
        Err(e) => format!("[awg-gui] WARNING: {e}\n"),
    };
    fs::create_dir_all(RUN_DIR)
        .and_then(|_| fs::set_permissions(RUN_DIR, fs::Permissions::from_mode(0o700)))
        .map_err(|e| format!("cannot create {RUN_DIR}: {e}"))?;
    Ok(log)
}

pub fn run() -> i32 {
    if !is_root() {
        eprintln!("--helper must be run as root (it is started by the GUI through pkexec)");
        return 1;
    }
    let install_log = match init() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if writeln!(out, "{READY}").and_then(|_| out.flush()).is_err() {
        return 0;
    }
    let mut pending_log = Some(install_log);
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let mut resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle(req),
            Err(e) => err(format!("bad request: {e}"), String::new()),
        };
        if let Some(l) = pending_log.take() {
            resp.output = l + &resp.output;
        }
        let json = serde_json::to_string(&resp).unwrap();
        if writeln!(out, "{json}").and_then(|_| out.flush()).is_err() {
            break;
        }
    }
    0
}

fn err(msg: String, output: String) -> Response {
    Response { ok: false, error: Some(msg), output, ..Default::default() }
}

fn ok(output: String) -> Response {
    Response { ok: true, output, ..Default::default() }
}

fn handle(req: Request) -> Response {
    match req {
        Request::Up { name, config, boot } => {
            let mut r = up(&name, &config);
            if r.ok {
                let res = if boot { set_boot(Some((&name, &config))) } else { set_boot(None) };
                match res {
                    Ok(l) => r.output += &l,
                    Err(e) => r.output += &format!("[awg-gui] WARNING: {e}\n"),
                }
            }
            r
        }
        Request::Down { name, config } => {
            let mut r = down(&name, config.as_deref());
            if r.ok && boot_tunnel().map(|(n, _)| n == name).unwrap_or(false) {
                match set_boot(None) {
                    Ok(l) => r.output += &l,
                    Err(e) => r.output += &format!("[awg-gui] WARNING: {e}\n"),
                }
            }
            r
        }
        Request::Status => match run_cmd(Command::new("awg").args(["show", "all", "dump"])) {
            Ok(o) => {
                let status = parse_dump(&o);
                let blocked = status.is_empty() && kill_switch_present();
                Response { ok: true, status: Some(status), blocked, ..Default::default() }
            }
            Err((e, o)) => err(e, o),
        },
        Request::SetBoot { tunnel } => match set_boot(tunnel.as_ref().map(|(n, c)| (n.as_str(), c.as_str()))) {
            Ok(l) => ok(l),
            Err(e) => err(e, String::new()),
        },
        Request::Unblock => {
            cleanup_hooks();
            if kill_switch_present() {
                err("cannot remove the kill-switch (nft delete table failed)".into(), String::new())
            } else {
                ok("[awg-gui] kill-switch removed\n".into())
            }
        }
    }
}

fn conf_path(name: &str) -> PathBuf {
    Path::new(RUN_DIR).join(format!("{name}.conf"))
}

fn write_conf(name: &str, config: &str) -> std::io::Result<PathBuf> {
    let path = conf_path(name);
    let _ = fs::remove_file(&path);
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    f.write_all(config.as_bytes())?;
    Ok(path)
}

fn kill_switch_present() -> bool {
    which("nft")
        && Command::new("nft")
            .args(["list", "table", "inet", KILL_SWITCH_TABLE])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

/// `ip rule`s of the LAN bypass (config.rs PREF_KEEP / PREF_BYPASS). awg-quick runs
/// PreDown only on a clean `down`, so remove them after failures and crashes too.
fn remove_lan_rules() {
    for fam in ["-4", "-6"] {
        for pref in ["5208", "5209"] {
            for _ in 0..64 {
                let ok = Command::new("ip").args([fam, "rule", "del", "pref", pref]).output();
                if !ok.map(|o| o.status.success()).unwrap_or(false) {
                    break;
                }
            }
        }
    }
}

/// Leftovers of the runtime hooks of a tunnel that is no longer up.
fn cleanup_hooks() {
    remove_kill_switch();
    remove_lan_rules();
    cleanup_stale_awg_quick();
}

/// When no AmneziaWG interface exists, remove what awg-quick leaves behind after a
/// crash (it cleans up only on `down`): its policy rules and `wg-quick-*` nft tables.
fn cleanup_stale_awg_quick() {
    let ifaces = run_cmd(Command::new("awg").args(["show", "interfaces"])).unwrap_or_default();
    if !ifaces.trim().is_empty() {
        return;
    }
    for fam in ["-4", "-6"] {
        let rules = run_cmd(Command::new("ip").args([fam, "rule", "show"])).unwrap_or_default();
        for line in rules.lines() {
            let stale = line.contains("lookup main suppress_prefixlength 0")
                || (line.contains("not from all fwmark") && line.contains("lookup"));
            if let Some(pref) = line.split(':').next().filter(|p| stale && p.trim().parse::<u32>().is_ok()) {
                let _ = Command::new("ip").args([fam, "rule", "del", "pref", pref.trim()]).output();
            }
        }
    }
    if which("nft") {
        let tables = run_cmd(Command::new("nft").args(["list", "tables"])).unwrap_or_default();
        for line in tables.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if let ["table", family @ ("ip" | "ip6"), name] = f.as_slice()
                && name.starts_with("wg-quick-")
            {
                let _ = Command::new("nft").args(["delete", "table", family, name]).output();
            }
        }
    }
}

fn remove_kill_switch() {
    if kill_switch_present() {
        let _ = Command::new("nft").args(["delete", "table", "inet", KILL_SWITCH_TABLE]).output();
    }
}

fn up(name: &str, config: &str) -> Response {
    if !is_valid_name(name) {
        return err(format!("invalid tunnel name: {name}"), String::new());
    }
    if Path::new("/sys/class/net").join(name).exists() {
        return err(format!("interface {name} already exists"), String::new());
    }
    let mut log = String::new();
    if let Err(e) = backend(&mut log) {
        return err(e, log);
    }
    if !which("awg-quick") || !which("awg") {
        return err("awg-quick / awg not found: install amneziawg-tools".into(), log);
    }
    if has_kill_switch(config) && !which("nft") {
        return err("the kill-switch needs nftables: sudo apt install nftables".into(), log);
    }
    // A kill-switch left by a tunnel that died would block resolving the endpoint.
    if kill_switch_present() {
        log.push_str("[awg-gui] removed a stale kill-switch\n");
    }
    cleanup_hooks();
    let path = match write_conf(name, config) {
        Ok(p) => p,
        Err(e) => return err(format!("cannot write config: {e}"), log),
    };
    // amneziawg-go puts its UAPI socket here; make sure it exists.
    let _ = fs::create_dir_all("/var/run/amneziawg");
    let mut cmd = Command::new("awg-quick");
    cmd.arg("up").arg(&path);
    match run_cmd(&mut cmd) {
        Ok(o) => ok(log + &o),
        Err((e, o)) => {
            let _ = fs::remove_file(&path);
            cleanup_hooks();
            err(e, log + &o)
        }
    }
}

fn down(name: &str, fallback: Option<&str>) -> Response {
    if !is_valid_name(name) {
        return err(format!("invalid tunnel name: {name}"), String::new());
    }
    let path = conf_path(name);
    let target = if path.exists() {
        path.clone()
    } else if let Some(cfg) = fallback {
        match write_conf(name, cfg) {
            Ok(p) => p,
            Err(e) => return err(format!("cannot write config: {e}"), String::new()),
        }
    } else {
        PathBuf::from(name) // system tunnel from /etc/amnezia/amneziawg
    };
    match run_cmd(Command::new("awg-quick").arg("down").arg(&target)) {
        Ok(o) => {
            let _ = fs::remove_file(&path);
            cleanup_hooks();
            ok(o)
        }
        Err((e, o)) => {
            // Interface already gone (e.g. amneziawg-go crashed): clean up anyway.
            if !Path::new("/sys/class/net").join(name).exists() {
                let _ = fs::remove_file(&path);
                cleanup_hooks();
                return ok(o + "[awg-gui] the interface was already gone\n");
            }
            err(e, o)
        }
    }
}

// ---------- restore at boot ----------

fn boot_tunnel() -> Option<(String, PathBuf)> {
    fs::read_dir(BOOT_DIR).ok()?.flatten().find_map(|e| {
        let p = e.path();
        let name = p.file_stem()?.to_str()?.to_string();
        (p.extension()? == "conf" && is_valid_name(&name)).then_some((name, p))
    })
}

/// Executable for the systemd unit: root must never run a file the user can replace.
fn boot_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let root_only = |p: &Path| fs::metadata(p).map(|m| m.uid() == 0 && m.mode() & 0o022 == 0).unwrap_or(false);
    let safe = exe.ancestors().all(|p| p.as_os_str().is_empty() || root_only(p));
    if safe {
        return Ok(exe);
    }
    let copy = Path::new(BIN_DIR).join("awg-gui");
    let data = fs::read(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    if fs::read(&copy).map(|b| b != data).unwrap_or(true) {
        fs::create_dir_all(BIN_DIR)
            .and_then(|_| write_atomic(&copy, &data, 0o755))
            .map_err(|e| format!("{}: {e}", copy.display()))?;
    }
    Ok(copy)
}

fn systemctl(args: &[&str]) -> Result<(), String> {
    run_cmd(Command::new("systemctl").args(args)).map(|_| ()).map_err(|(e, _)| e)
}

/// Remember (or forget) the tunnel to restore at boot and enable/disable the unit.
fn set_boot(tunnel: Option<(&str, &str)>) -> Result<String, String> {
    let had = boot_tunnel();
    if let Some((_, p)) = &had {
        let _ = fs::remove_file(p);
    }
    let unit_path = Path::new("/etc/systemd/system").join(BOOT_UNIT);
    let Some((name, config)) = tunnel else {
        if unit_path.exists() {
            let _ = systemctl(&["disable", BOOT_UNIT]);
            let _ = fs::remove_file(&unit_path);
            let _ = systemctl(&["daemon-reload"]);
        }
        return Ok(match had {
            Some((n, _)) => format!("[awg-gui] {n} will not be restored at boot\n"),
            None => String::new(),
        });
    };
    if !is_valid_name(name) {
        return Err(format!("invalid tunnel name: {name}"));
    }
    if !which("systemctl") {
        return Err("restoring at boot needs systemd".into());
    }
    fs::create_dir_all(BOOT_DIR)
        .and_then(|_| fs::set_permissions(LIB_DIR, fs::Permissions::from_mode(0o755)))
        .and_then(|_| fs::set_permissions(BOOT_DIR, fs::Permissions::from_mode(0o700)))
        .and_then(|_| write_atomic(&Path::new(BOOT_DIR).join(format!("{name}.conf")), config.as_bytes(), 0o600))
        .map_err(|e| format!("cannot save the boot tunnel: {e}"))?;
    let exe = boot_exe()?;
    let unit = format!(
        "# Created by awg-gui: restores the AmneziaWG tunnel that was active at shutdown.\n\
         [Unit]\nDescription=AmneziaWG tunnel restored at boot (awg-gui)\n\
         Wants=network-online.target\nAfter=network-online.target nss-lookup.target\n\n\
         [Service]\nType=oneshot\nExecStart=\"{}\" --boot\nTimeoutStartSec=180\n\n\
         [Install]\nWantedBy=multi-user.target\n",
        exe.display()
    );
    if fs::read_to_string(&unit_path).map(|u| u != unit).unwrap_or(true) {
        write_atomic(&unit_path, unit.as_bytes(), 0o644).map_err(|e| format!("{}: {e}", unit_path.display()))?;
        systemctl(&["daemon-reload"])?;
    }
    systemctl(&["enable", BOOT_UNIT])?;
    Ok(if had.map(|(n, _)| n == name).unwrap_or(false) {
        String::new()
    } else {
        format!("[awg-gui] {name} will be restored at boot\n")
    })
}

/// `awg-gui --boot`: bring up the remembered tunnel, retrying while the network comes up.
pub fn boot() -> i32 {
    if !is_root() {
        eprintln!("--boot must be run as root (by {BOOT_UNIT})");
        return 1;
    }
    if let Err(e) = init() {
        eprintln!("{e}");
        return 1;
    }
    let Some((name, path)) = boot_tunnel() else {
        println!("no tunnel to restore");
        return 0;
    };
    let config = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            return 1;
        }
    };
    for attempt in 1..=24 {
        if Path::new("/sys/class/net").join(&name).exists() {
            println!("{name} is already up");
            return 0;
        }
        let r = up(&name, &config);
        print!("{}", r.output);
        if r.ok {
            println!("{name} restored");
            return 0;
        }
        if r.error.as_deref().unwrap_or("").contains("already exists") {
            println!("{name} is already up");
            return 0;
        }
        eprintln!("attempt {attempt}: {}", r.error.unwrap_or_default());
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    1
}

/// Make sure some AmneziaWG implementation is available (kernel module or amneziawg-go).
fn backend(log: &mut String) -> Result<(), String> {
    if Path::new("/sys/module/amneziawg").exists() {
        return Ok(());
    }
    if Command::new("modprobe").arg("amneziawg").output().map(|o| o.status.success()).unwrap_or(false) {
        log.push_str("[awg-gui] loaded kernel module amneziawg\n");
        return Ok(());
    }
    if which("amneziawg-go") {
        return Ok(());
    }
    Err("no AmneziaWG backend: install the amneziawg kernel module (DKMS) or amneziawg-go".into())
}

fn which(bin: &str) -> bool {
    std::env::var("PATH").unwrap_or_default().split(':').any(|d| Path::new(d).join(bin).is_file())
}

fn run_cmd(cmd: &mut Command) -> Result<String, (String, String)> {
    let prog = cmd.get_program().to_string_lossy().to_string();
    // amneziawg-go keeps stdout/stderr open in its daemon only when LOG_LEVEL is set;
    // that would make output() wait forever.
    cmd.env_remove("LOG_LEVEL").stdin(std::process::Stdio::null());
    match cmd.output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stderr).to_string();
            s.push_str(&String::from_utf8_lossy(&o.stdout));
            if o.status.success() {
                Ok(s)
            } else {
                let last = s.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
                Err((format!("{prog} failed ({}): {last}", o.status), s))
            }
        }
        Err(e) => Err((format!("cannot run {prog}: {e}"), String::new())),
    }
}

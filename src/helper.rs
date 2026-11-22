//! Privileged helper: the same executable started as `pkexec awg-gui --helper`.
//! Reads JSON requests from stdin, runs `awg-quick` / `awg`, answers on stdout.
//! Exits when the GUI closes the pipe; tunnels stay up (like Windows services).

use crate::config::is_valid_name;
use crate::proto::{READY, Request, Response, parse_dump};
use std::{
    fs,
    io::{BufRead, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

const RUN_DIR: &str = "/run/awg-gui";
/// Embedded tools are unpacked here; the directory goes to the END of PATH,
/// so system-installed awg / awg-quick / amneziawg-go take precedence.
const BIN_DIR: &str = "/var/lib/awg-gui/bin";
const SYS_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

pub fn embedded() -> [(&'static str, Option<&'static [u8]>); 3] {
    [("amneziawg-go", AWG_GO), ("awg", AWG), ("awg-quick", AWG_QUICK)]
}

pub fn has_embedded_backend() -> bool {
    AWG_GO.is_some()
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
            .and_then(|_| {
                let tmp = path.with_extension("tmp");
                let _ = fs::remove_file(&tmp);
                let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o755).open(&tmp)?;
                f.write_all(data)?;
                drop(f);
                fs::rename(&tmp, &path)
            })
            .map_err(|e| format!("cannot install {}: {e}", path.display()))?;
        log.push_str(&format!("[awg-gui] installed embedded {name} to {}\n", path.display()));
    }
    Ok(log)
}

pub fn run() -> i32 {
    if fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(1) != 0 {
        eprintln!("--helper must be run as root (it is started by the GUI through pkexec)");
        return 1;
    }
    // SAFETY: single-threaded at this point.
    unsafe {
        std::env::set_var("PATH", format!("{SYS_PATH}:{BIN_DIR}"));
    }
    let install_log = match install_embedded() {
        Ok(l) => l,
        Err(e) => format!("[awg-gui] WARNING: {e}\n"),
    };
    if let Err(e) = fs::create_dir_all(RUN_DIR)
        .and_then(|_| fs::set_permissions(RUN_DIR, fs::Permissions::from_mode(0o700)))
    {
        eprintln!("cannot create {RUN_DIR}: {e}");
        return 1;
    }

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
    Response { ok: false, error: Some(msg), output, status: None }
}

fn handle(req: Request) -> Response {
    match req {
        Request::Up { name, config } => up(&name, &config),
        Request::Down { name, config } => down(&name, config.as_deref()),
        Request::Status => match run_cmd(Command::new("awg").args(["show", "all", "dump"])) {
            Ok(o) => Response { ok: true, status: Some(parse_dump(&o)), ..Default::default() },
            Err((e, o)) => err(e, o),
        },
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
    let path = match write_conf(name, config) {
        Ok(p) => p,
        Err(e) => return err(format!("cannot write config: {e}"), log),
    };
    // amneziawg-go puts its UAPI socket here; make sure it exists.
    let _ = fs::create_dir_all("/var/run/amneziawg");
    let mut cmd = Command::new("awg-quick");
    cmd.arg("up").arg(&path);
    match run_cmd(&mut cmd) {
        Ok(o) => Response { ok: true, output: log + &o, ..Default::default() },
        Err((e, o)) => {
            let _ = fs::remove_file(&path);
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
            Response { ok: true, output: o, ..Default::default() }
        }
        Err((e, o)) => {
            // Interface already gone (e.g. amneziawg-go crashed): clean up anyway.
            if !Path::new("/sys/class/net").join(name).exists() {
                let _ = fs::remove_file(&path);
            }
            err(e, o)
        }
    }
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

//! Optional check for a newer release on GitHub (off by default: without it the
//! program talks to nothing but the user's VPN servers). Only reports the release
//! page; installing stays manual (`apt install ./awg-gui_….deb`).
//!
//! The request is made with the system `curl` or `wget`, so no HTTP/TLS stack is
//! built into the program. It carries no personal data: a generic User-Agent
//! with the program version, nothing else.

use serde_json::Value;
use std::process::Command;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Release {
    /// Version without the leading `v`, e.g. "0.2.0".
    pub version: String,
    /// Release page.
    pub url: String,
}

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

fn api_url() -> String {
    let repo = env!("CARGO_PKG_REPOSITORY").trim_end_matches('/');
    let path = repo.strip_prefix("https://github.com/").unwrap_or("coinman-dev/awg-linux-client");
    format!("https://api.github.com/repos/{path}/releases?per_page=30")
}

fn download(url: &str) -> Result<String, String> {
    let ua = format!("awg-gui/{CURRENT}");
    let accept = "Accept: application/vnd.github+json";
    let attempts: [(&str, Vec<&str>); 2] = [
        ("curl", vec!["-fsSL", "--max-time", "20", "--proto", "=https", "-A", &ua, "-H", accept, url]),
        ("wget", vec!["-q", "-O", "-", "--timeout=20", "--https-only", "-U", &ua, "--header", accept, url]),
    ];
    let mut last = "neither curl nor wget is installed".to_string();
    for (prog, args) in attempts {
        match Command::new(prog).args(&args).stdin(std::process::Stdio::null()).output() {
            Ok(o) if o.status.success() => return Ok(String::from_utf8_lossy(&o.stdout).to_string()),
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                last = format!("{prog} failed ({}){}", o.status, if err.is_empty() { String::new() } else { format!(": {err}") });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => last = format!("cannot run {prog}: {e}"),
        }
    }
    Err(last)
}

/// The newest published release that is newer than this build, if any.
/// Prereleases are offered only to users of a prerelease.
pub fn check() -> Result<Option<Release>, String> {
    let body = download(&api_url())?;
    let v: Value = serde_json::from_str(&body).map_err(|e| format!("unexpected GitHub reply: {e}"))?;
    let list = v.as_array().ok_or("unexpected GitHub reply")?;
    Ok(newest(list, CURRENT))
}

fn newest(list: &[Value], current: &str) -> Option<Release> {
    let cur = Version::parse(current)?;
    let with_pre = !cur.pre.is_empty();
    list.iter()
        .filter(|r| !r.get("draft").and_then(Value::as_bool).unwrap_or(false))
        .filter(|r| with_pre || !r.get("prerelease").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|r| {
            let tag = r.get("tag_name")?.as_str()?;
            let ver = Version::parse(tag)?;
            (with_pre || ver.pre.is_empty()).then_some(())?;
            let url = r.get("html_url")?.as_str()?.to_string();
            Some((ver, Release { version: tag.trim_start_matches('v').to_string(), url }))
        })
        .filter(|(v, _)| *v > cur)
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, r)| r)
}

/// Is `version` newer than this build?
pub fn is_newer(version: &str) -> bool {
    match (Version::parse(version), Version::parse(CURRENT)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Semantic version, ordered by the semver rules (1.0.0-beta < 1.0.0-beta.2 < 1.0.0).
#[derive(PartialEq, Eq, Debug)]
struct Version {
    core: [u64; 3],
    pre: Vec<String>,
}

impl Version {
    fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches('v');
        let s = s.split('+').next()?; // build metadata does not count
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, p.split('.').map(str::to_string).collect()),
            None => (s, vec![]),
        };
        let mut it = core.split('.').map(|n| n.parse::<u64>().ok());
        let v = [it.next()??, it.next()??, it.next()??];
        it.next().is_none().then_some(Version { core: v, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        self.core.cmp(&other.core).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Equal,
            (true, false) => Greater,
            (false, true) => Less,
            (false, false) => {
                for (a, b) in self.pre.iter().zip(&other.pre) {
                    let o = match (a.parse::<u64>(), b.parse::<u64>()) {
                        (Ok(x), Ok(y)) => x.cmp(&y),
                        (Ok(_), Err(_)) => Less,
                        (Err(_), Ok(_)) => Greater,
                        (Err(_), Err(_)) => a.cmp(b),
                    };
                    if o != Equal {
                        return o;
                    }
                }
                self.pre.len().cmp(&other.pre.len())
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("0.1.0-beta") < v("0.1.0-beta.2"));
        assert!(v("0.1.0-beta.2") < v("0.1.0-beta.10"));
        assert!(v("0.1.0-beta.10") < v("0.1.0-rc.1"));
        assert!(v("0.1.0-rc.1") < v("0.1.0"));
        assert!(v("v0.1.0") < v("0.1.1"));
        assert!(v("0.9.9") < v("0.10.0"));
        assert_eq!(v("1.2.3+build5"), v("1.2.3"));
        assert!(Version::parse("1.2").is_none() && Version::parse("x").is_none());
    }

    #[test]
    fn pick_newest() {
        let list: Vec<Value> = serde_json::from_str(
            r#"[
            {"tag_name":"v0.3.0","html_url":"u3","draft":true,"prerelease":false},
            {"tag_name":"v0.2.0-beta","html_url":"u2b","draft":false,"prerelease":true},
            {"tag_name":"v0.1.1","html_url":"u11","draft":false,"prerelease":false},
            {"tag_name":"v0.1.0-beta","html_url":"u10b","draft":false,"prerelease":true}
        ]"#,
        )
        .unwrap();
        // prerelease users see prereleases; drafts never
        assert_eq!(newest(&list, "0.1.0-beta").unwrap().url, "u2b");
        // stable users only stable releases
        assert_eq!(newest(&list, "0.1.0").unwrap().url, "u11");
        assert_eq!(newest(&list, "0.1.1"), None);
        assert_eq!(newest(&list, "0.2.0-beta"), None);
    }

    /// Talks to api.github.com: run explicitly with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_github() {
        let r = check().expect("GitHub request");
        println!("current {CURRENT}, newer release: {r:?}");
    }
}

//! Protocol between the GUI (user) and the privileged helper (root, via pkexec):
//! newline-delimited JSON over the helper's stdin/stdout.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "cmd")]
pub enum Request {
    /// `boot`: also restore this tunnel at system boot (otherwise forget any boot tunnel).
    Up {
        name: String,
        config: String,
        #[serde(default)]
        boot: bool,
    },
    /// `config` is used only if the helper no longer has the config the tunnel was started with.
    /// A tunnel deactivated by the user is no longer restored at boot.
    Down { name: String, config: Option<String> },
    Status,
    /// Restore this (active) tunnel at boot, or nothing (`None`).
    SetBoot { tunnel: Option<(String, String)> },
    /// Remove a kill-switch left behind by a tunnel that went down on its own.
    Unblock,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Response {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    /// Combined stdout/stderr of the executed commands.
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub status: Option<Vec<IfaceStatus>>,
    /// (Status) the kill-switch blocks traffic but no tunnel is up.
    #[serde(default)]
    pub blocked: bool,
}

/// First line written by the helper once it is running as root.
pub const READY: &str = "AWG-GUI-HELPER-READY";

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct IfaceStatus {
    pub name: String,
    pub public_key: String,
    pub listen_port: u16,
    pub peers: Vec<PeerStatus>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PeerStatus {
    pub public_key: String,
    pub endpoint: Option<String>,
    pub allowed_ips: Vec<String>,
    /// Unix seconds; 0 = never.
    pub latest_handshake: u64,
    pub rx: u64,
    pub tx: u64,
    pub keepalive: Option<String>,
}

/// Parse `awg show all dump`. The first line of each interface describes the
/// interface itself (its field count varies between AmneziaWG versions), the
/// following lines are peers with a fixed layout.
pub fn parse_dump(text: &str) -> Vec<IfaceStatus> {
    let mut out: Vec<IfaceStatus> = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 4 {
            continue;
        }
        let none = |s: &str| if s == "(none)" || s == "off" { None } else { Some(s.to_string()) };
        if out.last().map(|i| i.name != f[0]).unwrap_or(true) {
            out.push(IfaceStatus {
                name: f[0].to_string(),
                public_key: none(f[2]).unwrap_or_default(),
                listen_port: f[3].parse().unwrap_or(0),
                peers: vec![],
            });
        } else if f.len() >= 9 {
            out.last_mut().unwrap().peers.push(PeerStatus {
                public_key: f[1].to_string(),
                endpoint: none(f[3]),
                allowed_ips: none(f[4])
                    .map(|s| s.split(',').map(str::to_string).collect())
                    .unwrap_or_default(),
                latest_handshake: f[5].parse().unwrap_or(0),
                rx: f[6].parse().unwrap_or(0),
                tx: f[7].parse().unwrap_or(0),
                keepalive: none(f[8]),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn dump() {
        let d = "awg0\tPRIV\tPUB\t51820\t4\t40\t70\t0\t0\t0\t0\t1\t2\t3\t4\t(null)\t(null)\t(null)\t(null)\t(null)\t(none)\t0\t0\t0\t0\t0\t0\toff\toff\toff\n\
                 awg0\tPEER\t(none)\t1.2.3.4:51820\t0.0.0.0/0,::/0\t1700000000\t100\t200\t25\n";
        let s = super::parse_dump(d);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].public_key, "PUB");
        assert_eq!(s[0].listen_port, 51820);
        assert_eq!(s[0].peers[0].allowed_ips.len(), 2);
        assert_eq!(s[0].peers[0].tx, 200);
        assert_eq!(s[0].peers[0].keepalive.as_deref(), Some("25"));
    }
}

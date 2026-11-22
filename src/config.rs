//! Minimal parser for (Amnezia)WireGuard `.conf` files.
//! The text itself is the source of truth (like in the Windows client);
//! parsing is used only for validation and for showing details.

use crate::keys;

#[derive(Clone, Debug, Default)]
pub struct Section {
    pub name: String,
    pub entries: Vec<(String, String)>,
}

impl Section {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// All values of a (possibly repeated) comma-separated key.
    pub fn list(&self, key: &str) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(key))
            .flat_map(|(_, v)| v.split(','))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Conf {
    pub sections: Vec<Section>,
}

/// AmneziaWG obfuscation parameters (shown in the details view).
pub const AWG_KEYS: &[&str] = &[
    "Jc", "Jmin", "Jmax", "S1", "S2", "S3", "S4", "H1", "H2", "H3", "H4", "I1", "I2", "I3", "I4",
    "I5",
];

/// AmneziaWG 3.0 interface parameters.
pub const AWG3_KEYS: &[&str] = &[
    "HeaderProtectionKey",
    "ContentPaddingAddition",
    "RekeyAfterTime",
    "RekeyTimeout",
    "RejectAfterTime",
    "KeepaliveTimeout",
    "MaxHandshakeAttempts",
];
/// AmneziaWG 3.1 interface parameters.
pub const AWG31_KEYS: &[&str] = &["RandomTrailers", "DisableCookies"];

impl Conf {
    pub fn interface(&self) -> Option<&Section> {
        self.sections.iter().find(|s| s.name.eq_ignore_ascii_case("Interface"))
    }

    pub fn peers(&self) -> impl Iterator<Item = &Section> {
        self.sections.iter().filter(|s| s.name.eq_ignore_ascii_case("Peer"))
    }

    pub fn public_key(&self) -> Option<String> {
        keys::public_from_private(self.interface()?.get("PrivateKey")?)
    }

    /// "AmneziaWG 3.1" / "3.0" / "2.0" / "1.5" / "1.0" by the parameters present.
    pub fn awg_version(&self) -> Option<&'static str> {
        let i = self.interface()?;
        if !self.is_awg() {
            return None;
        }
        if AWG31_KEYS.iter().any(|k| i.get(k).is_some()) {
            return Some("AmneziaWG 3.1");
        }
        if AWG3_KEYS.iter().any(|k| i.get(k).is_some()) {
            return Some("AmneziaWG 3.0");
        }
        let ranges = ["H1", "H2", "H3", "H4"].iter().any(|k| i.get(k).map(|v| v.contains('-')).unwrap_or(false));
        let s34 = i.get("S3").is_some() || i.get("S4").is_some();
        let cps = ["I1", "I2", "I3", "I4", "I5"].iter().any(|k| i.get(k).is_some());
        Some(if ranges || s34 {
            "AmneziaWG 2.0"
        } else if cps {
            "AmneziaWG 1.5"
        } else {
            "AmneziaWG 1.0"
        })
    }

    /// Does the config send all traffic through the tunnel (0.0.0.0/0 or ::/0)?
    pub fn is_full_tunnel(&self) -> bool {
        self.peers().any(|p| p.list("AllowedIPs").iter().any(|a| a.ends_with("/0")))
    }

    pub fn is_awg(&self) -> bool {
        self.interface()
            .map(|i| AWG_KEYS.iter().chain(AWG3_KEYS).chain(AWG31_KEYS).any(|k| i.get(k).is_some()))
            .unwrap_or(false)
    }
}

pub fn parse(text: &str) -> Result<Conf, String> {
    let mut conf = Conf::default();
    for (n, raw) in text.lines().enumerate() {
        let line = match raw.find('#') {
            Some(i) => &raw[..i],
            None => raw,
        }
        .trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim();
            if !name.eq_ignore_ascii_case("Interface") && !name.eq_ignore_ascii_case("Peer") {
                return Err(format!("line {}: unknown section [{}]", n + 1, name));
            }
            conf.sections.push(Section { name: name.to_string(), entries: vec![] });
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!("line {}: expected `Key = Value`", n + 1));
        };
        let Some(sec) = conf.sections.last_mut() else {
            return Err(format!("line {}: entry outside of a section", n + 1));
        };
        sec.entries.push((k.trim().to_string(), v.trim().to_string()));
    }
    Ok(conf)
}

/// Parse and check what awg-quick would choke on.
pub fn validate(text: &str) -> Result<Conf, String> {
    let conf = parse(text)?;
    let ifaces = conf.sections.iter().filter(|s| s.name.eq_ignore_ascii_case("Interface")).count();
    if ifaces != 1 {
        return Err("config must contain exactly one [Interface] section".into());
    }
    let iface = conf.interface().unwrap();
    match iface.get("PrivateKey") {
        None => return Err("[Interface] has no PrivateKey".into()),
        Some(k) if keys::decode_key(k).is_none() => return Err("PrivateKey is not a valid key".into()),
        _ => {}
    }
    for (i, p) in conf.peers().enumerate() {
        match p.get("PublicKey") {
            None => return Err(format!("[Peer] #{} has no PublicKey", i + 1)),
            Some(k) if keys::decode_key(k).is_none() => {
                return Err(format!("[Peer] #{}: PublicKey is not a valid key", i + 1));
            }
            _ => {}
        }
        if let Some(k) = p.get("PresharedKey") {
            if keys::decode_key(k).is_none() {
                return Err(format!("[Peer] #{}: PresharedKey is not a valid key", i + 1));
            }
        }
    }
    Ok(conf)
}

/// Private / local destinations that "LAN bypass" sends around the tunnel.
const LAN_V4: &[&str] =
    &["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "169.254.0.0/16", "224.0.0.0/4", "255.255.255.255/32"];
const LAN_V6: &[&str] = &["fc00::/7", "fe80::/10", "ff00::/8"];
/// `ip rule` priorities used by the bypass (lower = checked first; awg-quick uses ~32764).
const PREF_KEEP: u32 = 5208;
const PREF_BYPASS: u32 = 5209;

fn parse_net(s: &str) -> Option<(std::net::IpAddr, u8)> {
    let (a, l) = match s.split_once('/') {
        Some((a, l)) => (a, Some(l)),
        None => (s, None),
    };
    let ip: std::net::IpAddr = a.trim().trim_matches(['[', ']']).parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let len = match l {
        Some(l) => l.trim().parse::<u8>().ok().filter(|l| *l <= max)?,
        None => max,
    };
    Some((ip, len))
}

/// Network address with host bits cleared, e.g. 10.0.0.7/24 -> 10.0.0.0/24.
fn normalize_net(ip: std::net::IpAddr, len: u8) -> String {
    match ip {
        std::net::IpAddr::V4(v) => {
            let m = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            format!("{}/{len}", std::net::Ipv4Addr::from(u32::from(v) & m))
        }
        std::net::IpAddr::V6(v) => {
            let m = if len == 0 { 0 } else { u128::MAX << (128 - len) };
            format!("{}/{len}", std::net::Ipv6Addr::from(u128::from(v) & m))
        }
    }
}

fn in_lan(ip: std::net::IpAddr) -> bool {
    let nets = if ip.is_ipv4() { LAN_V4 } else { LAN_V6 };
    nets.iter().any(|n| {
        let (net, len) = parse_net(n).unwrap();
        normalize_net(ip, len) == normalize_net(net, len)
    })
}

/// Runtime copy of a full-tunnel config where local networks bypass the VPN.
/// Adds PostUp/PreDown hooks with `ip rule`s that run before awg-quick's
/// policy routing: private destinations use the main table, except the tunnel's
/// own addresses, private DNS servers and private AllowedIPs, which stay in it.
/// The saved config is not changed.
pub fn with_lan_bypass(text: &str) -> String {
    let Ok(conf) = parse(text) else { return text.to_string() };
    if !conf.is_full_tunnel() {
        return text.to_string();
    }
    let iface = conf.interface().cloned().unwrap_or_default();
    let mut keep: Vec<String> = vec![];
    let mut candidates = iface.list("Address");
    candidates.extend(iface.list("DNS"));
    for p in conf.peers() {
        candidates.extend(p.list("AllowedIPs"));
    }
    for c in candidates {
        if let Some((ip, len)) = parse_net(&c) {
            let n = normalize_net(ip, len);
            if len > 0 && in_lan(ip) && !keep.contains(&n) {
                keep.push(n);
            }
        }
    }
    let fam = |n: &str| if n.contains(':') { "-6" } else { "-4" };
    let mut up = String::from("f=$(awg show %i fwmark); [ \"$f\" = off ] || { t=$(printf %d \"$f\"); ");
    for n in &keep {
        up += &format!("ip {} rule add to {n} lookup $t pref {PREF_KEEP}; ", fam(n));
    }
    for n in LAN_V4.iter().chain(LAN_V6) {
        up += &format!("ip {} rule add to {n} lookup main pref {PREF_BYPASS}; ", fam(n));
    }
    up += "true; }";
    let mut down = String::new();
    for f in ["-4", "-6"] {
        for pref in [PREF_KEEP, PREF_BYPASS] {
            down += &format!("while ip {f} rule del pref {pref} 2>/dev/null; do :; done; ");
        }
    }
    down += "true";
    let hooks = format!("# awg-gui: local networks bypass the VPN\nPostUp = {up}\nPreDown = {down}\n");
    let mut out = String::new();
    let mut done = false;
    for line in text.split_inclusive('\n') {
        out.push_str(line);
        if !done && line.trim().eq_ignore_ascii_case("[Interface]") {
            if !line.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&hooks);
            done = true;
        }
    }
    out
}

/// Linux interface names: max 15 chars from [a-zA-Z0-9_=+.-] (awg-quick rule).
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 15
        && name != "."
        && name != ".."
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "_=+.-".contains(c))
}

pub fn sanitize_name(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "_=+.-".contains(c) { c } else { '_' })
        .collect();
    let trimmed = out.trim_matches(|c| c == '_' || c == '.').to_string();
    out = if trimmed.is_empty() { "awg".into() } else { trimmed };
    out.truncate(15);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    const SAMPLE: &str = "[Interface]\nPrivateKey = yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=\nAddress = 10.0.0.2/32, fd00::2/128\nDNS = 1.1.1.1\nJc = 4 # junk\nH1 = 1\n\n[Peer]\nPublicKey = xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=\nAllowedIPs = 0.0.0.0/0\nEndpoint = 1.2.3.4:51820\n";

    #[test]
    fn parse_sample() {
        let c = validate(SAMPLE).unwrap();
        let i = c.interface().unwrap();
        assert_eq!(i.list("Address"), vec!["10.0.0.2/32", "fd00::2/128"]);
        assert_eq!(i.get("jc"), Some("4"));
        assert!(c.is_awg());
        assert_eq!(c.peers().count(), 1);
        assert_eq!(
            c.public_key().unwrap(),
            "HIgo9xNzJMWLKASShiTqIybxZ0U3wGLiUeJ1PKf8ykw="
        );
    }

    #[test]
    fn lan_bypass() {
        let t = "[Interface]\nPrivateKey = yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=\nAddress = 10.66.66.2/32, 2a00:7c80:0:1d8:a00::2/128\nDNS = 1.1.1.1, 172.29.172.254\n\n[Peer]\nPublicKey = xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=\nAllowedIPs = 0.0.0.0/0, ::/0\n";
        let out = with_lan_bypass(t);
        validate(&out).unwrap();
        assert!(out.contains("ip -4 rule add to 10.66.66.2/32 lookup $t pref 5208"));
        assert!(out.contains("ip -4 rule add to 172.29.172.254/32 lookup $t pref 5208"));
        assert!(out.contains("ip -4 rule add to 192.168.0.0/16 lookup main pref 5209"));
        assert!(out.contains("ip -6 rule add to fc00::/7 lookup main pref 5209"));
        assert!(!out.contains("to 1.1.1.1"));
        assert!(!out.contains("to 2a00:7c80"), "public IPv6 address is not a LAN address");
        assert!(out.contains("PreDown = while ip -4 rule del pref 5208"));
        // the hooks must be valid bash (awg-quick evals them)
        let iface = parse(&out).unwrap().interface().cloned().unwrap();
        for k in ["PostUp", "PreDown"] {
            let cmd = iface.get(k).unwrap().replace("%i", "awg0");
            let st = std::process::Command::new("bash").args(["-n", "-c", &cmd]).status().unwrap();
            assert!(st.success(), "{k} is not valid bash: {cmd}");
        }
        // not a full tunnel: unchanged
        let split = t.replace("0.0.0.0/0, ::/0", "10.0.0.0/24");
        assert_eq!(with_lan_bypass(&split), split);
        assert_eq!(normalize_net("10.0.0.7".parse().unwrap(), 24), "10.0.0.0/24");
    }

    #[test]
    fn awg_versions() {
        let base = "[Interface]\nPrivateKey = yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=\n";
        assert_eq!(parse(base).unwrap().awg_version(), None);
        assert_eq!(parse(&format!("{base}Jc = 4\nH1 = 5\n")).unwrap().awg_version(), Some("AmneziaWG 1.0"));
        assert_eq!(parse(&format!("{base}Jc = 4\nI1 = <r 2>\n")).unwrap().awg_version(), Some("AmneziaWG 1.5"));
        assert_eq!(parse(&format!("{base}Jc = 4\nH1 = 1-9\n")).unwrap().awg_version(), Some("AmneziaWG 2.0"));
        assert_eq!(parse(&format!("{base}Jc = 4\nRekeyAfterTime = 100-140\n")).unwrap().awg_version(), Some("AmneziaWG 3.0"));
        assert_eq!(parse(&format!("{base}Jc = 4\nHeaderProtectionKey = x\nRandomTrailers = on\n")).unwrap().awg_version(), Some("AmneziaWG 3.1"));
        // 3.x parameters alone also mean AmneziaWG
        assert_eq!(parse(&format!("{base}DisableCookies = on\n")).unwrap().awg_version(), Some("AmneziaWG 3.1"));
    }

    #[test]
    fn names() {
        assert!(is_valid_name("wg0"));
        assert!(!is_valid_name("a very long name!"));
        assert_eq!(sanitize_name("My VPN (Germany).conf"), "My_VPN__Germany");
    }
}

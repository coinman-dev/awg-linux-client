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

    pub fn is_awg(&self) -> bool {
        self.interface()
            .map(|i| AWG_KEYS.iter().any(|k| i.get(k).is_some()))
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
    fn names() {
        assert!(is_valid_name("wg0"));
        assert!(!is_valid_name("a very long name!"));
        assert_eq!(sanitize_name("My VPN (Germany).conf"), "My_VPN__Germany");
    }
}

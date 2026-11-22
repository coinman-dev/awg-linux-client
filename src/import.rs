//! Import of tunnels from `.conf` files, pasted text and AmneziaVPN `vpn://` keys.

use base64::{
    Engine,
    alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use serde_json::Value;
use std::{io::Read, path::Path};

/// A tunnel candidate: suggested name (may be invalid/duplicate) + config text.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name_hint: String,
    pub text: String,
}

pub fn from_file(path: &Path) -> Result<Vec<Candidate>, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if crate::zip::is_zip(&data) {
        return from_zip(&data).map_err(|e| format!("{}: {e}", path.display()));
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    from_text(&decode_text(&data), &stem).map_err(|e| format!("{}: {e}", path.display()))
}

/// Every `.conf` file of a zip archive (e.g. made by "Export all tunnels to zip").
fn from_zip(data: &[u8]) -> Result<Vec<Candidate>, String> {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (name, content) in crate::zip::read(data)? {
        let file = name.rsplit('/').next().unwrap_or(&name);
        let Some(stem) = file.strip_suffix(".conf").or_else(|| file.strip_suffix(".CONF")) else { continue };
        match from_text(&decode_text(&content), stem) {
            Ok(c) => out.extend(c),
            Err(e) => errors.push(format!("{file}: {e}")),
        }
    }
    if out.is_empty() {
        return Err(if errors.is_empty() { "no .conf files in the archive".into() } else { errors.join("; ") });
    }
    Ok(out)
}

/// UTF-8, or UTF-16 with a byte order mark (Windows Notepad).
fn decode_text(data: &[u8]) -> String {
    let utf16 = |be: bool| {
        let units: Vec<u16> = data[2..]
            .as_chunks::<2>().0.iter()
            .map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match data {
        [0xff, 0xfe, ..] => utf16(false),
        [0xfe, 0xff, ..] => utf16(true),
        _ => String::from_utf8_lossy(data).to_string(),
    }
}

/// Accepts a raw .conf, a `vpn://...` key, or AmneziaVPN JSON.
pub fn from_text(text: &str, name_hint: &str) -> Result<Vec<Candidate>, String> {
    let t = text.trim().trim_start_matches('\u{feff}');
    if t.is_empty() {
        return Err("empty input".into());
    }
    if t.to_ascii_lowercase().contains("[interface]") {
        return Ok(vec![Candidate { name_hint: name_hint.to_string(), text: normalize(t) }]);
    }
    if let Some(rest) = t.strip_prefix("vpn://") {
        return decode_amnezia_key(rest, name_hint);
    }
    if t.starts_with('{') {
        let v: Value = serde_json::from_str(t).map_err(|e| format!("invalid JSON: {e}"))?;
        return from_amnezia_json(&v, name_hint);
    }
    // A key without the vpn:// prefix?
    if let Ok(c) = decode_amnezia_key(t, name_hint) {
        return Ok(c);
    }
    Err("not a WireGuard/AmneziaWG config or vpn:// key".into())
}

fn normalize(t: &str) -> String {
    let mut s = t.replace("\r\n", "\n");
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn decode_amnezia_key(data: &str, name_hint: &str) -> Result<Vec<Candidate>, String> {
    let cleaned: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    let cfg = GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    let raw = GeneralPurpose::new(&alphabet::URL_SAFE, cfg)
        .decode(&cleaned)
        .or_else(|_| GeneralPurpose::new(&alphabet::STANDARD, cfg).decode(&cleaned))
        .map_err(|e| format!("invalid vpn:// key (base64): {e}"))?;
    let json_bytes = qt_uncompress(&raw).or_else(|| zlib(&raw)).unwrap_or(raw);
    let v: Value =
        serde_json::from_slice(&json_bytes).map_err(|e| format!("invalid vpn:// key (JSON): {e}"))?;
    from_amnezia_json(&v, name_hint)
}

/// Qt's qCompress format: 4-byte big-endian length + zlib stream.
fn qt_uncompress(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < 6 {
        return None;
    }
    zlib(&raw[4..])
}

fn zlib(raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(raw).read_to_end(&mut out).ok()?;
    Some(out)
}

fn from_amnezia_json(root: &Value, name_hint: &str) -> Result<Vec<Candidate>, String> {
    let dns1 = root.get("dns1").and_then(Value::as_str).unwrap_or("1.1.1.1").to_string();
    let dns2 = root.get("dns2").and_then(Value::as_str).unwrap_or("1.0.0.1").to_string();
    let base_name = ["description", "name", "hostName"]
        .iter()
        .find_map(|k| root.get(*k).and_then(Value::as_str).filter(|s| !s.trim().is_empty()))
        .unwrap_or(name_hint)
        .to_string();

    let mut texts = Vec::new();
    collect_configs(root, &mut texts);
    let mut out: Vec<Candidate> = Vec::new();
    for t in texts {
        let t = t.replace("$PRIMARY_DNS", &dns1).replace("$SECONDARY_DNS", &dns2);
        let t = normalize(t.trim());
        if !out.iter().any(|c| c.text == t) {
            out.push(Candidate { name_hint: base_name.clone(), text: t });
        }
    }
    if out.is_empty() {
        return Err(
            "the key contains no ready AmneziaWG/WireGuard config (maybe it is a server-admin key)"
                .into(),
        );
    }
    Ok(out)
}

/// Walk JSON looking for `last_config` blobs and raw config strings.
fn collect_configs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if k == "last_config"
                    && let Some(c) = val
                        .as_str()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .and_then(|inner| config_from_last_config(&inner))
                {
                    out.push(c);
                    continue;
                }
                collect_configs(val, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_configs(x, out)),
        Value::String(s) if s.contains("[Interface]") => out.push(s.clone()),
        _ => {}
    }
}

fn config_from_last_config(v: &Value) -> Option<String> {
    let mtu = v.get("mtu").and_then(|m| m.as_str().map(str::to_string).or_else(|| m.as_u64().map(|n| n.to_string())));
    if let Some(c) = v.get("config").and_then(Value::as_str).filter(|c| c.contains("[Interface]")) {
        // The ready config may lack the MTU the server chose (it is a separate field).
        let has_mtu = c.lines().any(|l| l.split('=').next().unwrap_or("").trim().eq_ignore_ascii_case("MTU"));
        return Some(match mtu.filter(|m| !has_mtu && m.parse::<u16>().is_ok()) {
            Some(m) => c.replacen("[Interface]", &format!("[Interface]\nMTU = {m}"), 1),
            None => c.to_string(),
        });
    }
    // Build from separate fields (older AmneziaVPN exports).
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let priv_key = s("client_priv_key")?;
    let server_pub = s("server_pub_key")?;
    let host = s("hostName")?;
    let port = s("port").or_else(|| v.get("port").and_then(Value::as_u64).map(|p| p.to_string()))?;
    let mut c = String::from("[Interface]\n");
    c += &format!("PrivateKey = {priv_key}\n");
    if let Some(ip) = s("client_ip") {
        let ip = if ip.contains('/') { ip } else { format!("{ip}/32") };
        c += &format!("Address = {ip}\n");
    }
    c += "DNS = $PRIMARY_DNS, $SECONDARY_DNS\n";
    if let Some(mtu) = mtu {
        c += &format!("MTU = {mtu}\n");
    }
    for (json_key, conf_key) in [
        ("junkPacketCount", "Jc"),
        ("junkPacketMinSize", "Jmin"),
        ("junkPacketMaxSize", "Jmax"),
        ("initPacketJunkSize", "S1"),
        ("responsePacketJunkSize", "S2"),
        ("cookieReplyPacketJunkSize", "S3"),
        ("transportPacketJunkSize", "S4"),
        ("initPacketMagicHeader", "H1"),
        ("responsePacketMagicHeader", "H2"),
        ("underloadPacketMagicHeader", "H3"),
        ("transportPacketMagicHeader", "H4"),
        ("I1", "I1"),
        ("I2", "I2"),
        ("I3", "I3"),
        ("I4", "I4"),
        ("I5", "I5"),
    ] {
        if let Some(val) = s(json_key).filter(|x| !x.is_empty()) {
            c += &format!("{conf_key} = {val}\n");
        }
    }
    c += "\n[Peer]\n";
    c += &format!("PublicKey = {server_pub}\n");
    if let Some(psk) = s("psk_key").filter(|x| !x.is_empty()) {
        c += &format!("PresharedKey = {psk}\n");
    }
    let allowed = v
        .get("allowed_ips")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "0.0.0.0/0, ::/0".into());
    c += &format!("AllowedIPs = {allowed}\n");
    let host = if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host };
    c += &format!("Endpoint = {host}:{port}\n");
    c += &format!(
        "PersistentKeepalive = {}\n",
        s("persistent_keep_alive").unwrap_or_else(|| "25".into())
    );
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use std::io::Write;

    #[test]
    fn vpn_key_roundtrip() {
        let conf = "[Interface]\nPrivateKey = yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=\nDNS = $PRIMARY_DNS, $SECONDARY_DNS\n\n[Peer]\nPublicKey = xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=\n";
        let last = serde_json::json!({ "config": conf, "mtu": "1376" }).to_string();
        let root = serde_json::json!({
            "containers": [{ "awg": { "last_config": last }, "container": "amnezia-awg" }],
            "description": "My Server", "dns1": "9.9.9.9", "dns2": "8.8.8.8"
        })
        .to_string();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(root.as_bytes()).unwrap();
        let mut q = (root.len() as u32).to_be_bytes().to_vec();
        q.extend(z.finish().unwrap());
        let key = format!("vpn://{}", URL_SAFE_NO_PAD.encode(q));
        let c = from_text(&key, "x").unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name_hint, "My Server");
        assert!(c[0].text.contains("DNS = 9.9.9.9, 8.8.8.8"));
        assert!(c[0].text.contains("[Interface]\nMTU = 1376\n"), "{}", c[0].text);
        crate::config::validate(&c[0].text).unwrap();
    }

    #[test]
    fn zip_and_utf16() {
        let conf = "[INTERFACE]\nPrivateKey = yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=\n";
        let z = crate::zip::write(
            &[("x/home.conf".into(), conf.as_bytes().to_vec()), ("readme.txt".into(), b"hi".to_vec())],
            0,
        )
        .unwrap();
        let c = from_zip(&z).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name_hint, "home");
        let mut u16 = vec![0xff, 0xfe];
        u16.extend(conf.encode_utf16().flat_map(|u| u.to_le_bytes()));
        assert_eq!(decode_text(&u16), conf);
    }
}

//! Minimal ZIP support for tunnel archives (like the Windows client's
//! "Export all tunnels to zip" and zip import): deflate or stored entries, no zip64.

use std::io::{Read, Write};

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

pub fn is_zip(data: &[u8]) -> bool {
    data.starts_with(b"PK\x03\x04") || data.starts_with(b"PK\x05\x06")
}

/// DOS date/time of "now" (local time) for the entries.
fn dos_time(unix_local: i64) -> (u16, u16) {
    let (y, mo, d, h, mi, s) = crate::clock::civil(unix_local);
    let date = (((y.clamp(1980, 2107) - 1980) as u16) << 9) | ((mo as u16) << 5) | d as u16;
    let time = ((h as u16) << 11) | ((mi as u16) << 5) | (s as u16 / 2);
    (time, date)
}

/// Build an archive of `(file name, content)` entries.
pub fn write(entries: &[(String, Vec<u8>)], unix_local: i64) -> std::io::Result<Vec<u8>> {
    let (time, date) = dos_time(unix_local);
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(data)?;
        let packed = z.finish()?;
        let crc = crc32(data);
        let offset = out.len() as u32;
        let header = |sig: u32, central: bool| {
            let mut h = Vec::new();
            h.extend(sig.to_le_bytes());
            if central {
                h.extend(20u16.to_le_bytes()); // version made by
            }
            h.extend(20u16.to_le_bytes()); // version needed
            h.extend(0x0800u16.to_le_bytes()); // flags: UTF-8 names
            h.extend(8u16.to_le_bytes()); // deflate
            h.extend(time.to_le_bytes());
            h.extend(date.to_le_bytes());
            h.extend(crc.to_le_bytes());
            h.extend((packed.len() as u32).to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes()); // extra
            if central {
                h.extend(0u16.to_le_bytes()); // comment
                h.extend(0u16.to_le_bytes()); // disk
                h.extend(0u16.to_le_bytes()); // internal attrs
                h.extend((0o100600u32 << 16).to_le_bytes()); // unix mode -rw-------
                h.extend(offset.to_le_bytes());
            }
            h.extend(name.as_bytes());
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend(&packed);
        central.extend(header(0x0201_4b50, true));
    }
    let cd_offset = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]); // disk numbers
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(cd_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    Ok(out)
}

fn u16_at(d: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?))
}
fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?))
}

/// All regular files of an archive as `(name, content)`.
pub fn read(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let bad = || "damaged or unsupported zip archive".to_string();
    // End of central directory: last 22 bytes + up to 64 KiB of comment.
    let min = data.len().saturating_sub(22 + 0xffff);
    let eocd = (min..data.len().saturating_sub(21))
        .rev()
        .find(|&i| u32_at(data, i) == Some(0x0605_4b50))
        .ok_or_else(bad)?;
    let count = u16_at(data, eocd + 10).ok_or_else(bad)? as usize;
    let mut p = u32_at(data, eocd + 16).ok_or_else(bad)? as usize;
    let mut out = Vec::new();
    for _ in 0..count {
        if u32_at(data, p) != Some(0x0201_4b50) {
            return Err(bad());
        }
        let flags = u16_at(data, p + 8).ok_or_else(bad)?;
        let method = u16_at(data, p + 10).ok_or_else(bad)?;
        let packed = u32_at(data, p + 20).ok_or_else(bad)? as usize;
        let size = u32_at(data, p + 24).ok_or_else(bad)? as usize;
        let (nlen, xlen, clen) = (
            u16_at(data, p + 28).ok_or_else(bad)? as usize,
            u16_at(data, p + 30).ok_or_else(bad)? as usize,
            u16_at(data, p + 32).ok_or_else(bad)? as usize,
        );
        let local = u32_at(data, p + 42).ok_or_else(bad)? as usize;
        let name = String::from_utf8_lossy(data.get(p + 46..p + 46 + nlen).ok_or_else(bad)?).to_string();
        p += 46 + nlen + xlen + clen;
        if name.ends_with('/') {
            continue;
        }
        if flags & 1 != 0 {
            return Err(format!("{name}: encrypted zip entries are not supported"));
        }
        if u32_at(data, local) != Some(0x0403_4b50) || size > 1 << 20 {
            return Err(bad());
        }
        let start = local + 30 + u16_at(data, local + 26).ok_or_else(bad)? as usize
            + u16_at(data, local + 28).ok_or_else(bad)? as usize;
        let raw = data.get(start..start + packed).ok_or_else(bad)?;
        let content = match method {
            0 => raw.to_vec(),
            8 => {
                let mut v = Vec::with_capacity(size);
                flate2::read::DeflateDecoder::new(raw).take(1 << 20).read_to_end(&mut v).map_err(|_| bad())?;
                v
            }
            m => return Err(format!("{name}: unsupported compression method {m}")),
        };
        out.push((name, content));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn roundtrip() {
        let entries = vec![("a.conf".to_string(), b"[Interface]\n".repeat(50)), ("b.conf".to_string(), b"x".to_vec())];
        let z = super::write(&entries, 1_790_000_000).unwrap();
        assert!(super::is_zip(&z));
        assert_eq!(super::read(&z).unwrap(), entries);
        // readable by the system unzip, when there is one
        let path = std::env::temp_dir().join(format!("awg-gui-test-{}.zip", std::process::id()));
        std::fs::write(&path, &z).unwrap();
        if let Ok(o) = std::process::Command::new("unzip").arg("-t").arg(&path).output() {
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
        }
        let _ = std::fs::remove_file(&path);
        assert_eq!(super::crc32(b"123456789"), 0xcbf4_3926);
    }
}

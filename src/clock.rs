//! Local wall-clock time without extra dependencies (log stamps, file names, zip entries).

use std::time::{SystemTime, UNIX_EPOCH};

/// Local UTC offset in seconds (`date +%z`, evaluated once).
fn local_offset() -> i64 {
    use std::sync::OnceLock;
    static OFF: OnceLock<i64> = OnceLock::new();
    *OFF.get_or_init(|| {
        let out = std::process::Command::new("date").arg("+%z").output().ok();
        let s = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        if s.len() != 5 {
            return 0;
        }
        let sign = if s.starts_with('-') { -1 } else { 1 };
        let h: i64 = s[1..3].parse().unwrap_or(0);
        let m: i64 = s[3..5].parse().unwrap_or(0);
        sign * (h * 3600 + m * 60)
    })
}

/// Seconds since the epoch, shifted to local time.
pub fn now_local() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) + local_offset()
}

/// (year, month, day, hour, minute, second) of a local timestamp.
pub fn civil(t: i64) -> (i64, u32, u32, u32, u32, u32) {
    let (days, secs) = (t.div_euclid(86400), t.rem_euclid(86400));
    // Howard Hinnant's days-to-civil algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, (secs / 3600) as u32, (secs / 60 % 60) as u32, (secs % 60) as u32)
}

/// "2026-11-22 18:04:05"
pub fn stamp(t: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(t);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// "2026-11-22T180405" (for file names)
pub fn file_stamp(t: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(t);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}{mi:02}{s:02}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates() {
        assert_eq!(super::stamp(0), "1970-01-01 00:00:00");
        assert_eq!(super::stamp(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(super::stamp(1_795_370_645), "2026-11-22 18:04:05");
        assert_eq!(super::file_stamp(1_795_370_645), "2026-11-22T180405");
    }
}

//! Small utility helpers.

pub mod secrets;

/// Write bytes to a file, creating parent dirs.
pub fn write_bytes(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, data)
}

/// Format a duration as a human string like `1.2s` or `350ms`.
pub fn fmt_duration(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms >= 1000 {
        format!("{:.1}s", d.as_secs_f64())
    } else {
        format!("{ms}ms")
    }
}

/// A compact ISO-8601-ish timestamp for run directories.
pub fn timestamp_utc() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Simple UTC breakdown without chrono.
    let (y, mo, d, h, mi, s) = epoch_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// Convert epoch seconds to (year, month, day, hour, minute, second) UTC.
pub fn epoch_to_ymdhms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let days = (secs / 86400) as i64;
    let rem = (secs % 86400) as u32;
    let h = rem / 3600;
    let mi = (rem % 3600) / 60;
    let s = rem % 60;
    let (y, mo, d) = days_to_date(days);
    (y, mo, d, h, mi, s)
}

/// Convert days since 1970-01-01 to (year, month, day). Algorithm from
/// Howard Hinnant's date library (civil_from_days).
fn days_to_date(z: i64) -> (u32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = (y + if m <= 2 { 1 } else { 0 }) as u32;
    (y, m, d)
}

/// Best-effort: list available Korean-capable font families.
pub fn korean_font_available() -> Vec<String> {
    // Run fc-list if available; otherwise fall back to known paths.
    if let Ok(out) = std::process::Command::new("fc-list")
        .args([":lang=ko", "family"])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            let mut fams: Vec<String> = s
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            fams.sort();
            fams.dedup();
            return fams;
        }
    }
    // Fallback: check common font files.
    let mut fams = Vec::new();
    let candidates = [
        "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
        "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
    ];
    for c in candidates {
        if std::path::Path::new(c).exists() {
            fams.push(c.to_string());
        }
    }
    fams
}

/// /dev/shm size in MB (best effort).
pub fn shm_size_mb() -> Option<u64> {
    // statvfs via libc would be ideal; approximate from /proc/mounts + stat.
    let meta = std::fs::metadata("/dev/shm").ok()?;
    let _ = meta;
    // Fall back to df.
    let out = std::process::Command::new("df")
        .args(["-m", "/dev/shm"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|n| n.parse::<u64>().ok())
}

/// Validate a file path is safe to read/write: rejects `..` traversal and
/// absolute escapes outside the intended base.
///
/// ## Why
/// The agent may pass file paths (screenshots, file drops). Without validation
/// a path like `../../etc/passwd` could read or overwrite unintended files.
pub fn validate_path_within(
    base: &std::path::Path,
    target: &str,
) -> Result<std::path::PathBuf, String> {
    let target_path = std::path::Path::new(target);
    // Reject explicit traversal segments.
    for comp in target_path.components() {
        if comp.as_os_str() == ".." {
            return Err(format!("path traversal rejected: '{target}'"));
        }
    }
    // Canonicalize the base and join; ensure the result stays within base.
    let base_canon = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    let joined = base_canon.join(target_path);
    if !joined.starts_with(&base_canon) {
        return Err(format!("path escapes base: '{target}'"));
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_known_date() {
        // 1970-01-01 00:00:00 UTC
        let (y, mo, d, h, mi, s) = epoch_to_ymdhms(0);
        assert_eq!((y, mo, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
        // 2024-01-01 00:00:00 UTC = 1704067200
        let (y, mo, d, _, _, _) = epoch_to_ymdhms(1704067200);
        assert_eq!((y, mo, d), (2024, 1, 1));
    }
}

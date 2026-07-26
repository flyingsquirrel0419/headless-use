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
/// Paths that arrive over the JSON-RPC/MCP surface are chosen by the *agent*,
/// not by the operator who started the process. `trace.start` creates a
/// directory at a caller-supplied path and `replay` reads one, so without a
/// bound a prompt-injected agent could write into or read from anywhere the
/// process user can reach. Every such path is resolved against the working
/// directory via this function (see [`crate::cli::rpc::dispatch`]).
///
/// Paths passed as CLI arguments (`run --screenshot out.png`) are *not*
/// validated: those come from the operator, who already has a shell.
///
/// Relative paths are joined onto `base`; absolute paths are accepted only if
/// they already live inside `base`.
///
/// ## Why the deepest existing ancestor is canonicalized
/// The common case (`trace.start`) names a directory that does *not* exist yet,
/// so canonicalizing the full join fails and tells us nothing. But the missing
/// leaf can sit under a component that *does* exist and *is* a symlink pointing
/// outside `base` — `base/link -> /etc`, target `link/passwd.d`. Resolving only
/// the textual path would accept it. So we walk up to the deepest ancestor that
/// exists, canonicalize *that* (following every symlink on the way), check the
/// result is inside `base`, and then re-attach the not-yet-existing components.
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

    // Walk up until something exists, collecting the components we skipped.
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut cursor = joined;
    let anchor = loop {
        match std::fs::canonicalize(&cursor) {
            Ok(canon) => break canon,
            Err(_) => {
                let Some(name) = cursor.file_name().map(|n| n.to_os_string()) else {
                    // Ran out of ancestors without finding one that exists
                    // (only possible when `base` itself does not exist).
                    return Err(format!("path escapes base: '{target}'"));
                };
                missing.push(name);
                if !cursor.pop() {
                    return Err(format!("path escapes base: '{target}'"));
                }
            }
        }
    };
    // The deepest real directory must itself live inside the base.
    if !anchor.starts_with(&base_canon) {
        return Err(format!("path escapes base: '{target}'"));
    }
    // Re-attach the components that do not exist yet. None may be `..`: that
    // would walk back out of the ancestor we just proved is inside the base.
    let mut resolved = anchor;
    for name in missing.iter().rev() {
        if name == ".." || name == "." {
            return Err(format!("path traversal rejected: '{target}'"));
        }
        resolved.push(name);
    }
    if !resolved.starts_with(&base_canon) {
        return Err(format!("path escapes base: '{target}'"));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_path_rejects_traversal_and_escapes() {
        let base = std::env::temp_dir();
        assert!(validate_path_within(&base, "../etc/passwd").is_err());
        assert!(validate_path_within(&base, "a/../../b").is_err());
        assert!(validate_path_within(&base, "/etc/passwd").is_err());
        // A plain relative path lands inside the base.
        let ok = validate_path_within(&base, "runs/abc").expect("relative path allowed");
        assert!(ok.starts_with(std::fs::canonicalize(&base).unwrap_or(base)));
    }

    /// A uniquely-named scratch directory under the system temp dir. This repo
    /// has no `tempfile` dev-dependency on purpose, so uniqueness comes from
    /// pid + a process-wide counter and cleanup is the test's own job.
    fn unique_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hu-util-{tag}-{}-{n}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn validate_path_accepts_new_and_existing_paths_inside_base() {
        let base = unique_dir("inside");
        // (a) A path that does not exist yet is accepted and stays under base.
        let new_path = validate_path_within(&base, "runs/2026/new-run").expect("new path allowed");
        let base_canon = std::fs::canonicalize(&base).unwrap();
        assert!(new_path.starts_with(&base_canon), "got {new_path:?}");
        assert!(new_path.ends_with("runs/2026/new-run"), "got {new_path:?}");

        // (d) An existing path inside base is still accepted.
        std::fs::create_dir_all(base.join("existing/deep")).unwrap();
        let existing = validate_path_within(&base, "existing/deep").expect("existing path allowed");
        assert_eq!(existing, base_canon.join("existing/deep"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn validate_path_rejects_symlink_escape_for_nonexistent_leaf() {
        let base = unique_dir("symlink-base");
        let outside = unique_dir("symlink-outside");
        // A symlink *inside* base pointing outside it.
        std::os::unix::fs::symlink(&outside, base.join("link")).expect("symlink");

        // (b) The leaf does not exist yet, so the old code canonicalized
        // nothing and accepted the join. It must be rejected.
        let err = validate_path_within(&base, "link/new-run")
            .expect_err("symlinked escape must be rejected");
        assert!(err.contains("escapes base"), "unexpected error: {err}");

        // An existing file behind the same symlink is rejected too.
        std::fs::write(outside.join("secret.txt"), b"x").unwrap();
        assert!(validate_path_within(&base, "link/secret.txt").is_err());

        // The symlink target itself is outside, so even the bare link is out.
        assert!(validate_path_within(&base, "link").is_err());

        let _ = std::fs::remove_file(base.join("link"));
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn validate_path_still_rejects_textual_traversal() {
        let base = unique_dir("textual");
        // (c) Textual `..` is rejected wherever it appears.
        assert!(validate_path_within(&base, "..").is_err());
        assert!(validate_path_within(&base, "../etc/passwd").is_err());
        assert!(validate_path_within(&base, "runs/../../etc").is_err());
        assert!(validate_path_within(&base, "/etc/passwd").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

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

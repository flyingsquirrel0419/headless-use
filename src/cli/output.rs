//! Output helpers: JSON vs human text, exit codes.

use serde::Serialize;
use std::io::{self, Write};

/// Print a serializable value as JSON to stdout.
pub fn print_json<T: Serialize>(value: &T) {
    let s = serde_json::to_string_pretty(value)
        .unwrap_or_else(|e| serde_json::json!({ "error": format!("serialize: {e}") }).to_string());
    let _ = writeln!(io::stdout(), "{s}");
    let _ = io::stdout().flush();
}

/// Print plain text to stdout.
pub fn print_text(s: &str) {
    let _ = writeln!(io::stdout(), "{s}");
    let _ = io::stdout().flush();
}

/// Print an error to stderr with a recovery hint and return the exit code.
pub fn print_error(e: &crate::browser::BrowserError) -> i32 {
    let _ = writeln!(io::stderr(), "error[{}]: {}", e.code(), e);
    let _ = writeln!(io::stderr(), "recovery: {}", e.recovery());
    1
}

/// Print an anyhow error to stderr.
pub fn print_anyhow(e: &anyhow::Error) -> i32 {
    let _ = writeln!(io::stderr(), "error: {e}");
    1
}

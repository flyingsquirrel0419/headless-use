//! JSON-RPC over stdio.
//!
//! ## Why
//! A long-lived `headless-use serve` process keeps the browser alive; the agent
//! (or a tool wrapper) sends JSON-RPC requests over stdin/stdout. This avoids
//! restarting Chrome per command and keeps latency low.
//!
//! ## Framing
//! Each request/response is a single line of JSON (newline-delimited JSON-RPC).
//! Errors carry a `code`, `message`, and `recovery` hint.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Write};

use crate::browser::BrowserError;

/// JSON-RPC request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Request id.
    pub id: i64,
    /// Method name, e.g. `mouse.click`.
    pub method: String,
    /// Parameters.
    #[serde(default)]
    pub params: Value,
    /// Protocol version.
    #[serde(rename = "jsonrpc", default = "default_version")]
    pub version: String,
}

fn default_version() -> String {
    "2.0".into()
}

/// JSON-RPC response (success).
#[derive(Debug, Clone, Serialize)]
pub struct Response {
    /// Echoed request id.
    pub id: i64,
    /// Result.
    pub result: Value,
    /// Protocol version.
    #[serde(rename = "jsonrpc")]
    pub version: String,
}

/// JSON-RPC error response.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorResponse {
    /// Echoed request id.
    pub id: i64,
    /// Error object.
    pub error: ErrorObject,
    /// Protocol version.
    #[serde(rename = "jsonrpc")]
    pub version: String,
}

/// The error payload.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorObject {
    /// Machine-readable code string (e.g. `STALE_REFERENCE`).
    pub code: String,
    /// Human message.
    pub message: String,
    /// Recovery hint.
    pub recovery: String,
}

impl ErrorResponse {
    /// Build from a [`BrowserError`].
    pub fn from_browser(id: i64, e: &BrowserError) -> Self {
        Self {
            id,
            error: ErrorObject {
                code: e.code().to_string(),
                message: e.to_string(),
                recovery: e.recovery(),
            },
            version: "2.0".into(),
        }
    }
}

/// Protocol version string.
pub const PROTOCOL_VERSION: &str = "2.0";

/// Read one JSON-RPC request line from stdin. Returns None on EOF.
pub fn read_request<R: BufRead>(reader: &mut R) -> Option<Result<Request, serde_json::Error>> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(_) => return None,
        };
        if n == 0 {
            return None; // EOF
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        return Some(serde_json::from_str(trimmed));
    }
}

/// Write a response line to stdout.
pub fn write_response<W: Write>(writer: &mut W, resp: &impl Serialize) {
    if let Ok(s) = serde_json::to_string(resp) {
        let _ = writeln!(writer, "{s}");
        let _ = writer.flush();
    }
}

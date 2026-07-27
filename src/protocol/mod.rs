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
    /// headless-use result-schema version (see [`SCHEMA_VERSION`]).
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
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
    /// headless-use result-schema version (see [`SCHEMA_VERSION`]).
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
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
            schema_version: SCHEMA_VERSION,
        }
    }
}

/// Protocol version string.
pub const PROTOCOL_VERSION: &str = "2.0";

/// Version of the headless-use result/error schema carried on every JSON-RPC
/// response (`schemaVersion`), on `observe` results, and (as `formatVersion`)
/// in trace `metadata.json`. Bumped only when an existing field changes
/// meaning or is removed; purely additive changes do not bump it.
pub const SCHEMA_VERSION: u32 = 1;

/// Stream stdin lines from a dedicated OS thread.
///
/// ## Why a thread and not `read_line` in the async loop
/// `stdin().lock().read_line()` blocks. Called directly from an `async fn` it
/// parks a tokio worker thread for the entire time the client is idle — which,
/// for a long-lived `serve` session, is nearly always. It happened to work
/// because the multi-thread runtime had spare workers, which is luck, not
/// design: a runtime sized to the machine can be starved by it.
///
/// A plain `std::thread` (not `spawn_blocking`) is used deliberately: this
/// reader lives for the whole process, and holding a blocking-pool slot forever
/// is the same mistake one layer down. The thread exits when stdin reaches EOF
/// or the receiver is dropped.
pub fn stdin_lines() -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

/// Parse one JSON-RPC request from an already-read line.
///
/// Returns `None` for a blank line, which callers skip.
pub fn parse_request(line: &str) -> Option<Result<Request, serde_json::Error>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(serde_json::from_str(trimmed))
}

/// Write a response line to stdout.
pub fn write_response<W: Write>(writer: &mut W, resp: &impl Serialize) {
    if let Ok(s) = serde_json::to_string(resp) {
        let _ = writeln!(writer, "{s}");
        let _ = writer.flush();
    }
}

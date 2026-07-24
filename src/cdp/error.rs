//! Errors specific to the CDP transport.

use crate::BrowserError;

/// Errors raised by the CDP transport layer. These are mapped up to
/// [`BrowserError`] so callers get a single, structured error type.
#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    /// A WebSocket-level transport failure (connection reset, EOF, etc.).
    #[error("cdp transport error: {0}")]
    Transport(String),

    /// The server replied with a JSON-RPC error object.
    #[error("cdp protocol error in {method}: [{code}] {message}")]
    Protocol {
        /// CDP method that failed, e.g. `Input.dispatchMouseEvent`.
        method: String,
        /// Numeric CDP error code.
        code: i64,
        /// Human-readable CDP error message.
        message: String,
    },

    /// A response could not be deserialized into the expected shape.
    #[error("cdp deserialize error in {method}: {source}")]
    Deserialize {
        /// Method whose result we tried to parse.
        method: String,
        /// The underlying serde error.
        #[source]
        source: serde_json::Error,
    },

    /// An operation did not complete within the timeout.
    #[error("cdp timeout in {operation} after {timeout_ms}ms")]
    Timeout {
        /// What was being waited on.
        operation: String,
        /// Timeout in milliseconds.
        timeout_ms: u64,
    },

    /// The target (page) closed while we were talking to it.
    #[error("cdp target closed")]
    TargetClosed,
}

impl CdpError {
    /// Convert into the public, structured [`BrowserError`] used by upper layers.
    pub fn into_browser(self, method: &str) -> BrowserError {
        match self {
            CdpError::Transport(msg) => BrowserError::ConnectionFailed(msg),
            CdpError::Protocol { code, message, .. } => BrowserError::ProtocolError {
                method: method.to_string(),
                code,
                message,
            },
            CdpError::Deserialize { source, .. } => BrowserError::ProtocolError {
                method: method.to_string(),
                code: -32603,
                message: format!("deserialize: {source}"),
            },
            CdpError::Timeout {
                operation,
                timeout_ms,
            } => BrowserError::Timeout {
                operation,
                timeout_ms,
            },
            CdpError::TargetClosed => BrowserError::TargetClosed,
        }
    }
}

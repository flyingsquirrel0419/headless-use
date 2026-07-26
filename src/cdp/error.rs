//! Errors specific to the CDP transport.

use crate::BrowserError;

/// Errors raised by the CDP transport layer. These are mapped up to
/// [`BrowserError`] so callers get a single, structured error type.
#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    /// A WebSocket-level transport failure (connection reset, EOF, etc.).
    ///
    /// Also carries the two connection-lifecycle messages built by
    /// [`CdpError::connection_lost`] and [`CdpError::connection_closed`]. They
    /// are `Transport` payloads rather than variants of their own on purpose:
    /// every mapping of `CdpError` (`into_browser` here, `From<CdpError> for
    /// BrowserError` in `browser::error`) is an exhaustive match that funnels
    /// transport failures to the same `ConnectionFailed`, so a new variant would
    /// add cases everywhere without changing what any caller does. What callers
    /// (and users) actually act on is the message, so the message is what tells
    /// them whether a retry is worth attempting.
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
    /// The error for a request that was in flight (or queued) when the
    /// WebSocket dropped, while the client is redialing.
    ///
    /// CDP has no request replay: the browser may or may not have executed the
    /// command, and there is no id to correlate against once the socket is gone.
    /// So the call has to fail even though the client itself is recoverable —
    /// the message says so, instead of the bare "connection closed" the caller
    /// used to get with no hint that a retry could succeed.
    pub fn connection_lost(method: &str) -> Self {
        CdpError::Transport(format!(
            "connection dropped during {method}; re-establishing it. CDP cannot replay \
             requests, so this call was lost — retry it once the client is back"
        ))
    }

    /// The error for a request on a connection that is gone for good: either an
    /// intentional shutdown, or a reconnect that exhausted its attempt budget.
    pub fn connection_closed(method: &str) -> Self {
        CdpError::Transport(format!(
            "connection closed during {method}; the client is not reconnecting \
             (the browser or target is gone)"
        ))
    }

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

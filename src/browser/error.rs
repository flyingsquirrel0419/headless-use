//! Structured browser errors.
//!
//! ## Why a typed enum
//! The AI agent layer must decide *how* to recover. A bare `String` hides the
//! failure mode. Each variant here carries the data the agent needs: which
//! element, which operation, whether a reference is stale, and so on.

use crate::observe::RefId;

/// The single error type surfaced by the browser/runtime layer.
#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    /// No browser executable found on PATH or at the configured path.
    #[error("browser not found: {0}")]
    BrowserNotFound(String),

    /// The browser process failed to start.
    #[error("browser launch failed: {0}")]
    LaunchFailed(String),

    /// Could not connect to the CDP WebSocket or HTTP endpoint.
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    /// A CDP method returned a protocol-level error.
    #[error("protocol error in {method} [{code}]: {message}")]
    ProtocolError {
        /// Method that failed.
        method: String,
        /// Numeric CDP error code.
        code: i64,
        /// Error message from CDP.
        message: String,
    },

    /// An operation timed out.
    #[error("timeout in {operation} after {timeout_ms}ms")]
    Timeout {
        /// Operation that timed out.
        operation: String,
        /// Timeout duration in ms.
        timeout_ms: u64,
    },

    /// The target/page closed unexpectedly.
    #[error("target closed")]
    TargetClosed,

    /// A semantic reference did not resolve to any element.
    #[error("element not found: {0}")]
    ElementNotFound(String),

    /// An element exists but cannot be interacted with (covered, disabled,
    /// not visible, off-screen even after scroll, etc.).
    #[error("element not interactable: {0}")]
    ElementNotInteractable(String),

    /// A reference belongs to an older observe generation.
    #[error("stale reference {0}")]
    StaleReference(String),

    /// Invalid user input (bad coordinate, unknown key name, malformed path).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// An opaque lower-level error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Any other error, with context.
    #[error("{0}")]
    Other(String),
}

impl BrowserError {
    /// Stable machine-readable error code for JSON-RPC / MCP responses.
    pub fn code(&self) -> &'static str {
        match self {
            BrowserError::BrowserNotFound(_) => "BROWSER_NOT_FOUND",
            BrowserError::LaunchFailed(_) => "LAUNCH_FAILED",
            BrowserError::ConnectionFailed(_) => "CONNECTION_FAILED",
            BrowserError::ProtocolError { .. } => "PROTOCOL_ERROR",
            BrowserError::Timeout { .. } => "TIMEOUT",
            BrowserError::TargetClosed => "TARGET_CLOSED",
            BrowserError::ElementNotFound(_) => "ELEMENT_NOT_FOUND",
            BrowserError::ElementNotInteractable(_) => "ELEMENT_NOT_INTERACTABLE",
            BrowserError::StaleReference(_) => "STALE_REFERENCE",
            BrowserError::InvalidInput(_) => "INVALID_INPUT",
            BrowserError::Io(_) => "IO_ERROR",
            BrowserError::Other(_) => "INTERNAL_ERROR",
        }
    }

    /// A short recovery hint suitable for an AI agent.
    pub fn recovery(&self) -> String {
        match self {
            BrowserError::BrowserNotFound(_) => "Install a browser or set HEADLESS_USE_BROWSER_PATH / --browser-path.".into(),
            BrowserError::LaunchFailed(_) => "Run `headless-use doctor` to check libraries and /dev/shm.".into(),
            BrowserError::StaleReference(r) => format!("Reference {r} is stale. Run `headless-use observe` again and use the new reference."),
            BrowserError::ElementNotFound(r) => format!("Reference {r} not found. Run `headless-use observe` to refresh references."),
            BrowserError::ElementNotInteractable(msg) => format!("{msg}. Try scrolling the element into view, closing dialogs, or re-observing."),
            BrowserError::TargetClosed => "The page closed. Open a new page with `headless-use open <url>`.".into(),
            BrowserError::Timeout { operation, .. } => format!("'{operation}' timed out. Increase --timeout or wait for the page to stabilize with `headless-use wait`."),
            _ => "See the error message for details; run `headless-use doctor` to diagnose the environment.".into(),
        }
    }
}

impl From<crate::cdp::CdpError> for BrowserError {
    fn from(e: crate::cdp::CdpError) -> Self {
        match e {
            crate::cdp::CdpError::Transport(m) => BrowserError::ConnectionFailed(m),
            crate::cdp::CdpError::Protocol {
                method,
                code,
                message,
            } => BrowserError::ProtocolError {
                method,
                code,
                message,
            },
            crate::cdp::CdpError::Deserialize { method, source } => BrowserError::ProtocolError {
                method,
                code: -32603,
                message: source.to_string(),
            },
            crate::cdp::CdpError::Timeout {
                operation,
                timeout_ms,
            } => BrowserError::Timeout {
                operation,
                timeout_ms,
            },
            crate::cdp::CdpError::TargetClosed => BrowserError::TargetClosed,
        }
    }
}

/// Error with an attached reference id, for stale/not-found cases.
pub fn reference_error_not_found(r: RefId) -> BrowserError {
    BrowserError::ElementNotFound(format!("@{r}"))
}

/// Error with an attached reference id, for stale cases.
pub fn reference_error_stale(r: RefId) -> BrowserError {
    BrowserError::StaleReference(format!("@{r}"))
}

//! Network observation backed by the CDP event-based [`NetworkTracker`].
//!
//! ## Why CDP instead of the JS collector
//! The previous implementation read `window.__hu_network__`, a JS array fed by
//! monkey-patched `fetch`/`XMLHttpRequest`. That missed resource loads
//! (`<img>`, `<link>`, fonts), had double-recording bugs (XHR `error` + `loadend`
//! both pushed an entry), and could not see requests that bypassed the wrapper.
//!
//! `wait` and `browser_network` now share a single CDP-derived request history
//! maintained by [`NetworkTracker`], so the diagnostics the agent sees are the
//! same ones the stability check uses. URL query secrets are masked.

use crate::browser::BrowserError;
use crate::session::network_tracker::{NetworkTracker, TrackedRequest};

/// One observed network request/response (re-exported from the tracker so wait
/// and network use a single source of truth).
pub type NetworkEntry = TrackedRequest;

/// Error type alias for the module.
pub type NetworkError = BrowserError;

/// Read the CDP-derived network request history.
///
/// `tracker` is the same tracker `wait` uses, so a request in flight when the
/// agent calls `browser_network` is visible here (and counted as pending by
/// `wait`). Secrets in URLs are masked by the tracker at capture time.
pub async fn collect(
    _page: &crate::browser::Page,
    tracker: &std::sync::Arc<NetworkTracker>,
) -> Result<Vec<NetworkEntry>, BrowserError> {
    Ok(tracker.history().await)
}

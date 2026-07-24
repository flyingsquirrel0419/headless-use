//! Wait-until-stable: combine CDP network events and DOM mutation observation.
//!
//! ## Why CDP events + MutationObserver
//! - **Network**: CDP `Network.requestWillBeSent` / `loadingFinished` /
//!   `loadingFailed` tracks ALL requests (img, script, css, fetch, xhr,
//!   redirects). JS monkey-patching misses resource loads and has
//!   double-decrement bugs.
//! - **DOM**: `MutationObserver` detects content changes that don't alter
//!   element count (e.g. text-only updates, attribute changes). Element-count
//!   comparison misses these.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::browser::{BrowserError, Page};
use crate::session::network_tracker::NetworkTracker;

/// Options for waiting.
#[derive(Debug, Clone)]
pub struct WaitOptions {
    /// Overall timeout. Default: 10 seconds.
    pub timeout: Duration,
    /// Network must be idle (no in-flight requests) for this long.
    pub network_idle: Duration,
    /// DOM must be quiet (no mutations) for this long.
    pub dom_quiet: Duration,
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            network_idle: Duration::from_millis(500),
            dom_quiet: Duration::from_millis(300),
        }
    }
}

/// Result of a wait.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WaitResult {
    /// Whether the page stabilized.
    pub stable: bool,
    /// Why we returned (reason string).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Pending (in-flight) requests count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_requests: Option<u64>,
    /// Elapsed milliseconds.
    pub elapsed_ms: u64,
}

/// Install a MutationObserver that records the last mutation timestamp.
/// Returns immediately if already installed.
async fn install_mutation_observer(page: &Page) -> Result<(), BrowserError> {
    let expr = r#"(() => {
  if (window.__hu_mut_observer__) return 'already';
  window.__hu_last_mut__ = Date.now();
  const obs = new MutationObserver(() => {
    window.__hu_last_mut__ = Date.now();
  });
  obs.observe(document.documentElement, {
    childList: true, subtree: true,
    attributes: true, characterData: true,
  });
  window.__hu_mut_observer__ = obs;
  return 'installed';
})()"#;
    page.evaluate(expr).await?;
    Ok(())
}

/// Wait until the page is stable per `opts`.
/// Uses CDP Network events for request tracking and MutationObserver for DOM.
pub async fn wait_until_stable(
    page: &Page,
    opts: WaitOptions,
    tracker: &std::sync::Arc<tokio::sync::Mutex<NetworkTracker>>,
) -> Result<WaitResult, BrowserError> {
    let start = Instant::now();
    let mut last_net_change = Instant::now();

    // Install MutationObserver for precise DOM change detection.
    install_mutation_observer(page).await?;

    // Record the initial mutation timestamp so we have a baseline.
    let expr = "window.__hu_last_mut__ || 0";
    let baseline_mut: f64 = page
        .evaluate(expr)
        .await?
        .value()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let mut last_mut_ts = baseline_mut;

    while start.elapsed() < opts.timeout {
        // Read readyState and last mutation timestamp.
        let state_expr = r#"(() => {
          return JSON.stringify({
            ready: document.readyState,
            lastMut: window.__hu_last_mut__ || 0,
          });
        })()"#;
        let v = page.evaluate(state_expr).await?;
        let s = v.value().and_then(|v| v.as_str()).unwrap_or("{}");
        let obj: Value = serde_json::from_str(s).unwrap_or(Value::Object(Default::default()));
        let ready = obj.get("ready").and_then(|r| r.as_str()).unwrap_or("");
        let mut_ts = obj.get("lastMut").and_then(|m| m.as_f64()).unwrap_or(0.0);

        // Check in-flight count from CDP tracker.
        let pending = tracker.lock().await.pending().await;
        if pending > 0 {
            last_net_change = Instant::now();
        }

        // Check DOM mutations via timestamp comparison.
        if mut_ts != last_mut_ts {
            last_mut_ts = mut_ts;
        }

        // Convert JS timestamp to elapsed since last mutation.
        let now_js = page
            .evaluate("Date.now()")
            .await?
            .value()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let ms_since_mut = if now_js > last_mut_ts {
            now_js - last_mut_ts
        } else {
            0.0
        };
        let dom_idle = ms_since_mut >= opts.dom_quiet.as_millis() as f64;
        let net_idle = last_net_change.elapsed() >= opts.network_idle;

        if ready == "complete" && net_idle && dom_idle {
            return Ok(WaitResult {
                stable: true,
                reason: None,
                pending_requests: Some(pending),
                elapsed_ms: start.elapsed().as_millis() as u64,
            });
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Timed out.
    let pending = tracker.lock().await.pending().await;
    let reason = if pending > 0 {
        "continuous-network-activity".to_string()
    } else {
        "timeout".to_string()
    };
    Ok(WaitResult {
        stable: false,
        reason: Some(reason),
        pending_requests: Some(pending),
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

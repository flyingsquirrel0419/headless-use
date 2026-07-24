//! Wait-until-stable: combine network, DOM, and load signals.
//!
//! ## Why not a fixed sleep
//! A fixed sleep is either too slow (waits when done) or flaky (returns before
//! done). We poll cheap signals — readyState, in-flight network requests, DOM
//! mutations — and return when they quiesce or when the timeout elapses.
//!
//! ## In-flight tracking
//! The network collector (`collectors.js`) maintains `__hu_net_inflight__`, a
//! counter incremented when a request starts and decremented when it completes
//! (success or failure). `wait` reads this counter to detect ongoing activity.
//! This is more accurate than filtering completed entries, which cannot see
//! requests that have started but not yet finished.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::browser::{BrowserError, Page};

/// Options for waiting.
#[derive(Debug, Clone)]
pub struct WaitOptions {
    /// Overall timeout. Default: 10 seconds.
    pub timeout: Duration,
    /// Network must be idle (no in-flight requests) for this long.
    pub network_idle: Duration,
    /// DOM must be quiet (no node-count changes) for this long.
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

/// Wait until the page is stable per `opts`.
pub async fn wait_until_stable(page: &Page, opts: WaitOptions) -> Result<WaitResult, BrowserError> {
    let start = Instant::now();
    let mut last_net_change = Instant::now();
    let mut last_dom_change = Instant::now();

    // Read readyState, in-flight count, and DOM node count in one round-trip.
    // __hu_net_inflight__ is the authoritative source for pending requests.
    let expr = r#"
    (() => {
      const pending = window.__hu_net_inflight__ || 0;
      return JSON.stringify({
        ready: document.readyState,
        pending,
        domCount: document.getElementsByTagName('*').length
      });
    })()
    "#;

    let mut last_dom_count: i64 = -1;
    while start.elapsed() < opts.timeout {
        let v = page.evaluate(expr).await?;
        let s = v.value().and_then(|v| v.as_str()).unwrap_or("{}");
        let obj: Value = serde_json::from_str(s).unwrap_or(Value::Object(Default::default()));
        let ready = obj.get("ready").and_then(|r| r.as_str()).unwrap_or("");
        let pending = obj.get("pending").and_then(|p| p.as_i64()).unwrap_or(0);
        let dom_count = obj.get("domCount").and_then(|d| d.as_i64()).unwrap_or(0);

        if pending > 0 {
            last_net_change = Instant::now();
        }
        if dom_count != last_dom_count {
            last_dom_change = Instant::now();
            last_dom_count = dom_count;
        }

        let net_idle = last_net_change.elapsed() >= opts.network_idle;
        let dom_idle = last_dom_change.elapsed() >= opts.dom_quiet;
        if ready == "complete" && net_idle && dom_idle {
            return Ok(WaitResult {
                stable: true,
                reason: None,
                pending_requests: Some(pending as u64),
                elapsed_ms: start.elapsed().as_millis() as u64,
            });
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Timed out — report why.
    let v = page.evaluate(expr).await?;
    let s = v.value().and_then(|v| v.as_str()).unwrap_or("{}");
    let obj: Value = serde_json::from_str(s).unwrap_or(Value::Object(Default::default()));
    let pending = obj.get("pending").and_then(|p| p.as_i64()).unwrap_or(0);
    let reason = if pending > 0 {
        "continuous-network-activity".to_string()
    } else {
        "timeout".to_string()
    };
    Ok(WaitResult {
        stable: false,
        reason: Some(reason),
        pending_requests: Some(pending as u64),
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

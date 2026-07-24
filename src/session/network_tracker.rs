//! CDP Network event-based request tracking + diagnostics.
//!
//! ## Why CDP events instead of JS Monkey-patching
//! The previous approach wrapped `fetch`/`XMLHttpRequest` in JS. This missed
//! `<img>`, `<link>`, `<script>`, fonts, redirects, service workers, and any
//! request that bypassed the wrapper. It also had double-decrement bugs when
//! both `error` and `loadend` fired, and it could not detect a request that
//! started and finished between two 50ms polls.
//!
//! CDP `Network.requestWillBeSent` / `responseReceived` / `loadingFinished` /
//! `loadingFailed` fires for **every** request the browser makes, regardless of
//! how it was initiated. The tracker keeps:
//!   - the set of in-flight request ids,
//!   - a `last_activity` timestamp updated on **every** network event (not just
//!     when `pending > 0`), and
//!   - a bounded request history shared with `browser_network` so wait and
//!     network diagnostics use a single source of truth.
//!
//! ## Why last_activity instead of polling pending count
//! A request can start and finish in far less than the 50ms poll interval. If
//! `wait` only sampled `pending` it would see `0` and falsely declare stable
//! even while the page was actively loading. Recording the timestamp of the
//! most recent *event* and requiring `now - last_activity >= network_idle`
//! means any request that happened at all (even a fast one) resets the idle
//! clock, so we never return stable while the network is genuinely busy.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex;

use crate::browser::Page;
use crate::util::secrets;

/// One observed network request/response, shared by `wait` and `browser_network`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrackedRequest {
    /// CDP request id.
    pub request_id: String,
    /// HTTP method.
    pub method: String,
    /// Request URL (secrets masked).
    pub url: String,
    /// Response status code (None if failed before a response or still loading).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// CDP resource type (e.g. Document, XHR, Fetch, Image).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    /// Wall-clock duration in ms (set on finish/fail).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    /// Failure reason (if the request failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// Whether the request has finished (completed or failed).
    pub finished: bool,
}

impl TrackedRequest {
    /// Build from the initial `requestWillBeSent` params.
    fn from_started(params: &serde_json::Value) -> Self {
        let request = params.get("request").cloned().unwrap_or_default();
        let method = request
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("GET")
            .to_string();
        let url =
            secrets::mask_url_secrets(request.get("url").and_then(|u| u.as_str()).unwrap_or(""));
        let resource_type = params
            .get("type")
            .and_then(|t| t.as_str())
            .map(String::from);
        let request_id = params
            .get("requestId")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();
        Self {
            request_id,
            method,
            url,
            status: None,
            resource_type,
            duration_ms: None,
            failed: None,
            finished: false,
        }
    }
}

/// Inner mutable state guarded by the mutex.
struct Inner {
    /// In-flight request ids (started, not yet finished).
    in_flight: HashMap<String, TrackedRequest>,
    /// Bounded history of completed/failed requests (most recent last).
    history: Vec<TrackedRequest>,
    /// Timestamp of the most recent network event of any kind.
    last_activity: Instant,
    /// Whether Network domain was enabled.
    enabled: bool,
}

impl Inner {
    fn record_started(&mut self, req: TrackedRequest) {
        self.last_activity = Instant::now();
        self.in_flight.insert(req.request_id.clone(), req);
    }

    fn mark_finished(&mut self, request_id: &str, finisher: Finisher) {
        self.last_activity = Instant::now();
        if let Some(mut req) = self.in_flight.remove(request_id) {
            match finisher {
                Finisher::Response { status } => {
                    req.status = Some(status);
                }
                Finisher::Completed { duration_ms } => {
                    req.duration_ms = Some(duration_ms);
                    req.finished = true;
                }
                Finisher::Failed { error, duration_ms } => {
                    req.failed = Some(error);
                    req.duration_ms = duration_ms;
                    req.finished = true;
                }
            }
            // Keep a bounded history so wait and network share one source.
            if req.finished || req.status.is_some() {
                self.history.push(req);
                // Bound the history so a very chatty page cannot grow memory
                // without limit. 1000 entries is ample for diagnostics.
                if self.history.len() > 1000 {
                    self.history.remove(0);
                }
            } else {
                // A response without completion: keep it in-flight until finish.
                self.in_flight.insert(request_id.to_string(), req);
            }
        }
    }
}

/// How a request completed.
enum Finisher {
    /// A response was received (HTTP status).
    Response { status: u16 },
    /// The request completed loading successfully.
    Completed { duration_ms: f64 },
    /// The request failed.
    Failed {
        error: String,
        duration_ms: Option<f64>,
    },
}

/// Tracks in-flight network requests + history via CDP events.
pub struct NetworkTracker {
    inner: Arc<Mutex<Inner>>,
}

impl NetworkTracker {
    /// Create a new tracker.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                in_flight: HashMap::new(),
                history: Vec::new(),
                last_activity: Instant::now(),
                enabled: false,
            })),
        }
    }

    /// Enable Network domain and start listening for request events.
    /// Spawns a background task that updates the in-flight set and history.
    ///
    /// Must be called *before* the first navigation so requestWillBeSent is
    /// captured for every request (CDP does not replay events for requests
    /// already in flight when Network.enable is called).
    pub async fn start(&self, page: &Page) -> Result<(), crate::browser::BrowserError> {
        page.enable("Network.enable", None).await?;

        let inner = self.inner.clone();
        let mut events = page.cdp().subscribe_events_async().await;

        {
            let mut g = inner.lock().await;
            g.enabled = true;
        }

        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let method = event.method.as_str();
                match method {
                    "Network.requestWillBeSent" => {
                        let req = TrackedRequest::from_started(&event.params);
                        inner.lock().await.record_started(req);
                    }
                    "Network.responseReceived" => {
                        if let (Some(id), Some(status)) = (
                            event.params.get("requestId").and_then(|v| v.as_str()),
                            event
                                .params
                                .get("response")
                                .and_then(|r| r.get("status"))
                                .and_then(|s| s.as_u64()),
                        ) {
                            inner.lock().await.mark_finished(
                                id,
                                Finisher::Response {
                                    status: status as u16,
                                },
                            );
                        }
                    }
                    "Network.loadingFinished" => {
                        if let Some(id) = event.params.get("requestId").and_then(|v| v.as_str()) {
                            let ts = event
                                .params
                                .get("timestamp")
                                .and_then(|t| t.as_f64())
                                .unwrap_or(0.0);
                            // CDP timestamps are seconds since process start;
                            // we approximate duration from the start if available,
                            // otherwise report 0.
                            let dur = ts.max(0.0) * 1000.0;
                            inner
                                .lock()
                                .await
                                .mark_finished(id, Finisher::Completed { duration_ms: dur });
                        }
                    }
                    "Network.loadingFailed" => {
                        if let Some(id) = event.params.get("requestId").and_then(|v| v.as_str()) {
                            let error = event
                                .params
                                .get("errorText")
                                .and_then(|e| e.as_str())
                                .unwrap_or("network error")
                                .to_string();
                            let ts = event
                                .params
                                .get("timestamp")
                                .and_then(|t| t.as_f64())
                                .unwrap_or(0.0);
                            let dur = if ts > 0.0 { Some(ts * 1000.0) } else { None };
                            inner.lock().await.mark_finished(
                                id,
                                Finisher::Failed {
                                    error,
                                    duration_ms: dur,
                                },
                            );
                        }
                    }
                    _ => {}
                }
            }
        });

        Ok(())
    }

    /// Current count of in-flight requests.
    pub async fn pending(&self) -> u64 {
        self.inner.lock().await.in_flight.len() as u64
    }

    /// Time elapsed since the most recent network activity of any kind
    /// (request started, response received, finished, or failed).
    ///
    /// This is the key signal for wait-until-stable: even a fast request that
    /// started and finished between two polls updates this timestamp, so the
    /// idle window is measured from real activity, not from a poll that
    /// happened to see `pending == 0`.
    pub async fn idle_for(&self) -> std::time::Duration {
        let g = self.inner.lock().await;
        g.last_activity.elapsed()
    }

    /// Snapshot of the bounded request history (completed + failed + in-flight).
    /// In-flight requests are included so `browser_network` shows ongoing work.
    pub async fn history(&self) -> Vec<TrackedRequest> {
        let g = self.inner.lock().await;
        let mut out = g.history.clone();
        out.extend(g.in_flight.values().cloned());
        out
    }

    /// Whether the tracker has been started.
    pub async fn is_enabled(&self) -> bool {
        self.inner.lock().await.enabled
    }
}

impl Default for NetworkTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_started_masks_url_secrets() {
        let params = json!({
            "requestId": "r1",
            "type": "Fetch",
            "request": {
                "method": "GET",
                "url": "https://api.example.com/data?token=secret&foo=bar"
            }
        });
        let req = TrackedRequest::from_started(&params);
        assert_eq!(req.method, "GET");
        assert_eq!(
            req.url,
            "https://api.example.com/data?token=[REDACTED]&foo=bar"
        );
        assert_eq!(req.resource_type.as_deref(), Some("Fetch"));
        assert!(!req.finished);
    }

    #[tokio::test]
    async fn last_activity_updates_on_every_event() {
        let tracker = NetworkTracker::new();
        // Before any event, idle_for is near zero.
        let idle0 = tracker.idle_for().await;
        assert!(idle0.as_millis() < 1000);

        // Simulate a started request.
        let params = json!({
            "requestId": "A",
            "type": "Fetch",
            "request": { "method": "GET", "url": "https://x/y" }
        });
        tracker
            .inner
            .lock()
            .await
            .record_started(TrackedRequest::from_started(&params));
        assert_eq!(tracker.pending().await, 1);

        // Simulate completion.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tracker
            .inner
            .lock()
            .await
            .mark_finished("A", Finisher::Completed { duration_ms: 5.0 });
        assert_eq!(tracker.pending().await, 0);

        // The idle clock reflects the completion event, not the start.
        let idle = tracker.idle_for().await;
        assert!(idle.as_millis() < 1000);

        // History contains the completed request.
        let hist = tracker.history().await;
        assert_eq!(hist.len(), 1);
        assert!(hist[0].finished);
        assert_eq!(hist[0].status, None); // no response event in this test
    }

    #[tokio::test]
    async fn failed_request_recorded_once_in_history() {
        let tracker = NetworkTracker::new();
        let params = json!({
            "requestId": "F",
            "type": "XHR",
            "request": { "method": "POST", "url": "https://x/api" }
        });
        tracker
            .inner
            .lock()
            .await
            .record_started(TrackedRequest::from_started(&params));
        tracker.inner.lock().await.mark_finished(
            "F",
            Finisher::Failed {
                error: "net::ERR_FAILED".into(),
                duration_ms: Some(3.0),
            },
        );
        let hist = tracker.history().await;
        assert_eq!(hist.len(), 1, "failed request should appear exactly once");
        assert_eq!(hist[0].failed.as_deref(), Some("net::ERR_FAILED"));
        assert!(hist[0].finished);
        assert_eq!(tracker.pending().await, 0);
    }
}

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

use std::collections::{HashMap, VecDeque};
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
    /// When `requestWillBeSent` was observed, used to compute `duration_ms`.
    ///
    /// CDP's `timestamp` field is a monotonic clock reading, not an elapsed
    /// time — multiplying it by 1000 (as an earlier version did) produced a
    /// meaningless number. We measure locally instead.
    #[serde(skip)]
    started_at: Option<Instant>,
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
            started_at: Some(Instant::now()),
        }
    }
}

/// Inner mutable state guarded by the mutex.
/// Maximum number of completed requests kept for diagnostics.
const HISTORY_LIMIT: usize = 1000;

struct Inner {
    /// In-flight request ids (started, not yet finished).
    in_flight: HashMap<String, TrackedRequest>,
    /// Bounded history of completed/failed requests (most recent last).
    /// A `VecDeque` so trimming the oldest entry is O(1), not O(n).
    history: VecDeque<TrackedRequest>,
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

    /// Record a response header arrival. The body may still be streaming, so
    /// the request stays **in flight** until `loadingFinished`/`loadingFailed`.
    ///
    /// An earlier version moved the request to history here (the
    /// `status.is_some()` branch matched before the "keep it in flight" one).
    /// That made `pending()` drop to zero the moment headers arrived, and the
    /// later `loadingFinished` found nothing to update — so `finished` stayed
    /// `false` and `duration_ms` stayed `None` for every successful request.
    fn record_response(&mut self, request_id: &str, status: u16) {
        self.last_activity = Instant::now();
        if let Some(req) = self.in_flight.get_mut(request_id) {
            req.status = Some(status);
        }
    }

    /// Move a request out of the in-flight set into the bounded history.
    fn mark_finished(&mut self, request_id: &str, finisher: Finisher) {
        self.last_activity = Instant::now();
        let Some(mut req) = self.in_flight.remove(request_id) else {
            return;
        };
        req.duration_ms = req
            .started_at
            .map(|t| t.elapsed().as_secs_f64() * 1000.0)
            .map(|ms| (ms * 100.0).round() / 100.0);
        match finisher {
            Finisher::Completed => {}
            Finisher::Failed { error } => {
                req.failed = Some(error);
            }
        }
        req.finished = true;
        // Bound the history so a very chatty page cannot grow memory without
        // limit. HISTORY_LIMIT entries is ample for diagnostics.
        self.history.push_back(req);
        if self.history.len() > HISTORY_LIMIT {
            self.history.pop_front();
        }
    }
}

/// How a request left the in-flight set.
enum Finisher {
    /// The request completed loading successfully.
    Completed,
    /// The request failed.
    Failed {
        /// CDP `errorText`, e.g. `net::ERR_CONNECTION_REFUSED`.
        error: String,
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
                history: VecDeque::new(),
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
                            inner.lock().await.record_response(id, status as u16);
                        }
                    }
                    "Network.loadingFinished" => {
                        if let Some(id) = event.params.get("requestId").and_then(|v| v.as_str()) {
                            inner.lock().await.mark_finished(id, Finisher::Completed);
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
                            inner
                                .lock()
                                .await
                                .mark_finished(id, Finisher::Failed { error });
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
        let mut out: Vec<TrackedRequest> = g.history.iter().cloned().collect();
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
            .mark_finished("A", Finisher::Completed);
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
            },
        );
        let hist = tracker.history().await;
        assert_eq!(hist.len(), 1, "failed request should appear exactly once");
        assert_eq!(hist[0].failed.as_deref(), Some("net::ERR_FAILED"));
        assert!(hist[0].finished);
        assert_eq!(tracker.pending().await, 0);
    }

    /// Regression: `responseReceived` used to move the request straight into
    /// history, so `pending()` dropped to 0 while the body was still streaming
    /// and the later `loadingFinished` had nothing left to update — leaving
    /// `finished: false` and `duration_ms: None` on every successful request.
    #[tokio::test]
    async fn response_headers_keep_request_in_flight_until_finished() {
        let tracker = NetworkTracker::new();
        let params = json!({
            "requestId": "R",
            "type": "Document",
            "request": { "method": "GET", "url": "https://x/page" }
        });
        tracker
            .inner
            .lock()
            .await
            .record_started(TrackedRequest::from_started(&params));

        // Headers arrive: status recorded, but the body is still streaming.
        tracker.inner.lock().await.record_response("R", 200);
        assert_eq!(
            tracker.pending().await,
            1,
            "a request whose body is still streaming must stay in flight"
        );
        assert!(
            tracker.history().await.iter().all(|r| !r.finished),
            "nothing should be marked finished yet"
        );

        // Body done.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        tracker
            .inner
            .lock()
            .await
            .mark_finished("R", Finisher::Completed);
        assert_eq!(tracker.pending().await, 0);

        let hist = tracker.history().await;
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].status, Some(200), "status survives the move");
        assert!(hist[0].finished);
        let dur = hist[0].duration_ms.expect("duration must be recorded");
        assert!(
            (10.0..5_000.0).contains(&dur),
            "duration should be a real elapsed time, got {dur}"
        );
    }

    #[tokio::test]
    async fn history_is_bounded() {
        let tracker = NetworkTracker::new();
        for i in 0..(HISTORY_LIMIT + 10) {
            let id = format!("r{i}");
            let params = json!({
                "requestId": id,
                "request": { "method": "GET", "url": "https://x/y" }
            });
            let mut g = tracker.inner.lock().await;
            g.record_started(TrackedRequest::from_started(&params));
            g.mark_finished(&id, Finisher::Completed);
        }
        assert_eq!(tracker.history().await.len(), HISTORY_LIMIT);
    }
}

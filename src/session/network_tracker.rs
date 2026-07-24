//! CDP Network event-based in-flight request tracking.
//!
//! ## Why CDP events instead of JS monkey-patching
//! The previous approach wrapped `fetch`/`XMLHttpRequest` in JS. This missed
//! `<img>`, `<link>`, `<script>`, fonts, redirects, service workers, and any
//! request that bypassed the wrapper. It also had double-decrement bugs when
//! both `error` and `loadend` fired.
//!
//! CDP `Network.requestWillBeSent` / `loadingFinished` / `loadingFailed` fires
//! for **every** request the browser makes, regardless of how it was initiated.

use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::browser::Page;

/// Tracks in-flight network requests via CDP events.
pub struct NetworkTracker {
    /// Request IDs that have started but not yet finished.
    in_flight: Arc<Mutex<HashSet<String>>>,
}

impl NetworkTracker {
    /// Create a new tracker.
    pub fn new() -> Self {
        Self {
            in_flight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Enable Network domain and start listening for request events.
    /// Spawns a background task that updates the in-flight set.
    pub async fn start(&self, page: &Page) -> Result<(), crate::browser::BrowserError> {
        page.enable("Network.enable", None).await?;

        let in_flight = self.in_flight.clone();
        let mut events = page.cdp().subscribe_events_async().await;

        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                match event.method.as_str() {
                    "Network.requestWillBeSent" => {
                        if let Some(id) = event.params.get("requestId").and_then(|v| v.as_str()) {
                            in_flight.lock().await.insert(id.to_string());
                        }
                    }
                    "Network.loadingFinished" => {
                        if let Some(id) = event.params.get("requestId").and_then(|v| v.as_str()) {
                            in_flight.lock().await.remove(id);
                        }
                    }
                    "Network.loadingFailed" => {
                        if let Some(id) = event.params.get("requestId").and_then(|v| v.as_str()) {
                            in_flight.lock().await.remove(id);
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
        self.in_flight.lock().await.len() as u64
    }
}

impl Default for NetworkTracker {
    fn default() -> Self {
        Self::new()
    }
}

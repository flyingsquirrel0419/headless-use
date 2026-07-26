//! A CDP-attached page (tab). Owns a target session id and exposes
//! navigation, viewport, and screenshot operations.

use std::time::Duration;

use serde_json::{json, Value};

use crate::browser::BrowserError;
use crate::cdp::{CdpClient, Viewport};

/// A page attached via CDP.
pub struct Page {
    client: CdpClient,
    target_id: String,
    session_id: String,
}

impl Page {
    /// Create a new tab via `Target.createTarget` and attach a session.
    pub(crate) async fn create(browser: &crate::browser::Browser) -> Result<Self, BrowserError> {
        // Create the target.
        let result: Value = browser
            .cdp()
            .call(
                "Target.createTarget",
                Some(json!({ "url": "about:blank", "background": false })),
                Duration::from_secs(15),
            )
            .await
            .map_err(crate::cdp::CdpError::into_browser_error_ignore_method)?;

        let target_id = result
            .get("targetId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BrowserError::UnexpectedResponse {
                method: "Target.createTarget".into(),
                detail: "no targetId".into(),
            })?
            .to_string();

        // Attach to get a session id.
        let attach: Value = browser
            .cdp()
            .call(
                "Target.attachToTarget",
                Some(json!({ "targetId": target_id, "flatten": true })),
                Duration::from_secs(15),
            )
            .await
            .map_err(crate::cdp::CdpError::into_browser_error_ignore_method)?;
        let session_id = attach
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BrowserError::UnexpectedResponse {
                method: "Target.attachToTarget".into(),
                detail: "no sessionId".into(),
            })?
            .to_string();

        // The CDP client connects to the browser endpoint; to talk to a page
        // session we attach the sessionId to each request below via a wrapper.
        let page = Self {
            client: browser.cdp().clone(),
            target_id,
            session_id: session_id.clone(),
        };
        // Enable Page domain on the new session, then inject the console +
        // network collectors via addScriptToEvaluateOnNewDocument so they run
        // before any page JS and capture all activity from the first load.
        let _ = page.enable("Page.enable", None).await;
        let collector = include_str!("../session/collectors.js");
        let _: serde_json::Value = page
            .call(
                "Page.addScriptToEvaluateOnNewDocument",
                Some(serde_json::json!({ "source": collector })),
                std::time::Duration::from_secs(10),
            )
            .await?;
        // Stealth runs last on purpose: pre-load scripts execute in
        // registration order, and part of stealth's job is to make the
        // collector's `fetch`/console wrappers report native source. Patching
        // them before they exist would do nothing.
        if let Some(profile) = browser.stealth() {
            profile.apply_to_page(&page).await?;
        }
        Ok(page)
    }

    /// The CDP client (browser-level).
    pub fn cdp(&self) -> &CdpClient {
        &self.client
    }

    /// This page's target id.
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    /// This page's session id.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Send a CDP command scoped to this page's session (sessionId param).
    pub async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<T, BrowserError> {
        // Flatten mode: include sessionId in the request object itself.
        // We encode this by wrapping params into a request that the client
        // sends; the client already serializes id/method/params, so we inject
        // sessionId by using a special pseudo-method path.
        //
        // Implementation: we build the raw request manually to add sessionId,
        // then send via the client's low-level path.
        self.client
            .call_session(method, params, &self.session_id, timeout)
            .await
            .map_err(crate::cdp::CdpError::into_browser_error_ignore_method)
    }

    /// Fire-and-forget enable within the page session.
    pub async fn enable(&self, method: &str, params: Option<Value>) -> Result<(), BrowserError> {
        self.call::<Value>(method, params, Duration::from_secs(15))
            .await
            .map(|_| ())
    }

    /// Navigate to `url` and wait for load.
    ///
    /// Checks `Page.navigate` response for `errorText` (e.g.
    /// `net::ERR_BLOCKED_BY_ADMINISTRATOR`) and detects Chrome error pages
    /// (`chrome-error://chromewebdata/`) which load successfully but indicate
    /// a navigation failure.
    pub async fn goto(&self, url: &str) -> Result<(), BrowserError> {
        self.enable("Page.enable", None).await?;
        let result: Value = self
            .call(
                "Page.navigate",
                Some(json!({ "url": url })),
                Duration::from_secs(30),
            )
            .await?;
        // Check for CDP-level navigation error.
        if let Some(error_text) = result.get("errorText").and_then(|v| v.as_str()) {
            return Err(BrowserError::Navigation {
                url: url.to_string(),
                reason: error_text.to_string(),
            });
        }
        self.wait_load(Duration::from_secs(30)).await?;
        // Detect Chrome error pages — these load 'successfully' (readyState=complete)
        // but indicate a real failure (DNS error, blocked, etc.).
        let final_url = self.url().await.unwrap_or_default();
        if final_url.starts_with("chrome-error://") {
            return Err(BrowserError::Navigation {
                url: url.to_string(),
                reason: format!("Chrome error page ({final_url})"),
            });
        }
        Ok(())
    }

    /// Reload the current page via CDP `Page.reload` and wait for load.
    ///
    /// Unlike navigating to the current URL again, this preserves the history
    /// entry — including a POST body — which is what a user pressing reload
    /// gets.
    pub async fn reload(&self) -> Result<(), BrowserError> {
        self.enable("Page.enable", None).await?;
        // Stamp the *current* document. `wait_load` only polls
        // `document.readyState`, and the outgoing document is already
        // "complete" — so without a marker the wait would return instantly and
        // the caller would observe the pre-reload page. The marker does not
        // survive the document swap, so its absence proves the new document is
        // in place.
        let _ = self.evaluate_sync("window.__hu_reload_marker__ = 1").await;
        self.call::<Value>(
            "Page.reload",
            Some(json!({ "ignoreCache": false })),
            Duration::from_secs(30),
        )
        .await?;
        self.wait_document_swapped("__hu_reload_marker__", Duration::from_secs(30))
            .await?;
        self.wait_load(Duration::from_secs(30)).await?;
        let final_url = self.url().await.unwrap_or_default();
        if final_url.starts_with("chrome-error://") {
            return Err(BrowserError::Navigation {
                url: final_url.clone(),
                reason: "reload landed on a Chrome error page".into(),
            });
        }
        Ok(())
    }

    /// Wait until a marker previously set on `window` is gone, i.e. the
    /// document has been replaced. Used to make reload waits honest.
    async fn wait_document_swapped(
        &self,
        marker: &str,
        timeout: Duration,
    ) -> Result<(), BrowserError> {
        let deadline = std::time::Instant::now() + timeout;
        let expr = format!("!!window.{marker}");
        loop {
            let still_there = self
                .evaluate_sync(&expr)
                .await
                .ok()
                .and_then(|r| r.value().and_then(|v| v.as_bool()))
                .unwrap_or(false);
            if !still_there {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(BrowserError::Timeout {
                    operation: "Page.reload".into(),
                    timeout_ms: timeout.as_millis() as u64,
                });
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Wait for the page load to complete, or time out.
    ///
    /// Waits on `Page.loadEventFired` for this page's session, with a
    /// `document.readyState` poll as a backstop.
    ///
    /// ## Why both
    /// The event alone is not enough: it may have fired before we subscribed
    /// (a fast document), and it is only delivered if the `Page` domain is
    /// enabled, which a bare `Page` used outside a `Session` cannot assume.
    /// The poll alone is not enough either — it adds up to a poll interval of
    /// latency to every navigation. Racing them costs nothing and is correct in
    /// both directions.
    ///
    /// The poll uses [`Self::evaluate_sync`]. The earlier version used
    /// `evaluate`, i.e. `awaitPromise: true`, which is the documented hang class
    /// on any page running a perpetual `requestAnimationFrame` loop: waiting for
    /// load would itself hang until the 30s call timeout.
    pub async fn wait_load(&self, timeout: Duration) -> Result<(), BrowserError> {
        // Subscribe before the first readyState check so a load event that
        // fires between the check and the wait is not missed.
        let mut events = self.client.subscribe_events_async().await;
        let deadline = std::time::Instant::now() + timeout;

        loop {
            if self.ready_state_is_complete().await? {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(BrowserError::Timeout {
                    operation: "Page.wait_load".into(),
                    timeout_ms: timeout.as_millis() as u64,
                });
            }
            let poll_in = remaining.min(Duration::from_millis(50));
            tokio::select! {
                maybe_event = events.recv() => {
                    match maybe_event {
                        Some(ev) => {
                            if ev.method == "Page.loadEventFired"
                                && ev.session_id.as_deref() == Some(self.session_id.as_str())
                            {
                                return Ok(());
                            }
                        }
                        // Event bus gone: the connection is closing. Fall back to
                        // polling until the deadline rather than spinning on a
                        // closed channel.
                        None => {
                            tokio::time::sleep(poll_in).await;
                        }
                    }
                }
                _ = tokio::time::sleep(poll_in) => {}
            }
        }
    }

    /// True when `document.readyState === "complete"`.
    async fn ready_state_is_complete(&self) -> Result<bool, BrowserError> {
        let ready = self
            .evaluate_sync("document.readyState")
            .await?
            .value()
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default();
        Ok(ready == "complete")
    }

    /// Evaluate a JS expression in the page and return the typed result.
    pub async fn evaluate(
        &self,
        expression: &str,
    ) -> Result<crate::cdp::EvaluateResult, BrowserError> {
        self.call::<crate::cdp::EvaluateResult>(
            "Runtime.evaluate",
            Some(json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
            })),
            Duration::from_secs(30),
        )
        .await
        .and_then(|r| {
            if r.exception_details.is_some() {
                Err(BrowserError::Evaluation(format!(
                    "{:?}",
                    r.exception_details
                )))
            } else {
                Ok(r)
            }
        })
    }

    /// Evaluate a JS expression WITHOUT `awaitPromise`. Use this for
    /// expressions that return a plain value (string/number/JSON), never a
    /// Promise. On pages with a perpetual `requestAnimationFrame` loop (e.g.
    /// animated CAPTCHA fixtures), `Runtime.evaluate` with `awaitPromise: true`
    /// can hang because Chrome waits for the microtask queue to drain, which a
    /// busy rAF loop keeps non-empty. Non-awaiting evaluation returns as soon as
    /// the synchronous expression completes.
    pub async fn evaluate_sync(
        &self,
        expression: &str,
    ) -> Result<crate::cdp::EvaluateResult, BrowserError> {
        self.call::<crate::cdp::EvaluateResult>(
            "Runtime.evaluate",
            Some(json!({
                "expression": expression,
                "returnByValue": true,
            })),
            Duration::from_secs(15),
        )
        .await
        .and_then(|r| {
            if r.exception_details.is_some() {
                Err(BrowserError::Evaluation(format!(
                    "{:?}",
                    r.exception_details
                )))
            } else {
                Ok(r)
            }
        })
    }

    /// Find the first element matching a CSS selector and return its viewport
    /// bounding box (x, y, width, height).
    ///
    /// Tries the DOM domain first, then falls back to a synchronous
    /// `getBoundingClientRect` evaluation.
    ///
    /// ## Why both
    /// The DOM path was introduced because `Runtime.evaluate` with
    /// `awaitPromise: true` hangs on a page running a perpetual
    /// `requestAnimationFrame` loop. But the DOM agent is not immune either:
    /// on the same animated pages `DOM.getDocument` can exceed its timeout when
    /// the main thread is saturated, which is how the only end-to-end
    /// `dewiggle` test ended up permanently `#[ignore]`d. Now that
    /// [`Self::evaluate_sync`] exists — no `awaitPromise`, so no microtask
    /// wait — the fallback does not reintroduce the original hang.
    pub async fn element_box_by_selector(
        &self,
        selector: &str,
    ) -> Result<Option<(f64, f64, f64, f64)>, BrowserError> {
        match self.element_box_via_dom(selector).await {
            Ok(v) => Ok(v),
            Err(e) => {
                tracing::debug!(error = %e, selector, "DOM box lookup failed; using JS fallback");
                self.element_box_via_js(selector).await
            }
        }
    }

    /// `getBoundingClientRect` fallback for [`Self::element_box_by_selector`].
    async fn element_box_via_js(
        &self,
        selector: &str,
    ) -> Result<Option<(f64, f64, f64, f64)>, BrowserError> {
        let sel = serde_json::to_string(selector).unwrap_or_else(|_| "\"*\"".into());
        let expr = format!(
            "(()=>{{const e=document.querySelector({sel});if(!e)return null;const r=e.getBoundingClientRect();return JSON.stringify({{x:r.x,y:r.y,w:r.width,h:r.height}});}})()"
        );
        let Some(s) = self
            .evaluate_sync(&expr)
            .await?
            .value()
            .and_then(|v| v.as_str())
            .map(String::from)
        else {
            return Ok(None);
        };
        if s == "null" || s.is_empty() {
            return Ok(None);
        }
        let v: Value = serde_json::from_str(&s)
            .map_err(|e| BrowserError::Decode(format!("element box: {e}")))?;
        let get = |k: &str| v.get(k).and_then(|n| n.as_f64()).unwrap_or(0.0);
        Ok(Some((get("x"), get("y"), get("w"), get("h"))))
    }

    async fn element_box_via_dom(
        &self,
        selector: &str,
    ) -> Result<Option<(f64, f64, f64, f64)>, BrowserError> {
        // The DOM agent answers `getDocument` reliably only once enabled.
        self.enable("DOM.enable", None).await?;
        let doc: Value = self
            .call::<Value>(
                "DOM.getDocument",
                Some(json!({"depth": 0})),
                Duration::from_secs(10),
            )
            .await?;
        let root_node_id = doc
            .get("root")
            .and_then(|r| r.get("nodeId"))
            .and_then(|n| n.as_i64())
            .ok_or_else(|| BrowserError::UnexpectedResponse {
                method: "DOM.getDocument".into(),
                detail: "no root nodeId".into(),
            })?;
        let q: Value = self
            .call::<Value>(
                "DOM.querySelector",
                Some(json!({ "nodeId": root_node_id, "selector": selector })),
                Duration::from_secs(10),
            )
            .await?;
        let node_id = q.get("nodeId").and_then(|n| n.as_i64()).unwrap_or(0);
        if node_id == 0 {
            return Ok(None);
        }
        let bm: Value = self
            .call::<Value>(
                "DOM.getBoxModel",
                Some(json!({ "nodeId": node_id })),
                Duration::from_secs(10),
            )
            .await?;
        let quad = bm
            .get("model")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array());
        let Some(quad) = quad else {
            return Ok(None);
        };
        if quad.len() < 8 {
            return Ok(None);
        }
        let xs: Vec<f64> = (0..4)
            .map(|i| quad[i * 2].as_f64().unwrap_or(0.0))
            .collect();
        let ys: Vec<f64> = (0..4)
            .map(|i| quad[i * 2 + 1].as_f64().unwrap_or(0.0))
            .collect();
        let x = xs.iter().cloned().fold(f64::INFINITY, f64::min);
        let y = ys.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_x = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let max_y = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        Ok(Some((x, y, max_x - x, max_y - y)))
    }

    /// Set the device metrics (viewport + scale factor).
    pub async fn set_viewport(&self, vp: Viewport) -> Result<(), BrowserError> {
        self.call::<Value>(
            "Emulation.setDeviceMetricsOverride",
            Some(json!({
                "width": vp.width,
                "height": vp.height,
                "deviceScaleFactor": vp.device_scale_factor,
                "mobile": false,
            })),
            Duration::from_secs(10),
        )
        .await
        .map(|_| ())
    }

    /// Capture a screenshot as PNG bytes.
    ///
    /// `full_page` captures the entire scrollable content; `clip` captures a
    /// CSS-pixel region (x, y, width, height) when provided.
    pub async fn screenshot(
        &self,
        full_page: bool,
        clip: Option<(f64, f64, f64, f64)>,
    ) -> Result<Vec<u8>, BrowserError> {
        let mut params = json!({
            "format": "png",
            "captureBeyondViewport": full_page,
        });
        if full_page {
            params["fromSurface"] = json!(true);
        }
        if let Some((x, y, w, h)) = clip {
            params["clip"] = json!({
                "x": x, "y": y, "width": w, "height": h, "scale": 1.0
            });
        }
        let result: Value = self
            .call(
                "Page.captureScreenshot",
                Some(params),
                Duration::from_secs(30),
            )
            .await?;
        let data = result.get("data").and_then(|v| v.as_str()).ok_or_else(|| {
            BrowserError::UnexpectedResponse {
                method: "Page.captureScreenshot".into(),
                detail: "no data field".into(),
            }
        })?;
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
            .map_err(|e| BrowserError::Decode(format!("screenshot base64: {e}")))
    }

    /// Current URL via JS.
    pub async fn url(&self) -> Result<String, BrowserError> {
        Ok(self
            .evaluate_sync("location.href")
            .await?
            .value()
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default())
    }

    /// Page title via JS.
    pub async fn title(&self) -> Result<String, BrowserError> {
        Ok(self
            .evaluate_sync("document.title")
            .await?
            .value()
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default())
    }

    /// Scroll position (scrollX, scrollY) via JS.
    pub async fn scroll_position(&self) -> Result<(i64, i64), BrowserError> {
        let v = self
            .evaluate_sync("JSON.stringify({x: window.scrollX, y: window.scrollY})")
            .await?
            .value()
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "{}".into());
        let obj: Value = serde_json::from_str(&v)
            .map_err(|e| BrowserError::Decode(format!("scroll position: {e}")))?;
        Ok((
            obj.get("x").and_then(|x| x.as_i64()).unwrap_or(0),
            obj.get("y").and_then(|y| y.as_i64()).unwrap_or(0),
        ))
    }

    /// Close this page target.
    pub async fn close(&self, browser: &crate::browser::Browser) -> Result<(), BrowserError> {
        browser
            .cdp()
            .call::<Value>(
                "Target.closeTarget",
                Some(json!({ "targetId": self.target_id })),
                Duration::from_secs(10),
            )
            .await
            .map_err(crate::cdp::CdpError::into_browser_error_ignore_method)?;
        Ok(())
    }

    /// Inject the virtual cursor overlay into every new document.
    ///
    /// [Decision Log]
    /// - 목적과 의도: Make the agent's CDP mouse visually observable on
    ///   headless/Xvfb displays (where Input.dispatchMouseEvent renders no
    ///   OS cursor) for the live viewer and demos.
    /// - 기존 구현 및 제약 조건: Previously the cursor overlay was inlined
    ///   per-fixture, so it only existed on demo pages.
    /// - 검토한 주요 대안: Runtime.evaluate after each Input.* call to move a
    ///   div — couples the visual layer to the input engine and adds latency.
    /// - 선택한 방식: addScriptToEvaluateOnNewDocument so the overlay is
    ///   injected before page JS and survives navigation. The overlay listens
    ///   to the same pointer events CDP already dispatches, so it needs zero
    ///   input-engine cooperation.
    /// - 장점: Universal (works on any page), survives navigation, no input
    ///   coupling. 단점: Relies on CDP dispatching DOM pointer events.
    pub async fn inject_cursor_overlay(&self) -> Result<(), BrowserError> {
        let source = include_str!("../viewer/cursor-overlay.js");
        // Register for all future navigations so the overlay survives reloads
        // and SPA route changes (the script is idempotent via the
        // __hu_cursor_injected guard).
        self.call::<Value>(
            "Page.addScriptToEvaluateOnNewDocument",
            Some(json!({ "source": source })),
            Duration::from_secs(10),
        )
        .await?;
        // Also run it on the current document so the cursor appears
        // immediately. We base64-encode the source to avoid quoting issues,
        // then decode and execute via an inline <script> element so the code
        // runs in the page's global scope (not an evaluate closure, which can
        // confuse strict CSPs less than eval).
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, source);
        let expr = format!(
            "(function(){{try{{var s=document.createElement('script');s.textContent=atob('{b64}');document.documentElement.appendChild(s);}}catch(e){{}}}})();"
        );
        let _ = self.evaluate_sync(&expr).await;
        Ok(())
    }
}

/// Helper trait to convert CdpError into BrowserError without a method name.
///
/// This is a workaround because some call sites don't have a method name handy;
/// prefer `BrowserError::from(cdp_error)` where possible.
trait CdpErrorExt {
    fn into_browser_error_ignore_method(self) -> BrowserError;
}

impl CdpErrorExt for crate::cdp::CdpError {
    fn into_browser_error_ignore_method(self) -> BrowserError {
        BrowserError::from(self)
    }
}

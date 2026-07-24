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
            .ok_or_else(|| BrowserError::Other("Target.createTarget returned no targetId".into()))?
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
            .ok_or_else(|| {
                BrowserError::Other("Target.attachToTarget returned no sessionId".into())
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
            return Err(BrowserError::Other(format!(
                "navigation to '{url}' failed: {error_text}"
            )));
        }
        self.wait_load(Duration::from_secs(30)).await?;
        // Detect Chrome error pages — these load 'successfully' (readyState=complete)
        // but indicate a real failure (DNS error, blocked, etc.).
        let final_url = self.url().await.unwrap_or_default();
        if final_url.starts_with("chrome-error://") {
            return Err(BrowserError::Other(format!(
                "navigation to '{url}' resulted in a Chrome error page ({final_url})"
            )));
        }
        Ok(())
    }

    /// Wait for `Page.loadEventFired` (or timeout).
    pub async fn wait_load(&self, timeout: Duration) -> Result<(), BrowserError> {
        // Poll document.readyState via Runtime.evaluate as a robust fallback.
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let ready = self
                .evaluate("document.readyState")
                .await?
                .value()
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_default();
            if ready == "complete" {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(BrowserError::Timeout {
                    operation: "Page.wait_load".into(),
                    timeout_ms: timeout.as_millis() as u64,
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
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
                Err(BrowserError::Other(format!(
                    "Runtime.evaluate threw: {:?}",
                    r.exception_details
                )))
            } else {
                Ok(r)
            }
        })
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
        let data = result
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BrowserError::Other("no screenshot data".into()))?;
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
            .map_err(|e| BrowserError::Other(format!("base64 decode: {e}")))
    }

    /// Current URL via JS.
    pub async fn url(&self) -> Result<String, BrowserError> {
        Ok(self
            .evaluate("location.href")
            .await?
            .value()
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default())
    }

    /// Page title via JS.
    pub async fn title(&self) -> Result<String, BrowserError> {
        Ok(self
            .evaluate("document.title")
            .await?
            .value()
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default())
    }

    /// Scroll position (scrollX, scrollY) via JS.
    pub async fn scroll_position(&self) -> Result<(i64, i64), BrowserError> {
        let v = self
            .evaluate("JSON.stringify({x: window.scrollX, y: window.scrollY})")
            .await?
            .value()
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "{}".into());
        let obj: Value = serde_json::from_str(&v)
            .map_err(|e| BrowserError::Other(format!("scroll parse: {e}")))?;
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

    /// Inject the neon-arrow virtual cursor overlay into every new document.
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
        let source = include_str!("../../tests/fixtures/cursor-overlay.js");
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
        let _ = self.evaluate(&expr).await;
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

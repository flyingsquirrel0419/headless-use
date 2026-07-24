//! Session: the high-level entry point integrating browser, input, observe,
//! diagnostics, and trace.
//!
//! A [`Session`] owns a [`Browser`] and at least one active [`Page`]. It exposes
//! the agent-facing operations: open, observe, click, type, scroll, drag, etc.
//! All actions are recorded into a [`crate::trace::Trace`] when tracing is on.
//!
//! ## Why a Session layer
//! The CLI/MCP/RPC frontends all share the same operations but differ in
//! transport. Centralizing operations here guarantees consistent error mapping,
//! reference resolution, stale detection, and tracing regardless of frontend.

pub mod console;
pub mod network;
pub mod wait;

pub use console::{ConsoleEntry, ConsoleLevel};
pub use network::{NetworkEntry, NetworkError};

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::browser::{Browser, BrowserError, LaunchOptions, Page};
use crate::input::{Keyboard, Mouse, MouseButton};
use crate::observe::{Observation, ObserveBuilder, RefId};
use crate::trace::Trace;

/// A long-lived browser session.
pub struct Session {
    browser: Arc<Browser>,
    page: Arc<Page>,
    trace: Option<Arc<tokio::sync::Mutex<Trace>>>,
    last_observation: tokio::sync::Mutex<Option<Observation>>,
}

impl Session {
    /// Launch a browser and create a single page, returning a Session.
    pub async fn start(opts: LaunchOptions) -> Result<Self, BrowserError> {
        let browser = Browser::launch(opts).await?;
        let page = Arc::new(browser.new_page().await?);
        Ok(Self {
            browser: Arc::new(browser),
            page,
            trace: None,
            last_observation: tokio::sync::Mutex::new(None),
        })
    }

    /// Attach tracing.
    pub fn with_trace(mut self, trace: Trace) -> Self {
        self.trace = Some(Arc::new(tokio::sync::Mutex::new(trace)));
        self
    }

    /// The active page.
    pub fn page(&self) -> &Page {
        &self.page
    }

    /// The browser handle.
    pub fn browser(&self) -> &Browser {
        &self.browser
    }

    /// Navigate the active page to `url`.
    pub async fn open(&self, url: &str) -> Result<(), BrowserError> {
        self.record_action("open", json!({ "url": url })).await;
        self.page.goto(url).await?;
        Ok(())
    }

    /// Navigate (alias).
    pub async fn goto(&self, url: &str) -> Result<(), BrowserError> {
        self.open(url).await
    }

    /// Reload the page.
    pub async fn reload(&self) -> Result<(), BrowserError> {
        self.record_action("reload", json!({})).await;
        let url = self.page.url().await.unwrap_or_default();
        self.page.goto(&url).await
    }

    /// Build a mouse engine.
    pub fn mouse(&self) -> Mouse<'_> {
        Mouse::new(&self.page)
    }

    /// Build a keyboard engine.
    pub fn keyboard(&self) -> Keyboard<'_> {
        Keyboard::new(&self.page)
    }

    /// Observe the current page and cache the result.
    pub async fn observe(&self) -> Result<Observation, BrowserError> {
        let obs = ObserveBuilder::new(&self.page).build().await?;
        *self.last_observation.lock().await = Some(obs.clone());
        Ok(obs)
    }

    /// The last cached observation (if any).
    pub async fn last_observation(&self) -> Option<Observation> {
        self.last_observation.lock().await.clone()
    }

    /// Resolve a reference to a current center point, scrolling it into view.
    ///
    /// Uses the cached observation's generation to detect staleness, then
    /// re-queries the element's current bounds (references are positional, not
    /// cached, so a layout shift within the same generation still resolves).
    pub async fn resolve_ref(&self, ref_id: RefId) -> Result<(f64, f64), BrowserError> {
        let obs = self.last_observation().await.ok_or_else(|| {
            BrowserError::ElementNotFound("no observation; run `observe` first".to_string())
        })?;
        let el = obs
            .get(ref_id)
            .ok_or_else(|| BrowserError::ElementNotFound(format!("@e{ref_id}")))?;
        if !el.enabled {
            return Err(BrowserError::ElementNotInteractable(format!(
                "@e{ref_id} is disabled"
            )));
        }
        // Re-resolve current bounds via JS using the selector hint or role/name.
        let center = self.resolve_element_center(el).await?;
        Ok(center)
    }

    async fn resolve_element_center(
        &self,
        el: &crate::observe::ElementRef,
    ) -> Result<(f64, f64), BrowserError> {
        // Re-resolve the element's current bounds. We inject the search text via
        // JSON.stringify so any character (Korean, quotes, backslashes) is safe
        // and cannot break the JS string literal.
        // [Decision Log]
        // - 목적과 의도: 한글/특수문자가 포함된 요소 이름으로 안전하게 클릭 좌표 재해결.
        // - 기존 구현 및 제약 조건: 이름을 JS 단일 따옴표 문자열에 직접 삽입하여
        //   따옴표/백슬래시 외의 특수문자에서 JS 파싱 에러 발생.
        // - 검토한 주요 대안: 수동 이스케이프 확장, base64 인코딩.
        // - 선택한 방식: serde_json::to_string으로 JSON 문자열 생성 후 JS에 주입.
        // - 다른 대안 대신 이 방식을 선택한 이유: 모든 Unicode 문자에 안전, 간결함.
        // - 장점, 단점 및 영향: 한글/이모지/따옴표 모두 안전; 약간의 직렬화 오버헤드.
        let expr = if !el.selector_hint.is_empty() && el.selector_hint.starts_with('#') {
            let id = css_escape_id(el.selector_hint.trim_start_matches('#'));
            format!(
                "(()=>{{const e=document.querySelector('#'+'{id}');if(!e)return null;e.scrollIntoView({{block:'center'}});const r=e.getBoundingClientRect();return JSON.stringify({{x:r.x,y:r.y,w:r.width,h:r.height}});}})()"
            )
        } else {
            let name_json = serde_json::to_string(&el.name).unwrap_or_else(|_| "\"\"".into());
            let tag = tag_for_query(&el.tag_name);
            format!(
                "(()=>{{const els=[...document.querySelectorAll('{tag}')];const t={name_json};const e=els.find(x=>((x.innerText||x.textContent||'').trim()===t)||(x.getAttribute('aria-label')===t)||(x.getAttribute('placeholder')===t));if(!e)return null;e.scrollIntoView({{block:'center'}});const r=e.getBoundingClientRect();return JSON.stringify({{x:r.x,y:r.y,w:r.width,h:r.height}});}})()"
            )
        };
        let result = self.page.evaluate(&expr).await?;
        let value_str = result.value().and_then(|v| v.as_str()).map(String::from);
        let Some(s) = value_str else {
            return Err(BrowserError::ElementNotFound(format!(
                "@e{} could not be re-resolved (page changed; run observe again)",
                el.ref_id
            )));
        };
        if s == "null" || s.is_empty() {
            return Err(BrowserError::ElementNotFound(format!(
                "@e{} no longer exists (stale; run observe again)",
                el.ref_id
            )));
        }
        let v: serde_json::Value = serde_json::from_str(&s)
            .map_err(|e| BrowserError::Other(format!("bounds parse: {e}")))?;
        let x = v.get("x").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let y = v.get("y").and_then(|y| y.as_f64()).unwrap_or(0.0);
        let w = v.get("w").and_then(|w| w.as_f64()).unwrap_or(0.0);
        let h = v.get("h").and_then(|h| h.as_f64()).unwrap_or(0.0);
        if w <= 0.0 || h <= 0.0 {
            return Err(BrowserError::ElementNotInteractable(format!(
                "@e{} has zero size (hidden or collapsed)",
                el.ref_id
            )));
        }
        Ok((x + w / 2.0, y + h / 2.0))
    }

    /// Click a reference or coordinate.
    pub async fn click(
        &self,
        target: ClickTarget,
        button: MouseButton,
        count: u32,
        modifiers: crate::input::Modifiers,
        hold: Duration,
    ) -> Result<(), BrowserError> {
        let (x, y) = self.resolve_click_point(target).await?;
        self.record_action(
            "mouse.click",
            json!({ "x": x, "y": y, "button": button.as_cdp(), "count": count }),
        )
        .await;
        self.mouse()
            .click((x, y).into(), button, count, modifiers, hold)
            .await
    }

    /// Hover a reference or coordinate.
    pub async fn hover(&self, target: ClickTarget) -> Result<(), BrowserError> {
        let (x, y) = self.resolve_click_point(target).await?;
        self.record_action("hover", json!({ "x": x, "y": y })).await;
        self.mouse()
            .move_to((x, y).into(), crate::input::Modifiers::NONE)
            .await
    }

    async fn resolve_click_point(&self, target: ClickTarget) -> Result<(f64, f64), BrowserError> {
        match target {
            ClickTarget::Ref(id) => self.resolve_ref(id).await,
            ClickTarget::Point(p) => Ok((p.x, p.y)),
        }
    }

    /// Type text into the focused element.
    pub async fn type_text(
        &self,
        text: &str,
        delay: Duration,
        sensitive: bool,
    ) -> Result<(), BrowserError> {
        let payload = if sensitive {
            json!({ "text": "[REDACTED]", "sensitive": true })
        } else {
            json!({ "text": text })
        };
        self.record_action("type", payload).await;
        self.keyboard().type_text(text, delay).await
    }

    /// Insert text (CJK/emoji friendly).
    pub async fn insert_text(&self, text: &str, sensitive: bool) -> Result<(), BrowserError> {
        let payload = if sensitive {
            json!({ "text": "[REDACTED]", "sensitive": true })
        } else {
            json!({ "text": text })
        };
        self.record_action("insert-text", payload).await;
        self.keyboard().insert_text(text).await
    }

    /// Press a key chord.
    pub async fn key_press(&self, chord: &str) -> Result<(), BrowserError> {
        let (mods, key) =
            crate::input::keyboard::parse_chord(chord).map_err(BrowserError::InvalidInput)?;
        self.record_action("key.press", json!({ "chord": chord }))
            .await;
        if let Some(k) = key {
            self.keyboard().press(&k, mods).await
        } else {
            Ok(())
        }
    }

    /// Screenshot to bytes.
    pub async fn screenshot(&self, full_page: bool) -> Result<Vec<u8>, BrowserError> {
        self.record_action("screenshot", json!({ "fullPage": full_page }))
            .await;
        self.page.screenshot(full_page, None).await
    }

    /// Scroll by delta.
    pub async fn scroll(
        &self,
        dx: f64,
        dy: f64,
        at: Option<crate::input::Point>,
        duration: Duration,
        steps: u32,
    ) -> Result<(), BrowserError> {
        self.record_action("scroll", json!({ "dx": dx, "dy": dy }))
            .await;
        self.mouse()
            .scroll(dx, dy, at, crate::input::Modifiers::NONE, duration, steps)
            .await
    }

    /// Drag from->to.
    pub async fn drag(
        &self,
        from: crate::input::Point,
        to: crate::input::Point,
        button: MouseButton,
        duration: Duration,
        steps: u32,
    ) -> Result<(), BrowserError> {
        self.record_action(
            "mouse.drag",
            json!({ "from": [from.x, from.y], "to": [to.x, to.y] }),
        )
        .await;
        self.mouse()
            .drag(
                from,
                to,
                button,
                crate::input::Modifiers::NONE,
                duration,
                steps,
            )
            .await
    }

    /// Wait until the page is stable.
    pub async fn wait(&self, opts: wait::WaitOptions) -> Result<wait::WaitResult, BrowserError> {
        let r = wait::wait_until_stable(&self.page, opts).await?;
        self.record_action("wait", json!({ "stable": r.stable, "reason": r.reason }))
            .await;
        Ok(r)
    }

    /// Collect console entries.
    pub async fn console(&self) -> Result<Vec<ConsoleEntry>, BrowserError> {
        console::collect(&self.page).await
    }

    /// Collect network entries.
    pub async fn network(&self) -> Result<Vec<NetworkEntry>, BrowserError> {
        network::collect(&self.page).await
    }

    /// Shut down the session, flushing trace.
    pub async fn shutdown(self) {
        if let Some(t) = self.trace.as_ref() {
            let _ = t.lock().await.flush().await;
        }
        self.browser.shutdown().await;
    }

    async fn record_action(&self, kind: &str, params: serde_json::Value) {
        if let Some(t) = self.trace.as_ref() {
            t.lock().await.record(kind, params).await;
        }
    }
}

/// Where to click: a semantic reference or a viewport point.
#[derive(Debug, Clone)]
pub enum ClickTarget {
    /// A reference id from the last observe.
    Ref(RefId),
    /// A viewport coordinate.
    Point(crate::input::Point),
}

impl From<RefId> for ClickTarget {
    fn from(id: RefId) -> Self {
        ClickTarget::Ref(id)
    }
}

impl From<crate::input::Point> for ClickTarget {
    fn from(p: crate::input::Point) -> Self {
        ClickTarget::Point(p)
    }
}

/// CSS-escape an id for safe querySelector. Only allows safe chars.
fn css_escape_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Map a tag name to a query selector for name-based re-resolution.
fn tag_for_query(tag: &str) -> &'static str {
    match tag {
        "button" => "button, [role='button'], input[type='submit'], input[type='button']",
        "a" => "a[href], [role='link']",
        "input" => "input",
        "textarea" => "textarea",
        "select" => "select",
        "summary" => "summary",
        _ => "button, a[href], input, textarea, select, [role='button'], [role='link'], [role='checkbox'], [role='radio'], [role='tab'], summary",
    }
}

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
pub mod network_tracker;
pub mod wait;

pub use console::{ConsoleEntry, ConsoleLevel};
pub use network::{NetworkEntry, NetworkError};

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::browser::{Browser, BrowserError, LaunchOptions, Page};
use crate::input::{Keyboard, Mouse, MouseButton};
use crate::observe::{Observation, ObserveBuilder, RefId};
use crate::security::Policy;
use crate::trace::Trace;

/// A long-lived browser session.
pub struct Session {
    browser: Arc<Browser>,
    page: Arc<Page>,
    /// Trace recorder, behind a lock so it can be started/stopped at runtime
    /// via `trace.start`/`trace.stop` JSON-RPC without rebuilding the Session.
    trace: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Mutex<Trace>>>>>,
    last_observation: tokio::sync::Mutex<Option<Observation>>,
    /// Persistent cursor position shared across all Mouse instances.
    /// Without this, each mouse() call creates a fresh Mouse with pos=(0,0),
    /// breaking move→down→move→up sequences.
    cursor_pos: std::sync::Arc<tokio::sync::Mutex<crate::input::Point>>,
    /// Persistent button state shared across all Mouse instances.
    cursor_buttons: std::sync::Arc<tokio::sync::Mutex<u8>>,
    /// Navigation counter, incremented on every main-frame document change.
    /// Driven by CDP `Page.frameNavigated` (subscribed in [`Session::start`]),
    /// so ANY navigation invalidates references: button-click nav, form submit,
    /// redirect, `location.href`, reload, history back/forward — not just
    /// explicit `Session::open`. Stored behind an `Arc` so the background
    /// `frameNavigated` listener task can increment it without borrowing `self`.
    nav_generation: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// CDP event-based network tracker for accurate in-flight request counts.
    network_tracker: Arc<tokio::sync::Mutex<network_tracker::NetworkTracker>>,
    /// Host allow/deny navigation policy. When set, open/goto/reload consult it
    /// and return [`BrowserError::NavigationBlocked`] for disallowed hosts.
    policy: Arc<tokio::sync::Mutex<Policy>>,
    /// How the cursor travels to click/hover targets. A plain value, not a
    /// lock: it is chosen once when the session is built and never mutated, so
    /// there is nothing to guard.
    cursor_motion: crate::input::CursorMotion,
}

impl Session {
    /// Launch a browser and create a single page, returning a Session.
    pub async fn start(opts: LaunchOptions) -> Result<Self, BrowserError> {
        let browser = Browser::launch(opts).await?;
        let page = Arc::new(browser.new_page().await?);
        // Start CDP network event tracking for accurate wait-until-stable.
        // This tracks ALL requests (img, script, css, fetch, xhr) via CDP events,
        // not just JS-wrapped fetch/XHR.
        // (Tracker is started lazily on first wait to avoid issues during page creation.)
        let session = Self {
            browser: Arc::new(browser),
            page,
            trace: Arc::new(std::sync::Mutex::new(None)),
            last_observation: tokio::sync::Mutex::new(None),
            cursor_pos: std::sync::Arc::new(tokio::sync::Mutex::new(crate::input::Point::new(
                0.0, 0.0,
            ))),
            cursor_buttons: std::sync::Arc::new(tokio::sync::Mutex::new(0)),
            nav_generation: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            network_tracker: Arc::new(tokio::sync::Mutex::new(
                network_tracker::NetworkTracker::new(),
            )),
            policy: Arc::new(tokio::sync::Mutex::new(Policy::default())),
            cursor_motion: crate::input::CursorMotion::default(),
        };
        // Start CDP network tracking BEFORE any navigation so we don't miss
        // requestWillBeSent for requests already in flight.
        session
            .network_tracker
            .lock()
            .await
            .start(&session.page)
            .await?;
        // Subscribe to Page.frameNavigated so ANY main-frame document change
        // increments the navigation generation and invalidates observe
        // references. This catches button-click navigation, form submit,
        // redirect, location.href, reload, and history navigation — all of
        // which would otherwise leave stale references usable.
        // The broadcast event bus means this subscriber coexists with the
        // network tracker's subscriber.
        let page_client = session.page.cdp().clone();
        let nav_gen = session.nav_generation.clone();
        let mut page_events = page_client.subscribe_events_async().await;
        tokio::spawn(async move {
            while let Some(ev) = page_events.recv().await {
                if ev.method == "Page.frameNavigated" {
                    // Only invalidate on top-level (main frame) navigations.
                    // A frame with no parentId is the main frame. Same-document
                    // navigations (fragments, pushState) arrive as
                    // Page.navigatedWithinDocument and do not invalidate refs.
                    let is_main = ev
                        .params
                        .get("frame")
                        .and_then(|f| f.get("parentId"))
                        .is_none();
                    if is_main {
                        nav_gen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        });
        // Ensure Page domain is enabled so frameNavigated is delivered.
        let _ = session.page.enable("Page.enable", None).await;
        Ok(session)
    }

    /// Attach tracing.
    pub fn with_trace(self, trace: Trace) -> Self {
        // with_trace is a sync builder; tokio::Mutex::blocking_lock requires a
        // runtime to be available. Callers in async tests have one; CLI paths
        // use with_trace_async instead.
        *self.trace.lock().unwrap() = Some(Arc::new(tokio::sync::Mutex::new(trace)));
        self
    }

    /// Async version of [`Self::with_trace`].
    pub async fn with_trace_async(self, trace: Trace) -> Self {
        *self.trace.lock().unwrap() = Some(Arc::new(tokio::sync::Mutex::new(trace)));
        self
    }

    /// Start tracing at runtime, creating a new trace under `base`.
    /// Returns the run directory path. If tracing is already active, returns
    /// the existing run directory without creating a new one.
    pub async fn trace_start(&self, base: &std::path::Path) -> Result<String, BrowserError> {
        // Clone the existing trace Arc (if any) out of the guard before awaiting
        // so the std::sync::MutexGuard is not held across an await point.
        let existing = self.trace.lock().unwrap().as_ref().cloned();
        if let Some(t) = existing {
            return Ok(t.lock().await.dir().to_string_lossy().to_string());
        }
        let trace = Trace::new(base)
            .await
            .map_err(|e| BrowserError::Other(format!("trace start: {e}")))?;
        let dir = trace.dir().to_string_lossy().to_string();
        *self.trace.lock().unwrap() = Some(Arc::new(tokio::sync::Mutex::new(trace)));
        Ok(dir)
    }

    /// Stop tracing, flush the trace, and return the run directory path.
    pub async fn trace_stop(&self) -> Result<String, BrowserError> {
        let trace_opt = self.trace.lock().unwrap().take();
        if let Some(t) = trace_opt {
            let mut trace = t.lock().await;
            trace
                .flush()
                .await
                .map_err(|e| BrowserError::Other(format!("trace flush: {e}")))?;
            Ok(trace.dir().to_string_lossy().to_string())
        } else {
            Err(BrowserError::Other("no active trace to stop".into()))
        }
    }

    /// Attach a host allow/deny navigation policy. When set, navigation to a
    /// host not on the allow list (or on the deny list) returns
    /// [`BrowserError::NavigationBlocked`] before any request is made.
    /// A configured policy also restricts navigation to `http`/`https`.
    ///
    /// There is no blocking variant: an earlier `with_policy` used
    /// `Mutex::blocking_lock`, which panics when called from inside a tokio
    /// runtime — i.e. from every real caller.
    pub async fn with_policy_async(self, policy: Policy) -> Self {
        *self.policy.lock().await = policy;
        self
    }

    /// Choose how the cursor travels to click/hover targets.
    ///
    /// [`CursorMotion::Smooth`](crate::input::CursorMotion::Smooth) makes the
    /// cursor walk the path, emitting real intermediate `mouseMoved` events —
    /// visible in the live viewer, and enough to open hover menus that need
    /// movement. It costs the travel time on every click, so automation
    /// surfaces leave the default [`CursorMotion::Instant`](crate::input::CursorMotion::Instant).
    ///
    /// Synchronous and lock-free: `cursor_motion` is a plain field.
    pub fn with_cursor_motion(mut self, motion: crate::input::CursorMotion) -> Self {
        self.cursor_motion = motion;
        self
    }

    /// The configured cursor motion profile.
    pub fn cursor_motion(&self) -> crate::input::CursorMotion {
        self.cursor_motion
    }

    /// Current cursor position (shared state, persistent across mouse() calls).
    pub async fn cursor_position(&self) -> crate::input::Point {
        *self.cursor_pos.lock().await
    }

    /// Current navigation generation value. Increments on every main-frame
    /// document change via `Page.frameNavigated`. Exposed for diagnostics and
    /// tests; production callers use `resolve_ref` which checks staleness
    /// internally.
    pub fn nav_generation_value(&self) -> u32 {
        self.nav_generation
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Current in-flight network request count (via CDP events).
    pub async fn network_pending(&self) -> u64 {
        self.network_tracker.lock().await.pending().await
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
        self.check_policy(url).await?;
        self.record_action("open", json!({ "url": url })).await;
        self.page.goto(url).await?;
        // nav_generation is incremented by the Page.frameNavigated listener
        // spawned in start(). We do NOT bump it here to avoid double-counting:
        // every real document change (including this navigation) fires
        // frameNavigated, which is the single source of truth for generation.
        Ok(())
    }

    /// Navigate (alias).
    pub async fn goto(&self, url: &str) -> Result<(), BrowserError> {
        self.open(url).await
    }

    /// Reload the page.
    ///
    /// Issues CDP `Page.reload` rather than re-navigating to the current URL.
    /// A re-navigation is a different operation: it drops the history entry's
    /// POST body and scroll restoration, so a reloaded form-result page came
    /// back as a fresh GET. The policy is still consulted for the current URL,
    /// which is what a reload will re-request.
    pub async fn reload(&self) -> Result<(), BrowserError> {
        let url = self.page.url().await?;
        self.check_policy(&url).await?;
        self.record_action("reload", json!({})).await;
        self.page.reload().await
    }

    /// Build a mouse engine sharing the session's persistent cursor state.
    pub fn mouse(&self) -> Mouse<'_> {
        Mouse::new_with_state(
            &self.page,
            self.cursor_pos.clone(),
            self.cursor_buttons.clone(),
        )
    }

    /// Build a keyboard engine.
    pub fn keyboard(&self) -> Keyboard<'_> {
        Keyboard::new(&self.page)
    }

    /// Observe the current page and cache the result.
    pub async fn observe(&self) -> Result<Observation, BrowserError> {
        let mut obs = ObserveBuilder::new(&self.page).build().await?;
        // Stamp the observation with the current navigation generation so
        // resolve_ref can detect stale references after navigation.
        obs.nav_generation = self
            .nav_generation
            .load(std::sync::atomic::Ordering::Relaxed);
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
    /// Resolve a reference to a current center point.
    ///
    /// If `expected_generation` is provided (from `@g<gen>:e<num>` format),
    /// it is compared against the current observation's generation. A mismatch
    /// means the reference is stale.
    pub async fn resolve_ref(&self, ref_id: RefId) -> Result<(f64, f64), BrowserError> {
        self.resolve_ref_with_generation(ref_id, None).await
    }

    /// Resolve a reference with an explicit expected generation for stale detection.
    pub async fn resolve_ref_with_generation(
        &self,
        ref_id: RefId,
        expected_generation: Option<u32>,
    ) -> Result<(f64, f64), BrowserError> {
        let (x, y, w, h) = self
            .resolve_ref_rect_with_generation(ref_id, expected_generation)
            .await?;
        Ok((x + w / 2.0, y + h / 2.0))
    }

    /// Resolve a reference to its current **viewport-relative** bounding box
    /// `(x, y, width, height)`, scrolling it into view first.
    ///
    /// This is the shared primitive behind click-point resolution and element
    /// screenshots. Callers that need document coordinates (e.g.
    /// `Page.captureScreenshot`'s `clip`) must add the page scroll offset —
    /// read it *after* this call, since scrolling the element into view moves
    /// the page.
    pub async fn resolve_ref_rect_with_generation(
        &self,
        ref_id: RefId,
        expected_generation: Option<u32>,
    ) -> Result<(f64, f64, f64, f64), BrowserError> {
        let obs = self.last_observation().await.ok_or_else(|| {
            BrowserError::ElementNotFound("no observation; run `observe` first".to_string())
        })?;
        // Stale reference detection: if navigation occurred since the observation
        // was taken, the reference belongs to an old page and must not be used.
        let current_nav = self
            .nav_generation
            .load(std::sync::atomic::Ordering::Relaxed);
        if current_nav != obs.nav_generation {
            return Err(BrowserError::StaleReference(format!(
                "@e{ref_id} belongs to page generation {}, but the current page is generation {}. Run `observe` again.",
                obs.nav_generation, current_nav
            )));
        }
        // Also check explicit generation from @g<gen>:e<num> format.
        if let Some(gen) = expected_generation {
            if gen != obs.generation {
                return Err(BrowserError::StaleReference(format!(
                    "@g{gen}:e{ref_id} belongs to observe generation {}, but the current generation is {}. Run `observe` again.",
                    gen, obs.generation
                )));
            }
        }
        let el = obs
            .get(ref_id)
            .ok_or_else(|| BrowserError::ElementNotFound(format!("@e{ref_id}")))?;
        if !el.enabled {
            return Err(BrowserError::ElementNotInteractable(format!(
                "@e{ref_id} is disabled"
            )));
        }
        // Re-resolve current bounds via JS using the selector hint or role/name.
        self.resolve_element_rect(el).await
    }

    /// Re-query an observed element's current viewport-relative bounding box.
    async fn resolve_element_rect(
        &self,
        el: &crate::observe::ElementRef,
    ) -> Result<(f64, f64, f64, f64), BrowserError> {
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
            // [Decision Log] — querySelector 인자 안전 주입
            // - 목적과 의도: tag_for_query가 `a[href], [role='link']`처럼 작은따옴표를
            //   포함하는 선택자를 반환한다. 이전에는 이 선택자를 JS 문자열 리터럴
            //   안의 작은따옴표 영역(`'{tag}'`)에 직접 끼워 넣었기 때문에 안쪽
            //   작은따옴표가 문자열을 일찍 닫아버려 `SyntaxError: missing ) after
            //   argument list`가 발생했다. (구글 결과 페이지의 링크 클릭이 전부
            //   실패한 원인.)
            // - 검토한 주요 대안: 선택자에서 작은따옴표를 제거, 큰따옴표로 래핑.
            // - 선택한 방식: name과 동일하게 tag도 JSON 문자열로 인코딩하여 JS
            //   변수에 할당한 뒤 querySelectorAll에 전달.
            // - 장점: 임의의 CSS 선택자 문자에 안전, name과 일관된 패턴.
            let tag_json = serde_json::to_string(tag).unwrap_or_else(|_| "\"*\"".into());
            format!(
                "(()=>{{const sel={tag_json};const els=[...document.querySelectorAll(sel)];const t={name_json};const e=els.find(x=>((x.innerText||x.textContent||'').trim()===t)||(x.getAttribute('aria-label')===t)||(x.getAttribute('placeholder')===t));if(!e)return null;e.scrollIntoView({{block:'center'}});const r=e.getBoundingClientRect();return JSON.stringify({{x:r.x,y:r.y,w:r.width,h:r.height}});}})()"
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
        Ok((x, y, w, h))
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
        // Walk the cursor over first when smooth motion is on. `Mouse::click`
        // skips its own move once the cursor is already at the target, so this
        // does not double-dispatch.
        self.travel_to((x, y).into(), modifiers).await?;
        self.mouse()
            .click((x, y).into(), button, count, modifiers, hold)
            .await
    }

    /// Hover a reference or coordinate.
    pub async fn hover(&self, target: ClickTarget) -> Result<(), BrowserError> {
        let (x, y) = self.resolve_click_point(target).await?;
        self.record_action("hover", json!({ "x": x, "y": y })).await;
        self.travel_to((x, y).into(), crate::input::Modifiers::NONE)
            .await
    }

    /// Move the cursor to `p` according to the session's
    /// [`crate::input::CursorMotion`].
    async fn travel_to(
        &self,
        p: crate::input::Point,
        modifiers: crate::input::Modifiers,
    ) -> Result<(), BrowserError> {
        match self.cursor_motion {
            crate::input::CursorMotion::Instant => self.mouse().move_to(p, modifiers).await,
            crate::input::CursorMotion::Smooth { duration, steps } => {
                self.mouse()
                    .move_smooth(p, modifiers, duration, steps)
                    .await
            }
        }
    }

    async fn resolve_click_point(&self, target: ClickTarget) -> Result<(f64, f64), BrowserError> {
        match target {
            ClickTarget::Ref { id, generation } => {
                self.resolve_ref_with_generation(id, generation).await
            }
            ClickTarget::Point(p) => Ok((p.x, p.y)),
        }
    }

    /// Resolve a click target from a ref string that may include generation.
    pub async fn resolve_click_string(&self, ref_str: &str) -> Result<(f64, f64), BrowserError> {
        let (id, gen) = crate::observe::parse_ref_with_generation(ref_str)
            .map_err(BrowserError::InvalidInput)?;
        self.resolve_ref_with_generation(id, gen).await
    }

    /// Build a ClickTarget from a ref string (e.g. "@e3" or "@g12:e3").
    /// The generation is preserved so stale detection works end-to-end.
    pub fn click_target_from_ref(ref_str: &str) -> Result<ClickTarget, BrowserError> {
        let (id, gen) = crate::observe::parse_ref_with_generation(ref_str)
            .map_err(BrowserError::InvalidInput)?;
        Ok(ClickTarget::Ref {
            id,
            generation: gen,
        })
    }

    /// Type text into the focused element.
    ///
    /// Auto-detects password fields: if the focused element is a password input
    /// (type=password or autocomplete includes password), the text is treated
    /// as sensitive and redacted in the trace even when the caller did not pass
    /// `sensitive=true`. This prevents accidental password leakage when an
    /// agent forgets the flag. A final masking pass in `Trace::record` also
    /// redacts any key/value whose name looks like a secret.
    pub async fn type_text(
        &self,
        text: &str,
        delay: Duration,
        sensitive: bool,
    ) -> Result<(), BrowserError> {
        let auto_sensitive = self.is_focused_sensitive().await;
        let sensitive = sensitive || auto_sensitive;
        let payload = if sensitive {
            json!({ "text": "[REDACTED]", "sensitive": true })
        } else {
            json!({ "text": text })
        };
        self.record_action("type", payload).await;
        self.keyboard().type_text(text, delay).await
    }

    /// Insert text (CJK/emoji friendly).
    /// Auto-detects password fields for trace redaction.
    pub async fn insert_text(&self, text: &str, sensitive: bool) -> Result<(), BrowserError> {
        let auto_sensitive = self.is_focused_sensitive().await;
        let sensitive = sensitive || auto_sensitive;
        let payload = if sensitive {
            json!({ "text": "[REDACTED]", "sensitive": true })
        } else {
            json!({ "text": text })
        };
        self.record_action("insert-text", payload).await;
        self.keyboard().insert_text(text).await
    }

    /// Check if the currently focused element is a password/sensitive field.
    async fn is_focused_sensitive(&self) -> bool {
        let expr = r#"(() => {
            const el = document.activeElement;
            if (!el) return false;
            if (el.type === 'password') return true;
            const ac = el.getAttribute('autocomplete') || '';
            if (ac.includes('password') || ac.includes('current-password') || ac.includes('new-password')) return true;
            return false;
        })()"#;
        self.page
            .evaluate(expr)
            .await
            .ok()
            .and_then(|r| r.value().and_then(|v| v.as_bool()))
            .unwrap_or(false)
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

    /// Capture a screenshot. When `element` is `Some`, only that element's
    /// region is captured (resolved to a clip rectangle via the current
    /// observation, scrolling into view first). When `full_page` is true the
    /// whole scrollable content is captured. The two are mutually exclusive:
    /// an element clip takes precedence over `full_page`.
    ///
    /// When tracing is enabled, the PNG is also saved into the run's
    /// `screenshots/` directory so it appears in `report.html`.
    pub async fn screenshot(
        &self,
        full_page: bool,
        element: Option<ClickTarget>,
    ) -> Result<Vec<u8>, BrowserError> {
        let clip = match &element {
            Some(target) => Some(self.element_clip(target.clone()).await?),
            None => None,
        };
        let element_ref = match &element {
            Some(ClickTarget::Ref { id, .. }) => Some(format!("@e{id}")),
            _ => None,
        };
        let seq = self
            .record_action(
                "screenshot",
                json!({ "fullPage": full_page, "element": element_ref }),
            )
            .await;
        let data = self.page.screenshot(full_page, clip).await?;
        // Save to trace so report.html can embed the image, using the same
        // sequence number as the action so the report can match them.
        let trace = self.trace.lock().unwrap().as_ref().cloned();
        if let Some(t) = trace {
            let _ = t.lock().await.save_screenshot(seq, &data).await;
        }
        Ok(data)
    }

    /// Compute the `Page.captureScreenshot` clip rectangle for a target, in
    /// **document** coordinates.
    ///
    /// Two things this has to get right:
    ///
    /// 1. `getBoundingClientRect()` is viewport-relative but CDP's `clip` is
    ///    document-relative, so the page scroll offset must be added. Without
    ///    it, any element screenshot taken on a scrolled page captured the
    ///    wrong region. The offset is read *after* the element is scrolled into
    ///    view, because that scroll changes it.
    /// 2. For a `Ref` target we use the referenced element's own box. Going
    ///    through `elementFromPoint` at the element's center (the previous
    ///    behavior) returns the topmost *descendant* under that point — an
    ///    inner `<span>` rather than the `<button>` that was referenced — so
    ///    the clip came out too small. `elementFromPoint` is still the right
    ///    tool for a bare coordinate target, where there is no element to
    ///    resolve.
    async fn element_clip(
        &self,
        target: ClickTarget,
    ) -> Result<(f64, f64, f64, f64), BrowserError> {
        let (x, y, w, h) = match target {
            ClickTarget::Ref { id, generation } => {
                self.resolve_ref_rect_with_generation(id, generation)
                    .await?
            }
            ClickTarget::Point(p) => self.element_clip_at(p.x, p.y).await?,
        };
        let (scroll_x, scroll_y) = self.page.scroll_position().await?;
        Ok((x + scroll_x as f64, y + scroll_y as f64, w, h))
    }

    /// Compute a viewport-relative clip rectangle for the element under
    /// viewport point (x, y), via `elementFromPoint`.
    async fn element_clip_at(&self, x: f64, y: f64) -> Result<(f64, f64, f64, f64), BrowserError> {
        let expr = format!(
            "(()=>{{const e=document.elementFromPoint({x},{y});if(!e)return null;const r=e.getBoundingClientRect();if(r.width<=0||r.height<=0)return null;return JSON.stringify({{x:r.x,y:r.y,w:r.width,h:r.height}});}})()"
        );
        let s = self
            .page
            .evaluate(&expr)
            .await?
            .value()
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default();
        if s.is_empty() || s == "null" {
            return Err(BrowserError::ElementNotInteractable(format!(
                "no element at ({x:.0},{y:.0}) to screenshot"
            )));
        }
        let v: serde_json::Value = serde_json::from_str(&s)
            .map_err(|e| BrowserError::Other(format!("clip parse: {e}")))?;
        Ok((
            v.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0),
            v.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0),
            v.get("w").and_then(|v| v.as_f64()).unwrap_or(0.0),
            v.get("h").and_then(|v| v.as_f64()).unwrap_or(0.0),
        ))
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
        let r = wait::wait_until_stable(&self.page, opts, &self.network_tracker).await?;
        self.record_action("wait", json!({ "stable": r.stable, "reason": r.reason }))
            .await;
        Ok(r)
    }

    /// Collect console entries.
    pub async fn console(&self) -> Result<Vec<ConsoleEntry>, BrowserError> {
        console::collect(&self.page).await
    }

    /// Collect network entries from the shared CDP-derived request history.
    /// This is the same history `wait` uses, so diagnostics and stability
    /// checks always agree on what happened.
    pub async fn network(&self) -> Result<Vec<NetworkEntry>, BrowserError> {
        network::collect(&self.page, &self.network_tracker).await
    }

    /// Shut down the session, flushing trace.
    pub async fn shutdown(self) {
        let trace_opt = self.trace.lock().unwrap().clone();
        if let Some(t) = trace_opt {
            let _ = t.lock().await.flush().await;
        }
        self.browser.shutdown().await;
    }

    /// Check the navigation policy for `url`. Returns
    /// [`BrowserError::NavigationBlocked`] if the host is not permitted.
    async fn check_policy(&self, url: &str) -> Result<(), BrowserError> {
        let policy = self.policy.lock().await;
        if let Err(denial) = policy.allows(url) {
            return Err(BrowserError::NavigationBlocked(format!(
                "URL '{url}' rejected: {denial}"
            )));
        }
        Ok(())
    }

    /// Record an action and return its sequence number (0 if tracing is off).
    async fn record_action(&self, kind: &str, params: serde_json::Value) -> u64 {
        // Clone the Arc out of the guard before awaiting so the
        // std::sync::MutexGuard is not held across an await point.
        let trace = self.trace.lock().unwrap().as_ref().cloned();
        if let Some(t) = trace {
            t.lock().await.record(kind, params).await
        } else {
            0
        }
    }

    /// Record an arbitrary action with raw params (for tests verifying the
    /// final secret-masking defense layer in `Trace::record`). Production code
    /// should use the typed action methods instead.
    #[doc(hidden)]
    pub async fn record_action_raw(&self, kind: &str, params: serde_json::Value) {
        self.record_action(kind, params).await;
    }
}

/// Where to click: a semantic reference or a viewport point.
#[derive(Debug, Clone)]
pub enum ClickTarget {
    /// A reference id from the last observe, with an optional generation
    /// for stale detection. When generation is Some, resolve checks it
    /// against the current observation's generation.
    Ref {
        /// The reference id from observe.
        id: RefId,
        /// Optional generation for stale detection.
        generation: Option<u32>,
    },
    /// A viewport coordinate.
    Point(crate::input::Point),
}

impl From<RefId> for ClickTarget {
    fn from(id: RefId) -> Self {
        ClickTarget::Ref {
            id,
            generation: None,
        }
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

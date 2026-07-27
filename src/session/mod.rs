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
//!
//! ## Layout
//! `Session`'s inherent methods are split across sibling modules rather than
//! living in one file. Child modules can reach the struct's private fields, so
//! the split costs no encapsulation:
//!
//! - this file — lifecycle, policy, navigation, input, tracing, diagnostics
//! - [`resolve`] — turning an observed `@eN` back into live coordinates
//! - [`capture`] — screenshots, annotation, dewiggle, clip math
//! - [`console`], [`network`], [`network_tracker`], [`wait`] — page diagnostics
//!
//! This file was a 1000-line catch-all holding JS string generation, CSS
//! escaping, dewiggle orchestration and clip arithmetic next to `start()`.

pub mod capture;
pub mod click_report;
pub mod console;
pub mod network;
pub mod network_tracker;
pub mod resolve;
pub mod wait;

pub use capture::DewiggleOutput;
pub use click_report::{ClickEffects, ClickReport, HitInfo};
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
    ///
    /// A bare `Arc`, not `Arc<Mutex<_>>`: every `NetworkTracker` method takes
    /// `&self` and locks its own interior state, so an outer mutex only added a
    /// second lock to acquire — and a lock-ordering hazard — on every call.
    network_tracker: Arc<network_tracker::NetworkTracker>,
    /// Host allow/deny navigation policy. When set, open/goto/reload consult it
    /// and return [`BrowserError::NavigationBlocked`] for disallowed hosts.
    policy: Arc<tokio::sync::Mutex<Policy>>,
    /// How the cursor travels to click/hover targets. A plain value, not a
    /// lock: it is chosen once when the session is built and never mutated, so
    /// there is nothing to guard.
    cursor_motion: crate::input::CursorMotion,
    /// Requests rejected by the host policy at the network layer.
    policy_blocks: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Post-click effect observation window. Zero disables effect sampling.
    click_observe_window: Duration,
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
            network_tracker: Arc::new(network_tracker::NetworkTracker::new()),
            policy: Arc::new(tokio::sync::Mutex::new(Policy::default())),
            cursor_motion: crate::input::CursorMotion::default(),
            policy_blocks: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            click_observe_window: Duration::from_millis(300),
        };
        // Start CDP network tracking BEFORE any navigation so we don't miss
        // requestWillBeSent for requests already in flight.
        session.network_tracker.start(&session.page).await?;
        // Subscribe to Page.frameNavigated so ANY main-frame document change
        // increments the navigation generation and invalidates observe
        // references. This catches button-click navigation, form submit,
        // redirect, location.href, reload, and history navigation — all of
        // which would otherwise leave stale references usable.
        // The broadcast event bus means this subscriber coexists with the
        // network tracker's subscriber.
        let page_client = session.page.cdp().clone();
        let nav_gen = session.nav_generation.clone();
        let own_session = session.page.session_id().to_string();
        let mut page_events = page_client.subscribe_events_async().await;
        tokio::spawn(async move {
            while let Some(ev) = page_events.recv().await {
                // The bus carries events from every attached target. A popup or
                // OOPIF navigating is not this page navigating, and counting it
                // would invalidate refs that are still perfectly valid.
                if ev.session_id.as_deref() != Some(own_session.as_str()) {
                    continue;
                }
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
        //
        // This is deliberately fatal rather than best-effort. Every stale-@eN
        // guarantee this session makes rests on `Page.frameNavigated` arriving:
        // if the domain is not enabled the generation counter never moves, and
        // `resolve_ref_rect_with_generation` keeps handing out coordinates for
        // a page that no longer exists. A session that cannot detect navigation
        // is more dangerous than one that fails to start, so it fails to start.
        session.page.enable("Page.enable", None).await?;
        Ok(session)
    }

    /// Attach tracing.
    ///
    /// Synchronous, and safe to call from async code: the guard is a
    /// `std::sync::Mutex` held only for the assignment, never across an await.
    /// There used to be a `with_trace_async` beside this with a byte-identical
    /// body — an async fn that awaited nothing, kept because the name suggested
    /// it was needed.
    pub fn with_trace(self, trace: Trace) -> Self {
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
            .map_err(|e| BrowserError::Trace(format!("start: {e}")))?;
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
                .map_err(|e| BrowserError::Trace(format!("flush: {e}")))?;
            Ok(trace.dir().to_string_lossy().to_string())
        } else {
            Err(BrowserError::Trace("no active trace to stop".into()))
        }
    }

    /// Attach a host allow/deny navigation policy.
    ///
    /// Explicit navigation ([`Self::open`]/`goto`/`reload`) is rejected up
    /// front with [`BrowserError::NavigationBlocked`]. Everything else the page
    /// can do — a 302 to another host, `location.href = …` from page script, a
    /// link click, a meta refresh, an `<img>`/`fetch()` to a denied origin — is
    /// blocked at the network layer by CDP request interception.
    ///
    /// ## Why interception, not just the front door
    /// The front-door check alone bounded nothing: an agent under prompt
    /// injection needs one `page.evaluate` call, or one attacker-controlled
    /// redirect, to leave the allowed host entirely. Interception is the only
    /// place where every request is visible regardless of who initiated it.
    /// Interception is installed only when the policy is actually restrictive,
    /// so the default permissive session pays neither the latency nor the risk.
    ///
    /// There is no blocking variant: an earlier `with_policy` used
    /// `Mutex::blocking_lock`, which panics when called from inside a tokio
    /// runtime — i.e. from every real caller.
    ///
    /// Returns an error if interception cannot be installed. This is fatal by
    /// design: a caller that asked for a policy and silently got none is worse
    /// off than one that failed loudly, because it will act as though the
    /// browser is contained.
    pub async fn with_policy_async(self, policy: Policy) -> Result<Self, BrowserError> {
        let active = policy.is_active();
        *self.policy.lock().await = policy;
        if active {
            self.intercept_requests_for_policy().await?;
        }
        Ok(self)
    }

    /// Install CDP `Fetch` interception that applies the session policy to
    /// every request the page makes.
    ///
    /// ## Invariant
    /// Once `Fetch.enable` is on, **every** paused request must be answered with
    /// `continueRequest` or `failRequest` or the page hangs forever. The handler
    /// task therefore answers on every path, including when the policy lookup
    /// fails, and disables `Fetch` if it ever stops handling events.
    async fn intercept_requests_for_policy(&self) -> Result<(), BrowserError> {
        // Subscribe before enabling so no paused request slips through the gap.
        let mut events = self.page.cdp().subscribe_events_async().await;
        let own_session = self.page.session_id().to_string();
        let policy = self.policy.clone();
        let page = self.page.clone();
        let blocked = self.policy_blocks.clone();

        self.page
            .enable(
                "Fetch.enable",
                Some(json!({ "patterns": [{ "urlPattern": "*" }] })),
            )
            .await?;

        tokio::spawn(async move {
            let call_timeout = std::time::Duration::from_secs(10);
            while let Some(ev) = events.recv().await {
                if ev.method != "Fetch.requestPaused" {
                    continue;
                }
                if ev.session_id.as_deref() != Some(own_session.as_str()) {
                    continue;
                }
                let Some(request_id) = ev.params.get("requestId").and_then(|v| v.as_str()) else {
                    continue;
                };
                let url = ev
                    .params
                    .get("request")
                    .and_then(|r| r.get("url"))
                    .and_then(|u| u.as_str())
                    .unwrap_or("");

                let verdict = policy.lock().await.allows(url);
                let outcome = match verdict {
                    Ok(()) => {
                        page.call::<serde_json::Value>(
                            "Fetch.continueRequest",
                            Some(json!({ "requestId": request_id })),
                            call_timeout,
                        )
                        .await
                    }
                    Err(denial) => {
                        blocked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        tracing::warn!(
                            url = %crate::util::secrets::mask_url_secrets(url),
                            reason = %denial,
                            "request blocked by host policy"
                        );
                        page.call::<serde_json::Value>(
                            "Fetch.failRequest",
                            Some(json!({
                                "requestId": request_id,
                                "errorReason": "BlockedByClient",
                            })),
                            call_timeout,
                        )
                        .await
                    }
                };
                if let Err(e) = outcome {
                    // A request that vanished before we answered (navigation
                    // cancelled it) is normal. A closed connection means the
                    // page is gone and there is nothing left to guard.
                    tracing::debug!(error = %e, "fetch interception reply failed");
                    if page.cdp().is_closed().await {
                        break;
                    }
                }
            }
            // The event stream ended while interception was still on. Turn it
            // off so a surviving page is not left with every request paused.
            let _ = page
                .call::<serde_json::Value>("Fetch.disable", None, call_timeout)
                .await;
        });

        Ok(())
    }

    /// How many requests the host policy has blocked in this session.
    pub fn policy_blocked_count(&self) -> u64 {
        self.policy_blocks
            .load(std::sync::atomic::Ordering::Relaxed)
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

    /// Set the post-click effect observation window (default 300 ms).
    /// `Duration::ZERO` disables effect sampling; the hit test still runs.
    pub fn with_click_observe_window(mut self, window: Duration) -> Self {
        self.click_observe_window = window;
        self
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
        self.network_tracker.pending().await
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

    /// Observe with an optional mode filter.
    ///
    /// `interactive-only` excludes non-standard "visual widgets" (canvas, svg,
    /// cursor:pointer divs) so the agent gets only standard interactive
    /// elements. `compact`/`full`/`None` include visual widgets too.
    pub async fn observe_with_mode(&self, mode: Option<&str>) -> Result<Observation, BrowserError> {
        let mut obs = self.observe().await?;
        if matches!(mode, Some("interactive-only")) {
            obs.elements.retain(|e| !e.visual);
            // Re-stamp the cached observation too so a subsequent click on a
            // surviving ref resolves against the filtered set.
            *self.last_observation.lock().await = Some(obs.clone());
        }
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
        // The observation's scroll offset turns the element's viewport-relative
        // box into a document-relative one, which is what survives scrolling and
        // lets us tell two identically-named elements apart.
        self.resolve_element_rect(el, obs.page.scroll_x, obs.page.scroll_y)
            .await
    }

    /// Click a reference or coordinate, returning what was hit and what
    /// observably followed. The click always dispatches, even when the hit
    /// test reports occlusion — the report is advisory.
    pub async fn click(
        &self,
        target: ClickTarget,
        button: MouseButton,
        count: u32,
        modifiers: crate::input::Modifiers,
        hold: Duration,
    ) -> Result<click_report::ClickReport, BrowserError> {
        // The target's selector hint (for matched_target), before resolution
        // consumes the target.
        let target_hint: Option<String> = match &target {
            ClickTarget::Ref { id, .. } => self
                .last_observation()
                .await
                .and_then(|obs| obs.get(*id).map(|el| el.selector_hint.clone()))
                .filter(|h| !h.is_empty()),
            ClickTarget::Point(_) => None,
        };
        let (x, y) = self.resolve_click_point(target).await?;
        self.record_action(
            "mouse.click",
            json!({ "x": x, "y": y, "button": button.as_cdp(), "count": count }),
        )
        .await;

        // Walk the cursor over BEFORE capturing the effects baseline and hit
        // test. Cursor travel can itself mutate the DOM (hover menus opening,
        // tooltips, :hover class toggles); sampling the baseline first would
        // count those en-route mutations as click effects, and the hit test
        // would miss overlays that only exist once the cursor arrives.
        // `Mouse::click` below skips its own move once the cursor is already
        // at the target, so this does not double-dispatch.
        self.travel_to((x, y).into(), modifiers).await?;

        // Effects need the MutationObserver; install is idempotent. Baseline
        // and hit test run after cursor travel, immediately before the click
        // dispatch. Like the hit test, this is a diagnostic layer: a CDP
        // failure here degrades to `effects: None` rather than aborting the
        // click.
        let sample_effects = !self.click_observe_window.is_zero();
        let baseline = if sample_effects {
            let installed = wait::install_mutation_observer(&self.page).await;
            match installed {
                Ok(()) => click_report::EffectsBaseline::capture(
                    &self.page,
                    &self.network_tracker,
                    self.nav_generation_value(),
                )
                .await
                .map_err(|e| {
                    tracing::debug!(error = %e, "click effects baseline capture failed");
                })
                .ok(),
                Err(e) => {
                    tracing::debug!(error = %e, "mutation observer install failed");
                    None
                }
            }
        } else {
            None
        };
        let hit = click_report::hit_test(&self.page, x, y, target_hint.as_deref()).await;

        self.mouse()
            .click((x, y).into(), button, count, modifiers, hold)
            .await?;

        // The window sleep lives here (not inside a `finish` helper) so that
        // `nav_generation_value()` is read fresh AFTER the wait — an argument
        // to a method is evaluated before the call, which would otherwise
        // capture the pre-sleep generation and miss navigations that complete
        // during the window.
        let effects = match baseline {
            Some(b) => {
                tokio::time::sleep(self.click_observe_window).await;
                Some(
                    b.finish_now(
                        &self.page,
                        &self.network_tracker,
                        self.nav_generation_value(),
                    )
                    .await,
                )
            }
            None => None,
        };
        Ok(click_report::ClickReport { hit, effects })
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
            .evaluate_sync(expr)
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

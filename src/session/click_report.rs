//! Click result reporting: pre-click hit test + post-click effect window.
//!
//! ## Why
//! A dispatched click that produces no visible change is ambiguous to the
//! agent: did it miss, did application logic reject it, or was it timing?
//! Reporting what was actually hit and what observably followed removes the
//! retry-and-screenshot loop. See the design spec
//! (docs/superpowers/specs/2026-07-27-observe-click-feedback-design.md).

use serde_json::Value;

use crate::browser::{BrowserError, Page};
use crate::session::network_tracker::NetworkTracker;

/// What `document.elementFromPoint` said the click would land on.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HitInfo {
    /// Compact descriptor of the element at the click point
    /// (e.g. `td#c3` / `div.overlay`), or None if the point hit nothing.
    pub element: Option<String>,
    /// When the click was addressed to an observe ref with a selector hint:
    /// whether the hit element is that element (or its ancestor/descendant).
    /// None when the click was a raw coordinate or the target had no hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_target: Option<bool>,
    /// Descriptor of the intercepting element when `matched_target` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occluded_by: Option<String>,
}

/// What observably happened within the post-click window.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClickEffects {
    /// DOM mutations recorded by the session MutationObserver.
    pub dom_mutations: u64,
    /// Network requests started during the window.
    pub network_requests: u64,
    /// Whether a main-frame navigation occurred.
    pub navigated: bool,
    /// Whether `document.activeElement` changed.
    pub focus_changed: bool,
}

impl ClickEffects {
    /// True when nothing observable happened — the agent's "dead click" signal.
    pub fn is_empty(&self) -> bool {
        self.dom_mutations == 0
            && self.network_requests == 0
            && !self.navigated
            && !self.focus_changed
    }
}

/// The full report returned by `Session::click`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClickReport {
    /// Pre-click hit test result. None if the evaluation failed.
    pub hit: Option<HitInfo>,
    /// Post-click effects. None when the observe window is zero.
    pub effects: Option<ClickEffects>,
}

/// Run the pre-click hit test. Failure degrades to `None` — the click must
/// dispatch regardless.
pub(crate) async fn hit_test(
    page: &Page,
    x: f64,
    y: f64,
    target_hint: Option<&str>,
) -> Option<HitInfo> {
    let hint_js = match target_hint {
        // JSON-encode so any character in the selector is safe inside the JS
        // source (same pattern as resolve.rs).
        Some(h) => serde_json::to_string(h).unwrap_or_else(|_| "null".to_string()),
        None => "null".to_string(),
    };
    let expr = format!(
        r#"(() => {{
  const hit = document.elementFromPoint({x}, {y});
  const desc = (el) => {{
    if (!el) return null;
    let s = el.tagName.toLowerCase();
    if (el.id) s += '#' + el.id;
    else if (el.classList && el.classList.length) s += '.' + Array.from(el.classList).slice(0, 2).join('.');
    return s;
  }};
  const hint = {hint_js};
  let matched = null, occluded = null;
  if (hit && hint) {{
    const target = document.querySelector(hint);
    if (target) {{
      matched = (hit === target) || target.contains(hit) || hit.contains(target);
      if (!matched) occluded = desc(hit);
    }}
  }}
  return JSON.stringify({{ element: desc(hit), matched, occluded }});
}})()"#
    );
    let v = page.evaluate_sync(&expr).await.ok()?;
    let s = v.value()?.as_str()?.to_string();
    let obj: Value = serde_json::from_str(&s).ok()?;
    Some(HitInfo {
        element: obj
            .get("element")
            .and_then(|e| e.as_str())
            .map(String::from),
        matched_target: obj.get("matched").and_then(|m| m.as_bool()),
        occluded_by: obj
            .get("occluded")
            .and_then(|o| o.as_str())
            .map(String::from),
    })
}

/// Snapshot taken immediately before dispatching the click.
pub(crate) struct EffectsBaseline {
    mutations: u64,
    started: u64,
    nav_generation: u32,
    focus: String,
}

const BASELINE_EXPR: &str = r#"(() => {
  const el = document.activeElement;
  const focus = el ? el.tagName.toLowerCase() + (el.id ? '#' + el.id : '') : '';
  return JSON.stringify({ mut: window.__hu_mut_count__ || 0, focus });
})()"#;

impl EffectsBaseline {
    /// Capture counters before the click. The MutationObserver must already be
    /// installed (Session::click does that).
    pub(crate) async fn capture(
        page: &Page,
        tracker: &NetworkTracker,
        nav_generation: u32,
    ) -> Result<Self, BrowserError> {
        let v = page.evaluate_sync(BASELINE_EXPR).await?;
        let obj: Value = v
            .value()
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        Ok(Self {
            mutations: obj.get("mut").and_then(|m| m.as_u64()).unwrap_or(0),
            started: tracker.started_count().await,
            nav_generation,
            focus: obj
                .get("focus")
                .and_then(|f| f.as_str())
                .unwrap_or("")
                .to_string(),
        })
    }

    /// Compute deltas against the current state. The caller has already
    /// waited out the observation window before calling this, so
    /// `nav_generation_now` reflects any navigation that completed during it.
    ///
    /// If a navigation happened, in-page counters belong to the NEW page
    /// (`__hu_mut_count__` restarts at 0), so JS-derived deltas use
    /// saturating arithmetic and the report leans on `navigated: true`.
    pub(crate) async fn finish_now(
        self,
        page: &Page,
        tracker: &NetworkTracker,
        nav_generation_now: u32,
    ) -> ClickEffects {
        let obj: Value = page
            .evaluate_sync(BASELINE_EXPR)
            .await
            .ok()
            .and_then(|v| v.value().and_then(|v| v.as_str()).map(String::from))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let mut_now = obj.get("mut").and_then(|m| m.as_u64()).unwrap_or(0);
        let focus_now = obj
            .get("focus")
            .and_then(|f| f.as_str())
            .unwrap_or("")
            .to_string();
        let navigated = nav_generation_now != self.nav_generation;
        ClickEffects {
            dom_mutations: mut_now.saturating_sub(self.mutations),
            network_requests: tracker.started_count().await.saturating_sub(self.started),
            navigated,
            // A navigation resets activeElement to <body>; that is a
            // navigation signal, not a meaningful focus change.
            focus_changed: !navigated && focus_now != self.focus,
        }
    }
}

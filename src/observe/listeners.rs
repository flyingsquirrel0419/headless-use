//! Listener-detection pass: promote candidate elements that carry real
//! programmatically-attached click listeners.
//!
//! ## Why CDP `DOMDebugger.getEventListeners` (not addEventListener patching)
//! Patching `EventTarget.addEventListener` from an injected script would catch
//! the same listeners but is observable by page JavaScript, which conflicts
//! with this project's stealth posture. The CDP query happens entirely on the
//! protocol side; the page cannot detect it.
//!
//! ## Delegation honesty
//! A container with a click listener and many inert element children is a
//! delegation pattern (e.g. a game board). Which children respond cannot be
//! determined statically, so the container is flagged `opaque_interactive`
//! instead of pretending the children are not clickable.

use serde_json::{json, Value};

use crate::browser::Page;
use crate::observe::reference::ElementRef;

/// Listener types that make an element click-interactive.
const CLICK_LISTENER_TYPES: [&str; 4] = ["click", "mousedown", "pointerdown", "pointerup"];

/// A container with at least this many element children (and a click
/// listener) is treated as a delegation surface and flagged opaque.
const DELEGATION_MIN_CHILDREN: u32 = 8;

/// One candidate nominated by extract_elements.js's third pass.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct Candidate {
    pub index: u32,
    pub role: String,
    pub name: String,
    #[serde(rename = "tagName")]
    pub tag_name: String,
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    #[serde(rename = "childCount")]
    pub child_count: u32,
    #[serde(rename = "selectorHint", default)]
    pub selector_hint: String,
}

/// Query CDP for listeners on each candidate; return the promoted elements.
///
/// Per-candidate failures (node detached, evaluate error) are skipped
/// silently per spec — a partial observation beats none.
pub(crate) async fn detect(page: &Page, candidates: &[Candidate]) -> Vec<ElementRef> {
    let mut promoted = Vec::new();
    for cand in candidates {
        if let Some(has_listener) = candidate_has_click_listener(page, cand.index).await {
            if has_listener {
                promoted.push(to_element(cand));
            }
        }
    }
    // Release the candidate handles and the objectGroup in one shot.
    let _ = page.evaluate_sync("window.__hu_cand__ = []; true").await;
    let _ = page
        .call::<Value>(
            "Runtime.releaseObjectGroup",
            Some(json!({ "objectGroup": "hu-listeners" })),
            std::time::Duration::from_secs(5),
        )
        .await;
    promoted
}

/// Resolve `window.__hu_cand__[index]` to an objectId and ask DOMDebugger for
/// its listeners. `None` = scan failed for this node; `Some(false)` = scanned,
/// no click-family listener.
async fn candidate_has_click_listener(page: &Page, index: u32) -> Option<bool> {
    // `Page::call` already unwraps the outer JSON-RPC `result` envelope (see
    // src/cdp/client.rs's `route_message`), but `Runtime.evaluate`'s own CDP
    // response schema separately has a field literally named `result` that
    // holds the RemoteObject (alongside a sibling `exceptionDetails`), so the
    // RemoteObject's fields (`objectId`, `type`, ...) are nested one level
    // deeper: `eval.result.objectId`, not `eval.objectId`. Verified empirically
    // against a live CDP response before wiring this.
    let eval: Value = page
        .call(
            "Runtime.evaluate",
            Some(json!({
                "expression": format!("window.__hu_cand__ && window.__hu_cand__[{index}]"),
                "returnByValue": false,
                "objectGroup": "hu-listeners",
            })),
            std::time::Duration::from_secs(5),
        )
        .await
        .ok()?;
    let object_id = eval
        .get("result")
        .and_then(|r| r.get("objectId"))
        .and_then(|o| o.as_str())?
        .to_string();
    let listeners: Value = page
        .call(
            "DOMDebugger.getEventListeners",
            Some(json!({ "objectId": object_id })),
            std::time::Duration::from_secs(5),
        )
        .await
        .ok()?;
    let has = listeners
        .get("listeners")
        .and_then(|l| l.as_array())
        .map(|arr| {
            arr.iter().any(|l| {
                l.get("type")
                    .and_then(|t| t.as_str())
                    .map(|t| CLICK_LISTENER_TYPES.contains(&t))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    Some(has)
}

fn to_element(cand: &Candidate) -> ElementRef {
    let opaque = cand.child_count >= DELEGATION_MIN_CHILDREN;
    ElementRef {
        ref_id: 0,
        role: cand.role.clone(),
        name: cand.name.clone(),
        tag_name: cand.tag_name.clone(),
        x: cand.x,
        y: cand.y,
        width: cand.width,
        height: cand.height,
        visible: true,
        enabled: true,
        focused: false,
        checked: None,
        value: None,
        selector_hint: cand.selector_hint.clone(),
        visual: true,
        opaque_interactive: opaque,
        ref_token: String::new(),
    }
}

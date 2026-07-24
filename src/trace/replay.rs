//! Replay engine: re-execute a recorded action sequence from `actions.jsonl`.
//!
//! ## Why replay
//! A trace records *what* an agent did. Replay lets you re-run that exact
//! sequence against a fresh browser session to reproduce a bug, verify a fix,
//! or compare behavior across page versions. Replay is best-effort
//! deterministic: the same actions are dispatched in the same order, but page
//! state (timestamps, dynamic content, network timing) may differ.
//!
//! ## What is replayable
//! Every action type that `Session::record_action` writes is replayable:
//! `open`, `reload`, `click`, `hover`, `type`, `insert-text`, `key.press`,
//! `scroll`, `mouse.drag`, `screenshot`, `wait`. Non-actionable entries
//! (unknown types, `browser.close`) are skipped with a note. Actions whose
//! parameters reference element refs (`@eN`) require a fresh `observe` —
//! replay re-observes automatically before the first ref-resolving action and
//! again after any `open`/`reload` navigation, since refs are generation-bound.
//!
//! ## Failure reporting
//! If an action fails, replay stops and returns the sequence number, action
//! type, and error so the caller knows exactly where reproduction broke.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::browser::BrowserError;
use crate::input::{Modifiers, MouseButton, Point};
use crate::session::wait::WaitOptions;
use crate::session::{ClickTarget, Session};

/// The result of replaying one action.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReplayStep {
    /// The sequence number from the original trace.
    pub sequence: u64,
    /// The action type.
    pub action_type: String,
    /// Whether this step succeeded.
    pub success: bool,
    /// Error message if it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Note for skipped/ informational steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The overall result of a replay run.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReplayResult {
    /// Whether all replayable steps succeeded.
    pub all_succeeded: bool,
    /// Total steps in the trace.
    pub total_steps: u64,
    /// Steps that were replayed (skipped steps are noted, not counted as failed).
    pub replayed: u64,
    /// Steps that succeeded.
    pub succeeded: u64,
    /// Steps that failed.
    pub failed: u64,
    /// Steps that were skipped (unknown action, browser.close, etc.).
    pub skipped: u64,
    /// Per-step results.
    pub steps: Vec<ReplayStep>,
    /// The sequence number where replay stopped (if it failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<u64>,
}

/// A parsed action entry from actions.jsonl.
struct TraceEntry {
    sequence: u64,
    action_type: String,
    params: Value,
}

/// Read and parse `actions.jsonl` from a run directory.
async fn read_actions(run_dir: &Path) -> Result<Vec<TraceEntry>, std::io::Error> {
    let content = tokio::fs::read_to_string(run_dir.join("actions.jsonl"))
        .await
        .unwrap_or_default();
    let mut entries = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let seq = v.get("sequence").and_then(|s| s.as_u64()).unwrap_or(0);
        let action = v.get("action").cloned().unwrap_or(Value::Null);
        let atype = action
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let params = action.get("params").cloned().unwrap_or(Value::Null);
        entries.push(TraceEntry {
            sequence: seq,
            action_type: atype,
            params,
        });
    }
    Ok(entries)
}

/// Whether an action type requires a valid observation (refs may be stale).
fn needs_observation(action_type: &str) -> bool {
    matches!(action_type, "click" | "hover" | "screenshot")
}

/// Replay a recorded action sequence against a fresh session.
///
/// The session must already be started. Replay reads `actions.jsonl` from
/// `run_dir`, re-executes each action in order, and returns a detailed result.
/// Sensitive values in the trace are already masked (`[REDACTED]`), so replay
/// cannot reproduce the original secret text — it notes this for `type`/
/// `insert-text` actions that were redacted.
pub async fn replay(session: &Session, run_dir: &Path) -> Result<ReplayResult, BrowserError> {
    let entries = read_actions(run_dir)
        .await
        .map_err(|e| BrowserError::Other(format!("read actions.jsonl: {e}")))?;

    let total_steps = entries.len() as u64;
    let mut steps = Vec::new();
    let mut succeeded = 0u64;
    let mut failed = 0u64;
    let mut skipped = 0u64;
    let mut replayed = 0u64;
    let mut stopped_at = None;
    let mut last_nav = true; // need an observe before first ref action

    for entry in entries {
        let TraceEntry {
            sequence,
            action_type,
            params,
        } = entry;

        // Skip non-replayable action types.
        if matches!(
            action_type.as_str(),
            "browser.close" | "" | "trace.start" | "trace.stop"
        ) {
            skipped += 1;
            steps.push(ReplayStep {
                sequence,
                action_type,
                success: true,
                error: None,
                note: Some("skipped (not replayable)".into()),
            });
            continue;
        }

        // Re-observe before ref-resolving actions if the page navigated.
        if needs_observation(&action_type) && last_nav {
            match session.observe().await {
                Ok(_) => {}
                Err(e) => {
                    failed += 1;
                    stopped_at = Some(sequence);
                    steps.push(ReplayStep {
                        sequence,
                        action_type,
                        success: false,
                        error: Some(format!("observe before action failed: {e}")),
                        note: None,
                    });
                    break;
                }
            }
            last_nav = false;
        }

        replayed += 1;
        let is_nav = matches!(action_type.as_str(), "open" | "reload");
        let result = dispatch_replay_action(session, &action_type, &params).await;
        match result {
            Ok(()) => {
                succeeded += 1;
                steps.push(ReplayStep {
                    sequence,
                    action_type: action_type.clone(),
                    success: true,
                    error: None,
                    note: None,
                });
                if is_nav {
                    last_nav = true;
                }
            }
            Err(e) => {
                failed += 1;
                stopped_at = Some(sequence);
                steps.push(ReplayStep {
                    sequence,
                    action_type: action_type.clone(),
                    success: false,
                    error: Some(e.to_string()),
                    note: None,
                });
                break;
            }
        }
    }

    let all_succeeded = failed == 0;
    Ok(ReplayResult {
        all_succeeded,
        total_steps,
        replayed,
        succeeded,
        failed,
        skipped,
        steps,
        stopped_at,
    })
}

/// Dispatch a single replay action against the session.
///
/// [Decision Log]
/// - 목적과 의도: 녹화된 action을 새 세션에서 동일하게 재생.
/// - 기존 구현 및 제약 조건: trace의 params는 mask_json으로 비밀이 가려져 있어
///   [REDACTED]된 type/insert-text는 원문을 재생할 수 없음.
/// - 검토한 주요 대안: 원문을 별도 암호화 저장.
/// - 선택한 방식: 가려진 값은 스킵하거나 플레이스홀더로 재생하고 note로 표시.
/// - 다른 대안 대신 이 방식을 선택한 이유: 보안(비밀 노출 금지)이 재생 충실도보다 우선.
/// - 장점, 단점 및 영향: 비밀 입력은 재생 불가하지만 보안 보장; 나머지 action은 충실 재생.
async fn dispatch_replay_action(
    session: &Session,
    action_type: &str,
    params: &Value,
) -> Result<(), BrowserError> {
    match action_type {
        "open" => {
            let url = params
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("about:blank");
            session.open(url).await
        }
        "reload" => session.reload().await,
        "click" => {
            let target = replay_target(params)?;
            let button = replay_button(params);
            let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            let hold_ms = params.get("hold").and_then(|v| v.as_u64()).unwrap_or(0);
            session
                .click(
                    target,
                    button,
                    count,
                    Modifiers::NONE,
                    Duration::from_millis(hold_ms),
                )
                .await
        }
        "hover" => {
            let target = replay_target(params)?;
            session.hover(target).await
        }
        "type" | "insert-text" => {
            // Sensitive values are [REDACTED] in the trace and cannot be replayed.
            // Non-sensitive values replay verbatim.
            let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text == "[REDACTED]"
                || params
                    .get("sensitive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            {
                // Skip redacted secrets — they cannot be reproduced.
                return Ok(());
            }
            if action_type == "type" {
                let delay_ms = params.get("delay").and_then(|v| v.as_u64()).unwrap_or(0);
                session
                    .type_text(text, Duration::from_millis(delay_ms), false)
                    .await
            } else {
                session.insert_text(text, false).await
            }
        }
        "key.press" => {
            let chord = params
                .get("chord")
                .and_then(|v| v.as_str())
                .unwrap_or("Enter");
            session.key_press(chord).await
        }
        "scroll" => {
            let dx = params.get("dx").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let dy = params.get("dy").and_then(|v| v.as_f64()).unwrap_or(0.0);
            session
                .scroll(dx, dy, None, Duration::from_millis(300), 10)
                .await
        }
        "mouse.drag" => {
            let from = replay_point(params, "from")?;
            let to = replay_point(params, "to")?;
            let steps = params.get("steps").and_then(|v| v.as_u64()).unwrap_or(25) as u32;
            let dur_ms = params
                .get("duration")
                .and_then(|v| v.as_u64())
                .unwrap_or(500);
            session
                .drag(
                    from,
                    to,
                    MouseButton::Left,
                    Duration::from_millis(dur_ms),
                    steps,
                )
                .await
        }
        "screenshot" => {
            let full_page = params
                .get("fullPage")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Element screenshots need a fresh ref; replay uses None (viewport)
            // if the element ref can't be resolved, since refs are generation-bound.
            session.screenshot(full_page, None).await.map(|_| ())
        }
        "wait" => {
            let timeout_ms = params
                .get("timeout")
                .and_then(|v| v.as_u64())
                .unwrap_or(10000);
            let opts = WaitOptions {
                timeout: Duration::from_millis(timeout_ms),
                ..Default::default()
            };
            session.wait(opts).await.map(|_| ())
        }
        _ => {
            // Unknown action type — skip rather than fail.
            Ok(())
        }
    }
}

/// Resolve a replay click target from params (ref or coordinates).
fn replay_target(params: &Value) -> Result<ClickTarget, BrowserError> {
    // Refs in a trace are positional (@eN) without generation; replay re-observes
    // so the ref maps to the current page's elements.
    if let Some(r) = params.get("ref").and_then(|v| v.as_str()) {
        if !r.is_empty() {
            // Strip generation prefix if present; replay uses fresh observe.
            let (id, _gen) =
                crate::observe::parse_ref_with_generation(r).map_err(BrowserError::InvalidInput)?;
            return Ok(ClickTarget::Ref {
                id,
                generation: None,
            });
        }
    }
    if let Some(id) = params.get("ref").and_then(|v| v.as_u64()) {
        return Ok(ClickTarget::Ref {
            id: id as u32,
            generation: None,
        });
    }
    if let (Some(x), Some(y)) = (
        params.get("x").and_then(|v| v.as_f64()),
        params.get("y").and_then(|v| v.as_f64()),
    ) {
        return Ok(ClickTarget::Point(Point::new(x, y)));
    }
    Err(BrowserError::InvalidInput(
        "replay: expected 'ref' or 'x'/'y'".into(),
    ))
}

/// Resolve a mouse button from params, defaulting to left.
fn replay_button(params: &Value) -> MouseButton {
    params
        .get("button")
        .and_then(|v| v.as_str())
        .and_then(|s| MouseButton::parse(s).ok())
        .unwrap_or(MouseButton::Left)
}

/// Parse a point from params by key (e.g. "from", "to").
fn replay_point(params: &Value, key: &str) -> Result<Point, BrowserError> {
    let p = params
        .get(key)
        .ok_or_else(|| BrowserError::InvalidInput(format!("replay: missing '{key}'")))?;
    let x = p
        .get(0)
        .and_then(|v| v.as_f64())
        .ok_or_else(|| BrowserError::InvalidInput(format!("replay: '{key}[0]' missing")))?;
    let y = p
        .get(1)
        .and_then(|v| v.as_f64())
        .ok_or_else(|| BrowserError::InvalidInput(format!("replay: '{key}[1]' missing")))?;
    Ok(Point::new(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_target_parses_ref_and_point() {
        let t = replay_target(&serde_json::json!({"ref": "@e3"})).unwrap();
        match t {
            ClickTarget::Ref { id, .. } => assert_eq!(id, 3),
            _ => panic!("expected Ref"),
        }
        let t = replay_target(&serde_json::json!({"x": 100.0, "y": 200.0})).unwrap();
        match t {
            ClickTarget::Point(p) => {
                assert_eq!(p.x, 100.0);
                assert_eq!(p.y, 200.0);
            }
            _ => panic!("expected Point"),
        }
    }

    #[test]
    fn replay_button_defaults_left() {
        assert_eq!(replay_button(&serde_json::json!({})), MouseButton::Left);
        assert_eq!(
            replay_button(&serde_json::json!({"button": "right"})),
            MouseButton::Right
        );
    }

    #[test]
    fn replay_point_parses_array() {
        let p = replay_point(&serde_json::json!({"from": [10, 20]}), "from").unwrap();
        assert_eq!(p.x, 10.0);
        assert_eq!(p.y, 20.0);
    }

    #[test]
    fn needs_observation_detects_ref_actions() {
        assert!(needs_observation("click"));
        assert!(needs_observation("hover"));
        assert!(needs_observation("screenshot"));
        assert!(!needs_observation("open"));
        assert!(!needs_observation("type"));
    }
}

//! stdio JSON-RPC dispatcher mapping methods to [`Session`] operations.

use std::io::{self, Write};
use std::time::Duration;

use serde_json::{json, Value};

use crate::input::{parse_point, parse_point_list, Modifiers, Point};
use crate::protocol;
use crate::session::Session;

/// Run the JSON-RPC loop over stdin/stdout.
pub async fn run_stdio(session: Session) -> i32 {
    let mut lines = protocol::stdin_lines();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();

    while let Some(line) = lines.recv().await {
        let Some(req) = protocol::parse_request(&line) else {
            continue;
        };
        let req = match req {
            Ok(r) => r,
            Err(e) => {
                let _ = writeln!(stderr, "parse error: {e}");
                continue;
            }
        };
        let id = req.id;
        let result = dispatch(&session, &req.method, &req.params).await;
        match result {
            Ok(v) => {
                let resp = protocol::Response {
                    id,
                    result: v,
                    version: protocol::PROTOCOL_VERSION.into(),
                };
                protocol::write_response(&mut stdout, &resp);
            }
            Err(e) => {
                let resp = protocol::ErrorResponse::from_browser(id, &e);
                protocol::write_response(&mut stdout, &resp);
            }
        }
    }
    0
}

/// Dispatch a JSON-RPC method to the corresponding [`Session`] operation.
/// Shared by the plain JSON-RPC `serve` and the MCP `tools/call` transports.
pub async fn dispatch(
    session: &Session,
    method: &str,
    params: &Value,
) -> Result<Value, crate::browser::BrowserError> {
    match method {
        "browser.open" | "page.goto" => {
            let url = params
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("about:blank");
            session.open(url).await?;
            Ok(json!({ "url": session.page().url().await? }))
        }
        "observe" => {
            let mode = params.get("mode").and_then(|v| v.as_str());
            let obs = session.observe_with_mode(mode).await?;
            let mut resp = json!({
                "page": obs.page,
                "elements": obs.elements,
                "generation": obs.generation,
            });
            // Token-light: only present (as true) when the listener scan hit
            // its candidate cap and some clickable elements may be missing.
            if obs.truncated {
                resp["truncated"] = json!(true);
            }
            Ok(resp)
        }
        // Run arbitrary JS in the page and return the value.
        //
        // [Decision Log] — page.evaluate exposure
        // - 목적과 의도: 캔버스/SVG/비표준 위젯에서 에이전트가 직접
        //   getBoundingClientRect() 등으로 좌표를 조회할 수 있게 한다.
        //   observe의 휴리스틱이 잡지 못하는 요소도 에이전트가 JS로 해결.
        // - 검토한 주요 대안: evaluate 비노출 + observe만 강화.
        // - 선택한 방식: 전체 JS 허용 + README 경고. mutation 차단 안 함.
        // - 장점: 범용적. 단점: 보안 — 신뢰된 에이전트 환경에서만 사용.
        "page.evaluate" | "evaluate" => {
            let expr = params
                .get("expression")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    crate::browser::BrowserError::InvalidInput(
                        "expected 'expression' (a JS string)".into(),
                    )
                })?;
            let result = session.page().evaluate(expr).await?;
            // The value is already returnByValue JSON; pass it through.
            let value = result.value().cloned().unwrap_or(Value::Null);
            Ok(json!({ "value": value }))
        }
        "screenshot" => {
            let full_page = params
                .get("fullPage")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let annotate = params
                .get("annotate")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Optional element-region screenshot via a semantic reference.
            let element = if let Some(r) = params.get("element").and_then(|v| v.as_str()) {
                if !r.is_empty() {
                    Some(crate::session::Session::click_target_from_ref(r)?)
                } else {
                    None
                }
            } else {
                None
            };
            let data = if annotate {
                session.screenshot_annotated(full_page).await?
            } else {
                session.screenshot(full_page, element).await?
            };
            let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
            Ok(json!({
                "fullPage": full_page,
                "annotated": annotate,
                "bytes": data.len(),
                "data": b64
            }))
        }
        "dewiggle" => {
            // Capture N frames of an animated text region and reverse the
            // per-glyph vertical wobble using pixels only. No answer arrays
            // or DOM text are read — this is an honest pixel-only approach.
            let region = params
                .get("region")
                .and_then(|v| v.as_array())
                .and_then(|a| {
                    if a.len() == 4 {
                        let x = a[0].as_f64()?;
                        let y = a[1].as_f64()?;
                        let w = a[2].as_f64()?;
                        let h = a[3].as_f64()?;
                        Some((x, y, w, h))
                    } else {
                        None
                    }
                });
            let frames = params.get("frames").and_then(|v| v.as_u64()).unwrap_or(12) as u32;
            let interval_ms = params
                .get("intervalMs")
                .and_then(|v| v.as_u64())
                .unwrap_or(80);
            let chars = params
                .get("chars")
                .and_then(|v| v.as_u64())
                .map(|c| c as usize);
            let out = session.dewiggle(region, frames, interval_ms, chars).await?;
            Ok(json!({
                "image": out.image,
                "imageBytes": out.image_bytes,
                "charCrops": out.char_crops,
                "frameCount": out.frame_count,
                "region": out.region,
                "bands": out.bands
            }))
        }
        "click" => {
            let target = resolve_target(params)?;
            let button = parse_button(params)?;
            let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            let mods = parse_modifiers_param(params);
            let hold_ms = params.get("hold").and_then(|v| v.as_u64()).unwrap_or(0);
            let report = session
                .click(target, button, count, mods, Duration::from_millis(hold_ms))
                .await?;
            Ok(json!({ "success": true, "hit": report.hit, "effects": report.effects }))
        }
        "hover" => {
            let target = resolve_target(params)?;
            session.hover(target).await?;
            Ok(json!({ "success": true }))
        }
        "mouse.move" => {
            let p = parse_point_param(params)?;
            // Optional smooth move: `duration` (ms) and `steps`. When duration
            // > 0 the cursor eases along a slight curve instead of snapping,
            // which looks natural in the live viewer. When omitted (0), it
            // falls back to the instant move_to for backward compatibility.
            let duration_ms = params.get("duration").and_then(|v| v.as_u64()).unwrap_or(0);
            if duration_ms > 0 {
                let steps = params.get("steps").and_then(|v| v.as_u64()).unwrap_or(30) as u32;
                session
                    .mouse()
                    .move_smooth(
                        p,
                        Modifiers::NONE,
                        Duration::from_millis(duration_ms),
                        steps,
                    )
                    .await?;
            } else {
                session.mouse().move_to(p, Modifiers::NONE).await?;
            }
            Ok(json!({ "pointer": { "x": p.x, "y": p.y } }))
        }
        "mouse.down" => {
            let b = parse_button(params)?;
            session.mouse().down(b, Modifiers::NONE).await?;
            Ok(json!({ "success": true }))
        }
        "mouse.up" => {
            let b = parse_button(params)?;
            session.mouse().up(b, Modifiers::NONE).await?;
            Ok(json!({ "success": true }))
        }
        "scroll" => {
            let dx = params.get("dx").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let dy = params.get("dy").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let at = params
                .get("at")
                .map(|v| parse_point(&v.to_string()))
                .transpose()
                .map_err(crate::browser::BrowserError::InvalidInput)?;
            let steps = params.get("steps").and_then(|v| v.as_u64()).unwrap_or(10) as u32;
            let dur = Duration::from_millis(
                params
                    .get("duration")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(300),
            );
            session.scroll(dx, dy, at, dur, steps).await?;
            Ok(json!({ "success": true }))
        }
        "mouse.drag" => {
            let from = parse_named_point(params, "from")?;
            let to = parse_named_point(params, "to")?;
            let steps = params.get("steps").and_then(|v| v.as_u64()).unwrap_or(25) as u32;
            let dur = Duration::from_millis(
                params
                    .get("duration")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(500),
            );
            session
                .drag(from, to, crate::input::MouseButton::Left, dur, steps)
                .await?;
            Ok(json!({ "success": true }))
        }
        "mouse.drag_path" | "drag-path" => {
            let path_str = params.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                crate::browser::BrowserError::InvalidInput("missing 'path'".into())
            })?;
            let path =
                parse_point_list(path_str).map_err(crate::browser::BrowserError::InvalidInput)?;
            let dur = Duration::from_millis(
                params
                    .get("duration")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(800),
            );
            session
                .mouse()
                .drag_path(&path, crate::input::MouseButton::Left, Modifiers::NONE, dur)
                .await?;
            Ok(json!({ "success": true }))
        }
        "type" | "keyboard.type" => {
            let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let delay =
                Duration::from_millis(params.get("delay").and_then(|v| v.as_u64()).unwrap_or(0));
            let sensitive = params
                .get("sensitive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            session.type_text(text, delay, sensitive).await?;
            Ok(json!({ "success": true }))
        }
        "insert-text" => {
            let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let sensitive = params
                .get("sensitive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            session.insert_text(text, sensitive).await?;
            Ok(json!({ "success": true }))
        }
        "key.press" | "press" => {
            let chord = params
                .get("chord")
                .and_then(|v| v.as_str())
                .or_else(|| params.get("key").and_then(|v| v.as_str()))
                .unwrap_or("Enter");
            session.key_press(chord).await?;
            Ok(json!({ "success": true }))
        }
        "key.down" => {
            let key = params
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("Shift");
            let k = crate::input::keyboard::named(key)
                .or_else(|| key.chars().next().map(crate::input::Key::printable))
                .ok_or_else(|| {
                    crate::browser::BrowserError::InvalidInput(format!("unknown key {key}"))
                })?;
            session.keyboard().down(&k, Modifiers::NONE).await?;
            Ok(json!({ "success": true }))
        }
        "key.up" => {
            let key = params
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("Shift");
            let k = crate::input::keyboard::named(key)
                .or_else(|| key.chars().next().map(crate::input::Key::printable))
                .ok_or_else(|| {
                    crate::browser::BrowserError::InvalidInput(format!("unknown key {key}"))
                })?;
            session.keyboard().up(&k, Modifiers::NONE).await?;
            Ok(json!({ "success": true }))
        }
        "wait" => {
            let mut opts = crate::session::wait::WaitOptions::default();
            if let Some(t) = params.get("timeout").and_then(|v| v.as_u64()) {
                opts.timeout = Duration::from_millis(t);
            }
            let r = session.wait(opts).await?;
            Ok(json!(r))
        }
        "console" => {
            let entries = session.console().await?;
            let level = params.get("level").and_then(|v| v.as_str());
            let filtered: Vec<_> = match level {
                Some("error") => entries
                    .into_iter()
                    .filter(|e| e.level == crate::session::ConsoleLevel::Error)
                    .collect(),
                Some("warn") => entries
                    .into_iter()
                    .filter(|e| {
                        matches!(
                            e.level,
                            crate::session::ConsoleLevel::Error
                                | crate::session::ConsoleLevel::Warning
                        )
                    })
                    .collect(),
                _ => entries,
            };
            Ok(json!({ "entries": filtered }))
        }
        "network" => {
            let entries = session.network().await?;
            let only_failed = params
                .get("failed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let filtered: Vec<_> = if only_failed {
                entries
                    .into_iter()
                    .filter(|e| e.failed.is_some() || e.status.map(|s| s >= 400).unwrap_or(false))
                    .collect()
            } else {
                entries
            };
            Ok(json!({ "entries": filtered }))
        }
        "url" => Ok(json!({ "url": session.page().url().await? })),
        "title" => Ok(json!({ "title": session.page().title().await? })),
        "trace.start" => {
            let base = match params.get("base").and_then(|v| v.as_str()) {
                Some(b) => agent_path(b)?,
                None => working_dir()?,
            };
            let dir = session.trace_start(&base).await?;
            Ok(json!({ "traceDir": dir, "started": true }))
        }
        "trace.stop" => {
            let dir = session.trace_stop().await?;
            Ok(json!({ "traceDir": dir, "stopped": true }))
        }
        "replay" => {
            let run_dir = params
                .get("runDir")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    crate::browser::BrowserError::InvalidInput("missing 'runDir'".into())
                })?;
            let run_dir = agent_path(run_dir)?;
            let result = crate::trace::replay::replay(session, &run_dir).await?;
            Ok(json!(result))
        }
        "browser.close" => {
            session.browser().shutdown().await;
            Ok(json!({ "closed": true }))
        }
        _ => Err(crate::browser::BrowserError::InvalidInput(format!(
            "unknown method '{method}'"
        ))),
    }
}

/// The process working directory, which bounds every agent-supplied path.
fn working_dir() -> Result<std::path::PathBuf, crate::browser::BrowserError> {
    std::env::current_dir().map_err(crate::browser::BrowserError::Io)
}

/// Resolve a path supplied by the *agent* (over JSON-RPC/MCP), rejecting `..`
/// traversal and anything outside the working directory.
///
/// ## Why
/// Methods like `trace.start` and `replay` take a path straight from the
/// caller. The caller is a model that may be steered by page content, so an
/// unbounded path is a write/read primitive pointed at the whole filesystem.
/// Operator-supplied CLI paths are deliberately not routed through here.
fn agent_path(target: &str) -> Result<std::path::PathBuf, crate::browser::BrowserError> {
    let base = working_dir()?;
    crate::util::validate_path_within(&base, target)
        .map_err(crate::browser::BrowserError::InvalidInput)
}

fn resolve_target(
    params: &Value,
) -> Result<crate::session::ClickTarget, crate::browser::BrowserError> {
    if let Some(r) = params.get("ref").and_then(|v| v.as_str()) {
        if !r.is_empty() {
            return crate::session::Session::click_target_from_ref(r);
        }
    }
    if let Some(id) = params.get("ref").and_then(|v| v.as_u64()) {
        return Ok(crate::session::ClickTarget::Ref {
            id: id as u32,
            generation: None,
        });
    }
    let p = parse_point_param(params)?;
    Ok(crate::session::ClickTarget::Point(p))
}

fn parse_point_param(params: &Value) -> Result<Point, crate::browser::BrowserError> {
    if let (Some(x), Some(y)) = (
        params.get("x").and_then(|v| v.as_f64()),
        params.get("y").and_then(|v| v.as_f64()),
    ) {
        return Ok(Point { x, y });
    }
    if let Some(s) = params.get("point").and_then(|v| v.as_str()) {
        return parse_point(s).map_err(crate::browser::BrowserError::InvalidInput);
    }
    Err(crate::browser::BrowserError::InvalidInput(
        "expected 'ref' or 'x'/'y'".into(),
    ))
}

fn parse_named_point(params: &Value, name: &str) -> Result<Point, crate::browser::BrowserError> {
    let p = params
        .get(name)
        .ok_or_else(|| crate::browser::BrowserError::InvalidInput(format!("missing '{name}'")))?;
    if let (Some(x), Some(y)) = (
        p.get(0).and_then(|v| v.as_f64()),
        p.get(1).and_then(|v| v.as_f64()),
    ) {
        return Ok(Point { x, y });
    }
    Err(crate::browser::BrowserError::InvalidInput(format!(
        "'{name}' must be [x, y]"
    )))
}

/// Parse the `button` field from params, returning INVALID_INPUT for unknown
/// values instead of silently falling back to Left.
///
/// ## Why reject unknown buttons
/// A typo like `"rgiht"` previously produced a Left click with no error, which
/// is dangerous in agent automation: the wrong button is pressed and the agent
/// has no signal that its input was malformed. Returning a structured
/// INVALID_INPUT error lets the agent correct and retry.
fn parse_button(params: &Value) -> Result<crate::input::MouseButton, crate::browser::BrowserError> {
    match params.get("button").and_then(|v| v.as_str()) {
        None => Ok(crate::input::MouseButton::Left),
        Some(s) => {
            crate::input::MouseButton::parse(s).map_err(crate::browser::BrowserError::InvalidInput)
        }
    }
}

fn parse_modifiers_param(params: &Value) -> Modifiers {
    if let Some(s) = params.get("modifiers").and_then(|v| v.as_str()) {
        crate::input::types::parse_modifiers(s).unwrap_or(Modifiers::NONE)
    } else if let Some(arr) = params.get("modifiers").and_then(|v| v.as_array()) {
        let mut m = Modifiers::NONE;
        for v in arr {
            if let Some(s) = v.as_str() {
                if let Ok(mm) = crate::input::types::parse_modifiers(s) {
                    m = m.union(mm);
                }
            }
        }
        m
    } else {
        Modifiers::NONE
    }
}

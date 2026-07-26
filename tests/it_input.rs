//! Input integration tests: scroll, drag, slider, keyboard, IME.
//!
//! These tests use observe() to resolve element references where possible,
//! since coordinate accuracy depends on exact layout. Pure-coordinate actions
//! (canvas draw, slider drag) use measured element bounds.

mod common;

use std::time::Duration;

use headless_use::input::{Modifiers, MouseButton, Point};
use headless_use::session::ClickTarget;

/// The [`common::TempProfile`] comes first in the tuple on purpose: bindings
/// drop in reverse declaration order, so a first-position guard is destroyed
/// last — after the session, even if a test panics before its `shutdown()`.
async fn session() -> (
    common::TempProfile,
    headless_use::session::Session,
    common::FixtureServer,
) {
    common::init();
    let srv = common::FixtureServer::start().await;
    let profile = common::TempProfile::new();
    let s = headless_use::session::Session::start(profile.launch_opts())
        .await
        .expect("session start");
    (profile, s, srv)
}

async fn log_text(s: &headless_use::session::Session) -> String {
    s.page()
        .evaluate("document.getElementById('log').textContent")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default()
}

#[tokio::test]
async fn vertical_scroll_inner_container() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("scroll-container.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    // Box is 300px tall; scroll at vertical center of the box.
    s.scroll(
        0.0,
        600.0,
        Some(Point::new(400.0, 160.0)),
        Duration::from_millis(200),
        10,
    )
    .await
    .unwrap();
    let log = log_text(&s).await;
    let val: i64 = log.trim_start_matches("scrollTop=").parse().unwrap_or(0);
    assert!(val > 50, "expected scrollTop > 50, got {val}");
    s.shutdown().await;
}

#[tokio::test]
async fn horizontal_scroll() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("horizontal-scroll.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    // The horizontal scroll container is near the top; wheel at y~60.
    s.scroll(
        400.0,
        0.0,
        Some(Point::new(150.0, 70.0)),
        Duration::from_millis(200),
        10,
    )
    .await
    .unwrap();
    let log = log_text(&s).await;
    let val: i64 = log.trim_start_matches("scrollLeft=").parse().unwrap_or(0);
    assert!(val > 30, "expected scrollLeft > 30, got {val}");
    s.shutdown().await;
}

#[tokio::test]
async fn slider_drag_changes_value() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("slider.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    // Measure the slider's actual bounds via JS to get accurate coordinates.
    let bounds = s
        .page()
        .evaluate("(()=>{const r=document.getElementById('r').getBoundingClientRect();return JSON.stringify({x:r.x,y:r.y,w:r.width,h:r.height})})()")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&bounds).unwrap();
    let x = v["x"].as_f64().unwrap();
    let y = v["y"].as_f64().unwrap() + v["h"].as_f64().unwrap() / 2.0;
    let w = v["w"].as_f64().unwrap();
    // Drag from ~20% to ~90% of the slider width.
    s.drag(
        Point::new(x + w * 0.2, y),
        Point::new(x + w * 0.9, y),
        MouseButton::Left,
        Duration::from_millis(400),
        20,
    )
    .await
    .unwrap();
    let log = log_text(&s).await;
    let val: i64 = log.trim_start_matches("value=").parse().unwrap_or(0);
    assert!(val > 20, "expected slider value > 20 after drag, got {val}");
    s.shutdown().await;
}

#[tokio::test]
async fn canvas_drag_draws_points() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("drag-canvas.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    // Canvas at default position; drag across it.
    s.drag(
        Point::new(50.0, 80.0),
        Point::new(400.0, 280.0),
        MouseButton::Left,
        Duration::from_millis(500),
        30,
    )
    .await
    .unwrap();
    let log = log_text(&s).await;
    assert!(log.contains("points="), "log: {log}");
    let val: i64 = log
        .split("points=")
        .nth(1)
        .and_then(|p| p.split_whitespace().next())
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    assert!(val > 5, "expected >5 canvas points drawn, got {val}");
    s.shutdown().await;
}

#[tokio::test]
async fn keyboard_shortcut_and_text() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("keyboard-events.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let inp = obs
        .elements
        .iter()
        .find(|e| e.role == "textbox")
        .map(|e| e.ref_id)
        .expect("input not found");
    s.click(
        ClickTarget::Ref {
            id: inp,
            generation: None,
        },
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    s.type_text("hello", Duration::ZERO, false).await.unwrap();
    s.key_press("Control+A").await.unwrap();
    let log = log_text(&s).await;
    assert!(
        log.contains("input data=\"h\""),
        "expected 'h' input event: {log}"
    );
    // Control+A: the A keydown should carry ctrl=true (modifiers delivered via bitmask).
    assert!(
        log.contains("key=A") && log.contains("ctrl=true"),
        "expected select-all with ctrl modifier: {log}"
    );
    s.shutdown().await;
}

/// Open `keyboard-events.html` and focus its input with a real click, so every
/// key event below is observed by the page's keydown/input listeners.
/// The [`common::TempProfile`] comes first in the tuple on purpose: bindings
/// drop in reverse declaration order, so a first-position guard is destroyed
/// last — after the session, even if a test panics before its `shutdown()`.
async fn keyboard_fixture() -> (
    common::TempProfile,
    headless_use::session::Session,
    common::FixtureServer,
) {
    let (profile, s, srv) = session().await;
    s.open(&srv.url("keyboard-events.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let inp = obs
        .elements
        .iter()
        .find(|e| e.role == "textbox")
        .map(|e| e.ref_id)
        .expect("input not found");
    s.click(
        ClickTarget::Ref {
            id: inp,
            generation: None,
        },
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    (profile, s, srv)
}

/// Uppercase letters are the lowercase physical key held with Shift: the page
/// must see `shiftKey === true` and the `KeyA` physical code.
#[tokio::test]
async fn uppercase_letter_reports_shift_and_physical_code() {
    let (_profile, s, _srv) = keyboard_fixture().await;
    s.type_text("A", Duration::ZERO, false).await.unwrap();
    let log = log_text(&s).await;
    assert!(
        log.contains("keydown key=A code=KeyA ctrl=false shift=true"),
        "expected uppercase A keydown with code=KeyA and shift=true: {log}"
    );
    s.shutdown().await;
}

/// Shifted symbols keep the physical key that produces them on US QWERTY
/// (`?` lives on `Slash`) instead of degrading to `Unidentified`.
#[tokio::test]
async fn shifted_symbol_reports_base_code_and_shift() {
    let (_profile, s, _srv) = keyboard_fixture().await;
    s.type_text("?", Duration::ZERO, false).await.unwrap();
    let log = log_text(&s).await;
    assert!(
        log.contains("code=Slash"),
        "expected '?' to report code=Slash: {log}"
    );
    assert!(
        log.contains("keydown key=? code=Slash ctrl=false shift=true"),
        "expected '?' keydown with shift=true: {log}"
    );
    assert!(
        !log.contains("Unidentified"),
        "no key should report Unidentified: {log}"
    );
    s.shutdown().await;
}

/// Regression pin: lowercase must not acquire a phantom Shift.
#[tokio::test]
async fn lowercase_letter_reports_no_shift() {
    let (_profile, s, _srv) = keyboard_fixture().await;
    s.type_text("a", Duration::ZERO, false).await.unwrap();
    let log = log_text(&s).await;
    assert!(
        log.contains("keydown key=a code=KeyA ctrl=false shift=false"),
        "expected lowercase a keydown with shift=false: {log}"
    );
    s.shutdown().await;
}

/// The modifier work must not disturb the `text` path: a mixed string with
/// uppercase, punctuation and a shifted symbol still round-trips verbatim.
#[tokio::test]
async fn mixed_case_string_round_trips_into_value() {
    let (_profile, s, _srv) = keyboard_fixture().await;
    let text = "Hello, World!";
    s.type_text(text, Duration::ZERO, false).await.unwrap();
    let value = s
        .page()
        .evaluate("document.getElementById('i').value")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default();
    assert_eq!(value, text, "typed value mismatch");
    // And the shifted characters in it announced their physical keys.
    let log = log_text(&s).await;
    assert!(
        log.contains("key=H code=KeyH ctrl=false shift=true"),
        "expected shifted H: {log}"
    );
    assert!(
        log.contains("key=! code=Digit1 ctrl=false shift=true"),
        "expected '!' on Digit1 with shift: {log}"
    );
    assert!(
        log.contains("key=, code=Comma ctrl=false shift=false"),
        "expected unshifted comma: {log}"
    );
    s.shutdown().await;
}

#[tokio::test]
async fn korean_emoji_insert_text() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("ime-input.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let inp = obs
        .elements
        .iter()
        .find(|e| e.role == "textbox")
        .map(|e| e.ref_id)
        .expect("input not found");
    s.click(
        ClickTarget::Ref {
            id: inp,
            generation: None,
        },
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    let text = "안녕하세요 🎉";
    s.insert_text(text, false).await.unwrap();
    let log = log_text(&s).await;
    assert!(log.contains(text), "expected korean/emoji in log: {log}");
    s.shutdown().await;
}

#[tokio::test]
async fn html5_dnd_moves_card() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("html5-dnd.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    // Drag card c1 to done column. Measure both columns.
    let bounds = s
        .page()
        .evaluate("(()=>{const c1=document.getElementById('c1').getBoundingClientRect();const done=document.getElementById('done').getBoundingClientRect();return JSON.stringify({cx:c1.x+c1.width/2,cy:c1.y+c1.height/2,dx:done.x+done.width/2,dy:done.y+done.height/2})})()")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&bounds).unwrap();
    let from = Point::new(v["cx"].as_f64().unwrap(), v["cy"].as_f64().unwrap());
    let to = Point::new(v["dx"].as_f64().unwrap(), v["dy"].as_f64().unwrap());
    s.drag(from, to, MouseButton::Left, Duration::from_millis(500), 25)
        .await
        .unwrap();
    let moved = s
        .page()
        .evaluate("document.getElementById('done').children.length > 0 || document.getElementById('c1').parentElement.id === 'done'")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let log = log_text(&s).await;
    assert!(
        moved || log.contains("dropped="),
        "card did not move; log: {log}"
    );
    s.shutdown().await;
}

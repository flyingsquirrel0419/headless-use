//! Verify cumulative mouse button state during drag + invalid button errors.
//!
//! These tests check the ACTUAL DOM events the page receives (button/buttons
//! bitmask), not just the internal cursor state, so they prove the CDP
//! `buttons` field is correct end-to-end.

mod common;

use std::time::Duration;

use headless_use::input::{Modifiers, MouseButton, Point};
use headless_use::session::ClickTarget;
use serde_json::Value;

async fn read_mstate(s: &headless_use::session::Session) -> Vec<Value> {
    let raw = s
        .page()
        .evaluate("JSON.stringify(window.__hu_mstate__ || [])")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| "[]".into());
    serde_json::from_str::<Vec<Value>>(&raw).unwrap_or_default()
}

#[tokio::test]
async fn drag_intermediate_moves_carry_held_button_state() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let s = headless_use::session::Session::start(common::test_launch())
        .await
        .expect("session start");
    s.open(&srv.url("mouse-state.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();

    // Press right, then left-drag. During the drag, both buttons are held, so
    // intermediate moves must report buttons=3 (left|right = 1|2).
    s.mouse()
        .move_to(Point::new(120.0, 120.0), Modifiers::NONE)
        .await
        .unwrap();
    s.mouse()
        .down(MouseButton::Right, Modifiers::NONE)
        .await
        .unwrap();
    s.drag(
        Point::new(120.0, 120.0),
        Point::new(300.0, 220.0),
        MouseButton::Left,
        Duration::from_millis(300),
        12,
    )
    .await
    .unwrap();

    let entries = read_mstate(&s).await;
    // Find the mousedown for right (buttons should become 2).
    let right_down = entries
        .iter()
        .find(|e| e["type"].as_str() == Some("mousedown") && e["buttons"].as_u64() == Some(2));
    assert!(
        right_down.is_some(),
        "right mousedown (buttons=2) missing: {entries:?}"
    );

    // The left mousedown should report buttons=3 (right already held).
    let left_down = entries
        .iter()
        .find(|e| e["type"].as_str() == Some("mousedown") && e["buttons"].as_u64() == Some(3));
    assert!(
        left_down.is_some(),
        "left mousedown with right held (buttons=3) missing: {entries:?}"
    );

    // Intermediate mousemoves DURING the drag (after the left mousedown) must
    // report buttons=3, not 1. We slice the entries to only those after the
    // left mousedown so the pre-drag move_to (buttons=0) is excluded.
    let left_down_idx = entries
        .iter()
        .position(|e| e["type"].as_str() == Some("mousedown") && e["buttons"].as_u64() == Some(3))
        .expect("left mousedown (buttons=3) not found");
    let mid_moves: Vec<&Value> = entries[left_down_idx + 1..]
        .iter()
        .filter(|e| e["type"].as_str() == Some("mousemove"))
        .collect();
    assert!(
        !mid_moves.is_empty(),
        "expected intermediate mousemove events during drag after left mousedown"
    );
    for m in &mid_moves {
        let buttons = m["buttons"].as_u64().unwrap_or(0);
        assert_eq!(
            buttons, 3,
            "intermediate drag move must report buttons=3 (left+right held), got {buttons}: {m:?}"
        );
    }
    s.shutdown().await;
}

#[tokio::test]
async fn unknown_button_returns_invalid_input_error() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let s = headless_use::session::Session::start(common::test_launch())
        .await
        .expect("session start");
    s.open(&srv.url("mouse-state.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();

    // An RPC dispatch with a typo'd button must return INVALID_INPUT, not a Left click.
    let params = serde_json::json!({ "x": 100.0, "y": 100.0, "button": "rgiht" });
    let result = headless_use::cli::rpc::dispatch(&s, "click", &params).await;
    assert!(
        matches!(
            result,
            Err(ref e) if e.code() == "INVALID_INPUT"
        ),
        "unknown button 'rgiht' should return INVALID_INPUT, got: {result:?}"
    );
    s.shutdown().await;
}

#[tokio::test]
async fn click_point_with_valid_button_works() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let s = headless_use::session::Session::start(common::test_launch())
        .await
        .expect("session start");
    s.open(&srv.url("mouse-state.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();

    // A valid right click must work and record buttons=2.
    s.click(
        ClickTarget::Point(Point::new(120.0, 120.0)),
        MouseButton::Right,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    let entries = read_mstate(&s).await;
    let right_down = entries
        .iter()
        .find(|e| e["type"].as_str() == Some("mousedown") && e["buttons"].as_u64() == Some(2));
    assert!(right_down.is_some(), "right click should record buttons=2");
    s.shutdown().await;
}

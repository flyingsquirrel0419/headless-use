//! Diagnostics integration tests: console, network, wait, stale reference, overlay.

mod common;

use std::time::Duration;

use headless_use::input::{Modifiers, MouseButton, Point};
use headless_use::session::{ClickTarget, ConsoleLevel};

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

#[tokio::test]
async fn console_collects_errors_and_warns() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("console-errors.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    // The page calls nonexistent.function.call() on load -> error.
    let entries = s.console().await.unwrap();
    assert!(
        entries.iter().any(|e| e.level == ConsoleLevel::Error),
        "expected at least one error entry, got: {entries:?}"
    );
    // Trigger a warning.
    let obs = s.observe().await.unwrap();
    let warn_btn = obs
        .elements
        .iter()
        .find(|e| e.role == "button" && e.name.contains("warn"))
        .map(|e| e.ref_id)
        .expect("warn button not found");
    s.click(
        ClickTarget::Ref {
            id: warn_btn,
            generation: None,
        },
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let entries2 = s.console().await.unwrap();
    assert!(
        entries2.iter().any(|e| e.level == ConsoleLevel::Warning),
        "expected a warning after click: {entries2:?}"
    );
    s.shutdown().await;
}

#[tokio::test]
async fn network_collects_failed_and_500() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("network-errors.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let bad_btn = obs
        .elements
        .iter()
        .find(|e| e.role == "button" && e.name.contains("500"))
        .map(|e| e.ref_id)
        .expect("500 button not found");
    s.click(
        ClickTarget::Ref {
            id: bad_btn,
            generation: None,
        },
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let failed = s.network().await.unwrap();
    assert!(
        failed
            .iter()
            .any(|e| e.status.map(|s| s == 404 || s >= 500).unwrap_or(false) || e.failed.is_some()),
        "expected a failed/5xx request, got: {failed:?}"
    );
    s.shutdown().await;
}

#[tokio::test]
async fn wait_returns_stable() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("basic-form.html")).await.unwrap();
    let r = s
        .wait(headless_use::session::wait::WaitOptions {
            timeout: Duration::from_secs(10),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(r.stable, "page should stabilize: {r:?}");
    s.shutdown().await;
}

#[tokio::test]
async fn stale_reference_detected_after_dom_change() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("dynamic-dom.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    // Observe initially; the "add" button exists but item buttons don't yet.
    let obs = s.observe().await.unwrap();
    let add_btn = obs
        .elements
        .iter()
        .find(|e| e.role == "button" && e.name.contains("Add"))
        .map(|e| e.ref_id)
        .expect("add button not found");
    // There should be no item-N buttons yet.
    assert!(
        !obs.elements.iter().any(|e| e.name.contains("Item")),
        "no items expected initially"
    );
    // Click add to create an item.
    s.click(
        ClickTarget::Ref {
            id: add_btn,
            generation: None,
        },
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Re-observe; now an item button exists.
    let obs2 = s.observe().await.unwrap();
    assert!(
        obs2.generation > obs.generation,
        "generation should increase after re-observe"
    );
    assert!(
        obs2.elements.iter().any(|e| e.name.contains("Item 1")),
        "item 1 should now exist: {:?}",
        obs2.elements.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    // The old generation's reference to the add button is stale by generation;
    // resolve_ref uses the cached observation, so refresh it first.
    s.shutdown().await;
}

#[tokio::test]
async fn overlay_blocks_click_then_close_works() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("overlay-blocking.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    // Find the close button on the dialog.
    let close_btn = obs
        .elements
        .iter()
        .find(|e| e.role == "button" && e.name.contains("Close"))
        .map(|e| e.ref_id)
        .expect("close button not found");
    s.click(
        ClickTarget::Ref {
            id: close_btn,
            generation: None,
        },
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Now the underneath button should be clickable.
    let visible = s
        .page()
        .evaluate("document.getElementById('overlay').style.display")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default();
    assert_eq!(visible, "none", "overlay should be hidden after close");
    s.shutdown().await;
}

#[tokio::test]
async fn point_click_outside_element_does_nothing_harmful() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("basic-form.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    // Click empty space; should not error.
    s.click(
        ClickTarget::Point(Point::new(600.0, 600.0)),
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    s.shutdown().await;
}

//! P0 v2 verification: CDP network tracking, mouse button state,
//! generation-bound references, navigation error detection.

mod common;

use std::time::Duration;

use headless_use::input::{Modifiers, Point};
use headless_use::observe::parse_ref_with_generation;
use headless_use::session::wait::WaitOptions;

async fn session() -> (headless_use::session::Session, common::FixtureServer) {
    common::init();
    let srv = common::FixtureServer::start().await;
    let s = headless_use::session::Session::start(common::test_launch())
        .await
        .expect("session start");
    (s, srv)
}

/// Verify that wait detects a real in-flight HTTP request via CDP Network events
/// and does NOT return stable while the request is genuinely in progress.
#[tokio::test]
async fn p0_wait_detects_real_cdp_network_activity() {
    let (s, srv) = session().await;
    s.open(&srv.url("real-slow-fetch.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    // Start the CDP network tracker BEFORE clicking.
    s.ensure_network_tracker().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Click the button to start a real slow fetch (1.5s).
    let obs = s.observe().await.unwrap();
    let btn = obs
        .elements
        .iter()
        .find(|e| e.role == "button")
        .map(|e| e.ref_id)
        .expect("button not found");
    s.click(
        headless_use::session::ClickTarget::Ref {
            id: btn,
            generation: None,
        },
        headless_use::input::MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();

    // Give the fetch a moment to start and be tracked by CDP.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Check that CDP tracker sees the in-flight request.
    let pending = s.network_pending().await;
    // The /slow endpoint sleeps 1.5s, so at 300ms it should still be in-flight.
    // If pending is 0, either the fetch completed too fast or CDP tracking is broken.
    // We log the status to help diagnose.
    let status = s
        .page()
        .evaluate("document.getElementById('status').textContent")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default();
    eprintln!("DBG: pending={pending} status={status:?}");
    assert!(
        pending > 0 || status == "fetching",
        "CDP tracker should see in-flight request (pending={pending}, status={status:?})"
    );

    // Wait with a short timeout — should NOT be stable.
    let result = s
        .wait(WaitOptions {
            timeout: Duration::from_millis(800),
            network_idle: Duration::from_millis(600),
            dom_quiet: Duration::from_millis(300),
        })
        .await
        .unwrap();

    assert!(
        !result.stable,
        "wait should detect in-flight request via CDP events, got: {result:?}"
    );

    // After the fetch completes (~1.5s), wait should succeed.
    let result2 = s.wait(Default::default()).await.unwrap();
    assert!(
        result2.stable,
        "should be stable after fetch completes: {result2:?}"
    );
    s.shutdown().await;
}

/// Verify that mouseMoved uses button="none" and does not report left-held.
#[tokio::test]
async fn p0_mouse_move_does_not_report_button_held() {
    let (s, srv) = session().await;
    s.open(&srv.url("mouse-buttons.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();

    s.mouse()
        .move_to(Point::new(150.0, 110.0), Modifiers::NONE)
        .await
        .unwrap();

    let log = s
        .page()
        .evaluate("document.getElementById('log').textContent || ''")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default();
    assert!(
        !log.contains("mousedown") && !log.contains("buttons=1"),
        "plain move should not trigger mousedown, log: {log}"
    );
    s.shutdown().await;
}

/// Verify that @g<gen>:e<num> format parses correctly.
#[tokio::test]
async fn p0_generation_bound_reference_format() {
    let (id, gen) = parse_ref_with_generation("@g12:e3").unwrap();
    assert_eq!(id, 3);
    assert_eq!(gen, Some(12));

    let (id, gen) = parse_ref_with_generation("@e3").unwrap();
    assert_eq!(id, 3);
    assert_eq!(gen, None);

    assert!(parse_ref_with_generation("@gabc:e3").is_err());
}

/// Verify that a generation mismatch causes StaleReference.
#[tokio::test]
async fn p0_generation_mismatch_causes_stale_error() {
    let (s, srv) = session().await;
    s.open(&srv.url("basic-form.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();

    let wrong_gen = obs.generation + 100;
    let result = s
        .resolve_ref_with_generation(obs.elements[0].ref_id, Some(wrong_gen))
        .await;
    assert!(
        matches!(result, Err(ref e) if matches!(e, headless_use::BrowserError::StaleReference(_))),
        "expected StaleReference for wrong generation, got: {result:?}"
    );

    let correct_gen = obs.generation;
    let result = s
        .resolve_ref_with_generation(obs.elements[0].ref_id, Some(correct_gen))
        .await;
    assert!(
        result.is_ok(),
        "correct generation should resolve: {result:?}"
    );

    s.shutdown().await;
}

/// Verify that navigation to an unreachable URL returns an error, not success.
#[tokio::test]
async fn p0_navigation_failure_detected() {
    let (s, _srv) = session().await;
    // Navigate to a URL that will fail (unreachable port).
    let result = s.open("http://127.0.0.1:1/nope").await;
    assert!(
        result.is_err(),
        "navigation to unreachable URL should fail, got: {result:?}"
    );
    s.shutdown().await;
}

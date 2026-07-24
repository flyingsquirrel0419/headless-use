//! P0 v2 verification: in-flight network detection, mouse button state,
//! generation-bound references.

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

/// Verify that wait reads __hu_net_inflight__ and reports unstable when > 0.
#[tokio::test]
async fn p0_wait_reads_inflight_counter() {
    let (s, srv) = session().await;
    s.open(&srv.url("basic-form.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();

    // Inject a fake in-flight count to simulate an ongoing request.
    s.page()
        .evaluate("window.__hu_net_inflight__ = 3")
        .await
        .unwrap();

    // Wait with a short timeout — the page should NOT be stable.
    let result = s
        .wait(WaitOptions {
            timeout: Duration::from_millis(600),
            network_idle: Duration::from_millis(400),
            dom_quiet: Duration::from_millis(300),
        })
        .await
        .unwrap();

    assert!(
        !result.stable,
        "wait should report unstable when __hu_net_inflight__ > 0, got: {:?}",
        result
    );

    // Clear the counter and wait again — should be stable now.
    // Use a slightly longer dom_quiet since our evaluate calls may have
    // changed the DOM node count.
    tokio::time::sleep(Duration::from_millis(400)).await;
    s.page()
        .evaluate("window.__hu_net_inflight__ = 0")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let result2 = s.wait(Default::default()).await.unwrap();
    assert!(
        result2.stable,
        "should be stable when inflight=0, got: {:?}",
        result2
    );
    s.shutdown().await;
}

/// Verify that mouseMoved uses button="none" and does not report left-held.
#[tokio::test]
async fn p0_mouse_move_does_not_report_button_held() {
    let (s, srv) = session().await;
    s.open(&srv.url("mouse-buttons.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();

    // Move the cursor — the fixture logs all mouse events with button info.
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
        "expected StaleReference for wrong generation, got: {:?}",
        result
    );

    let correct_gen = obs.generation;
    let result = s
        .resolve_ref_with_generation(obs.elements[0].ref_id, Some(correct_gen))
        .await;
    assert!(
        result.is_ok(),
        "correct generation should resolve: {:?}",
        result
    );

    s.shutdown().await;
}

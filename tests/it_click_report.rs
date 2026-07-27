//! Integration tests: click hit test + effect observation window.
//!
//! Effect sampling is opt-in: the default click returns `effects: None` and
//! only runs the pre-click hit test. These tests opt in per call via
//! `click_with_effects_window` (or per session via `with_click_observe_window`).

mod common;

use std::time::Duration;

use headless_use::input::{Modifiers, MouseButton, Point};
use headless_use::session::{ClickTarget, DEFAULT_CLICK_EFFECTS_WINDOW};

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

/// Click at a point with effect sampling opted in.
async fn click_at(
    s: &headless_use::session::Session,
    x: f64,
    y: f64,
) -> headless_use::session::click_report::ClickReport {
    s.click_with_effects_window(
        ClickTarget::Point(Point::new(x, y)),
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
        Some(DEFAULT_CLICK_EFFECTS_WINDOW),
    )
    .await
    .unwrap()
}

/// Center of the fixture element with the given id, via observe.
///
/// Opts into the listener scan: the fixture's `#board` is a delegation
/// container that only listener detection promotes to an element.
async fn center_of(s: &headless_use::session::Session, hint: &str) -> (f64, f64) {
    let obs = s.observe_with_options(None, true).await.unwrap();
    let el = obs
        .elements
        .iter()
        .find(|e| e.selector_hint == hint)
        .unwrap_or_else(|| panic!("{hint} not observed"));
    el.center()
}

#[tokio::test]
async fn effective_click_reports_mutations() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let (bx, by) = center_of(&s, "#board").await;
    // Top-left cell: board center minus one cell (cells are 60px).
    let report = click_at(&s, bx - 60.0, by - 60.0).await;
    let effects = report.effects.expect("effects were opted in");
    assert!(
        effects.dom_mutations > 0,
        "placing an X mutates the DOM: {effects:?}"
    );
    assert!(!effects.navigated);
    s.shutdown().await;
}

#[tokio::test]
async fn default_click_skips_effect_sampling() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let (bx, by) = center_of(&s, "#board").await;
    // Plain click(): the light default. No effects window, but the hit test
    // still runs.
    let report = s
        .click(
            ClickTarget::Point(Point::new(bx - 60.0, by - 60.0)),
            MouseButton::Left,
            1,
            Modifiers::NONE,
            Duration::ZERO,
        )
        .await
        .unwrap();
    assert!(
        report.effects.is_none(),
        "effects must be None unless opted in"
    );
    assert!(report.hit.is_some(), "hit test runs on every click");
    s.shutdown().await;
}

#[tokio::test]
async fn rejected_click_reports_zero_effects() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let (bx, by) = center_of(&s, "#board").await;
    let cell = (bx - 60.0, by - 60.0);
    let first = click_at(&s, cell.0, cell.1).await;
    assert!(first.effects.unwrap().dom_mutations > 0);
    // Same (now taken) cell: game logic ignores the click entirely.
    let second = click_at(&s, cell.0, cell.1).await;
    let effects = second.effects.expect("effects were opted in");
    assert_eq!(
        effects.dom_mutations, 0,
        "taken cell: no DOM change, agent must see zero effects"
    );
    s.shutdown().await;
}

/// An id-less div promoted by the listener pass has an empty selectorHint;
/// ref re-resolution must fall back to tag+name+geometry instead of failing
/// as stale.
#[tokio::test]
async fn idless_promoted_div_is_clickable_via_ref() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    // The listener scan is opt-in; this test depends on listener promotion.
    let obs = s.observe_with_options(None, true).await.unwrap();
    let noid = obs
        .elements
        .iter()
        .find(|e| e.tag_name == "div" && e.name == "no-id button")
        .expect("id-less listener div promoted by observe");
    assert!(
        noid.selector_hint.is_empty(),
        "fixture element must have no id (that is the point of this test)"
    );
    let target = headless_use::session::Session::click_target_from_ref(&noid.ref_token).unwrap();
    let report = s
        .click_with_effects_window(
            target,
            MouseButton::Left,
            1,
            Modifiers::NONE,
            Duration::ZERO,
            Some(DEFAULT_CLICK_EFFECTS_WINDOW),
        )
        .await
        .expect("ref click on id-less promoted div must resolve, not go stale");
    let effects = report.effects.expect("effects were opted in");
    assert!(
        effects.dom_mutations > 0,
        "listener updates #status: the click must land on the div: {effects:?}"
    );
    s.shutdown().await;
}

#[tokio::test]
async fn occluded_target_is_reported() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("overlay-blocking.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let button = obs
        .elements
        .iter()
        .find(|e| e.selector_hint == "#underneath")
        .expect("underneath button observed");
    let target = headless_use::session::Session::click_target_from_ref(&button.ref_token).unwrap();
    let report = s
        .click(
            target,
            MouseButton::Left,
            1,
            Modifiers::NONE,
            Duration::ZERO,
        )
        .await
        .unwrap();
    let hit = report.hit.expect("hit test ran");
    assert_eq!(hit.matched_target, Some(false), "overlay covers the button");
    let occluder = hit.occluded_by.expect("occluder reported");
    assert!(occluder.contains("overlay"), "occluder was: {occluder}");
    s.shutdown().await;
}

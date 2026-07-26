//! Integration tests: click hit test + effect observation window.

mod common;

use std::time::Duration;

use headless_use::input::{Modifiers, MouseButton, Point};
use headless_use::session::ClickTarget;

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

async fn click_at(
    s: &headless_use::session::Session,
    x: f64,
    y: f64,
) -> headless_use::session::click_report::ClickReport {
    s.click(
        ClickTarget::Point(Point::new(x, y)),
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap()
}

/// Center of the fixture element with the given id, via observe.
async fn center_of(s: &headless_use::session::Session, hint: &str) -> (f64, f64) {
    let obs = s.observe().await.unwrap();
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
    let effects = report.effects.expect("default window is on");
    assert!(
        effects.dom_mutations > 0,
        "placing an X mutates the DOM: {effects:?}"
    );
    assert!(!effects.navigated);
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
    let effects = second.effects.expect("default window is on");
    assert_eq!(
        effects.dom_mutations, 0,
        "taken cell: no DOM change, agent must see zero effects"
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
        .click(target, MouseButton::Left, 1, Modifiers::NONE, Duration::ZERO)
        .await
        .unwrap();
    let hit = report.hit.expect("hit test ran");
    assert_eq!(hit.matched_target, Some(false), "overlay covers the button");
    let occluder = hit.occluded_by.expect("occluder reported");
    assert!(occluder.contains("overlay"), "occluder was: {occluder}");
    s.shutdown().await;
}

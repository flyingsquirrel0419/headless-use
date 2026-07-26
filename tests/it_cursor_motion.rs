//! Cursor motion + overlay appearance.
//!
//! These need a real headless Chrome. They verify the two things the unit
//! tests cannot reach: that `CursorMotion::Smooth` actually puts intermediate
//! `mousemove` events on the page (rather than just looking different), and
//! that the injected overlay is the solid pointer rather than the old neon one.

#![cfg(test)]

mod common;

use std::time::Duration;

use headless_use::input::{CursorMotion, Modifiers, MouseButton};
use headless_use::session::{ClickTarget, Session};

async fn session_with(motion: CursorMotion) -> (Session, common::FixtureServer) {
    common::init();
    let srv = common::FixtureServer::start().await;
    let s = Session::start(common::test_launch())
        .await
        .unwrap()
        .with_cursor_motion(motion);
    (s, srv)
}

async fn move_count(s: &Session) -> f64 {
    s.page()
        .evaluate("window.__moves")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_f64())
        .unwrap_or(-1.0)
}

/// Instant motion is one jump: the page sees a single mousemove before the
/// click, and no intermediate positions.
#[tokio::test]
async fn instant_motion_emits_a_single_move() {
    let (s, srv) = session_with(CursorMotion::Instant).await;
    s.open(&srv.url("mousemove-counter.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    s.page().evaluate("window.__resetMoves()").await.unwrap();

    s.click(
        ClickTarget::Point((600.0, 450.0).into()),
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();

    let moves = move_count(&s).await;
    assert_eq!(
        moves, 1.0,
        "instant motion should dispatch exactly one mousemove, got {moves}"
    );
    s.shutdown().await;
}

/// Smooth motion walks the path, so the page receives a burst of moves at
/// distinct positions — which is what makes hover-dependent UI work and what
/// the viewer renders as travel.
#[tokio::test]
async fn smooth_motion_emits_intermediate_moves() {
    let (s, srv) = session_with(CursorMotion::smooth_default()).await;
    s.open(&srv.url("mousemove-counter.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();

    // Put the cursor at a known origin first, then measure only the travel.
    s.hover(ClickTarget::Point((20.0, 20.0).into()))
        .await
        .unwrap();
    s.page().evaluate("window.__resetMoves()").await.unwrap();

    s.click(
        ClickTarget::Point((600.0, 450.0).into()),
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();

    let moves = move_count(&s).await;
    assert!(
        moves > 5.0,
        "smooth motion should dispatch many intermediate mousemoves, got {moves}"
    );
    // The click still lands: travel must not replace the press/release.
    let clicks = s
        .page()
        .evaluate("window.__clicks")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert_eq!(clicks, 1.0, "the click itself must still be delivered");
    s.shutdown().await;
}

/// Regression: `Mouse::click` used to re-issue a move to the destination it
/// had just been walked to, adding a phantom event to the stream.
#[tokio::test]
async fn smooth_click_does_not_double_move_at_the_target() {
    let (s, srv) = session_with(CursorMotion::smooth_default()).await;
    s.open(&srv.url("mousemove-counter.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    s.hover(ClickTarget::Point((600.0, 450.0).into()))
        .await
        .unwrap();
    // Cursor is already exactly on the target; a click should add no moves.
    s.page().evaluate("window.__resetMoves()").await.unwrap();
    s.click(
        ClickTarget::Point((600.0, 450.0).into()),
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    let moves = move_count(&s).await;
    assert_eq!(
        moves, 0.0,
        "clicking where the cursor already is should emit no mousemove, got {moves}"
    );
    s.shutdown().await;
}

/// The injected overlay is the solid pointer, not the previous neon arrow.
#[tokio::test]
async fn overlay_is_the_solid_pointer() {
    let (s, srv) = session_with(CursorMotion::Instant).await;
    s.page().inject_cursor_overlay().await.unwrap();
    s.open(&srv.url("mousemove-counter.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    // Move so the overlay attaches and becomes visible.
    s.hover(ClickTarget::Point((300.0, 300.0).into()))
        .await
        .unwrap();

    let markup = s
        .page()
        .evaluate("document.getElementById('hu-cursor').innerHTML")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .expect("overlay element must exist");

    assert!(
        markup.contains("#ffffff"),
        "pointer should be solid white: {markup}"
    );
    assert!(
        markup.contains("#111111"),
        "pointer should have a dark outline: {markup}"
    );
    assert!(
        !markup.contains("5ce1ff") && !markup.contains("a0f0ff"),
        "the neon palette should be gone: {markup}"
    );
    assert!(
        !markup.contains("feGaussianBlur"),
        "the blur halo should be gone: {markup}"
    );

    // The ripple element replaces the old persistent ring.
    let has_ripple = s
        .page()
        .evaluate("!!document.getElementById('hu-cursor-ripple')")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(has_ripple, "click ripple element should be injected");

    s.shutdown().await;
}

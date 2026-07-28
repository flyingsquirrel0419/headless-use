//! Verify canonical generation-bound references appear in observe output and
//! that button-click navigation invalidates references via Page.frameNavigated.

mod common;

use std::time::Duration;

use headless_use::input::{Modifiers, MouseButton};
use headless_use::session::ClickTarget;

#[tokio::test]
async fn observe_output_includes_canonical_ref_token() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let _s_profile = common::TempProfile::new();
    let s = headless_use::session::Session::start(_s_profile.launch_opts())
        .await
        .expect("session start");
    s.open(&srv.url("basic-form.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let gen = obs.generation;
    // Every element must carry a canonical @g<gen>:e<num> ref token.
    assert!(!obs.elements.is_empty(), "expected some elements");
    for el in &obs.elements {
        let expected = format!("@g{}:e{}", gen, el.ref_id);
        assert_eq!(
            el.ref_token, expected,
            "element ref_token mismatch: got {:?} expected {:?}",
            el.ref_token, expected
        );
    }
    // The compact text must also use the real generation (not hardcoded 1).
    let compact = obs.to_compact();
    assert!(
        compact.contains(&format!("@g{gen}:e")),
        "compact output should use generation {gen}, got: {compact}"
    );
    s.shutdown().await;
}

#[tokio::test]
async fn button_click_navigation_invalidates_references() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let _s_profile = common::TempProfile::new();
    let s = headless_use::session::Session::start(_s_profile.launch_opts())
        .await
        .expect("session start");
    // click-nav.html: clicking the button does a real document navigation
    // (location.href), which must fire Page.frameNavigated and invalidate refs.
    s.open(&srv.url("click-nav.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let gen_before = s.nav_generation_value();
    let btn = obs
        .elements
        .iter()
        .find(|e| e.role == "button")
        .map(|e| e.ref_id)
        .expect("button not found");

    // Click the button — this navigates via a real user action, not open().
    s.click(
        ClickTarget::Ref {
            id: btn,
            generation: None,
        },
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    // Give frameNavigated a moment to be processed by the listener task.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let gen_after = s.nav_generation_value();
    assert!(
        gen_after > gen_before,
        "button-click navigation should increment nav_generation via Page.frameNavigated (before={gen_before}, after={gen_after})"
    );

    // The old reference should now be stale (its nav_generation no longer matches).
    let result = s.resolve_ref(btn).await;
    assert!(
        matches!(result, Err(ref e) if matches!(e, headless_use::BrowserError::StaleReference(_))),
        "old reference should be stale after button-click navigation, got: {result:?}"
    );
    s.shutdown().await;
}

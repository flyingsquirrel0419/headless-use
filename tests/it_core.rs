//! Core integration tests: launch, navigate, screenshot, observe, click, type.

mod common;

use std::time::Duration;

use headless_use::input::{Modifiers, MouseButton};
use headless_use::observe::parse_ref;
use headless_use::session::ClickTarget;

async fn session() -> (headless_use::session::Session, common::FixtureServer) {
    common::init();
    let srv = common::FixtureServer::start().await;
    let s = headless_use::session::Session::start(common::test_launch())
        .await
        .expect("session start");
    (s, srv)
}

#[tokio::test]
async fn launch_navigate_screenshot() {
    let (s, srv) = session().await;
    s.open(&srv.url("basic-form.html")).await.unwrap();
    let url = s.page().url().await.unwrap();
    assert!(url.contains("basic-form.html"));
    let png = s.screenshot(false, None).await.unwrap();
    assert!(png.len() > 1000, "screenshot too small: {}", png.len());
    s.shutdown().await;
}

#[tokio::test]
async fn observe_finds_interactive_elements() {
    let (s, srv) = session().await;
    s.open(&srv.url("basic-form.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    // basic-form has: email, pass, remember, submit button, signup link = 5 interactive
    assert!(
        obs.elements.len() >= 4,
        "expected >=4 elements, got {}",
        obs.elements.len()
    );
    let has_button = obs
        .elements
        .iter()
        .any(|e| e.role == "button" && e.name.contains("로그인"));
    assert!(
        has_button,
        "login button not found: {:?}",
        obs.elements
            .iter()
            .map(|e| (&e.role, &e.name))
            .collect::<Vec<_>>()
    );
    s.shutdown().await;
}

#[tokio::test]
async fn click_and_type_login_flow() {
    let (s, srv) = session().await;
    s.open(&srv.url("basic-form.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    // Find email textbox ref
    let email_ref = obs
        .elements
        .iter()
        .find(|e| e.role == "textbox" && e.name.contains("이메일"))
        .map(|e| e.ref_id)
        .expect("email field not found");
    let pass_ref = obs
        .elements
        .iter()
        .find(|e| e.role == "textbox" && e.name.contains("비밀번호"))
        .map(|e| e.ref_id)
        .expect("pass field not found");
    let submit_ref = obs
        .elements
        .iter()
        .find(|e| e.role == "button" && e.name.contains("로그인"))
        .map(|e| e.ref_id)
        .expect("submit not found");

    s.click(
        ClickTarget::Ref {
            id: email_ref,
            generation: None,
        },
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    s.type_text("user@example.com", Duration::ZERO, false)
        .await
        .unwrap();

    s.click(
        ClickTarget::Ref {
            id: pass_ref,
            generation: None,
        },
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    s.insert_text("비밀123!", false).await.unwrap();

    s.click(
        ClickTarget::Ref {
            id: submit_ref,
            generation: None,
        },
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();

    let title = s.page().title().await.unwrap();
    assert_eq!(title, "Dashboard");
    s.shutdown().await;
}

#[tokio::test]
async fn coordinate_click_records_event() {
    let (s, srv) = session().await;
    s.open(&srv.url("mouse-buttons.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    // Click center of the target div.
    s.click(
        ClickTarget::Point(headless_use::input::Point::new(150.0, 110.0)),
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    let log = s
        .page()
        .evaluate("document.getElementById('log').textContent")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default();
    assert!(log.contains("click button=left"), "log was: {log}");
    s.shutdown().await;
}

#[tokio::test]
async fn right_click_and_double_click() {
    let (s, srv) = session().await;
    s.open(&srv.url("mouse-buttons.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    // Right click -> contextmenu event
    s.click(
        ClickTarget::Point(headless_use::input::Point::new(150.0, 110.0)),
        MouseButton::Right,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    let log = s
        .page()
        .evaluate("document.getElementById('log').textContent")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default();
    assert!(
        log.contains("contextmenu") && log.contains("button=right"),
        "log: {log}"
    );

    // Double click
    s.click(
        ClickTarget::Point(headless_use::input::Point::new(160.0, 110.0)),
        MouseButton::Left,
        2,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    let log2 = s
        .page()
        .evaluate("document.getElementById('log').textContent")
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default();
    assert!(log2.contains("dblclick"), "log: {log2}");
    s.shutdown().await;
}

#[tokio::test]
async fn parse_ref_helper() {
    assert_eq!(parse_ref("@e5").unwrap(), 5);
    assert_eq!(parse_ref("e9").unwrap(), 9);
}

//! Full end-to-end scenario: launch → login → drag → verify → trace → cleanup.
//!
//! This mirrors the README Quick Start, executed programmatically against local
//! fixtures, to guarantee the documented flow actually works.

mod common;

use std::time::Duration;

use headless_use::input::{Modifiers, MouseButton};
use headless_use::session::ClickTarget;
use headless_use::trace::Trace;

#[tokio::test]
async fn full_e2e_login_drag_trace_screenshot() {
    common::init();
    let srv = common::FixtureServer::start().await;

    // Start a session with tracing enabled.
    let trace = Trace::new(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
        .await
        .unwrap();
    let run_dir = trace.dir().to_path_buf();
    let _s_profile = common::TempProfile::new();
    let s = headless_use::session::Session::start(_s_profile.launch_opts())
        .await
        .unwrap()
        .with_trace(trace);

    // 1. Open the login page.
    s.open(&srv.url("basic-form.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();

    // 2. Observe to get references.
    let obs = s.observe().await.unwrap();
    let email = obs
        .elements
        .iter()
        .find(|e| e.role == "textbox" && e.name.contains("이메일"))
        .map(|e| e.ref_id)
        .unwrap();
    let pass = obs
        .elements
        .iter()
        .find(|e| e.role == "textbox" && e.name.contains("비밀번호"))
        .map(|e| e.ref_id)
        .unwrap();
    let submit = obs
        .elements
        .iter()
        .find(|e| e.role == "button" && e.name.contains("로그인"))
        .map(|e| e.ref_id)
        .unwrap();

    // 3. Fill the form using real keyboard input.
    s.click(
        ClickTarget::Ref {
            id: email,
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
            id: pass,
            generation: None,
        },
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap();
    s.insert_text("비밀번호!23", true).await.unwrap();
    s.key_press("Tab").await.unwrap();

    // 4. Submit and verify navigation.
    s.click(
        ClickTarget::Ref {
            id: submit,
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

    // 5. No console errors expected on this page.
    let console = s.console().await.unwrap();
    assert!(
        !console
            .iter()
            .any(|e| e.level == headless_use::session::ConsoleLevel::Error),
        "unexpected console errors: {:?}",
        console
    );

    // 6. Switch to a drag fixture and drag a slider.
    s.open(&srv.url("slider.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
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
    s.drag(
        headless_use::input::Point::new(x + w * 0.2, y),
        headless_use::input::Point::new(x + w * 0.9, y),
        MouseButton::Left,
        Duration::from_millis(400),
        20,
    )
    .await
    .unwrap();

    // 7. Scroll down.
    s.scroll(0.0, 200.0, None, Duration::from_millis(200), 10)
        .await
        .unwrap();

    // 8. Screenshot.
    let png = s.screenshot(false, None).await.unwrap();
    assert!(png.len() > 1000);

    // 9. Network check (no failed requests on a static page).
    let net = s.network().await.unwrap();
    assert!(
        !net.iter().any(|e| e.failed.is_some()),
        "unexpected failed requests: {:?}",
        net
    );

    // 10. Shutdown and verify trace artifacts.
    s.shutdown().await;
    let actions = std::fs::read_to_string(run_dir.join("actions.jsonl")).unwrap();
    assert!(actions.contains("open"));
    assert!(actions.contains("mouse.click"));
    assert!(actions.contains("mouse.drag"));
    assert!(actions.contains("scroll"));
    assert!(actions.contains("screenshot"));
    assert!(std::fs::metadata(run_dir.join("report.html")).is_ok());
    assert!(std::fs::metadata(run_dir.join("metadata.json")).is_ok());
    // Ensure the password was redacted in the trace.
    assert!(
        !actions.contains("비밀번호!23"),
        "password leaked into trace"
    );
    let _ = std::fs::remove_dir_all(&run_dir);
}

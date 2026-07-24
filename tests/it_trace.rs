//! Trace + replay + report tests.

mod common;

use std::time::Duration;

use headless_use::input::{Modifiers, MouseButton};
use headless_use::session::ClickTarget;
use headless_use::trace::Trace;

#[tokio::test]
async fn trace_records_actions_and_writes_report() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let trace = Trace::new(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
        .await
        .expect("trace new");
    let dir = trace.dir().to_path_buf();
    let session = headless_use::session::Session::start(common::test_launch())
        .await
        .unwrap()
        .with_trace(trace);
    session.open(&srv.url("basic-form.html")).await.unwrap();
    session.wait(Default::default()).await.unwrap();
    let obs = session.observe().await.unwrap();
    let email = obs
        .elements
        .iter()
        .find(|e| e.role == "textbox")
        .map(|e| e.ref_id)
        .unwrap();
    session
        .click(
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
    session
        .type_text("traced@example.com", Duration::ZERO, false)
        .await
        .unwrap();
    let png = session.screenshot(false, None).await.unwrap();
    assert!(png.len() > 1000);
    session.shutdown().await;

    // Verify the trace artifacts exist.
    let actions = std::fs::read_to_string(dir.join("actions.jsonl")).unwrap();
    assert!(actions.contains("\"type\":\"open\""), "actions: {actions}");
    assert!(
        actions.contains("\"type\":\"mouse.click\""),
        "actions: {actions}"
    );
    assert!(actions.contains("\"type\":\"type\""), "actions: {actions}");
    let meta = std::fs::read_to_string(dir.join("metadata.json")).unwrap();
    assert!(meta.contains("version"));
    let report = std::fs::read_to_string(dir.join("report.html")).unwrap();
    assert!(report.contains("headless-use trace report"));
    assert!(report.contains("mouse.click"));
    // Cleanup the run dir to avoid leaving artifacts.
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn trace_redacts_sensitive_type() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let trace = Trace::new(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
        .await
        .unwrap();
    let dir = trace.dir().to_path_buf();
    let session = headless_use::session::Session::start(common::test_launch())
        .await
        .unwrap()
        .with_trace(trace);
    session.open(&srv.url("basic-form.html")).await.unwrap();
    session.wait(Default::default()).await.unwrap();
    // Type a password marked sensitive; it must be redacted in the trace.
    session
        .type_text("super-secret-password", Duration::ZERO, true)
        .await
        .unwrap();
    session.shutdown().await;
    let actions = std::fs::read_to_string(dir.join("actions.jsonl")).unwrap();
    assert!(
        !actions.contains("super-secret-password"),
        "secret leaked in trace: {actions}"
    );
    assert!(
        actions.contains("[REDACTED]"),
        "expected redaction in: {actions}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

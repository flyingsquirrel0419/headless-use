//! Verify forced secret redaction at the trace/output boundary.
//!
//! These tests prove that secrets are redacted even when the caller forgets
//! `sensitive=true`, because:
//!   1. `type_text`/`insert_text` auto-detect password fields, and
//!   2. `Trace::record` applies `mask_json` as a final defense layer.

mod common;

use std::time::Duration;

use headless_use::input::{Modifiers, MouseButton};
use headless_use::session::ClickTarget;
use headless_use::trace::Trace;

#[tokio::test]
async fn type_into_password_field_redacted_without_sensitive_flag() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let trace = Trace::new(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
        .await
        .unwrap();
    let run_dir = trace.dir().to_path_buf();
    let _s_profile = common::TempProfile::new();
    let s = headless_use::session::Session::start(_s_profile.launch_opts())
        .await
        .unwrap()
        .with_trace(trace);

    s.open(&srv.url("password-field.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let pass_ref = obs
        .elements
        .iter()
        .find(|e| e.name.contains("password"))
        .map(|e| e.ref_id)
        .expect("password field not found");

    // Click the password field, then type WITHOUT sensitive=true.
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
    // Type the actual secret. The caller forgot sensitive=true.
    s.type_text("MySecretPassword123!", Duration::ZERO, false)
        .await
        .unwrap();

    s.shutdown().await;

    // The trace must NOT contain the raw password.
    let actions = std::fs::read_to_string(run_dir.join("actions.jsonl")).unwrap();
    assert!(
        !actions.contains("MySecretPassword123!"),
        "PASSWORD LEAKED into trace despite no sensitive flag: {actions}"
    );
    // It should contain the redaction marker.
    assert!(
        actions.contains("[REDACTED]"),
        "expected [REDACTED] in trace, got: {actions}"
    );
    let _ = std::fs::remove_dir_all(&run_dir);
}

#[tokio::test]
async fn trace_masks_json_params_with_secret_keys() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let trace = Trace::new(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
        .await
        .unwrap();
    let run_dir = trace.dir().to_path_buf();
    let _s_profile = common::TempProfile::new();
    let s = headless_use::session::Session::start(_s_profile.launch_opts())
        .await
        .unwrap()
        .with_trace(trace);

    s.open(&srv.url("basic-form.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();

    // Record an action directly with a secret-looking key/value. Even though
    // no caller flagged it, the final mask_json pass must redact it.
    s.record_action_raw(
        "test.secret",
        serde_json::json!({
            "url": "https://api.example.com/data?token=supersecretvalue",
            "authorization": "Bearer xyz123",
            "password": "hunter2",
            "name": "alice"
        }),
    )
    .await;
    s.shutdown().await;

    let actions = std::fs::read_to_string(run_dir.join("actions.jsonl")).unwrap();
    assert!(
        !actions.contains("supersecretvalue"),
        "token leaked into trace: {actions}"
    );
    assert!(
        !actions.contains("hunter2"),
        "password leaked into trace: {actions}"
    );
    assert!(
        !actions.contains("Bearer xyz123"),
        "authorization leaked into trace: {actions}"
    );
    // Non-secret values should be preserved.
    assert!(
        actions.contains("alice"),
        "non-secret value lost: {actions}"
    );
    let _ = std::fs::remove_dir_all(&run_dir);
}

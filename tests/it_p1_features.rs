//! P1 feature tests: host policy enforcement, element-region screenshot,
//! trace screenshot saving + report.html image embedding.

mod common;

use headless_use::security::Policy;
use headless_use::session::ClickTarget;
use headless_use::trace::Trace;

/// The [`common::TempProfile`] comes first in the tuple on purpose: bindings
/// drop in reverse declaration order, so a first-position guard is destroyed
/// last — after the session, even if a test panics before its `shutdown()`.
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

/// Host allow/deny policy must block navigation to disallowed hosts before any
/// request reaches the network, returning NAVIGATION_BLOCKED.
#[tokio::test]
async fn host_policy_blocks_disallowed_navigation() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let policy = Policy {
        allow_hosts: vec!["127.0.0.1".into()],
        deny_hosts: vec![],
    };
    let _s_profile = common::TempProfile::new();
    let s = headless_use::session::Session::start(_s_profile.launch_opts())
        .await
        .unwrap()
        .with_policy_async(policy)
        .await
        .unwrap();

    // Allowed host works.
    s.open(&srv.url("basic-form.html")).await.unwrap();

    // Denied host is blocked before any request.
    let result = s.open("http://evil.example.com/steal").await;
    assert!(
        matches!(
            result,
            Err(ref e) if e.code() == "NAVIGATION_BLOCKED"
        ),
        "navigation to evil.example.com should be blocked, got: {result:?}"
    );
    s.shutdown().await;
}

/// Deny list takes precedence over allow list.
#[tokio::test]
async fn deny_list_takes_precedence_over_allow() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let policy = Policy {
        allow_hosts: vec!["127.0.0.1".into()],
        deny_hosts: vec!["127.0.0.1".into()],
    };
    let _s_profile = common::TempProfile::new();
    let s = headless_use::session::Session::start(_s_profile.launch_opts())
        .await
        .unwrap()
        .with_policy_async(policy)
        .await
        .unwrap();
    let result = s.open(&srv.url("basic-form.html")).await;
    assert!(
        matches!(
            result,
            Err(ref e) if e.code() == "NAVIGATION_BLOCKED"
        ),
        "deny list should block even allow-listed host, got: {result:?}"
    );
    s.shutdown().await;
}

/// Element-region screenshot captures only the element's bounding box, which is
/// smaller than a full-viewport screenshot.
#[tokio::test]
async fn element_screenshot_captures_region() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("basic-form.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    // Find the submit button (로그인) — it's a small element.
    let submit = obs
        .elements
        .iter()
        .find(|e| e.role == "button" && e.name.contains("로그인"))
        .map(|e| e.ref_id)
        .expect("submit button not found");

    // Element screenshot.
    let element_png = s
        .screenshot(
            false,
            Some(ClickTarget::Ref {
                id: submit,
                generation: None,
            }),
        )
        .await
        .unwrap();
    // Full viewport screenshot.
    let full_png = s.screenshot(false, None).await.unwrap();

    assert!(
        element_png.len() > 100,
        "element screenshot too small: {} bytes",
        element_png.len()
    );
    // The element region must be smaller than the full viewport.
    assert!(
        element_png.len() < full_png.len(),
        "element screenshot ({}) should be smaller than viewport ({})",
        element_png.len(),
        full_png.len()
    );
    s.shutdown().await;
}

/// When tracing is enabled, a screenshot is saved into the run's screenshots/
/// directory and embedded (base64) in report.html.
#[tokio::test]
async fn trace_saves_screenshot_and_embeds_in_report() {
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

    // Take a screenshot — it should be saved to the trace.
    let png = s.screenshot(false, None).await.unwrap();
    assert!(png.len() > 1000);

    s.shutdown().await;

    // The screenshots directory should contain at least one PNG.
    let shots_dir = run_dir.join("screenshots");
    let entries: Vec<_> = std::fs::read_dir(&shots_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "expected at least one saved screenshot in {shots_dir:?}"
    );

    // report.html should embed the screenshot as a base64 data URI.
    let report = std::fs::read_to_string(run_dir.join("report.html")).unwrap();
    assert!(
        report.contains("data:image/png;base64,"),
        "report.html should embed screenshot as base64 data URI"
    );
    assert!(
        report.contains("screenshot"),
        "report.html should contain the screenshot action row"
    );
    let _ = std::fs::remove_dir_all(&run_dir);
}

/// Verify that the run_oneshot CLI path also saves a screenshot to a file when
/// requested (the --screenshot flag), and that trace screenshots work alongside.
#[tokio::test]
async fn rpc_screenshot_with_element_works() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("basic-form.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let gen = obs.generation;
    let submit = obs
        .elements
        .iter()
        .find(|e| e.role == "button" && e.name.contains("로그인"))
        .map(|e| e.ref_id)
        .expect("submit button not found");
    let ref_token = format!("@g{}:e{}", gen, submit);

    // RPC dispatch with element reference.
    let params = serde_json::json!({ "element": ref_token });
    let result = headless_use::cli::rpc::dispatch(&s, "screenshot", &params)
        .await
        .unwrap();
    assert!(result["bytes"].as_u64().unwrap() > 100);
    assert!(result["data"].as_str().unwrap().len() > 100);
    s.shutdown().await;
}

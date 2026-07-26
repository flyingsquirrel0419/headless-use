//! Host-policy enforcement on the paths a page controls.
//!
//! The front-door check (`Session::open` rejecting a denied URL) was already
//! covered. These tests cover what it does not: everything the *page* can
//! initiate once it is loaded — a server redirect, page script assigning
//! `location`, and subresource requests. Those are the paths a prompt-injected
//! agent or a hostile page actually uses, so a policy that only guards `open`
//! bounds nothing.
//!
//! Both the allowed and the denied host are real, reachable loopback addresses
//! (`127.0.0.1` and `127.0.0.2`, same fixture server). A denied host that fails
//! DNS would make these tests pass with the enforcement removed.

mod common;

use headless_use::security::Policy;
use headless_use::session::Session;

/// Allow 127.0.0.1 only; 127.0.0.2 is the reachable-but-denied host.
fn loopback_only_policy() -> Policy {
    Policy {
        allow_hosts: vec!["127.0.0.1".into()],
        deny_hosts: vec![],
    }
}

/// The [`common::TempProfile`] comes first in the tuple on purpose: bindings
/// drop in reverse declaration order, so a first-position guard is destroyed
/// last — after the session, even if a test panics before its `shutdown()`.
async fn session_with_policy() -> (common::TempProfile, Session) {
    let profile = common::TempProfile::new();
    let s = Session::start(profile.launch_opts())
        .await
        .unwrap()
        .with_policy_async(loopback_only_policy())
        .await
        .unwrap();
    (profile, s)
}

/// A page navigating itself via script must not escape the allow list.
/// This is the exact bypass available to an agent with `page.evaluate`.
#[tokio::test]
async fn script_navigation_to_denied_host_is_blocked() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let (_profile, s) = session_with_policy().await;

    s.open(&srv.url("basic-form.html")).await.unwrap();
    let denied = srv.url_via("127.0.0.2", "basic-form.html");

    let _ = s
        .page()
        .evaluate(&format!("location.href = {}", json_str(&denied)))
        .await;

    // Give the navigation attempt time to be issued and refused.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    let url = s.page().url().await.unwrap();
    assert!(
        !url.contains("127.0.0.2"),
        "script navigation reached the denied host: {url}"
    );
    assert!(
        s.policy_blocked_count() > 0,
        "policy should have blocked at least one request"
    );
    s.shutdown().await;
}

/// A 302 from an allowed host to a denied one must be refused. The caller only
/// ever named the allowed URL, so the front-door check passes and cannot help.
#[tokio::test]
async fn server_redirect_to_denied_host_is_blocked() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let (_profile, s) = session_with_policy().await;

    let denied = srv.url_via("127.0.0.2", "basic-form.html");
    // The redirect URL itself is on the allowed host.
    let _ = s.open(&srv.redirect_to(&denied)).await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let url = s.page().url().await.unwrap();
    assert!(
        !url.contains("127.0.0.2"),
        "redirect landed on the denied host: {url}"
    );
    assert!(
        s.policy_blocked_count() > 0,
        "policy should have blocked the redirected request"
    );
    s.shutdown().await;
}

/// Subresource loads (`fetch`, images, scripts) are requests too. A policy that
/// only inspects top-level navigation lets a page exfiltrate to any host.
#[tokio::test]
async fn subresource_request_to_denied_host_is_blocked() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let (_profile, s) = session_with_policy().await;

    s.open(&srv.url("basic-form.html")).await.unwrap();
    let denied = srv.url_via("127.0.0.2", "api/ok");

    let result = s
        .page()
        .evaluate(&format!(
            "fetch({}).then(r => 'status:' + r.status).catch(e => 'blocked')",
            json_str(&denied)
        ))
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        s.policy_blocked_count() > 0,
        "policy should have blocked the subresource fetch, evaluate returned {result:?}"
    );
    s.shutdown().await;
}

/// Regression pin: with interception installed, ordinary allowed traffic still
/// loads. An enforcement layer that also breaks the happy path is not a fix.
#[tokio::test]
async fn allowed_host_still_loads_with_interception_on() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let (_profile, s) = session_with_policy().await;

    s.open(&srv.url("basic-form.html")).await.unwrap();
    let title = s.page().title().await.unwrap();
    assert!(!title.is_empty(), "allowed page should load normally");

    let obs = s.observe().await.unwrap();
    assert!(
        !obs.elements.is_empty(),
        "observe should find elements on the allowed page"
    );
    assert_eq!(
        s.policy_blocked_count(),
        0,
        "nothing on the allowed host should have been blocked"
    );
    s.shutdown().await;
}

/// Wildcard host patterns must match case-insensitively, like the exact ones.
#[tokio::test]
async fn wildcard_allow_host_is_case_insensitive() {
    let p = Policy {
        allow_hosts: vec!["*.Example.COM".into()],
        deny_hosts: vec![],
    };
    assert!(p.allows("http://api.example.com/x").is_ok());
    assert!(p.allows("http://EXAMPLE.com/x").is_ok());
    assert!(p.allows("http://example.org/x").is_err());
}

/// JSON-encode a string for safe embedding in a JS expression.
fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

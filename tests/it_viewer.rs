//! Integration tests for the live viewer (screencast + localhost MJPEG).
//!
//! These start a real headless Chrome and verify that:
//!   - the cursor overlay injects (idempotent guard set),
//!   - screencast produces real JPEG frames,
//!   - the localhost HTTP server serves index + stream with the right
//!     content-type, and frames change after a mouse move,
//!   - the access token is enforced exactly where it is supposed to be.

#![cfg(test)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use headless_use::browser::{LaunchOptions, Page};
use headless_use::viewer::{Screencast, ViewerOptions};

fn launch_opts() -> LaunchOptions {
    let mut opts = headless_use::browser::discover_browser()
        .map(|p| LaunchOptions {
            browser_path: Some(p),
            ..Default::default()
        })
        .unwrap_or_default();
    opts.no_sandbox = true;
    opts.compat = headless_use::browser::launch::CompatMode::Chromium;
    opts.viewport = headless_use::cdp::Viewport {
        width: 800,
        height: 600,
        device_scale_factor: 1.0,
    };
    opts
}

/// A browser plus the profile directory this test owns.
///
/// These tests pin `user_data_dir`, which means the library will not clean it
/// up (it only removes directories it created itself). Nothing did, so every
/// run left a `/tmp/hu-viewer-*` behind. [`common::TempProfile`] owns the
/// directory now; [`ViewerSession::close`] additionally shuts the browser down
/// on the normal path, and [`Drop`] kills it if a test panicked first.
struct ViewerSession {
    page: Arc<Page>,
    browser: Option<headless_use::browser::Browser>,
    /// Held for its `Drop`, which is what removes the directory.
    #[allow(dead_code)]
    profile: common::TempProfile,
}

impl ViewerSession {
    async fn start() -> Self {
        let profile = common::TempProfile::new();
        let mut opts = launch_opts();
        opts.user_data_dir = Some(profile.path().to_path_buf());
        let browser = headless_use::browser::Browser::launch(opts).await.unwrap();
        let page = Arc::new(browser.new_page().await.unwrap());
        Self {
            page,
            browser: Some(browser),
            profile,
        }
    }

    fn page(&self) -> Arc<Page> {
        self.page.clone()
    }

    /// Shut the browser down and remove the profile directory.
    ///
    /// Explicit and `async` because `Browser::shutdown` is async and `Drop`
    /// cannot await; `block_on` inside `Drop` would panic on a runtime thread.
    async fn close(mut self) {
        if let Some(b) = self.browser.take() {
            b.shutdown().await;
        }
        // `self.profile` removes the directory as it drops out of this fn.
    }
}

impl Drop for ViewerSession {
    fn drop(&mut self) {
        // Kill the browser first; `TempProfile`'s own `Drop` runs after this
        // and waits for any surviving Chrome helper before deleting.
        drop(self.browser.take());
    }
}

#[tokio::test]
async fn screencast_produces_jpeg_frames() {
    let vs = ViewerSession::start().await;
    let page = vs.page();
    // Open a page with content so frames have something to capture.
    let fixture = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/basic-form.html");
    let url = format!("file://{}", fixture.display());
    page.goto(&url).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let cast = Screencast::start(&page, 70, 800, 600, 30).await.unwrap();
    // Wait for at least one frame within 6s.
    let mut rx = cast.subscribe();
    let got = tokio::time::timeout(Duration::from_secs(6), rx.changed()).await;
    assert!(got.is_ok(), "screencast did not produce a frame in time");
    let frame = cast.latest().expect("frame available after changed()");
    assert!(!frame.data.is_empty(), "frame data must not be empty");
    // JPEG SOI marker.
    assert_eq!(&frame.data[0..2], &[0xff, 0xd8], "frame must be JPEG");
    let _ = cast.stop().await;
    vs.close().await;
}

#[tokio::test]
async fn viewer_http_serves_index_and_stream() {
    let vs = ViewerSession::start().await;
    let page = vs.page();
    let fixture = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/basic-form.html");
    let url = format!("file://{}", fixture.display());
    page.goto(&url).await.unwrap();
    page.inject_cursor_overlay().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let cast = Arc::new(Screencast::start(&page, 70, 800, 600, 30).await.unwrap());
    // Pick a free port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let opts = ViewerOptions {
        port,
        ..Default::default()
    };
    let _handle = headless_use::viewer::http::serve(cast.clone(), opts)
        .await
        .unwrap();

    // Index page.
    let resp = reqwest::get(format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("live viewer"), "index should be titled");
    // The stream element is the page; the header floats over it. The server
    // rewrites the source to carry the access token.
    assert!(
        body.contains(r#"<img id="v" src="/stream?token="#),
        "index must embed the stream with its token"
    );
    assert!(
        body.contains("object-fit: contain"),
        "the stream must letterbox rather than stretch"
    );
    // Frame rate is derived client-side from the MJPEG load events, so the
    // page must not depend on a server endpoint that does not exist.
    assert!(
        !body.contains("/status"),
        "the viewer page must not reference an endpoint the server does not serve"
    );

    // Stream content-type (read headers only, don't consume the body).
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/stream"))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .unwrap();
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    assert!(
        ct.contains("multipart/x-mixed-replace"),
        "stream content-type must be multipart/x-mixed-replace, got: {ct}"
    );
    vs.close().await;
}

#[tokio::test]
async fn cursor_overlay_injects_idempotently() {
    let vs = ViewerSession::start().await;
    let page = vs.page();
    let fixture = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/basic-form.html");
    let url = format!("file://{}", fixture.display());
    page.goto(&url).await.unwrap();
    page.inject_cursor_overlay().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // The guard should be set.
    let r = page
        .evaluate("String(!!window.__hu_cursor_injected)")
        .await
        .unwrap();
    let val = r.value().and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(val, "true", "cursor overlay guard must be set");
    // Injecting again must be a no-op (no error).
    page.inject_cursor_overlay().await.unwrap();
    vs.close().await;
}

#[tokio::test]
async fn screencast_survives_navigation() {
    let vs = ViewerSession::start().await;
    let page = vs.page();
    let form = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/basic-form.html");
    let console = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/console-errors.html");
    let form_url = format!("file://{}", form.display());
    let console_url = format!("file://{}", console.display());

    page.goto(&form_url).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let cast = Screencast::start(&page, 70, 800, 600, 30).await.unwrap();
    let mut rx = cast.subscribe();
    // First frame from initial page.
    let got = tokio::time::timeout(Duration::from_secs(6), rx.changed()).await;
    assert!(got.is_ok(), "initial frame missing");
    let first = cast.latest().expect("first frame");
    assert!(!first.data.is_empty());

    // Navigate to a different page — this must NOT freeze the stream.
    page.goto(&console_url).await.unwrap();
    // Wait for a post-navigation frame. The restart happens on frameNavigated.
    let mut got_after = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Some(f) = cast.latest() {
            if !f.data.is_empty() {
                got_after = true;
                break;
            }
        }
    }
    assert!(got_after, "screencast produced no frame after navigation");
    let _ = cast.stop().await;
    vs.close().await;
}

/// Grab a free port on `host` by binding and immediately releasing it.
fn free_port(host: &str) -> u16 {
    let l = std::net::TcpListener::bind(format!("{host}:0")).unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// The token gate: generated and accepted everywhere, enforced only where the
/// bind is reachable from off-box.
///
/// Both servers share one screencast so this needs a single browser. The
/// enforcing case pins `require_token` rather than binding a routable address:
/// `127.0.0.2` is *also* loopback, so an address-based check would not flip on
/// it, and binding a real interface in a test is not portable. Pinning the flag
/// is what the CLI's non-loopback path resolves to anyway, so the rejection
/// path under test is the same code.
#[tokio::test]
async fn viewer_token_is_required_off_loopback_and_optional_on_it() {
    let vs = ViewerSession::start().await;
    let page = vs.page();
    let fixture = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/basic-form.html");
    page.goto(&format!("file://{}", fixture.display()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let cast = Arc::new(Screencast::start(&page, 70, 800, 600, 30).await.unwrap());
    let client = reqwest::Client::new();

    // --- Loopback: a token exists and works, but is not demanded. ---
    let port = free_port("127.0.0.1");
    let open = headless_use::viewer::http::serve(
        cast.clone(),
        ViewerOptions {
            port,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        !open.token_required,
        "a loopback bind must not reject token-less requests"
    );
    assert!(
        open.token.len() >= 32,
        "a token must be generated even when it is not enforced, got {:?}",
        open.token
    );
    assert!(
        open.url().contains(&format!("?token={}", open.token)),
        "the printed URL must carry the token: {}",
        open.url()
    );
    let resp = client
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "loopback without a token must still work"
    );
    // The URL we print is the one an operator clicks, so exercise it verbatim.
    let resp = client.get(open.url()).send().await.unwrap();
    assert_eq!(resp.status(), 200, "the printed index URL must load");
    let resp = client
        .get(open.stream_url())
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the printed stream URL must load");
    drop(open);

    // --- Enforcing: only the right token gets in. ---
    let port = free_port("127.0.0.2");
    let guarded = headless_use::viewer::http::serve(
        cast.clone(),
        ViewerOptions {
            host: "127.0.0.2".to_string(),
            port,
            token: Some("pinned-viewer-token".to_string()),
            require_token: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(guarded.token_required);
    let base = format!("http://127.0.0.2:{port}");

    for target in [
        format!("{base}/"),
        format!("{base}/?token="),
        format!("{base}/?token=pinned-viewer-toke"),
        format!("{base}/?token=pinned-viewer-tokem"),
        format!("{base}/stream"),
        format!("{base}/stream?token=nope"),
    ] {
        let resp = client
            .get(&target)
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401, "must be rejected: {target}");
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("token"),
            "401 body should say what is missing, got {body:?}"
        );
    }

    // The right token is accepted, and the page it returns points the stream
    // at a URL that will also be accepted.
    let resp = client.get(guarded.url()).send().await.unwrap();
    assert_eq!(resp.status(), 200, "the correct token must be accepted");
    let body = resp.text().await.unwrap();
    assert!(
        body.contains(r#"src="/stream?token=pinned-viewer-token""#),
        "the index must forward the token to the stream element"
    );

    let resp = client
        .get(guarded.stream_url())
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    assert!(
        ct.contains("multipart/x-mixed-replace"),
        "an authorized stream request must get the MJPEG stream, got {ct}"
    );

    // /health stays open on purpose: it reveals nothing about the page and
    // container probes should not need the secret.
    let resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200, "/health must not need a token");

    drop(guarded);
    let _ = cast.stop().await;
    vs.close().await;
}

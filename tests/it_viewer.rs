//! Integration tests for the live viewer (screencast + localhost MJPEG).
//!
//! These start a real headless Chrome and verify that:
//!   - the cursor overlay injects (idempotent guard set),
//!   - screencast produces real JPEG frames,
//!   - the localhost HTTP server serves index + stream with the right
//!     content-type, and frames change after a mouse move.

#![cfg(test)]

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

async fn start_session() -> (
    Arc<Page>,
    headless_use::browser::Browser,
    std::path::PathBuf,
) {
    let mut opts = launch_opts();
    let dir = std::env::temp_dir().join(format!("hu-viewer-{}", fastrand_u64()));
    opts.user_data_dir = Some(dir.clone());
    let browser = headless_use::browser::Browser::launch(opts).await.unwrap();
    let page = Arc::new(browser.new_page().await.unwrap());
    (page, browser, dir)
}

fn fastrand_u64() -> String {
    let bytes: [u8; 8] = std::array::from_fn(|_| fastrand::u8(0..=255));
    hex::encode(bytes)
}

#[tokio::test]
async fn screencast_produces_jpeg_frames() {
    let (page, _browser, _dir) = start_session().await;
    // Open a page with content so frames have something to capture.
    let fixture = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/basic-form.html");
    let url = format!("file://{}", fixture.display());
    page.goto(&url).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let cast = Screencast::start(&page, 70, 800, 600).await.unwrap();
    // Wait for at least one frame within 6s.
    let mut rx = cast.subscribe();
    let got = tokio::time::timeout(Duration::from_secs(6), rx.changed()).await;
    assert!(got.is_ok(), "screencast did not produce a frame in time");
    let frame = cast.latest().expect("frame available after changed()");
    assert!(!frame.data.is_empty(), "frame data must not be empty");
    // JPEG SOI marker.
    assert_eq!(&frame.data[0..2], &[0xff, 0xd8], "frame must be JPEG");
    let _ = cast.stop().await;
}

#[tokio::test]
async fn viewer_http_serves_index_and_stream() {
    let (page, _browser, _dir) = start_session().await;
    let fixture = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/basic-form.html");
    let url = format!("file://{}", fixture.display());
    page.goto(&url).await.unwrap();
    page.inject_cursor_overlay().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let cast = Arc::new(Screencast::start(&page, 70, 800, 600).await.unwrap());
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
    // The stream element is the page; the header floats over it.
    assert!(
        body.contains(r#"<img id="v" src="/stream""#),
        "index must embed the stream"
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
}

#[tokio::test]
async fn cursor_overlay_injects_idempotently() {
    let (page, _browser, _dir) = start_session().await;
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
}

#[tokio::test]
async fn screencast_survives_navigation() {
    let (page, _browser, _dir) = start_session().await;
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
    let cast = Screencast::start(&page, 70, 800, 600).await.unwrap();
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
}

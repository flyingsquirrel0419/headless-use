//! Integration test for the stdio JSON-RPC `serve` mode.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn binary() -> String {
    env!("CARGO_BIN_EXE_headless-use").to_string()
}

fn send(stdin: &mut impl Write, method: &str, params: Value, id: i64) {
    let req = json!({ "id": id, "method": method, "params": params, "jsonrpc": "2.0" });
    writeln!(stdin, "{}", serde_json::to_string(&req).unwrap()).unwrap();
    stdin.flush().unwrap();
}

/// Read one JSON line with an overall deadline. Skips blank lines.
fn recv(reader: &mut impl BufRead, timeout: Duration) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    let mut line = String::new();
    loop {
        if Instant::now() > deadline {
            return None;
        }
        line.clear();
        // Non-blocking-ish: read_line blocks until data or EOF. Since serve writes
        // one line per response, this returns promptly when a response arrives.
        match reader.read_line(&mut line) {
            Ok(0) => return None, // EOF
            Ok(_) => {
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                return serde_json::from_str(t).ok();
            }
            Err(_) => return None,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_rpc_observe_click_type() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let profile = common::TempProfile::new();
    let mut child = Command::new(binary())
        .args(["serve", "--no-sandbox", "--user-data-dir"])
        .arg(profile.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let to = Duration::from_secs(25);

    send(
        &mut stdin,
        "browser.open",
        json!({ "url": srv.url("basic-form.html") }),
        1,
    );
    let r1 = recv(&mut stdout, to).expect("open response");
    assert!(r1["result"]["url"]
        .as_str()
        .unwrap()
        .contains("basic-form.html"));

    send(&mut stdin, "observe", json!({}), 2);
    let r2 = recv(&mut stdout, to).expect("observe response");
    let elements = r2["result"]["elements"].as_array().unwrap();
    assert!(elements.len() >= 4);

    let email_ref = elements
        .iter()
        .find(|e| e["role"] == "textbox" && e["name"].as_str().unwrap().contains("이메일"))
        .map(|e| e["ref_id"].as_u64().unwrap())
        .unwrap();
    send(
        &mut stdin,
        "click",
        json!({ "ref": format!("@e{}", email_ref) }),
        3,
    );
    let r3 = recv(&mut stdout, to).expect("click response");
    assert_eq!(r3["result"]["success"], json!(true));

    send(
        &mut stdin,
        "type",
        json!({ "text": "agent@example.com" }),
        4,
    );
    let r4 = recv(&mut stdout, to).expect("type response");
    assert_eq!(r4["result"]["success"], json!(true));

    send(&mut stdin, "observe", json!({}), 5);
    let r5 = recv(&mut stdout, to).expect("observe2 response");
    let val = r5["result"]["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["role"] == "textbox" && e["name"].as_str().unwrap().contains("이메일"))
        .and_then(|e| e["value"].as_str())
        .map(String::from);
    assert_eq!(val.as_deref(), Some("agent@example.com"));

    send(&mut stdin, "browser.close", json!({}), 99);
    let _ = recv(&mut stdout, to);
    drop(stdin);
    common::shutdown_child(child);
}

/// `page.evaluate` lets the agent run arbitrary JS to query element coordinates
/// directly — the escape hatch for canvas/SVG/non-standard widgets that observe's
/// heuristic misses. It returns the JSON value of the expression.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_page_evaluate_returns_value() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let profile = common::TempProfile::new();
    let mut child = Command::new(binary())
        .args(["serve", "--no-sandbox", "--user-data-dir"])
        .arg(profile.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let to = Duration::from_secs(25);

    send(
        &mut stdin,
        "browser.open",
        json!({ "url": srv.url("visual-widget.html") }),
        1,
    );
    let _ = recv(&mut stdout, to).expect("open response");

    // Query the fake-captcha box bounds directly via JS.
    send(
        &mut stdin,
        "page.evaluate",
        json!({
            "expression": "(()=>{const e=document.getElementById('fake-captcha');const r=e.getBoundingClientRect();return JSON.stringify({x:r.x,y:r.y,w:r.width,h:r.height});})()"
        }),
        2,
    );
    let r = recv(&mut stdout, to).expect("evaluate response");
    let val_str = r["result"]["value"].as_str().expect("value");
    let v: Value = serde_json::from_str(val_str).expect("parse");
    // The fake-captcha is at left:200px, top:150px, width 200 (+20 padding), height 70.
    assert_eq!(v["x"].as_f64(), Some(200.0));
    assert_eq!(v["y"].as_f64(), Some(150.0));

    send(&mut stdin, "browser.close", json!({}), 99);
    let _ = recv(&mut stdout, to);
    drop(stdin);
    common::shutdown_child(child);
}

/// observe must capture non-standard visual widgets (canvas, cursor:pointer
/// divs) so an agent gets a ref for them instead of guessing coordinates.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_observe_captures_visual_widgets() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let profile = common::TempProfile::new();
    let mut child = Command::new(binary())
        .args(["serve", "--no-sandbox", "--user-data-dir"])
        .arg(profile.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let to = Duration::from_secs(25);

    send(
        &mut stdin,
        "browser.open",
        json!({ "url": srv.url("visual-widget.html") }),
        1,
    );
    let _ = recv(&mut stdout, to).expect("open response");

    // Default observe: should include the canvas and the cursor:pointer divs.
    send(&mut stdin, "observe", json!({ "mode": "compact" }), 2);
    let r = recv(&mut stdout, to).expect("observe response");
    let elements = r["result"]["elements"].as_array().unwrap();
    let has_canvas = elements
        .iter()
        .any(|e| e["tagName"] == "canvas" || e["role"] == "canvas");
    assert!(has_canvas, "observe should capture the canvas widget");
    let has_clickable_div = elements
        .iter()
        .any(|e| e["role"] == "div" && e["visual"] == true);
    assert!(
        has_clickable_div,
        "observe should capture the cursor:pointer div as a visual widget"
    );

    // interactive-only mode must EXCLUDE visual widgets.
    send(
        &mut stdin,
        "observe",
        json!({ "mode": "interactive-only" }),
        3,
    );
    let r2 = recv(&mut stdout, to).expect("observe interactive-only");
    let elems_io = r2["result"]["elements"].as_array().unwrap();
    let visual_count = elems_io.iter().filter(|e| e["visual"] == true).count();
    assert_eq!(
        visual_count, 0,
        "interactive-only must exclude visual widgets"
    );

    send(&mut stdin, "browser.close", json!({}), 99);
    let _ = recv(&mut stdout, to);
    drop(stdin);
    common::shutdown_child(child);
}

/// screenshot --annotate returns a PNG with boxes/labels drawn over it. We
/// verify it returns valid base64 PNG data larger than the raw screenshot
/// (annotations add pixels) and that it reports the annotated flag.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_screenshot_annotate_returns_png_with_boxes() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let profile = common::TempProfile::new();
    let mut child = Command::new(binary())
        .args(["serve", "--no-sandbox", "--user-data-dir"])
        .arg(profile.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let to = Duration::from_secs(25);

    send(
        &mut stdin,
        "browser.open",
        json!({ "url": srv.url("visual-widget.html") }),
        1,
    );
    let _ = recv(&mut stdout, to).expect("open response");

    // Raw screenshot.
    send(&mut stdin, "screenshot", json!({}), 2);
    let raw = recv(&mut stdout, to).expect("raw screenshot");
    let raw_bytes = raw["result"]["bytes"].as_u64().unwrap();

    // Annotated screenshot.
    send(&mut stdin, "screenshot", json!({ "annotate": true }), 3);
    let ann = recv(&mut stdout, to).expect("annotated screenshot");
    assert_eq!(ann["result"]["annotated"], json!(true));
    let ann_bytes = ann["result"]["bytes"].as_u64().unwrap();
    // Annotated image has boxes/labels drawn on top, so it's at least as big,
    // usually bigger due to recompression of changed pixels.
    assert!(
        ann_bytes >= raw_bytes / 2,
        "annotated bytes ({ann_bytes}) should be a plausible PNG size vs raw ({raw_bytes})"
    );
    // The data must decode as a PNG.
    let data = ann["result"]["data"].as_str().unwrap();
    let png = base64_decode(data);
    assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n", "must be a PNG");

    send(&mut stdin, "browser.close", json!({}), 99);
    let _ = recv(&mut stdout, to);
    drop(stdin);
    common::shutdown_child(child);
}

fn base64_decode(s: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .expect("base64")
}

/// Dewiggle must capture multiple frames, realign them, and return a valid
/// PNG plus per-glyph crops. This exercises the full Session::dewiggle path
/// over JSON-RPC against the local wiggle-text fixture.
///
/// This was `#[ignore]`d for a year of commits on the theory that a perpetual
/// rAF loop wedges CDP unless a screencast is pumping the page. That was not
/// the cause. The test alone in this file used the default single-threaded
/// runtime, while `recv` blocks the thread on the child's stdout — starving
/// the fixture server task that shares that runtime, so the page never
/// finished loading and the next CDP call timed out. The animation was a
/// red herring; `flavor = "multi_thread"`, as every other test here already
/// used, is the fix. It now runs in CI like everything else.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_dewiggle_returns_realigned_png_and_char_crops() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let profile = common::TempProfile::new();
    let mut child = Command::new(binary())
        .args(["serve", "--no-sandbox", "--user-data-dir"])
        .arg(profile.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let to = Duration::from_secs(40);

    send(
        &mut stdin,
        "browser.open",
        json!({ "url": srv.url("wiggle-text.html") }),
        1,
    );
    let _ = recv(&mut stdout, to).expect("open response");

    // Let the fixture finish loading + start its animation. Animated fixtures
    // need a longer settle; wait up to 10s.
    send(&mut stdin, "wait", json!({ "timeout": 10000 }), 7);
    let _ = recv(&mut stdout, to);

    // Dewiggle: 8 frames, 6 glyph bands. Auto-detect the canvas region.
    send(
        &mut stdin,
        "dewiggle",
        json!({ "frames": 8, "intervalMs": 60, "chars": 6 }),
        2,
    );
    // Dewiggle captures N screenshots sequentially, which can take a while
    // in a cold headless browser, so allow a generous timeout.
    let resp = recv(&mut stdout, Duration::from_secs(90)).expect("dewiggle response");
    assert_eq!(
        resp["result"]["frameCount"],
        json!(8),
        "dewiggle did not return 8 frames; full response: {resp}"
    );
    // Region auto-detected from the canvas bounding box.
    assert!(
        resp["result"]["region"]["width"].as_f64().unwrap() > 100.0,
        "region should be auto-detected from canvas"
    );
    // Six glyph bands.
    assert_eq!(resp["result"]["bands"].as_array().unwrap().len(), 6);
    // Per-glyph crops present.
    let crops = resp["result"]["charCrops"].as_array().unwrap();
    assert_eq!(crops.len(), 6, "should have 6 glyph crops");
    // The realigned image is a valid PNG.
    let img = resp["result"]["image"].as_str().unwrap();
    let png = base64_decode(img);
    assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n", "image must be a PNG");
    assert!(
        resp["result"]["imageBytes"].as_u64().unwrap() > 500,
        "realigned PNG should be non-trivial"
    );
    // Each crop is also a valid PNG.
    for (i, c) in crops.iter().enumerate() {
        let b = base64_decode(c.as_str().unwrap());
        assert_eq!(&b[0..8], b"\x89PNG\r\n\x1a\n", "crop {i} must be a PNG");
    }

    send(&mut stdin, "browser.close", json!({}), 99);
    let _ = recv(&mut stdout, to);
    drop(stdin);
    common::shutdown_child(child);
}

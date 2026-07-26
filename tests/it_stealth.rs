//! Stealth mode: launch flags, the JS pre-load script, and what a page sees.
//!
//! Two layers are covered here:
//!   1. `stealth_preload_script_patches_a_headless_dom` runs the real
//!      `stealth.js` against a fake headless DOM under `node`. It is the only
//!      way to execute that file outside a browser, and it catches the failure
//!      mode that matters most: a script that throws (or silently patches
//!      nothing) still "injects" successfully over CDP.
//!   2. The browser tests assert the observable result end to end — the values
//!      a bot check actually reads.

mod common;

use headless_use::session::Session;

/// Launch options with stealth on, plus the root-safe sandbox setting the rest
/// of the suite uses.
fn stealth_launch(profile: &common::TempProfile) -> headless_use::LaunchOptions {
    headless_use::LaunchOptions {
        stealth: true,
        ..profile.launch_opts()
    }
}

#[test]
fn stealth_preload_script_patches_a_headless_dom() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let harness = format!("{manifest}/tests/js/stealth_dom.mjs");
    let script = format!("{manifest}/src/browser/stealth.js");
    let out = match std::process::Command::new("node")
        .args([&harness, &script])
        .output()
    {
        Ok(out) => out,
        // node is not a build dependency of this crate; skipping keeps the
        // suite runnable on a bare machine.
        Err(e) => {
            eprintln!("skipping: node unavailable ({e})");
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stealth.js checks failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("all ") && stdout.contains("checks passed"),
        "harness produced no summary:\n{stdout}"
    );
}

#[tokio::test]
async fn stealth_hides_the_headless_signals_a_bot_check_reads() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let _profile = common::TempProfile::new();
    let s = Session::start(stealth_launch(&_profile))
        .await
        .expect("session start");
    s.open(&srv.url("basic-form.html")).await.unwrap();

    let probe = r#"JSON.stringify({
        webdriver: navigator.webdriver,
        ua: navigator.userAgent,
        brands: (navigator.userAgentData ? navigator.userAgentData.brands : []).map(b => b.brand).join(','),
        plugins: navigator.plugins.length,
        hasChrome: !!window.chrome,
        chromeHeight: window.outerHeight - window.innerHeight,
        screenBiggerThanWindow: screen.width >= window.innerWidth && screen.height > window.innerHeight,
        webglRenderer: (() => {
            try {
                const gl = document.createElement('canvas').getContext('webgl');
                if (!gl) return 'NO_WEBGL';
                const ext = gl.getExtension('WEBGL_debug_renderer_info');
                return ext ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) : 'NO_EXT';
            } catch (e) { return 'ERR'; }
        })(),
        nativeWebdriverGetter: String(Object.getOwnPropertyDescriptor(Navigator.prototype, 'webdriver')?.get)
            .includes('[native code]'),
        nativeFetch: String(window.fetch).includes('[native code]'),
    })"#;
    let raw = s
        .page()
        .evaluate(probe)
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .expect("probe returned a string");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();

    assert_eq!(v["webdriver"], serde_json::json!(false), "{v}");
    let ua = v["ua"].as_str().unwrap();
    assert!(!ua.contains("Headless"), "UA still says headless: {ua}");
    let brands = v["brands"].as_str().unwrap();
    assert!(
        !brands.contains("Headless"),
        "client-hint brands still say headless: {brands}"
    );
    assert!(
        v["plugins"].as_i64().unwrap() > 0,
        "empty PluginArray is a headless signature: {v}"
    );
    assert_eq!(v["hasChrome"], serde_json::json!(true), "{v}");
    assert!(
        v["chromeHeight"].as_i64().unwrap() > 0,
        "outerHeight == innerHeight means no browser UI: {v}"
    );
    assert_eq!(v["screenBiggerThanWindow"], serde_json::json!(true), "{v}");
    let renderer = v["webglRenderer"].as_str().unwrap();
    assert!(
        !renderer.contains("SwiftShader") && renderer != "NO_WEBGL",
        "WebGL renderer betrays software rendering: {renderer}"
    );
    // The patches themselves must not be detectable.
    assert_eq!(v["nativeWebdriverGetter"], serde_json::json!(true), "{v}");
    assert_eq!(v["nativeFetch"], serde_json::json!(true), "{v}");

    s.shutdown().await;
}

#[tokio::test]
async fn stealth_reaches_cross_origin_iframes() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let _profile = common::TempProfile::new();
    let s = Session::start(stealth_launch(&_profile))
        .await
        .expect("session start");
    // 127.0.0.1 vs localhost is a different origin, so this frame is an OOPIF —
    // the same shape as a CAPTCHA widget's iframe, and a separate CDP target
    // that the page's own pre-load script never reaches.
    let cross_origin = srv.url("basic-form.html").replace("127.0.0.1", "localhost");
    let page_url = srv.url("basic-form.html");
    s.open(&page_url).await.unwrap();
    let inject = format!(
        r#"new Promise(resolve => {{
            const f = document.createElement('iframe');
            f.src = {cross_origin:?};
            f.onload = () => resolve('loaded');
            document.body.appendChild(f);
            setTimeout(() => resolve('timeout'), 10000);
        }})"#
    );
    let loaded = s
        .page()
        .evaluate(&inject)
        .await
        .unwrap()
        .value()
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default();
    // Stealth pauses every auto-attached target (`waitForDebuggerOnStart`) so
    // the patches land before the frame's own scripts run. If any path forgets
    // to resume, the frame never fires `load` and this is what catches it.
    assert_eq!(
        loaded, "loaded",
        "the auto-attached frame must be resumed, not left paused"
    );

    s.shutdown().await;
}

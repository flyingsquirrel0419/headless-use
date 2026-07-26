# Viewer Streaming Quality + Page Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lower live-viewer stream latency, make `--fps` real, add a `/status` endpoint, and rebuild the viewer page with a status panel, light mode, and refined theme.

**Architecture:** All server work stays in the hand-written HTTP server (`src/viewer/http.rs`) — no new dependencies. `--fps` becomes a per-connection write throttle (the watch channel already keeps only the latest frame, so skipped sends lose nothing). `/status` is fed by a 2s read-only CDP poll task in `commands.rs` through a shared `Arc<RwLock<StatusInfo>>`. The viewer page is a single static `INDEX_HTML` string; the redesign swaps that string only.

**Tech Stack:** Rust, tokio, serde_json, CDP (`Page.startScreencast`, `Runtime.evaluate`), hand-written HTTP/1.0, vanilla HTML/CSS/JS.

**Spec:** `docs/superpowers/specs/2026-07-26-viewer-streaming-quality-design.md`

## Global Constraints

- No new crate dependencies (project rule: hand-written HTTP, zero-dep viewer).
- Viewer default bind stays `127.0.0.1`; do not touch bind logic.
- Never mutate the observed page's DOM. Status polling uses read-only `Runtime.evaluate`.
- Browser-backed tests (`tests/it_viewer.rs`) cannot run in this container (Chrome exits SIGTRAP). Verify them with `cargo test --test it_viewer --no-run` (compile only) and run unit tests with `cargo test --lib`.
- Every change passes `cargo fmt --check` and `cargo clippy --all-targets --all-features -- -D warnings` before commit.
- Commit messages: conventional (`feat:`/`fix:`/`docs:`). NEVER add a `Co-Authored-By: Claude` trailer.
- Rust doc comments in this repo often carry a `[Decision Log]` block (목적과 의도 / 대안 / 주의). Preserve existing blocks; extend them when changing the file's behavior.

---

### Task 1: Frame write tuning — TCP_NODELAY + real `--fps` throttle

**Files:**
- Modify: `src/viewer/http.rs` (ViewerOptions, serve accept loop, write_stream, new `fps_to_interval`, new tests module)
- Modify: `src/cli/mod.rs:235-237` (`--fps` doc comment), tests module (parse test)
- Modify: `src/cli/commands.rs:268-272` (pass `min_frame_interval`)

**Interfaces:**
- Produces: `pub fn fps_to_interval(fps: u32) -> Duration` in `src/viewer/http.rs`; `ViewerOptions.min_frame_interval: Duration` field. Task 2/3 do not depend on these, but Task 2 edits the same files — run this task first to avoid conflicts.

- [ ] **Step 1: Write failing unit tests**

Append to the bottom of `src/viewer/http.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// `--fps N` must mean "at most N frames per second on the wire".
    /// Clamped to 1..=60: 0 would divide by zero, and Chrome never
    /// screencasts above ~60.
    #[test]
    fn fps_to_interval_maps_and_clamps() {
        assert_eq!(fps_to_interval(30), Duration::from_micros(33_333));
        assert_eq!(fps_to_interval(15), Duration::from_micros(66_666));
        // Below range clamps to 1 fps, above range clamps to 60.
        assert_eq!(fps_to_interval(0), Duration::from_micros(1_000_000));
        assert_eq!(fps_to_interval(1000), Duration::from_micros(16_666));
    }

    /// The default option set must match the CLI default of 30 fps, or the
    /// two drift silently.
    #[test]
    fn default_min_frame_interval_is_30fps() {
        assert_eq!(
            ViewerOptions::default().min_frame_interval,
            fps_to_interval(30)
        );
    }
}
```

Add to the `tests` module in `src/cli/mod.rs` (it already exists with a `parse_from` helper):

```rust
    /// `--fps` is wired to the viewer throttle; default 30, overridable.
    #[test]
    fn view_fps_defaults_and_overrides() {
        let cli = parse_from(&["headless-use", "view"]);
        let Commands::View(args) = cli.command else {
            panic!("expected view");
        };
        assert_eq!(args.fps, 30);

        let cli = parse_from(&["headless-use", "view", "--fps", "15"]);
        let Commands::View(args) = cli.command else {
            panic!("expected view");
        };
        assert_eq!(args.fps, 15);
    }
```

(Check how the existing tests in that module destructure `cli.command` and copy that exact pattern — if they use `match` instead of `let-else`, follow suit.)

- [ ] **Step 2: Run tests, verify failure**

Run: `cargo test --lib viewer::http::tests 2>&1 | tail -20`
Expected: compile error — `fps_to_interval` not found, no field `min_frame_interval`.

- [ ] **Step 3: Implement**

In `src/viewer/http.rs`:

1. Add to `ViewerOptions`:

```rust
    /// Minimum interval between frame writes per connection. Frames arriving
    /// faster are skipped; the watch channel keeps only the latest frame, so
    /// a skipped send is replaced by a newer frame, never queued.
    pub min_frame_interval: Duration,
```

and in `Default::default()`:

```rust
            min_frame_interval: fps_to_interval(30),
```

2. Add the free function (near the top, after `ViewerOptions`):

```rust
/// Convert a `--fps` cap to a per-connection minimum write interval.
/// Clamped to 1..=60: 0 would divide by zero, and Chrome's screencast
/// never exceeds ~60 fps anyway.
pub fn fps_to_interval(fps: u32) -> Duration {
    let fps = u64::from(fps.clamp(1, 60));
    Duration::from_micros(1_000_000 / fps)
}
```

3. In the accept loop in `serve()`, right after a successful accept:

```rust
                    // Frames are latency-sensitive; never let Nagle hold them.
                    let _ = stream.set_nodelay(true);
```

4. Thread the interval into `write_stream`. Change `handle_connection` to take and pass `min_frame_interval: Duration` (read it from `ViewerOptions` in `serve()` next to `first_frame_timeout` and move both into the spawn). Change `write_stream`'s signature:

```rust
async fn write_stream(
    stream: &mut tokio::net::TcpStream,
    screencast: &Arc<Screencast>,
    first_frame_timeout: Duration,
    min_frame_interval: Duration,
) -> std::io::Result<()> {
```

In its body, after the two initial sends and before the main loop:

```rust
    let mut last_sent = tokio::time::Instant::now();
```

and rework the `Ok(Ok(()))` arm of the main loop:

```rust
            Ok(Ok(())) => {
                // Throttle: enforce --fps as a per-connection cap. A skipped
                // frame is not lost — the next send picks up the newest frame
                // from the watch channel, and the 1s heartbeat re-sends the
                // latest frame if the stream goes quiet right after a skip.
                if last_sent.elapsed() < min_frame_interval {
                    continue;
                }
                let frame = rx.borrow().clone();
                if let Some(frame) = frame {
                    if write_frame(stream, &frame).await.is_err() {
                        break; // client disconnected
                    }
                    last_sent = tokio::time::Instant::now();
                }
            }
```

The heartbeat arm (`Err(_)`) also sets `last_sent = tokio::time::Instant::now();` after a successful `write_frame`.

5. In `src/cli/commands.rs`, replace the `viewer_opts` construction:

```rust
    let viewer_opts = crate::viewer::ViewerOptions {
        host: args.viewer_host.clone(),
        port: args.viewer_port,
        min_frame_interval: crate::viewer::http::fps_to_interval(args.fps),
        ..Default::default()
    };
```

6. In `src/cli/mod.rs`, fix the lying doc comment on `fps`:

```rust
    /// Max frames per second written to each viewer connection (1..60).
    /// Frames above the cap are skipped, never queued.
    #[arg(long, default_value_t = 30)]
    pub fps: u32,
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test --lib 2>&1 | tail -5`
Expected: all unit tests pass, including the three new ones.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
git add src/viewer/http.rs src/cli/mod.rs src/cli/commands.rs
git commit -m "feat(viewer): TCP_NODELAY on stream sockets and wire --fps as a real write throttle"
```

---

### Task 2: `/status` endpoint + page-info poll task

**Files:**
- Modify: `src/viewer/http.rs` (StatusInfo, status_json, serve signature, route, tests)
- Modify: `src/cli/commands.rs` (status arc + poll task in `view`)
- Modify: `tests/it_viewer.rs` (serve() call site compiles; `/status` assertions)

**Interfaces:**
- Consumes: `fps_to_interval` / `min_frame_interval` from Task 1 (same files, already merged).
- Produces: `pub struct StatusInfo { pub url: String, pub title: String, pub viewport_width: u32, pub viewport_height: u32, pub quality: u32 }` (derives `Debug, Clone, Default`), `pub fn status_json(s: &StatusInfo) -> String`, and the new `serve` signature `serve(screencast: Arc<Screencast>, status: Arc<tokio::sync::RwLock<StatusInfo>>, opts: ViewerOptions)`. Task 3's page JS fetches `/status` and reads `{url, title, viewport:{width,height}, quality}`.

- [ ] **Step 1: Write failing unit tests**

Append inside the `tests` module in `src/viewer/http.rs`:

```rust
    /// /status carries whatever URL the page is on — including query strings
    /// with quotes or unicode — so serialization must be real JSON, not
    /// string concatenation.
    #[test]
    fn status_json_shape_and_escaping() {
        let s = StatusInfo {
            url: "https://ex.com/?q=\"a\"".to_string(),
            title: "타이틀 — test".to_string(),
            viewport_width: 1280,
            viewport_height: 720,
            quality: 80,
        };
        let v: serde_json::Value = serde_json::from_str(&status_json(&s)).unwrap();
        assert_eq!(v["url"], "https://ex.com/?q=\"a\"");
        assert_eq!(v["title"], "타이틀 — test");
        assert_eq!(v["viewport"]["width"], 1280);
        assert_eq!(v["viewport"]["height"], 720);
        assert_eq!(v["quality"], 80);
    }
```

- [ ] **Step 2: Run tests, verify failure**

Run: `cargo test --lib viewer::http::tests 2>&1 | tail -10`
Expected: compile error — `StatusInfo` / `status_json` not found.

- [ ] **Step 3: Implement server side**

In `src/viewer/http.rs`:

1. Below `ViewerOptions`:

```rust
/// Live page info served at `/status`, updated by the CLI's poll task.
/// Read-only from the page's perspective: the poller only evaluates
/// `location.href` and `document.title`, never mutating the DOM.
#[derive(Debug, Clone, Default)]
pub struct StatusInfo {
    /// Current page URL (empty until the first poll lands).
    pub url: String,
    /// Current page title.
    pub title: String,
    /// Screencast max width (the stream's pixel budget, not the live frame).
    pub viewport_width: u32,
    /// Screencast max height.
    pub viewport_height: u32,
    /// JPEG quality (1..100) the screencast was started with.
    pub quality: u32,
}

/// Serialize a status snapshot. serde_json handles escaping; the URL and
/// title are page-controlled strings and must never be concatenated raw.
pub fn status_json(s: &StatusInfo) -> String {
    serde_json::json!({
        "url": s.url,
        "title": s.title,
        "viewport": { "width": s.viewport_width, "height": s.viewport_height },
        "quality": s.quality,
    })
    .to_string()
}
```

2. Change `serve` to accept the shared status and thread it to connections:

```rust
pub async fn serve(
    screencast: Arc<Screencast>,
    status: Arc<tokio::sync::RwLock<StatusInfo>>,
    opts: ViewerOptions,
) -> Result<ViewerHandle, std::io::Error> {
```

Inside the accept arm, clone it alongside the screencast (`let st = status.clone();`) and pass to `handle_connection`.

3. `handle_connection` gains `status: &Arc<tokio::sync::RwLock<StatusInfo>>` and routes:

```rust
        "/status" => {
            let body = status_json(&*status.read().await);
            write_json(stream, &body).await
        }
```

4. Add `write_json` next to `write_text`:

```rust
async fn write_json(stream: &mut tokio::net::TcpStream, body: &str) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.0 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await
}
```

5. Extend the file's `[Decision Log]` header comment: `/status` added, fed by a CLI-side poll, serves last-known snapshot (a poll miss keeps the previous value rather than erroring).

- [ ] **Step 4: Implement CLI side**

In `src/cli/commands.rs` `view()`, after the screencast starts and before `viewer_opts`:

```rust
    // Shared page-info snapshot for the viewer's /status endpoint. A CDP poll
    // task refreshes url/title every 2s; on a poll miss the last-known value
    // stays (transient CDP errors must not flicker the status panel).
    let status = std::sync::Arc::new(tokio::sync::RwLock::new(
        crate::viewer::http::StatusInfo {
            url: String::new(),
            title: String::new(),
            viewport_width: max_w,
            viewport_height: max_h,
            quality: args.quality.clamp(1, 100),
        },
    ));
    let poll_client = session.page().cdp().clone();
    let poll_sid = session.page().session_id().to_string();
    let poll_status = status.clone();
    let poll = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let r = poll_client
                .call_session::<crate::cdp::EvaluateResult>(
                    "Runtime.evaluate",
                    Some(serde_json::json!({
                        "expression":
                            "JSON.stringify({u: location.href, t: document.title})",
                        "returnByValue": true,
                    })),
                    &poll_sid,
                    std::time::Duration::from_secs(5),
                )
                .await;
            let Ok(res) = r else { continue };
            let Some(s) = res.value().and_then(|v| v.as_str().map(String::from)) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else {
                continue;
            };
            let mut st = poll_status.write().await;
            if let Some(u) = v.get("u").and_then(|x| x.as_str()) {
                st.url = u.to_string();
            }
            if let Some(t) = v.get("t").and_then(|x| x.as_str()) {
                st.title = t.to_string();
            }
        }
    });
```

Pass it to the server (`serve(screencast.clone(), status.clone(), viewer_opts)`), and after `run_stdio` returns, before `screencast.stop()`:

```rust
    poll.abort();
```

(If `EvaluateResult`'s accessor is not `.value()`, check `src/browser/page.rs:419-427` — `Page::url` uses the exact accessor; copy it.)

- [ ] **Step 5: Fix `tests/it_viewer.rs` call site + add `/status` assertions**

In `viewer_http_serves_index_and_stream`:

1. Update the serve call:

```rust
    let status = Arc::new(tokio::sync::RwLock::new(
        headless_use::viewer::http::StatusInfo {
            url: "file:///fixture".to_string(),
            title: "basic form".to_string(),
            viewport_width: 800,
            viewport_height: 600,
            quality: 70,
        },
    ));
    let _handle = headless_use::viewer::http::serve(cast.clone(), status, opts)
        .await
        .unwrap();
```

2. DELETE the `!body.contains("/status")` assertion (its premise — the endpoint does not exist — is gone). Task 3 replaces it with positive markers.

3. Add after the index checks:

```rust
    // /status serves the shared snapshot as JSON.
    let resp = reqwest::get(format!("http://127.0.0.1:{port}/status"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    assert!(ct.contains("application/json"), "got: {ct}");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["url"], "file:///fixture");
    assert_eq!(v["title"], "basic form");
    assert_eq!(v["viewport"]["width"], 800);
    assert_eq!(v["quality"], 70);

    // Adding /status must not have loosened routing: unknown paths still 404.
    let resp = reqwest::get(format!("http://127.0.0.1:{port}/nope"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
```

(If `serde_json` is not already a dev-dependency import in this test, it is available via `reqwest::Response::json` + `serde_json::Value`; check the `[dev-dependencies]` in `Cargo.toml` — serde_json is a main dependency, so `use serde_json;` works in integration tests only if listed; if the compiler objects, assert on `resp.text()` containing `"\"quality\":70"` instead.)

- [ ] **Step 6: Verify**

Run: `cargo test --lib 2>&1 | tail -5`
Expected: PASS including `status_json_shape_and_escaping`.

Run: `cargo test --test it_viewer --no-run 2>&1 | tail -3`
Expected: compiles (cannot execute in container — SIGTRAP).

- [ ] **Step 7: fmt + clippy + commit**

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
git add src/viewer/http.rs src/cli/commands.rs tests/it_viewer.rs
git commit -m "feat(viewer): /status endpoint with 2s read-only page-info poll"
```

---

### Task 3: Viewer page redesign — status panel, light mode, theme

**Files:**
- Modify: `src/viewer/http.rs` (`INDEX_HTML` only)
- Modify: `tests/it_viewer.rs` (index markup assertions)

**Interfaces:**
- Consumes: `/status` JSON `{url, title, viewport:{width,height}, quality}` from Task 2.
- Produces: nothing downstream.

- [ ] **Step 1: Update integration assertions first (the executable spec)**

In `tests/it_viewer.rs` `viewer_http_serves_index_and_stream`, replace the index-body assertion block (keep the `live viewer` title, `<img id="v" src="/stream"`, and `object-fit: contain` checks) and add:

```rust
    // Status panel: the page must poll the endpoint the server serves and
    // have slots for title/url/quality.
    assert!(body.contains("fetch('/status')"), "page must poll /status");
    assert!(body.contains(r#"id="title""#), "status panel: title slot");
    assert!(body.contains(r#"id="url""#), "status panel: url slot");
    assert!(body.contains(r#"id="quality""#), "status panel: quality slot");
    // Theme: variables + light mode.
    assert!(
        body.contains("prefers-color-scheme: light"),
        "page must support light mode"
    );
    assert!(body.contains("--bg:"), "palette must use CSS variables");
    // The panel must render page-controlled strings as text, never HTML.
    assert!(
        !body.contains("innerHTML"),
        "page-controlled strings must go through textContent"
    );
```

- [ ] **Step 2: Verify the assertions fail against the current page**

Run: `cargo test --test it_viewer --no-run 2>&1 | tail -3`
Expected: compiles. (Runtime failure is unverifiable in-container; the compile gate plus Step 4's string greps stand in.)

Run: `grep -c "fetch('/status')" src/viewer/http.rs`
Expected: `0` (not yet implemented).

- [ ] **Step 3: Replace `INDEX_HTML`**

Replace the whole `INDEX_HTML` constant (and update its preceding doc comment to mention the status panel, the `/status` poll, and the light/dark palette). New value:

```rust
const INDEX_HTML: &str = r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>headless-use — live viewer</title>
<style>
  :root {
    color-scheme: light dark;
    --bg: #0b0b0d;
    --glow: rgba(110, 125, 255, .08);
    --fg: #e8e8ea;
    --muted: #8a8a94;
    --sep: #4a4a52;
    --pill: rgba(255, 255, 255, .08);
    --ring: rgba(255, 255, 255, .07);
    --shadow: rgba(0, 0, 0, .55);
    --bar: rgba(11, 11, 13, .92);
  }
  @media (prefers-color-scheme: light) {
    :root {
      --bg: #f2f2f5;
      --glow: rgba(90, 110, 255, .10);
      --fg: #1c1c21;
      --muted: #6b6b76;
      --sep: #c2c2cc;
      --pill: rgba(0, 0, 0, .06);
      --ring: rgba(0, 0, 0, .08);
      --shadow: rgba(25, 25, 50, .28);
      --bar: rgba(242, 242, 245, .92);
    }
  }
  * { box-sizing: border-box; }
  html, body {
    margin: 0; height: 100%;
    background: var(--bg); color: var(--fg);
    font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
    overflow: hidden;
  }
  .stage {
    position: absolute; inset: 0;
    display: flex; align-items: center; justify-content: center;
    padding: 16px;
    background: radial-gradient(1100px 650px at 50% 28%, var(--glow), transparent 70%);
  }
  .frame {
    position: relative;
    max-width: 100%; max-height: 100%;
    border-radius: 10px;
    box-shadow: 0 0 0 1px var(--ring), 0 24px 60px var(--shadow);
    overflow: hidden;
    background: #000;
    line-height: 0;
  }
  img {
    display: block;
    max-width: 100%; max-height: calc(100vh - 32px);
    width: auto; height: auto;
    object-fit: contain;
  }
  .bar {
    position: fixed; top: 0; left: 0; right: 0;
    display: flex; align-items: center; gap: 10px;
    padding: 12px 18px;
    font-size: 13px;
    background: linear-gradient(180deg, var(--bar), transparent);
    transition: opacity .35s ease;
    pointer-events: none;
    z-index: 2;
    min-width: 0;
  }
  body.idle .bar { opacity: 0; }
  .dot {
    width: 7px; height: 7px; border-radius: 50%;
    background: #35c759; box-shadow: 0 0 8px rgba(53, 199, 89, .7);
    animation: pulse 2s ease-in-out infinite;
    flex: none;
  }
  body.stalled .dot { background: #f5a524; box-shadow: 0 0 8px rgba(245, 165, 36, .7); }
  @keyframes pulse { 50% { opacity: .35; } }
  .title { font-weight: 600; letter-spacing: -.01em; white-space: nowrap; }
  .title:empty::before { content: "headless-use"; }
  .url {
    background: var(--pill);
    border-radius: 999px;
    padding: 3px 12px;
    color: var(--muted);
    max-width: 40vw;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .url:empty { display: none; }
  .sep { color: var(--sep); }
  .meta { color: var(--muted); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .spacer { margin-left: auto; }
</style></head>
<body>
<div class="bar">
  <span class="dot"></span>
  <span class="title" id="title"></span>
  <span class="url" id="url"></span>
  <span class="spacer"></span>
  <span class="meta" id="dims"></span>
  <span class="sep">·</span>
  <span class="meta" id="state">connecting…</span>
  <span class="sep">·</span>
  <span class="meta" id="quality"></span>
</div>
<div class="stage">
  <div class="frame"><img id="v" src="/stream" alt="live page stream"></div>
</div>
<script>
(function () {
  var img = document.getElementById('v');
  var state = document.getElementById('state');
  var dims = document.getElementById('dims');
  var title = document.getElementById('title');
  var url = document.getElementById('url');
  var quality = document.getElementById('quality');
  var body = document.body;

  // The MJPEG stream is a single never-ending response, so `load` fires once
  // per frame. Counting those gives an honest frame rate without any server
  // support.
  var frames = 0, lastSeen = 0;
  img.addEventListener('load', function () {
    frames++;
    lastSeen = Date.now();
    if (img.naturalWidth) {
      dims.textContent = img.naturalWidth + '×' + img.naturalHeight;
    }
  });
  img.addEventListener('error', function () {
    state.textContent = 'stream ended';
    body.classList.add('stalled');
  });

  setInterval(function () {
    var fps = frames; frames = 0;
    if (!lastSeen) { return; }
    // Chrome only emits screencast frames on repaint, so a static page is
    // idle, not broken. Say so rather than showing 0 fps as an error.
    var quiet = Date.now() - lastSeen > 2500;
    body.classList.toggle('stalled', quiet);
    state.textContent = quiet ? 'idle' : fps + ' fps';
  }, 1000);

  // Status panel: poll the page url/title the server snapshots every 2s.
  // url/title are page-controlled strings — textContent only, never HTML.
  function poll() {
    fetch('/status').then(function (r) { return r.json(); }).then(function (s) {
      if (s.title) { title.textContent = s.title; }
      if (s.url) { url.textContent = s.url; }
      if (s.quality) { quality.textContent = 'q' + s.quality; }
    }).catch(function () { /* keep last shown values */ });
  }
  poll();
  setInterval(poll, 2000);

  // Fade the header out while nothing is happening at the viewer end.
  var idleTimer;
  function wake() {
    body.classList.remove('idle');
    clearTimeout(idleTimer);
    idleTimer = setTimeout(function () { body.classList.add('idle'); }, 3000);
  }
  ['mousemove', 'keydown', 'touchstart'].forEach(function (e) {
    window.addEventListener(e, wake, { passive: true });
  });
  wake();
})();
</script>
</body></html>"#;
```

- [ ] **Step 4: Verify markers + build**

```bash
grep -c "fetch('/status')" src/viewer/http.rs        # expect 1
grep -c "prefers-color-scheme: light" src/viewer/http.rs  # expect 1
grep -c "innerHTML" src/viewer/http.rs               # expect 0
cargo test --lib 2>&1 | tail -3                      # unit tests still pass
cargo test --test it_viewer --no-run 2>&1 | tail -3  # integration compiles
```

- [ ] **Step 5: Visual smoke check (best-effort)**

The container cannot run Chrome, but the HTML itself can be eyeballed: extract it to the scratchpad and open-check structure.

```bash
awk '/const INDEX_HTML/,/"#;/' src/viewer/http.rs | sed '1s/.*r#"//; $s/"#;.*//' > /tmp/claude-1000/-root-computer-use/5e5f67b5-4a40-4c55-8507-588b08f50993/scratchpad/index.html
```

Read the extracted file once to confirm no truncated tags. (If a browser IS available at execution time: `headless-use view` + open the URL, check dark and light with the OS theme toggle.)

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
git add src/viewer/http.rs tests/it_viewer.rs
git commit -m "feat(viewer): status panel, light mode, and refined theme for the live viewer page"
```

---

### Task 4: Docs + changelog

**Files:**
- Modify: `README.md` (Live viewer section, ~line 232), `README.ko.md` (same section)
- Modify: `CHANGELOG.md` (Unreleased)
- Modify: `docs/superpowers/specs/2026-07-26-viewer-streaming-quality-design.md` (Status: implemented)

**Interfaces:** none.

- [ ] **Step 1: Update docs**

In `README.md`'s Live viewer section add one paragraph (mirror it in Korean in `README.ko.md` — read the existing section first and match its tone):

```markdown
The viewer page shows a status panel (page title, URL, resolution, fps, JPEG
quality) fed by a read-only `/status` endpoint, and follows your OS light/dark
theme. `--fps N` caps the stream's frame rate per connection (1–60, default
30); frames above the cap are skipped, never queued.
```

In `CHANGELOG.md` under Unreleased (match the existing entry style — read the top of the file first):

```markdown
- feat(viewer): live viewer status panel (`/status` endpoint), light mode, per-connection `--fps` throttle, TCP_NODELAY on stream sockets
```

Flip the spec's `Status: approved` line to `Status: implemented`.

- [ ] **Step 2: Commit**

```bash
git add README.md README.ko.md CHANGELOG.md docs/superpowers/specs/2026-07-26-viewer-streaming-quality-design.md
git commit -m "docs(viewer): document status panel, light mode, and --fps throttle"
```

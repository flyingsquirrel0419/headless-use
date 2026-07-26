//! Localhost-only MJPEG HTTP server for the live viewer.
//!
//! Serves a tiny HTML index at `/` and an MJPEG stream at `/stream` that pulls
//! the latest screencast frame from a shared [`super::screencast::Screencast`].
//!
//! [Decision Log]
//! - 목적과 의도: Let a human/test watch the agent-controlled page live in a
//!   browser tab at http://127.0.0.1:PORT/, with the cursor overlay visible.
//! - 기존 구현 및 제약 조건: No HTTP server existed; screenshots were one-shot.
//! - 검토한 주요 대안: Pull in axum/hyper — adds dependency weight and binary
//!   size for a single-endpoint localhost MJPEG stream.
//! - 선택한 방식: tokio TcpListener + hand-written HTTP/1.0 multipart
//!   x-mixed-replace response. MJPEG is a trivial text protocol here.
//! - 장점: Zero new dependencies, minimal binary growth. 단점: Hand-written
//!   HTTP is minimal (no keep-alive); fine for a single local viewer.
//! - 수정 시 주의: The default bind is 127.0.0.1 and should stay that way.
//!   `--viewer-host` can widen it (added for remote viewing over a trusted
//!   network), and that is a deliberate exposure decision by the operator:
//!   **the stream carries whatever the agent-controlled page is showing —
//!   including logged-in session content.** Access is gated by a bearer token
//!   in the URL (see [`ViewerOptions::require_token`]); [`serve`] still logs a
//!   warning whenever the bind address is not loopback, because a token does
//!   not make cleartext HTTP on a shared network a good idea.
//!   This is separate from the CDP endpoint, which is always 127.0.0.1-only.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::screencast::{Frame, Screencast};

/// Viewer configuration.
#[derive(Debug, Clone)]
pub struct ViewerOptions {
    /// Bind address. Defaults to 127.0.0.1 (loopback only). Set to 0.0.0.0 to
    /// expose the viewer on all interfaces for remote viewing. Exposing it
    /// lets anyone on the network watch and observe the agent-controlled page.
    pub host: String,
    /// Bind port.
    pub port: u16,
    /// Max time to wait for the first frame on a stream connection.
    pub first_frame_timeout: Duration,
    /// Bearer token accepted as the `token` query parameter on every viewer
    /// endpoint. `None` means "generate a random one at startup"; pin it with
    /// `--viewer-token` when the URL has to be stable (a bookmark, a tunnel
    /// config, a script).
    ///
    /// A query parameter rather than a header because the primary UX is a
    /// human pasting the printed URL into a browser, and a browser cannot be
    /// told to attach an `Authorization` header to a top-level navigation or
    /// to an `<img>` load.
    pub token: Option<String>,
    /// Whether a valid token is *required*.
    ///
    /// `None` (the default) derives it from the bind address, which is the
    /// only split that is both safe and not annoying:
    ///
    /// - **Non-loopback bind** (`--viewer-host 0.0.0.0`, a LAN address, …):
    ///   **required**. Every request without a matching `?token=` is answered
    ///   `401`. This is the case that is actually reachable by other machines,
    ///   so leaving it open is a real hole rather than a theoretical one.
    /// - **Loopback bind** (`127.0.0.1`, `::1`, `127.0.0.2`, …): **optional**.
    ///   A token is still generated, still printed, and still accepted, but a
    ///   request that omits it is served normally. Anything that can reach a
    ///   loopback socket is already running as (or under) this user and can
    ///   read the token out of the process' own argv or output anyway, so
    ///   rejecting would break the common `headless-use view` + click-the-link
    ///   flow for no gain.
    ///
    /// `Some(true)` / `Some(false)` pin the behaviour regardless of address.
    /// Tests use `Some(true)` to exercise the rejection path without having to
    /// bind a routable interface.
    pub require_token: Option<bool>,
}

impl Default for ViewerOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 7780,
            first_frame_timeout: Duration::from_secs(10),
            token: None,
            require_token: None,
        }
    }
}

/// A fresh 128-bit token, hex-encoded.
///
/// Local helper on purpose: `browser::launch::short_uuid` is the same idea for
/// temp directory names, but that one is 64 bits — fine for uniqueness, not for
/// something an attacker may guess. This is not a CSPRNG (`fastrand` is a PRNG),
/// which is worth knowing about: the token raises the bar on a LAN-exposed
/// viewer, it is not a secret worth defending against an offline attacker.
fn generate_token() -> String {
    let bytes: [u8; 16] = std::array::from_fn(|_| fastrand::u8(0..=255));
    hex::encode(bytes)
}

/// Compare two byte strings without an early return on the first difference.
///
/// Lengths are compared up front — that does leak the token length, which is
/// public information (it is printed in the URL) and not worth the complexity
/// of a length-blinded compare.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Extract `key` from the query string of a request target such as
/// `/stream?token=abc&x=1`. Returns the raw (still percent-encoded) value.
fn query_param<'a>(target: &'a str, key: &str) -> Option<&'a str> {
    let (_, query) = target.split_once('?')?;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

/// Maximum number of connections served at once.
///
/// Each connection is one MJPEG stream that holds a clone of the latest JPEG
/// frame while writing it, so connections cost memory and a file descriptor
/// each and never finish on their own — an MJPEG response is unbounded by
/// design. The viewer's job is a handful of browser tabs watching one page;
/// 32 leaves generous room for reload churn (a refreshed tab's old connection
/// lingers until the write fails) while putting a hard ceiling on what a
/// non-loopback bind (`--viewer-host 0.0.0.0`) can consume. The cap is checked
/// before the token is, so unauthorized traffic cannot exhaust the pool either.
/// Beyond it we answer 503 and close instead of spawning without limit.
const MAX_CONNECTIONS: usize = 32;

/// Handle to a running viewer server. Drop to stop.
///
/// Dropping stops the accept loop *and* tears down connections that are already
/// streaming: every connection task selects on the same shutdown signal, so a
/// dropped handle ends the live `/stream` responses instead of leaving their
/// heartbeat loops running until each client happens to disconnect.
pub struct ViewerHandle {
    /// The bound localhost address.
    pub addr: SocketAddr,
    /// The token this server accepts. Always present, even when
    /// [`ViewerHandle::token_required`] is false, so the printed URL is
    /// identical in both modes and keeps working if the bind is widened later.
    pub token: String,
    /// Whether requests without a matching token are rejected with `401`.
    pub token_required: bool,
    /// Broadcasts the shutdown flag to the accept loop and every live
    /// connection. Set to `true` on drop; dropping the sender is itself a
    /// shutdown signal, so a leaked handle still cannot outlive the process.
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl Drop for ViewerHandle {
    fn drop(&mut self) {
        // Best-effort: signal the accept loop and all live connections to exit.
        // If every receiver is already gone this is a no-op.
        let _ = self.shutdown.send(true);
    }
}

/// Resolve once the shutdown flag is set or the sender is dropped.
async fn shutdown_signalled(rx: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            return; // handle dropped without sending: also a shutdown
        }
    }
}

impl ViewerHandle {
    /// The viewer index URL, token included.
    ///
    /// The token is always in the URL, not only when it is enforced: this is
    /// the string an operator copies, and it must keep working if they later
    /// re-run with `--viewer-host 0.0.0.0`.
    pub fn url(&self) -> String {
        format!("http://{}/?token={}", self.addr, self.token)
    }
    /// The MJPEG stream URL, token included.
    pub fn stream_url(&self) -> String {
        format!("http://{}/stream?token={}", self.addr, self.token)
    }
}

/// Start the viewer HTTP server bound to 127.0.0.1.
///
/// `screencast` is shared (Arc) across connections so multiple tabs read the
/// same latest frame.
pub async fn serve(
    screencast: Arc<Screencast>,
    opts: ViewerOptions,
) -> Result<ViewerHandle, std::io::Error> {
    // A bad --viewer-host must surface as a clean error, not a panic.
    let addr: SocketAddr = format!("{}:{}", opts.host, opts.port)
        .parse()
        .map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "invalid viewer bind address '{}:{}': {e}",
                    opts.host, opts.port
                ),
            )
        })?;
    let listener = TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;

    let token = opts.token.clone().unwrap_or_else(generate_token);
    // See `ViewerOptions::require_token`: enforced off loopback, accepted but
    // not demanded on loopback, and pinnable either way.
    let token_required = opts.require_token.unwrap_or(!bound_addr.ip().is_loopback());

    if !bound_addr.ip().is_loopback() {
        tracing::warn!(
            addr = %bound_addr,
            "viewer is bound to a non-loopback address; access requires the viewer token, but the \
             stream itself is unencrypted HTTP and anyone holding the URL can watch the \
             agent-controlled browser"
        );
        eprintln!(
            "warning: viewer bound to {bound_addr} (not loopback). Access requires the token in \
             the URL, but the stream is plain HTTP — anyone who obtains that URL, or who can \
             observe the traffic, can watch the agent-controlled page."
        );
    }

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    let first_frame_timeout = opts.first_frame_timeout;
    let auth = Arc::new(Auth {
        token: token.clone(),
        required: token_required,
    });
    // Permits are handed to connection tasks and released when they end, so
    // this counts *live* connections rather than total accepts.
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = shutdown_signalled(&mut shutdown_rx) => break,
                accept = listener.accept() => {
                    let (mut stream, peer) = match accept {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let permit = match Arc::clone(&slots).try_acquire_owned() {
                        Ok(p) => p,
                        Err(_) => {
                            tracing::warn!(
                                %peer,
                                max = MAX_CONNECTIONS,
                                "viewer connection limit reached; rejecting"
                            );
                            // Answer inline rather than spawning: spawning per
                            // rejection is the very unboundedness we are
                            // capping. The write is one small buffer and is
                            // time-bounded, so a stuck peer cannot wedge the
                            // accept loop; at worst it slows accepts while we
                            // are already at capacity, which is the backpressure
                            // we want.
                            let _ = tokio::time::timeout(
                                REJECT_WRITE_TIMEOUT,
                                write_text(&mut stream, 503, "too many connections"),
                            )
                            .await;
                            continue;
                        }
                    };
                    let sc = screencast.clone();
                    let to = first_frame_timeout;
                    let auth = auth.clone();
                    let mut conn_shutdown = shutdown_rx.clone();
                    tokio::spawn(async move {
                        // Shutdown cancels the connection future wherever it is
                        // parked — including the /stream heartbeat loop, which
                        // would otherwise keep re-sending frames until the
                        // client went away on its own.
                        tokio::select! {
                            r = handle_connection(&mut stream, &sc, to, &auth) => { let _ = r; }
                            _ = shutdown_signalled(&mut conn_shutdown) => {}
                        }
                        drop(permit);
                    });
                }
            }
        }
    });

    Ok(ViewerHandle {
        addr: bound_addr,
        token,
        token_required,
        shutdown: shutdown_tx,
    })
}

/// The token and whether it is enforced, shared by every connection task.
struct Auth {
    token: String,
    required: bool,
}

impl Auth {
    /// Whether a request target (`/stream?token=…`) may be served.
    fn permits(&self, target: &str) -> bool {
        if !self.required {
            return true;
        }
        query_param(target, "token")
            .is_some_and(|t| constant_time_eq(t.as_bytes(), self.token.as_bytes()))
    }
}

/// How long we are willing to spend telling a peer it was rejected.
const REJECT_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

async fn handle_connection(
    stream: &mut tokio::net::TcpStream,
    screencast: &Arc<Screencast>,
    first_frame_timeout: Duration,
    auth: &Auth,
) -> std::io::Result<()> {
    // Read until we have the full request line. A single `read` can return a
    // partial line (the request may be split across TCP segments), which would
    // route the connection to 404 for no reason.
    let Some(request_line) = read_request_line(stream).await else {
        return write_text(stream, 400, "bad request").await;
    };
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();
    let path = target.split('?').next().unwrap_or("/");

    // `/health` is deliberately outside the gate: it answers a fixed `ok` and
    // discloses nothing about the page, so a container/uptime probe should not
    // need the secret. Everything that can reveal page content is gated.
    if path != "/health" && !auth.permits(&target) {
        return write_text(stream, 401, "unauthorized: append ?token=<viewer token>").await;
    }

    match path {
        "/" | "/index.html" => write_index(stream, auth).await,
        "/stream" | "/mjpeg" => write_stream(stream, screencast, first_frame_timeout).await,
        "/health" => write_text(stream, 200, "ok").await,
        _ => write_text(stream, 404, "not found").await,
    }
}

/// Read bytes until the first CRLF/LF and return the request line.
///
/// Bounded by `MAX_REQUEST_LINE` so a client that never sends a newline cannot
/// make us buffer without limit. Returns `None` on EOF or an over-long line.
async fn read_request_line(stream: &mut tokio::net::TcpStream) -> Option<String> {
    const MAX_REQUEST_LINE: usize = 8 * 1024;
    let mut acc: Vec<u8> = Vec::with_capacity(256);
    let mut buf = [0u8; 512];
    loop {
        if let Some(pos) = acc.iter().position(|b| *b == b'\n') {
            let line = String::from_utf8_lossy(&acc[..pos]).trim_end().to_string();
            return Some(line);
        }
        if acc.len() > MAX_REQUEST_LINE {
            return None;
        }
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
        }
    }
}

async fn write_index(stream: &mut tokio::net::TcpStream, auth: &Auth) -> std::io::Result<()> {
    // The page loads the stream itself, so it has to carry the token forward.
    // Rewriting the `<img>` source here (rather than having the page read
    // `location.search`) keeps the token out of any JS string and means the
    // markup is honest about what it fetches.
    let body = INDEX_HTML.replace(
        r#"src="/stream""#,
        &format!(r#"src="/stream?token={}""#, auth.token),
    );
    let body = body.as_str();
    let resp = format!(
        "HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes()).await
}

async fn write_text(
    stream: &mut tokio::net::TcpStream,
    code: u16,
    msg: &str,
) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let resp = format!(
        "HTTP/1.0 {code} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{msg}",
        msg.len()
    );
    stream.write_all(resp.as_bytes()).await
}

async fn write_stream(
    stream: &mut tokio::net::TcpStream,
    screencast: &Arc<Screencast>,
    first_frame_timeout: Duration,
) -> std::io::Result<()> {
    let header = "HTTP/1.0 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=huframe\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
    stream.write_all(header.as_bytes()).await?;

    let mut rx = screencast.subscribe();

    // If a frame is already available, send it immediately.
    if let Some(frame) = screencast.latest() {
        let _ = write_frame(stream, &frame).await;
    }
    // Wait for the first new frame if none was available.
    let _ = tokio::time::timeout(first_frame_timeout, rx.changed()).await;
    {
        let frame = rx.borrow().clone();
        if let Some(frame) = frame {
            let _ = write_frame(stream, &frame).await;
        }
    }

    // Main loop: wait for new frames. On static pages Chrome stops emitting
    // frames once nothing repaints, so `rx.changed()` would block forever.
    // We poll with a 1s timeout and, when no new frame arrives, re-send the
    // latest cached frame as a heartbeat so the viewer tab keeps showing a
    // live image and detects disconnects promptly.
    let heartbeat = Duration::from_secs(1);
    loop {
        let changed = tokio::time::timeout(heartbeat, rx.changed()).await;
        match changed {
            Ok(Ok(())) => {
                let frame = rx.borrow().clone();
                if let Some(frame) = frame {
                    if write_frame(stream, &frame).await.is_err() {
                        break; // client disconnected
                    }
                }
            }
            Ok(Err(_)) => break, // sender dropped (screencast stopped)
            Err(_) => {
                // Heartbeat: no new frame in 1s (static page). Re-send the
                // latest frame so the stream stays alive for the viewer.
                if let Some(frame) = screencast.latest() {
                    if write_frame(stream, &frame).await.is_err() {
                        break; // client disconnected
                    }
                }
            }
        }
    }
    Ok(())
}

async fn write_frame(stream: &mut tokio::net::TcpStream, frame: &Frame) -> std::io::Result<()> {
    let part = format!(
        "--huframe\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
        frame.data.len()
    );
    stream.write_all(part.as_bytes()).await?;
    stream.write_all(&frame.data).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await
}

/// The viewer index page.
///
/// ## Why the stream owns the whole window
/// The previous layout stacked a header, a full-width image and a paragraph of
/// explanation down the page, so a 1280x720 stream overflowed the fold on a
/// laptop and the thing you actually came to watch was never fully visible.
/// The stream is now the page: it fills the viewport and letterboxes via
/// `object-fit: contain`, so the aspect ratio is honored at any window size.
///
/// The header floats over the top edge and fades out after a few idle seconds,
/// because it is orientation, not content — useful when you arrive, noise once
/// you are watching. Moving the mouse brings it back.
///
/// No new endpoints: this is a static string, and everything it shows comes
/// from the page itself or from the image element's own load events.
const INDEX_HTML: &str = r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>headless-use — live viewer</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  html, body {
    margin: 0; height: 100%;
    background: #0b0b0d; color: #e8e8ea;
    font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
    overflow: hidden;
  }
  .stage {
    position: absolute; inset: 0;
    display: flex; align-items: center; justify-content: center;
    padding: 16px;
  }
  .frame {
    position: relative;
    max-width: 100%; max-height: 100%;
    border-radius: 10px;
    box-shadow: 0 0 0 1px rgba(255,255,255,.07), 0 24px 60px rgba(0,0,0,.55);
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
    background: linear-gradient(180deg, rgba(11,11,13,.92), rgba(11,11,13,0));
    transition: opacity .35s ease;
    pointer-events: none;
    z-index: 2;
  }
  body.idle .bar { opacity: 0; }
  .dot {
    width: 7px; height: 7px; border-radius: 50%;
    background: #35c759; box-shadow: 0 0 8px rgba(53,199,89,.7);
    animation: pulse 2s ease-in-out infinite;
    flex: none;
  }
  body.stalled .dot { background: #f5a524; box-shadow: 0 0 8px rgba(245,165,36,.7); }
  @keyframes pulse { 50% { opacity: .35; } }
  .name { font-weight: 600; letter-spacing: -.01em; }
  .sep { color: #4a4a52; }
  .meta { color: #8a8a94; font-variant-numeric: tabular-nums; }
  .spacer { margin-left: auto; }
</style></head>
<body>
<div class="bar">
  <span class="dot"></span>
  <span class="name">headless-use</span>
  <span class="sep">/</span>
  <span class="meta" id="state">connecting…</span>
  <span class="spacer"></span>
  <span class="meta" id="dims"></span>
</div>
<div class="stage">
  <div class="frame"><img id="v" src="/stream" alt="live page stream"></div>
</div>
<script>
(function () {
  var img = document.getElementById('v');
  var state = document.getElementById('state');
  var dims = document.getElementById('dims');
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

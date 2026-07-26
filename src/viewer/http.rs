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
//!   including logged-in session content — and there is no authentication on
//!   it.** [`serve`] logs a warning whenever the bind address is not loopback.
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
}

impl Default for ViewerOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 7780,
            first_frame_timeout: Duration::from_secs(10),
        }
    }
}

/// Handle to a running viewer server. Drop to stop (best-effort).
pub struct ViewerHandle {
    /// The bound localhost address.
    pub addr: SocketAddr,
    /// Shut the server down cleanly. Consumed on drop.
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for ViewerHandle {
    fn drop(&mut self) {
        // Best-effort: signal the accept loop to exit. If the receiver is
        // already gone this is a no-op.
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

impl ViewerHandle {
    /// The localhost URL of the viewer index page.
    pub fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }
    /// The MJPEG stream URL.
    pub fn stream_url(&self) -> String {
        format!("http://{}/stream", self.addr)
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
    if !bound_addr.ip().is_loopback() {
        tracing::warn!(
            addr = %bound_addr,
            "viewer is bound to a non-loopback address; the page stream is unauthenticated and \
             anyone who can reach this address can watch the agent-controlled browser"
        );
        eprintln!(
            "warning: viewer bound to {bound_addr} (not loopback). The stream is unauthenticated — \
             anyone who can reach this address can watch the agent-controlled page."
        );
    }

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let first_frame_timeout = opts.first_frame_timeout;
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => break,
                accept = listener.accept() => {
                    let (mut stream, _peer) = match accept {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let sc = screencast.clone();
                    let to = first_frame_timeout;
                    tokio::spawn(async move {
                        let _ = handle_connection(&mut stream, &sc, to).await;
                    });
                }
            }
        }
    });

    Ok(ViewerHandle {
        addr: bound_addr,
        shutdown: Some(shutdown_tx),
    })
}

async fn handle_connection(
    stream: &mut tokio::net::TcpStream,
    screencast: &Arc<Screencast>,
    first_frame_timeout: Duration,
) -> std::io::Result<()> {
    // Read until we have the full request line. A single `read` can return a
    // partial line (the request may be split across TCP segments), which would
    // route the connection to 404 for no reason.
    let Some(request_line) = read_request_line(stream).await else {
        return write_text(stream, 400, "bad request").await;
    };
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();

    match path.as_str() {
        "/" | "/index.html" => write_index(stream).await,
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

async fn write_index(stream: &mut tokio::net::TcpStream) -> std::io::Result<()> {
    let body = INDEX_HTML;
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
        404 => "Not Found",
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

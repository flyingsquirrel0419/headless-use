//! CDP `Page.startScreencast` frame capture.
//!
//! Subscribes to `Page.screencastFrame` events, decodes the base64 JPEG, keeps
//! only the most recent frame (backpressure drops stale frames rather than
//! queuing them), and acks each frame as CDP requires.
//!
//! ## Why an idle re-arm timer
//! Chrome's `Page.startScreencast` only emits `Page.screencastFrame` when the
//! page repaints. Two failure modes make the live viewer freeze:
//!   1. Static pages never repaint after the initial load, so no new frames.
//!   2. Iframe-heavy sites (e.g. naver) fire many `Page.frameNavigated` events
//!      for subframes right after load; Chrome then appears to stop emitting
//!      screencastFrame entirely — even mouse moves (which DO repaint on
//!      simple sites) produce no frames.
//!
//! To keep the stream alive in both cases we re-issue `Page.startScreencast`
//! whenever no frame has arrived for `IDLE_REARM_MS`. Re-arming resends a
//! frame on the next repaint (mouse move, animation, or the re-arm's own
//! implicit first frame) without mutating the page DOM.
//!
//! [Decision Log]
//! - 목적과 의도: Provide a live frame stream for the localhost viewer without
//!   polling `Page.captureScreenshot` (which is heavy and not designed for 30fps).
//! - 기존 구현 및 제약 조건: No screencast support existed; screenshots were
//!   one-shot base64 PNG captures.
//! - 검토한 주요 대안: (a) Poll captureScreenshot at intervals — high CPU, jank.
//!   (b) Inject a JS repaint stimulus — mutates the page DOM, against the
//!   project rule of not modifying the original page.
//! - 선택한 방식: CDP Page.startScreencast + Page.screencastFrame events, with
//!   a watch channel (held in an Arc) carrying only the latest frame.
//!   frameNavigated triggers a restart; an idle re-arm timer (no frame for
//!   IDLE_REARM_MS) re-issues startScreencast so frozen streams recover.
//! - 장점: Real ~30fps, low overhead, survives navigation AND iframe storms,
//!   no page DOM mutation. 단점: Periodic startScreencast calls add a little
//!   CDP traffic (~one call per 2s when idle); acceptable for a viewer.
//! - 수정 시 주의: Every screencastFrame MUST be acked (Page.screencastFrameAck)
//!   or Chrome stops sending frames after ~2 buffered frames. The Arc<sender>
//!   is shared between the background task and the handle so `latest()` and
//!   `subscribe()` always see the frames the task sends. The idle re-arm must
//!   NOT fire while frames are actively arriving (reset the timer on each).

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::browser::{BrowserError, Page};
use crate::cdp::CdpClient;

/// Re-issue `Page.startScreencast` if no frame has arrived for this long.
/// 2s is short enough to feel live on static pages, long enough not to fight
/// an active stream.
const IDLE_REARM_MS: u64 = 2000;

/// A captured screencast frame (JPEG bytes + metadata).
#[derive(Debug, Clone)]
pub struct Frame {
    /// Decoded JPEG bytes.
    pub data: Vec<u8>,
    /// Frame metadata from CDP (sessionId, timestamp, etc.).
    pub metadata: Value,
}

/// Manages a CDP screencast session on a page.
///
/// The `watch::Sender` is held in an `Arc` shared between this handle and the
/// background decode/ack task, so `latest()` and `subscribe()` always observe
/// the frames the task publishes.
pub struct Screencast {
    /// Latest frame channel, shared with the background task via Arc.
    latest_tx: Arc<watch::Sender<Option<Frame>>>,
    /// CDP client clone for stop().
    client: CdpClient,
    /// Page session id for stop().
    session_id: String,
    /// Shutdown signal for the background task. Once `true`, the task exits
    /// and never re-issues `Page.startScreencast` again.
    shutdown_tx: watch::Sender<bool>,
    /// The background task handle, so stop()/Drop can make sure the task is
    /// actually gone (not merely signalled).
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Screencast {
    /// Start a screencast on `page`. Returns the screencast handle. The
    /// background task acks every frame, updates the latest-frame channel,
    /// restarts the cast on main-frame navigation, and re-arms the cast when
    /// idle so the stream never freezes.
    ///
    /// `max_fps` caps how often a decoded frame is published to viewers. Chrome
    /// has no frame-rate control (`everyNthFrame` throttles by repaint count,
    /// not by time), so the cap is applied here: every frame is still acked —
    /// skipping an ack stalls the whole cast — but frames arriving inside the
    /// interval are dropped instead of forwarded. `0` means no cap.
    ///
    /// The `--fps` CLI flag used to be parsed and then never read by anything;
    /// this is the behavior its help text always claimed.
    pub async fn start(
        page: &Page,
        quality: u32,
        max_width: u32,
        max_height: u32,
        max_fps: u32,
    ) -> Result<Self, BrowserError> {
        // Subscribe to events BEFORE starting the cast so we never miss the
        // first frame. The CDP client fans events out to every subscriber.
        let mut events = page.cdp().subscribe_events_async().await;

        // Start the cast.
        page.enable("Page.enable", None).await?;
        start_cast(page, quality, max_width, max_height).await?;

        let (latest_tx, _) = watch::channel::<Option<Frame>>(None);
        let latest_tx = Arc::new(latest_tx);

        // Background task: decode + ack frames, restart on navigation, re-arm
        // when idle. It holds an Arc clone of the SAME sender the handle
        // exposes, so latest()/subscribe() see every published frame.
        let page_client = page.cdp().clone();
        let session_id = page.session_id().to_string();
        let latest_tx_task = latest_tx.clone();
        let start_params = (quality, max_width, max_height);
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            // Track the last time we received a frame, so the idle re-arm timer
            // resets whenever frames are actively flowing.
            let mut last_frame = tokio::time::Instant::now();
            let idle = Duration::from_millis(IDLE_REARM_MS);
            let min_interval = if max_fps == 0 {
                Duration::ZERO
            } else {
                Duration::from_micros(1_000_000 / max_fps as u64)
            };
            let mut last_published: Option<tokio::time::Instant> = None;
            loop {
                let idle_timer = tokio::time::sleep_until(last_frame + idle);
                tokio::pin!(idle_timer);
                tokio::select! {
                    biased;
                    // Shutdown wins over everything: once stop()/Drop signals,
                    // exit without re-arming or restarting the cast.
                    _ = shutdown_rx.changed() => break,
                    ev = events.recv() => {
                        let Some(ev) = ev else { break; };
                        match ev.method.as_str() {
                            "Page.frameNavigated" => {
                                let is_main = ev
                                    .params
                                    .get("frame")
                                    .and_then(|f| f.get("parentId"))
                                    .is_none();
                                if is_main {
                                    // Chrome stops sending frames after a
                                    // main-frame swap; restart the cast after
                                    // a short render warm-up.
                                    tokio::time::sleep(Duration::from_millis(150)).await;
                                    // Shutdown may have been signalled during
                                    // the warm-up sleep: never restart after
                                    // stop().
                                    if *shutdown_rx.borrow() {
                                        break;
                                    }
                                    let _ = start_cast_via_client(
                                        &page_client,
                                        &session_id,
                                        start_params.0,
                                        start_params.1,
                                        start_params.2,
                                    )
                                    .await;
                                    last_frame = tokio::time::Instant::now();
                                }
                            }
                            "Page.screencastFrame" => {
                                let params = &ev.params;
                                let frame_session_id = params
                                    .get("sessionId")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                // Ack the frame (required) so Chrome keeps
                                // sending frames.
                                let _ = page_client
                                    .call_session::<Value>(
                                        "Page.screencastFrameAck",
                                        Some(json!({ "sessionId": frame_session_id })),
                                        &session_id,
                                        Duration::from_secs(5),
                                    )
                                    .await;
                                let now = tokio::time::Instant::now();
                                let due = last_published
                                    .is_none_or(|t| now.duration_since(t) >= min_interval);
                                if due {
                                    if let Some(data_b64) =
                                        params.get("data").and_then(|v| v.as_str())
                                    {
                                        let data = base64::engine::general_purpose::STANDARD
                                            .decode(data_b64)
                                            .unwrap_or_default();
                                        let frame = Frame {
                                            data,
                                            metadata: params.clone(),
                                        };
                                        let _ = latest_tx_task.send(Some(frame));
                                        last_published = Some(now);
                                    }
                                }
                                // Reset the idle timer: we just got a frame.
                                last_frame = tokio::time::Instant::now();
                            }
                            _ => {}
                        }
                    }
                    _ = &mut idle_timer => {
                        // No frame for IDLE_REARM_MS: the stream has gone idle
                        // (static page, or Chrome stopped emitting after an
                        // iframe storm). Re-issue startScreencast so Chrome
                        // sends a fresh frame on the next repaint. This does
                        // NOT mutate the page DOM.
                        let _ = start_cast_via_client(
                            &page_client,
                            &session_id,
                            start_params.0,
                            start_params.1,
                            start_params.2,
                        )
                        .await;
                        // Push the re-arm point forward so we don't spam while
                        // waiting for the post-rearm frame.
                        last_frame = tokio::time::Instant::now();
                    }
                }
            }
            // Shutdown signalled or event channel closed (page/target gone):
            // drop and exit.
        });

        Ok(Self {
            latest_tx,
            client: page.cdp().clone(),
            session_id: page.session_id().to_string(),
            shutdown_tx,
            task: std::sync::Mutex::new(Some(task)),
        })
    }

    /// Subscribe to the latest-frame channel.
    pub fn subscribe(&self) -> watch::Receiver<Option<Frame>> {
        self.latest_tx.subscribe()
    }

    /// Get the latest frame if available (non-blocking).
    pub fn latest(&self) -> Option<Frame> {
        self.latest_tx.borrow().clone()
    }

    /// Stop the screencast without awaiting the response.
    ///
    /// Used by [`Drop`], which cannot await. Prefer [`Self::stop`] where you
    /// can observe the result.
    fn stop_detached(&self) {
        // Signal first, then abort: the background task must never issue
        // another Page.startScreencast after this point.
        let _ = self.shutdown_tx.send(true);
        if let Some(task) = self.task.lock().ok().and_then(|mut t| t.take()) {
            task.abort();
        }
        let client = self.client.clone();
        let session_id = self.session_id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = client
                    .call_session::<Value>(
                        "Page.stopScreencast",
                        None,
                        &session_id,
                        Duration::from_secs(5),
                    )
                    .await;
            });
        }
    }

    /// Stop the screencast.
    ///
    /// Ordering matters: the background task is signalled and awaited *before*
    /// `Page.stopScreencast` is sent, so its navigation-restart and idle-re-arm
    /// paths can never re-issue `Page.startScreencast` after this returns.
    /// Idempotent: a second call is a no-op apart from re-sending the
    /// (harmless) stop command.
    pub async fn stop(&self) -> Result<(), BrowserError> {
        let _ = self.shutdown_tx.send(true);
        let task = self.task.lock().ok().and_then(|mut t| t.take());
        if let Some(mut task) = task {
            // The task exits promptly on the shutdown signal; the timeout is a
            // backstop against a wedged CDP call inside the loop.
            if tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }
        self.client
            .call_session::<Value>(
                "Page.stopScreencast",
                None,
                &self.session_id,
                Duration::from_secs(5),
            )
            .await
            .map_err(BrowserError::from)?;
        Ok(())
    }

    /// Whether the background task has terminated. Test/diagnostic hook.
    pub fn task_finished(&self) -> bool {
        self.task
            .lock()
            .map(|t| t.as_ref().is_none_or(|h| h.is_finished()))
            .unwrap_or(true)
    }
}

impl Drop for Screencast {
    /// Best-effort stop, so a dropped handle does not leave Chrome encoding and
    /// emitting JPEG frames for a stream nobody reads — and cancels the
    /// background task so it cannot re-arm the cast afterwards.
    fn drop(&mut self) {
        self.stop_detached();
    }
}

/// Issue `Page.startScreencast` through the page handle (sessionId-scoped).
async fn start_cast(
    page: &Page,
    quality: u32,
    max_width: u32,
    max_height: u32,
) -> Result<(), BrowserError> {
    page.call::<Value>(
        "Page.startScreencast",
        Some(json!({
            "format": "jpeg",
            "quality": quality,
            "maxWidth": max_width,
            "maxHeight": max_height,
            "everyNthFrame": 1,
        })),
        Duration::from_secs(10),
    )
    .await?;
    Ok(())
}

/// Issue `Page.startScreencast` via the raw CDP client + session id (used by
/// the background task, which only holds a cloned client, not the Page).
async fn start_cast_via_client(
    client: &CdpClient,
    session_id: &str,
    quality: u32,
    max_width: u32,
    max_height: u32,
) -> Result<(), BrowserError> {
    client
        .call_session::<Value>(
            "Page.startScreencast",
            Some(json!({
                "format": "jpeg",
                "quality": quality,
                "maxWidth": max_width,
                "maxHeight": max_height,
                "everyNthFrame": 1,
            })),
            session_id,
            Duration::from_secs(10),
        )
        .await
        .map_err(BrowserError::from)?;
    Ok(())
}

//! CDP `Page.startScreencast` frame capture.
//!
//! Subscribes to `Page.screencastFrame` events, decodes the base64 JPEG, keeps
//! only the most recent frame (backpressure drops stale frames rather than
//! queuing them), and acks each frame as CDP requires.
//!
//! On main-frame navigation (`Page.frameNavigated`) the screencast is
//! automatically restarted, because Chrome stops delivering frames after the
//! page's main document is replaced (e.g. `browser.open`). This keeps the live
//! viewer working across navigations without the caller doing anything.
//!
//! [Decision Log]
//! - 목적과 의도: Provide a live frame stream for the localhost viewer without
//!   polling `Page.captureScreenshot` (which is heavy and not designed for 30fps).
//! - 기존 구현 및 제약 조건: No screencast support existed; screenshots were
//!   one-shot base64 PNG captures.
//! - 검토한 주요 대안: Poll captureScreenshot at intervals — high CPU, jank,
//!   and not a real-time stream.
//! - 선택한 방식: CDP Page.startScreencast + Page.screencastFrame events,
//!   with a watch channel (held in an Arc) carrying only the latest frame.
//!   frameNavigated triggers a restart so navigations don't freeze the stream.
//! - 장점: Real ~30fps, low overhead, survives navigation. 단점: Requires the
//!   page session to stay attached; stops if the target closes.
//! - 수정 시 주의: Every screencastFrame MUST be acked (Page.screencastFrameAck)
//!   or Chrome stops sending frames after ~2 buffered frames. The Arc<sender>
//!   is shared between the background task and the handle so `latest()` and
//!   `subscribe()` always see the frames the task sends.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::browser::{BrowserError, Page};
use crate::cdp::CdpClient;

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
}

impl Screencast {
    /// Start a screencast on `page`. Returns the screencast handle. The
    /// background task acks every frame, updates the latest-frame channel,
    /// and restarts the cast on main-frame navigation.
    pub async fn start(
        page: &Page,
        quality: u32,
        max_width: u32,
        max_height: u32,
    ) -> Result<Self, BrowserError> {
        // Subscribe to events BEFORE starting the cast so we never miss the
        // first frame. The CDP client fans events out to every subscriber.
        let mut events = page.cdp().subscribe_events_async().await;

        // Start the cast.
        page.enable("Page.enable", None).await?;
        start_cast(page, quality, max_width, max_height).await?;

        let (latest_tx, _) = watch::channel::<Option<Frame>>(None);
        let latest_tx = Arc::new(latest_tx);

        // Background task: decode + ack frames, restart on navigation.
        // It holds an Arc clone of the SAME sender the handle exposes, so
        // latest()/subscribe() see every published frame.
        let page_client = page.cdp().clone();
        let session_id = page.session_id().to_string();
        let latest_tx_task = latest_tx.clone();
        let start_params = (quality, max_width, max_height);
        tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                if ev.method == "Page.frameNavigated" {
                    let is_main = ev
                        .params
                        .get("frame")
                        .and_then(|f| f.get("parentId"))
                        .is_none();
                    if is_main {
                        // Chrome stops sending frames after a main-frame swap;
                        // restart the cast after a short render warm-up.
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        let _ = start_cast_via_client(
                            &page_client,
                            &session_id,
                            start_params.0,
                            start_params.1,
                            start_params.2,
                        )
                        .await;
                    }
                    continue;
                }
                if ev.method != "Page.screencastFrame" {
                    continue;
                }
                let params = &ev.params;
                let frame_session_id = params
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Ack the frame (required) so Chrome keeps sending frames.
                let _ = page_client
                    .call_session::<Value>(
                        "Page.screencastFrameAck",
                        Some(json!({ "sessionId": frame_session_id })),
                        &session_id,
                        Duration::from_secs(5),
                    )
                    .await;

                if let Some(data_b64) = params.get("data").and_then(|v| v.as_str()) {
                    let data = base64::engine::general_purpose::STANDARD
                        .decode(data_b64)
                        .unwrap_or_default();
                    let frame = Frame {
                        data,
                        metadata: params.clone(),
                    };
                    let _ = latest_tx_task.send(Some(frame));
                }
            }
            // Event channel closed (page/target gone): drop and exit.
        });

        Ok(Self {
            latest_tx,
            client: page.cdp().clone(),
            session_id: page.session_id().to_string(),
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

    /// Stop the screencast.
    pub async fn stop(&self) -> Result<(), BrowserError> {
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

//! Async CDP client over a single WebSocket.
//!
//! One [`CdpClient`] owns one WebSocket connection to a CDP target (either the
//! browser endpoint or a page session). It:
//!   - assigns monotonically increasing request ids,
//!   - matches responses to pending requests via a map,
//!   - fans out unsolicited CDP events to subscribers,
//!   - supports timeouts and cancellation on drop.
//!
//! ## Why a broadcast event bus (not a single subscriber)
//! The previous design stored a single `event_tx`. Calling `subscribe_events`
//! a second time silently replaced the first subscriber, so the Network tracker,
//! the `Page.frameNavigated` generation listener, and the console collector
//! could not coexist. We now keep a `Vec` of senders so every subsystem that
//! needs CDP events gets its own receiver. This is the foundation that lets
//! `wait` use `Network.*` events while the session independently listens for
//! `Page.frameNavigated` to invalidate observe references.
//!
//! ## Why not expose raw JSON
//! Raw CDP responses are loosely-typed `serde_json::Value`. Each domain method
//! here returns a typed struct (or `()` when there is no useful payload), so
//! the rest of the codebase never pattern-matches on JSON keys.
//!
//! ## Reconnection: what it restores, and what it does not
//! One background task (the *supervisor*) owns the socket for the client's whole
//! life. It pumps writes, reads frames, sends keepalive pings, and — when the
//! socket drops unexpectedly — redials [`Inner::url`] with bounded exponential
//! backoff. Three things are deliberate:
//!
//!   - **Event subscribers survive.** The subscriber list lives in `Inner`, not
//!     in the reader task, so the receivers handed out by
//!     [`CdpClient::subscribe_events_async`] (network tracker, `Page.frameNavigated`
//!     listener, screencast, host-policy `Fetch` interceptor) keep working across
//!     a redial without being re-registered.
//!   - **In-flight requests still fail.** CDP has no request replay, so a call
//!     that was outstanding when the socket died is answered with
//!     [`CdpError::connection_lost`], which says the connection dropped and is
//!     being re-established.
//!   - **Reconnect restores the transport only.** It does *not* restore CDP
//!     state, and it cannot from this layer:
//!       * `Target.attachToTarget` sessions are scoped to one WebSocket. After a
//!         redial every `sessionId` this client ever handed out is dead, and
//!         [`CdpClient::call_session`] with it fails `Session with given id not found`.
//!       * Per-session domain enables (`Page.enable`, `Network.enable`,
//!         `Fetch.enable`, `DOM.enable`) and `Page.addScriptToEvaluateOnNewDocument`
//!         die with those sessions, and the browser-level connection cannot
//!         re-issue them because it does not know which targets were attached or
//!         which domains each caller turned on.
//!
//!     Fixing that belongs one layer up: `Browser` would have to hold the pages
//!     it created, and `Page` would need a mutable `session_id` plus a record of
//!     the enables it issued, so a reconnect signal could drive re-attach +
//!     re-enable. The hook for exactly that is
//!     [`CdpClient::subscribe_reconnects`]; until something uses it, a reconnect
//!     logs a `warn!` telling the operator that sessions and domain state were
//!     *not* restored. Reconnecting silently and letting callers believe the
//!     session is healthy would be worse than staying dead.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio_tungstenite::tungstenite::Message;

use crate::cdp::error::CdpError;
use crate::cdp::types::{RemoteObject, TargetInfo};

/// An unsolicited CDP event delivered to a subscriber.
#[derive(Debug, Clone)]
pub struct CdpEvent {
    /// CDP method name, e.g. `Log.entryAdded`.
    pub method: String,
    /// Event payload (params).
    pub params: Value,
    /// The target session this event came from, when the frame carried one.
    ///
    /// ## Why this field exists
    /// The event bus fans every frame out to every subscriber. Without the
    /// originating `sessionId` a subscriber cannot tell a main-page event from
    /// one belonging to a popup, an OOPIF, or any auto-attached child target —
    /// so a second target silently cross-wires network accounting, navigation
    /// invalidation, and request interception. Subscribers that are scoped to
    /// one page MUST compare this against [`Page::session_id`] and ignore
    /// events from other sessions.
    ///
    /// `None` means a browser-level event (no `sessionId` in the frame).
    ///
    /// [`Page::session_id`]: crate::browser::Page::session_id
    pub session_id: Option<String>,
}

/// An outstanding CDP request: the method name (so an error can name the call
/// that failed) and the channel its response is delivered on.
type PendingCall = (String, oneshot::Sender<Result<Value, CdpError>>);

/// The concrete WebSocket type `tokio_tungstenite::connect_async` returns.
type WsConn =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Keepalive ping interval. Without it a silently dead connection (peer gone,
/// no FIN/RST — NAT timeout, suspended container, wedged browser) is invisible
/// until some call burns its full 30s timeout. A ping fails to send, or its TCP
/// write eventually errors, which surfaces the drop and starts a reconnect.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
/// First backoff step; doubles each attempt up to [`RECONNECT_MAX_BACKOFF`].
const RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
/// Backoff ceiling.
const RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(3);
/// Total reconnect budget. 6 attempts at 100/200/400/800/1600/3000ms is ~6.1s
/// of redialing — long enough to ride out a transient socket error, short enough
/// that a genuinely dead browser is reported as closed promptly (the session
/// supervisor polls [`CdpClient::is_closed`] to decide the browser is gone).
const RECONNECT_MAX_ATTEMPTS: u32 = 6;

/// Inner shared state.
struct Inner {
    /// The endpoint this client dialed, kept so the supervisor can redial it.
    url: String,
    /// False while the socket is down and a redial is in flight. Distinct from
    /// `closed_flag`, which means "gone for good, no more reconnects".
    connected: AtomicBool,
    /// Subscribers notified after each successful redial. See
    /// [`CdpClient::subscribe_reconnects`].
    reconnected: Mutex<Vec<mpsc::UnboundedSender<()>>>,
    next_id: AtomicU64,
    /// Pending requests, keyed by request id. The method name is kept alongside
    /// the sender so a protocol error can name the call that failed; without it
    /// every `CdpError::Protocol` reported an empty method.
    pending: Mutex<HashMap<u64, PendingCall>>,
    /// All active event subscribers. Every `subscribe_events_async` call adds a
    /// sender here; the reader task fans each event out to all of them.
    /// Why a Vec instead of a single Option: multiple subsystems (Network
    /// tracker, Page.frameNavigated listener, console collector) need CDP
    /// events concurrently. A single slot would silently drop the previous
    /// subscriber.
    subscribers: Mutex<Vec<mpsc::UnboundedSender<CdpEvent>>>,
    closed: Notify,
    closed_flag: Mutex<bool>,
}

/// A CDP WebSocket client for a single target.
#[derive(Clone)]
pub struct CdpClient {
    inner: Arc<Inner>,
    /// `UnboundedSender` is already `Clone + Send + Sync`; an earlier `Arc<Mutex<_>>`
    /// around it serialized every send for no gain.
    write: mpsc::UnboundedSender<Message>,
}

impl CdpClient {
    /// Connect to a CDP WebSocket endpoint and spawn the background supervisor
    /// task that owns the socket.
    ///
    /// The supervisor is tied to the lifetime of the returned client: when the
    /// last clone is dropped the write channel closes, the supervisor shuts the
    /// socket down and exits. Until then it reconnects across unexpected drops —
    /// see the module docs for what a reconnect does and does not restore.
    pub async fn connect(url: &str) -> Result<Self, CdpError> {
        let ws = dial(url).await?;

        let (write_tx, write_rx) = mpsc::unbounded_channel::<Message>();
        let inner = Arc::new(Inner {
            url: url.to_string(),
            connected: AtomicBool::new(true),
            reconnected: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(Vec::new()),
            closed: Notify::new(),
            closed_flag: Mutex::new(false),
        });

        tokio::spawn(supervise(inner.clone(), ws, write_rx));

        Ok(Self {
            inner,
            write: write_tx,
        })
    }

    /// Subscribe to successful reconnects.
    ///
    /// Each redial sends one `()` to every subscriber. Nothing consumes this
    /// yet; it exists because it is the signal an upper layer needs to repair
    /// what the CDP layer cannot (re-`Target.attachToTarget` every page and
    /// re-issue its domain enables — see the module docs). A subscriber whose
    /// receiver is dropped is removed on the next send.
    pub async fn subscribe_reconnects(&self) -> mpsc::UnboundedReceiver<()> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inner.reconnected.lock().await.push(tx);
        rx
    }

    /// Whether the socket is up right now. `false` means either a redial is in
    /// flight (calls fail fast with [`CdpError::connection_lost`]) or the client
    /// is closed for good — [`Self::is_closed`] distinguishes the two.
    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::Relaxed)
    }

    /// Subscribe to unsolicited CDP events. Multiple subscribers are supported
    /// concurrently; each call returns an independent receiver and the event is
    /// fanned out to all of them. This lets the Network tracker, the
    /// `Page.frameNavigated` generation listener, and any other subsystem
    /// receive events at the same time.
    ///
    /// Each call registers a new subscriber without disturbing existing ones.
    ///
    /// There is no blocking variant: an earlier `subscribe_events` used
    /// `Mutex::blocking_lock`, which panics when called from inside a tokio
    /// runtime — i.e. from every real caller in this codebase.
    pub async fn subscribe_events_async(&self) -> mpsc::UnboundedReceiver<CdpEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inner.subscribers.lock().await.push(tx);
        rx
    }

    /// Call a CDP method with typed params, returning a typed result.
    ///
    /// Generic `T` should implement [`DeserializeOwned`]; pass `serde_json::Value`
    /// or `()` if the result shape is unimportant.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<T, CdpError> {
        self.call_inner(method, params, None, timeout).await
    }

    /// The single request/response implementation behind [`Self::call`] and
    /// [`Self::call_session`]; `session_id` selects CDP flatten mode.
    async fn call_inner<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
        session_id: Option<&str>,
        timeout: Duration,
    ) -> Result<T, CdpError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let mut req = serde_json::json!({
            "id": id,
            "method": method,
            "params": params.unwrap_or(Value::Object(Default::default())),
        });
        if let Some(sid) = session_id {
            req["sessionId"] = Value::String(sid.to_string());
        }
        let text = serde_json::to_string(&req)
            .map_err(|e| CdpError::Transport(format!("serialize request: {e}")))?;

        let (tx, rx) = oneshot::channel();
        let method_owned = method.to_string();
        self.inner
            .pending
            .lock()
            .await
            .insert(id, (method_owned.clone(), tx));

        // Guard: remove pending entry if we time out / are cancelled.
        let inner = self.inner.clone();
        let timeout_fut = tokio::time::sleep(timeout);

        // A send error means the supervisor task is gone, i.e. the client is
        // closed for good. While a reconnect is merely *in flight* the send
        // succeeds and the supervisor fails this request explicitly with
        // `connection_lost` (see `drain_while_disconnected`), so the caller can
        // tell "retry later" from "this client is finished".
        if self.write.send(Message::Text(text)).is_err() {
            inner.pending.lock().await.remove(&id);
            return Err(CdpError::connection_closed(&method_owned));
        }

        tokio::pin!(timeout_fut);
        let result = tokio::select! {
            biased;
            _ = &mut timeout_fut => {
                inner.pending.lock().await.remove(&id);
                return Err(CdpError::Timeout {
                    operation: method_owned.clone(),
                    timeout_ms: timeout.as_millis() as u64,
                });
            }
            res = rx => match res {
                Ok(Ok(val)) => Ok(val),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(CdpError::Transport("response channel dropped".into())),
            },
        };

        let val = result?;
        serde_json::from_value::<T>(val).map_err(|source| CdpError::Deserialize {
            method: method_owned,
            source,
        })
    }

    /// Fire-and-forget event enable. Equivalent to [`Self::call`] with no result.
    pub async fn enable(&self, method: &str, params: Option<Value>) -> Result<(), CdpError> {
        self.call::<serde_json::Value>(method, params, default_timeout())
            .await
            .map(|_| ())
    }

    /// Returns true if the client is finished: shut down intentionally, or
    /// dropped and unable to reconnect within its attempt budget. A transient
    /// drop that is still being redialed reports `false` here (and `false` from
    /// [`Self::is_connected`]).
    pub async fn is_closed(&self) -> bool {
        *self.inner.closed_flag.lock().await
    }

    /// Wait until the client is finished (as in [`Self::is_closed`]).
    pub async fn closed(&self) {
        if self.is_closed().await {
            return;
        }
        self.inner.closed.notified().await;
    }
}

/// Default per-call timeout for CDP requests.
pub fn default_timeout() -> Duration {
    Duration::from_secs(30)
}

/// Open one WebSocket to `url`.
async fn dial(url: &str) -> Result<WsConn, CdpError> {
    let (ws, _resp) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| CdpError::Transport(format!("connect {url}: {e}")))?;
    Ok(ws)
}

/// Why [`pump`] returned.
enum Exit {
    /// The client is finished: every [`CdpClient`] clone was dropped, or the
    /// peer sent a Close frame (an intentional goodbye — `Browser.close`, or a
    /// target that closed). Neither warrants a redial.
    Shutdown,
    /// The socket failed or ended without a Close frame. This is the case a
    /// reconnect is for.
    Dropped,
}

/// Owns the socket for the client's whole life: pump, then redial, then pump
/// again, until shutdown or the reconnect budget runs out.
///
/// One task rather than the previous reader+writer pair, because reconnecting
/// means replacing the socket, and a split sink/stream cannot be swapped from
/// two independent tasks without a shared, locked slot in between. Reads and
/// writes are still concurrent — `pump` selects over both.
async fn supervise(
    inner: Arc<Inner>,
    first: WsConn,
    mut write_rx: mpsc::UnboundedReceiver<Message>,
) {
    let mut ws = first;
    loop {
        match pump(&inner, ws, &mut write_rx).await {
            Exit::Shutdown => break,
            Exit::Dropped => {}
        }
        inner.connected.store(false, Ordering::Relaxed);
        // In-flight requests cannot be replayed; fail them with an error that
        // says a reconnect is under way.
        fail_pending(&inner, CdpError::connection_lost).await;

        match reconnect(&inner, &mut write_rx).await {
            Some(next) => {
                ws = next;
                inner.connected.store(true, Ordering::Relaxed);
                notify_reconnect(&inner).await;
            }
            None => break,
        }
    }

    inner.connected.store(false, Ordering::Relaxed);
    fail_pending(&inner, CdpError::connection_closed).await;
    let mut flag = inner.closed_flag.lock().await;
    *flag = true;
    inner.closed.notify_waiters();
}

/// Run one connection: forward queued writes, demux incoming frames, keepalive.
async fn pump(
    inner: &Arc<Inner>,
    mut ws: WsConn,
    write_rx: &mut mpsc::UnboundedReceiver<Message>,
) -> Exit {
    let mut ping = tokio::time::interval(KEEPALIVE_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await; // the first tick completes immediately; skip it

    loop {
        tokio::select! {
            outgoing = write_rx.recv() => match outgoing {
                Some(msg) => {
                    if ws.send(msg).await.is_err() {
                        return Exit::Dropped;
                    }
                }
                // Every CdpClient clone is gone: intentional shutdown.
                None => {
                    let _ = ws.close(None).await;
                    return Exit::Shutdown;
                }
            },
            frame = ws.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    if let Err(e) = route_message(inner, &text).await {
                        tracing::debug!(error = %e, "cdp route failure");
                    }
                }
                Some(Ok(Message::Binary(bin))) => {
                    if let Ok(text) = std::str::from_utf8(&bin) {
                        let _ = route_message(inner, text).await;
                    }
                }
                // A Close frame is the peer saying goodbye on purpose —
                // `Browser.close`, or a target that closed. The endpoint behind
                // this URL is gone, so redialing it would only produce a
                // reconnect storm on every normal shutdown.
                Some(Ok(Message::Close(_))) => {
                    let _ = ws.close(None).await;
                    return Exit::Shutdown;
                }
                // tungstenite answers pings and pongs itself.
                Some(Ok(_frame)) => {}
                Some(Err(e)) => {
                    tracing::debug!(error = %e, "cdp websocket read error");
                    return Exit::Dropped;
                }
                None => return Exit::Dropped,
            },
            _ = ping.tick() => {
                if ws.send(Message::Ping(Vec::new())).await.is_err() {
                    return Exit::Dropped;
                }
            }
        }
    }
}

/// Redial with bounded exponential backoff.
///
/// Returns `None` when the budget is exhausted or every client was dropped
/// while we were waiting.
async fn reconnect(
    inner: &Arc<Inner>,
    write_rx: &mut mpsc::UnboundedReceiver<Message>,
) -> Option<WsConn> {
    let mut delay = RECONNECT_INITIAL_BACKOFF;
    for attempt in 1..=RECONNECT_MAX_ATTEMPTS {
        tracing::warn!(
            url = %inner.url,
            attempt,
            max_attempts = RECONNECT_MAX_ATTEMPTS,
            backoff_ms = delay.as_millis() as u64,
            "cdp connection lost; reconnecting"
        );
        if !backoff_draining(inner, write_rx, delay).await {
            // No clients left; nobody is waiting for this connection.
            return None;
        }
        match dial(&inner.url).await {
            Ok(ws) => {
                tracing::info!(url = %inner.url, attempt, "cdp reconnected");
                tracing::warn!(
                    "cdp reconnect restored the transport only: page sessions from \
                     Target.attachToTarget are per-connection and are now invalid, and the \
                     domains enabled on them (Page/Network/Fetch/DOM) were NOT re-enabled. \
                     Calls using a pre-drop sessionId will fail with 'Session with given id \
                     not found' until the Browser/Page layer re-attaches and re-enables."
                );
                return Some(ws);
            }
            Err(e) => {
                tracing::warn!(url = %inner.url, attempt, error = %e, "cdp reconnect attempt failed");
            }
        }
        delay = (delay * 2).min(RECONNECT_MAX_BACKOFF);
    }
    tracing::warn!(
        url = %inner.url,
        attempts = RECONNECT_MAX_ATTEMPTS,
        "cdp reconnect gave up; client is closed"
    );
    None
}

/// Wait `delay`, answering anything a caller sends meanwhile.
///
/// Returns `false` if every [`CdpClient`] clone was dropped during the wait.
/// Requests queued while the socket is down are failed immediately rather than
/// left to time out: the write channel is unbounded, so without this they would
/// sit there for their full 30s timeout even though we know they cannot be sent.
async fn backoff_draining(
    inner: &Arc<Inner>,
    write_rx: &mut mpsc::UnboundedReceiver<Message>,
    delay: Duration,
) -> bool {
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return true,
            outgoing = write_rx.recv() => match outgoing {
                Some(msg) => drain_while_disconnected(inner, &msg).await,
                None => return false,
            },
        }
    }
}

/// Fail the pending entry for a request that was queued while the socket was
/// down, by reading its id back out of the serialized frame.
async fn drain_while_disconnected(inner: &Arc<Inner>, msg: &Message) {
    let Message::Text(text) = msg else { return };
    let Ok(val) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let Some(id) = val.get("id").and_then(|v| v.as_u64()) else {
        return;
    };
    let entry = inner.pending.lock().await.remove(&id);
    if let Some((method, tx)) = entry {
        let _ = tx.send(Err(CdpError::connection_lost(&method)));
    }
}

/// Answer every outstanding request with `make_err(method)`.
async fn fail_pending(inner: &Arc<Inner>, make_err: fn(&str) -> CdpError) {
    let drained: Vec<PendingCall> = inner
        .pending
        .lock()
        .await
        .drain()
        .map(|(_, call)| call)
        .collect();
    for (method, tx) in drained {
        let _ = tx.send(Err(make_err(&method)));
    }
}

/// Tell reconnect subscribers the socket is back, dropping dead subscribers.
async fn notify_reconnect(inner: &Arc<Inner>) {
    let mut subs = inner.reconnected.lock().await;
    subs.retain(|tx| tx.send(()).is_ok());
}

async fn route_message(inner: &Arc<Inner>, text: &str) -> Result<(), CdpError> {
    let val: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Ok(()), // ignore non-JSON frames
    };
    if let Some(id) = val.get("id").and_then(|v| v.as_u64()) {
        // Response to a request.
        let sender = inner.pending.lock().await.remove(&id);
        if let Some((method, sender)) = sender {
            let result = if let Some(err) = val.get("error") {
                Err(CdpError::Protocol {
                    method,
                    code: err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1),
                    message: err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                })
            } else {
                Ok(val.get("result").cloned().unwrap_or(Value::Null))
            };
            let _ = sender.send(result);
        }
    } else if let Some(method) = val.get("method").and_then(|m| m.as_str()) {
        // Unsolicited event.
        let params = val.get("params").cloned().unwrap_or(Value::Null);
        let event = CdpEvent {
            method: method.to_string(),
            params,
            session_id: val
                .get("sessionId")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string()),
        };
        // Fan out to every subscriber. Drop any whose receiver is gone so the
        // list doesn't grow unbounded over a long session.
        let mut subs = inner.subscribers.lock().await;
        // retain keeps only senders whose receiver is still alive.
        subs.retain(|tx| tx.send(event.clone()).is_ok());
    }
    Ok(())
}

/// Convenience: fetch the browser-level websocket URL from /json/version.
pub async fn browser_ws_url(http_endpoint: &str) -> Result<String, CdpError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| CdpError::Transport(format!("http client: {e}")))?;
    let v: Value = client
        .get(http_endpoint)
        .send()
        .await
        .map_err(|e| CdpError::Transport(format!("get {http_endpoint}: {e}")))?
        .json()
        .await
        .map_err(|e| CdpError::Transport(format!("json {http_endpoint}: {e}")))?;
    v.get("webSocketDebuggerUrl")
        .and_then(|u| u.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| CdpError::Transport("missing webSocketDebuggerUrl".into()))
}

/// List available page targets via the HTTP /json endpoint.
pub async fn list_targets(http_endpoint: &str) -> Result<Vec<TargetInfo>, CdpError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| CdpError::Transport(format!("http client: {e}")))?;
    let v: Vec<Value> = client
        .get(http_endpoint)
        .send()
        .await
        .map_err(|e| CdpError::Transport(format!("get {http_endpoint}: {e}")))?
        .json()
        .await
        .map_err(|e| CdpError::Transport(format!("json {http_endpoint}: {e}")))?;
    Ok(v.into_iter().filter_map(TargetInfo::from_value).collect())
}

/// Result of evaluating a JS expression via Runtime.evaluate.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct EvaluateResult {
    /// The remote object representing the result value.
    #[serde(rename = "result")]
    pub result: RemoteObject,
    /// Exception details, if the expression threw.
    #[serde(rename = "exceptionDetails", default)]
    pub exception_details: Option<Value>,
}

impl EvaluateResult {
    /// Convenience accessor for the result's value field (JSON).
    pub fn value(&self) -> Option<&Value> {
        self.result.value.as_ref()
    }
}

impl CdpClient {
    /// Send a CDP command scoped to a target session (flatten mode).
    ///
    /// In CDP "flatten" mode, the request object carries a `sessionId` field
    /// so responses are matched by id as usual. This is how per-page commands
    /// work when attached through the browser-level connection.
    pub async fn call_session<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
        session_id: &str,
        timeout_dur: Duration,
    ) -> Result<T, CdpError> {
        self.call_inner(method, params, Some(session_id), timeout_dur)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// Drive a client through a mid-request disconnect and assert the three
    /// properties reconnection promises: the in-flight call fails with an error
    /// that says a redial is under way, an event subscriber registered *before*
    /// the drop still receives events afterwards, and the client is usable again.
    ///
    /// A bare WebSocket server stands in for Chrome: this exercises the
    /// transport, which is the only thing reconnection actually repairs.
    #[tokio::test]
    async fn reconnects_after_drop_and_keeps_event_subscribers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("ws://{addr}");

        let server = tokio::spawn(async move {
            // Connection 1: take one request, then vanish without a Close frame
            // (what a crashed peer or a reset connection looks like).
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            let _in_flight = ws.next().await;
            drop(ws);

            // Connection 2: the redial. Push an event, then answer the next call.
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            ws.send(Message::Text(
                r#"{"method":"Test.event","params":{"n":1}}"#.to_string(),
            ))
            .await
            .unwrap();
            while let Some(Ok(msg)) = ws.next().await {
                if let Message::Text(text) = msg {
                    let v: Value = serde_json::from_str(&text).unwrap();
                    let id = v["id"].as_u64().unwrap();
                    ws.send(Message::Text(format!(
                        r#"{{"id":{id},"result":{{"ok":true}}}}"#
                    )))
                    .await
                    .unwrap();
                    break;
                }
            }
            // Hold the socket open until the client goes away.
            while ws.next().await.is_some() {}
        });

        let client = CdpClient::connect(&url).await.unwrap();
        let mut events = client.subscribe_events_async().await;

        let err = client
            .call::<Value>("Test.lost", None, Duration::from_secs(5))
            .await
            .expect_err("a request in flight when the socket dies must fail");
        assert!(
            err.to_string().contains("re-establishing"),
            "in-flight failure must say a reconnect is under way, got: {err}"
        );

        let ev = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("event after reconnect timed out")
            .expect("subscriber channel must survive the reconnect");
        assert_eq!(ev.method, "Test.event");

        let v: Value = client
            .call("Test.after", None, Duration::from_secs(10))
            .await
            .expect("client must be usable after reconnecting");
        assert_eq!(v["ok"], Value::Bool(true));

        assert!(
            !client.is_closed().await,
            "a reconnected client is not closed"
        );
        assert!(client.is_connected());

        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    /// A peer that goes away for good must close the client rather than redial
    /// forever, so `is_closed` still means "the browser is gone".
    #[tokio::test]
    async fn gives_up_and_closes_when_the_peer_stays_down() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("ws://{addr}");

        let accept = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            // Free the port: every redial from here on is refused.
            drop(listener);
            ws
        });

        let client = CdpClient::connect(&url).await.unwrap();
        let ws = accept.await.unwrap();
        drop(ws);

        // ~6.1s of backoff, then the client reports itself closed — which is
        // what the session supervisor polls to decide the browser is gone.
        tokio::time::timeout(Duration::from_secs(30), client.closed())
            .await
            .expect("client must give up redialing and close");
        assert!(client.is_closed().await);
        assert!(!client.is_connected());

        let err = client
            .call::<Value>("Test.gone", None, Duration::from_secs(5))
            .await
            .expect_err("calls on a closed client must fail immediately");
        assert!(
            err.to_string().contains("not reconnecting"),
            "closed-client failure must say so, got: {err}"
        );
    }
}

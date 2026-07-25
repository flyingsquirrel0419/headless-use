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

use std::collections::HashMap;
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
}

/// Inner shared state.
struct Inner {
    next_id: Mutex<u64>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, CdpError>>>>,
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
    write: Arc<Mutex<mpsc::UnboundedSender<Message>>>,
}

impl CdpClient {
    /// Connect to a CDP WebSocket endpoint and spawn a background reader task.
    ///
    /// The reader task is tied to the lifetime of the returned client; when the
    /// last clone is dropped the writer half closes and the reader exits.
    pub async fn connect(url: &str) -> Result<Self, CdpError> {
        let (ws, _resp) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| CdpError::Transport(format!("connect {url}: {e}")))?;
        let (mut sink, mut stream) = ws.split();

        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Message>();
        let inner = Arc::new(Inner {
            next_id: Mutex::new(0),
            pending: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(Vec::new()),
            closed: Notify::new(),
            closed_flag: Mutex::new(false),
        });

        // Writer task: forwards queued messages to the WebSocket sink.
        let writer_inner = inner.clone();
        let writer = tokio::spawn(async move {
            while let Some(msg) = write_rx.recv().await {
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
            // Signal closed when the writer ends (all senders dropped).
            let mut flag = writer_inner.closed_flag.lock().await;
            *flag = true;
            writer_inner.closed.notify_waiters();
            let _ = sink.close().await;
        });

        // Reader task: demux incoming frames into responses/events.
        let reader_inner = inner.clone();
        tokio::spawn(async move {
            while let Some(frame) = stream.next().await {
                match frame {
                    Ok(Message::Text(text)) => {
                        if let Err(e) = route_message(&reader_inner, &text).await {
                            tracing::debug!(error = %e, "cdp route failure");
                        }
                    }
                    Ok(Message::Binary(bin)) => {
                        if let Ok(text) = std::str::from_utf8(&bin) {
                            let _ = route_message(&reader_inner, text).await;
                        }
                    }
                    Ok(Message::Ping(p)) => {
                        // tungstenite auto-pongs, but be defensive.
                        let _ = p;
                    }
                    Ok(Message::Close(_)) | Err(_) => {
                        break;
                    }
                    _ => {}
                }
            }
            // Drain pending requests with a transport error.
            let mut pending = reader_inner.pending.lock().await;
            for (_, tx) in pending.drain() {
                let _ = tx.send(Err(CdpError::Transport("connection closed".into())));
            }
            let mut flag = reader_inner.closed_flag.lock().await;
            *flag = true;
            reader_inner.closed.notify_waiters();
            drop(writer);
        });

        Ok(Self {
            inner,
            write: Arc::new(Mutex::new(write_tx)),
        })
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
        let id = {
            let mut g = self.inner.next_id.lock().await;
            *g += 1;
            *g
        };
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
        self.inner.pending.lock().await.insert(id, tx);

        // Guard: remove pending entry if we time out / are cancelled.
        let inner = self.inner.clone();
        let method_owned = method.to_string();
        let timeout_fut = tokio::time::sleep(timeout);

        let send_result = {
            let w = self.write.lock().await;
            w.send(Message::Text(text))
        };
        if let Err(e) = send_result {
            inner.pending.lock().await.remove(&id);
            return Err(CdpError::Transport(format!("send: {e}")));
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

    /// Returns true if the underlying connection is closed.
    pub async fn is_closed(&self) -> bool {
        *self.inner.closed_flag.lock().await
    }

    /// Wait until the connection closes.
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

async fn route_message(inner: &Arc<Inner>, text: &str) -> Result<(), CdpError> {
    let val: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Ok(()), // ignore non-JSON frames
    };
    if let Some(id) = val.get("id").and_then(|v| v.as_u64()) {
        // Response to a request.
        let sender = inner.pending.lock().await.remove(&id);
        if let Some(sender) = sender {
            let result = if let Some(err) = val.get("error") {
                Err(CdpError::Protocol {
                    method: String::new(),
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

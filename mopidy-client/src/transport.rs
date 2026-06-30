//! WebSocket transport layer for Mopidy JSON-RPC 2.0 communication.
//!
//! Provides:
//! - JSON-RPC 2.0 request/response framing (task 4.1)
//! - Reconnecting loop with exponential backoff + jitter (task 4.2)
//! - Connection-state signals published on every transition (task 4.3)
//! - Typed event parsing from JSON-RPC notifications (task 4.5)

use crate::state::MopidyConnectionState;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{
    MaybeTlsStream, tungstenite::Message, WebSocketStream,
};
use tracing::{debug, error, info, warn};

// ── JSON-RPC 2.0 types ───────────────────────────────────────────────────────

/// A numeric request ID used to correlate requests with responses.
pub type RequestId = u64;

/// Outgoing JSON-RPC 2.0 request: `{ "jsonrpc": "2.0", "id": N, "method": "...", "params": [...] }`
#[derive(Debug, Clone, serde::Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    #[serde(rename = "id")]
    pub request_id: RequestId,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    /// Build a JSON-RPC 2.0 request with no parameters (empty array).
    pub fn call(method: impl Into<String>, request_id: RequestId) -> Self {
        Self {
            jsonrpc: "2.0",
            request_id,
            method: method.into(),
            params: Some(Value::Array(vec![])),
        }
    }

    /// Build a JSON-RPC 2.0 request with explicit params.
    pub fn call_with_params(
        method: impl Into<String>,
        request_id: RequestId,
        params: Value,
    ) -> Self {
        Self {
            jsonrpc: "2.0",
            request_id,
            method: method.into(),
            params: Some(params),
        }
    }

    /// Serialize this request into a WebSocket text [`Message`].
    pub fn to_message(&self) -> Result<Message, TransportError> {
        let json = serde_json::to_value(self).map_err(TransportError::Serialize)?;
        Ok(Message::Text(json.to_string().into()))
    }

    /// Serialize this request as raw bytes (for lower-level writing).
    pub fn to_bytes(&self) -> Result<Vec<u8>, TransportError> {
        let json = serde_json::to_value(self).map_err(TransportError::Serialize)?;
        Ok(json.to_string().into_bytes())
    }
}

/// Incoming JSON-RPC 2.0 message envelope (parsed from the WebSocket stream).
#[derive(Debug, Clone)]
pub struct JsonRpcMessage {
    pub jsonrpc: String,
    pub request_id: Option<RequestId>,
    pub method: Option<String>,
    pub result: Option<Value>,
    pub error: Option<Value>,
}

impl<'de> serde::Deserialize<'de> for JsonRpcMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Helper {
            jsonrpc: String,
            #[serde(rename = "id", default)]
            request_id: Option<RequestId>,
            #[serde(default)]
            method: Option<String>,
            #[serde(default)]
            result: Option<Value>,
            #[serde(default)]
            error: Option<Value>,
        }

        let h: Helper = serde::Deserialize::deserialize(deserializer)?;
        Ok(JsonRpcMessage {
            jsonrpc: h.jsonrpc,
            request_id: h.request_id,
            method: h.method,
            result: h.result,
            error: h.error,
        })
    }
}

impl JsonRpcMessage {
    /// Parse a raw WebSocket [`Message`] into a JSON-RPC envelope.
    pub fn from_message(msg: Message) -> Result<Self, TransportError> {
        let bytes = msg.into_data();
        let text = String::from_utf8(bytes.to_vec()).map_err(TransportError::InvalidUtf8)?;
        serde_json::from_str(&text).map_err(|e| TransportError::Parse(e.to_string()))
    }

    /// Returns `true` when this is a **reply** — carries an `id` and no `method`.
    pub fn is_reply(&self) -> bool {
        self.request_id.is_some() && self.method.is_none()
    }

    /// Returns `true` when this is an **event/notification** — has a `method` field.
    pub fn is_event(&self) -> bool {
        self.method.is_some()
    }
}

// ── Transport error types ────────────────────────────────────────────────────

/// Errors that can occur during WebSocket communication.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// Low-level I/O failure.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// WebSocket protocol-level error from tungstenite.
    #[error("WebSocket protocol error: {0}")]
    Tungstenite(#[source] tokio_tungstenite::tungstenite::Error),

    /// Incoming message was not valid UTF-8 text.
    #[error("Incoming message was not valid UTF-8: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),

    /// Failed to parse a JSON-RPC envelope from the message bytes.
    #[error("JSON parse error: {0}")]
    Parse(String),

    /// The internal command channel was closed by the receiver side.
    #[error("Channel closed by receiver")]
    ChannelClosed,

    /// Failed to serialize a request struct into JSON.
    #[error("Message serialization failed: {0}")]
    Serialize(#[source] serde_json::Error),
}

// ── Backoff policy (task 4.2) ────────────────────────────────────────────────

/// Parameters controlling the exponential backoff with jitter strategy.
///
/// Defaults: initial = 500 ms, factor = 2, max_delay = 30 s, jitter ± 20 %.
#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    /// Initial delay before the first retry (~500 ms).
    pub initial: std::time::Duration,
    /// Multiplicative factor applied to the current delay on each failure (~2).
    pub factor: f64,
    /// Maximum delay cap (~30 s).
    pub max_delay: std::time::Duration,
    /// Jitter range as a fraction of the computed delay (± 20 % → 0.2).
    pub jitter_range: f64,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: std::time::Duration::from_millis(500),
            factor: 2.0,
            max_delay: std::time::Duration::from_secs(30),
            jitter_range: 0.2,
        }
    }
}

impl BackoffPolicy {
    /// Compute the wait interval for a given retry attempt (0-indexed).
    ///
    /// Applies exponential growth (`factor`^attempt), caps at `max_delay`,
    /// then adds ±`jitter_range` random jitter.
    pub fn jitter_delay(&self, attempt: u32) -> std::time::Duration {
        let base = self.initial.mul_f64(self.factor.powi(attempt as i32));
        let capped = base.min(self.max_delay);

        // Apply ±jitter random factor.
        let jitter_factor = 1.0 + (fastrand::f64() * 2.0 - 1.0) * self.jitter_range;
        capped.mul_f64(jitter_factor)
    }
}

/// Mopidy domain event types parsed from incoming JSON-RPC notifications.
///
/// Each inbound message that carries a `method` field (i.e. an event) is
/// deserialized into one of these variants and forwarded through the bounded
/// event channel.  Slice 0 logs every event; later slices consume them.
#[derive(Debug, Clone)]
pub enum MopidyEvent {
    /// Playback state changed (play / pause / stop).
    PlaybackStateChanged,
    /// Tracklist has been modified.
    TracklistChanged,
    /// A known but unmodelled event variant (placeholder for later slices).
    Other { method: String },
}

// ── Concrete stream alias ────────────────────────────────────────────────────

/// The underlying I/O stream type carried by our WebSocket connections.
type MopidyStream = MaybeTlsStream<tokio::net::TcpStream>;

// ── Event parsing helper (task 4.5) ──────────────────────────────────────────

/// Parse a JSON-RPC notification message into a typed [`MopidyEvent`].
///
/// Dispatches on the `method` string; matches known Mopidy event families
/// and falls through to [`MopidyEvent::Other`] for anything unmodelled.
pub fn parse_mopidy_event(msg: &JsonRpcMessage) -> Option<MopidyEvent> {
    let method = msg.method.as_ref()?;

    match method.as_str() {
        // Playback state notifications emitted by mopidy/core.
        "core.playbackstate.changes"
        | "playback_state_changed"
        | "core.playback.stateChanged" => Some(MopidyEvent::PlaybackStateChanged),

        // Tracklist modification notifications.
        "core.tracklist.changes"
        | "tracklist_changed"
        | "core.tracklist.changed" => Some(MopidyEvent::TracklistChanged),

        // Everything else — forward with the raw method name for later
        // slices to handle.
        other => Some(MopidyEvent::Other { method: other.to_string() }),
    }
}

// ── Mopidy WebSocket client (reconnect — task 4.2) ───────────────────────────

/// Internal commands sent to the reconnect loop via a channel.
#[derive(Debug)]
pub(crate) enum ClientCmd {
    /// Write a JSON-RPC message to the active WebSocket connection.
    SendMessage(Message),
}

/// Tracks pending oneshot senders so that typed call wrappers can await
/// the matched reply by request ID.
type PendingReplies = Arc<Mutex<HashMap<RequestId, oneshot::Sender<Result<JsonRpcMessage, TransportError>>>>>;

/// Shared, mutable connection state that all typed-call wrappers can check
/// before dispatching (task 4.4).
type SharedState = Arc<Mutex<MopidyConnectionState>>;

/// A reconnecting WebSocket JSON-RPC client for Mopidy.
///
/// On construction the client spawns an async task on the current tokio runtime
/// that drives the connect → read → dispatch → reconnect loop (task 4.2).
/// The client does **not** block application boot — if Mopidy is unreachable at
/// start, the loop enters BackingOff and retries indefinitely.
pub struct MopidyWsClient {
    cmd_tx: mpsc::Sender<ClientCmd>,
    id_counter: Arc<AtomicU64>,
    pending_replies: PendingReplies,
    /// Shared connection state — updated by the reconnect loop on every
    /// transition (Disconnected / BackingOff / Connecting / Connected).
    conn_state: SharedState,
}

impl MopidyWsClient {
    /// Create a new client and start the reconnect loop on the **current** tokio runtime.
    ///
    /// The `on_event_tx` sender receives typed [`MopidyEvent`] messages parsed
    /// from JSON-RPC notifications (messages with a `method` field).  In slice 0
    /// these are logged; later slices consume them.
    ///
    /// The `on_reply_tx` sender delivers typed replies correlated by request ID.
    ///
    /// The `state_tx` sender publishes [`MopidyConnectionState`] transitions
    /// (Disconnected → BackingOff → Connecting → Connected).  In slice 0 these
    /// are forwarded through the reply channel and logged; later slices consume
    /// them for fallback-chain and mid-episode-restart logic.
    pub fn spawn(
        url: String,
        policy: Option<BackoffPolicy>,
        on_event_tx: mpsc::Sender<MopidyEvent>,
        on_reply_tx: mpsc::Sender<JsonRpcMessage>,
        state_tx: mpsc::Sender<MopidyConnectionState>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<ClientCmd>(64);
        let pending_replies: PendingReplies = Arc::new(Mutex::new(HashMap::new()));
        let conn_state: SharedState = Arc::new(Mutex::new(MopidyConnectionState::Disconnected));

        tokio::spawn(Self::reconnect_loop(
            url,
            policy,
            on_event_tx,
            on_reply_tx,
            state_tx,
            cmd_rx,
            pending_replies.clone(),
            conn_state.clone(),
        ));

        Self {
            cmd_tx,
            id_counter: Arc::new(AtomicU64::new(0)),
            pending_replies,
            conn_state,
        }
    }

    /// Returns `true` when the client is in the Connected state.
    ///
    /// Typed-call wrappers call this before dispatching requests; if it
    /// returns `false` they return [`MopidyClientError::NotConnected`] instead
    /// of hanging (task 4.4).
    pub fn is_connected(&self) -> bool {
        if let Ok(state) = self.conn_state.lock() {
            matches!(*state, MopidyConnectionState::Connected)
        } else {
            false // poisoned lock → treat as disconnected
        }
    }

    /// Send a JSON-RPC request over the active WebSocket (if connected).
    pub async fn send_request(&self, req: &JsonRpcRequest) -> Result<(), TransportError> {
        let msg = req.to_message()?;
        self.cmd_tx
            .send(ClientCmd::SendMessage(msg))
            .await
            .map_err(|_| TransportError::ChannelClosed)
    }

    /// Build a typed request, send it, and await the matched reply on the
    /// pending-reply oneshot channel.  Returns the raw [`JsonRpcMessage`] so
    /// callers (typically method wrappers in `methods` module) can deserialize
    /// the result.
    ///
    /// This is the primitive all mechanical `call*` wrappers use:
    /// 1. Allocate a unique request ID,
    /// 2. Register an oneshot sender keyed by that ID,
    /// 3. Dispatch the request frame through [`ClientCmd`] → the session,
    /// 4. Await the matching reply or fail on channel drop.
    pub async fn send_and_await(
        &self,
        method: impl Into<String>,
        params: Option<Value>,
    ) -> Result<JsonRpcMessage, TransportError> {
        let request_id = self.id_counter.fetch_add(1, Ordering::Relaxed).wrapping_add(1);

        // Register an oneshot receiver for this specific ID.
        let (reply_tx, reply_rx) = oneshot::channel();
        {
            let mut map = self.pending_replies.lock().map_err(|_| TransportError::ChannelClosed)?;
            map.insert(request_id, reply_tx);
        }

        // Build and dispatch the frame.
        let msg = JsonRpcRequest {
            jsonrpc: "2.0",
            request_id,
            method: method.into(),
            params: Some(params.unwrap_or_else(|| Value::Array(vec![]))),
        }
        .to_message()?;

        self.cmd_tx
            .send(ClientCmd::SendMessage(msg))
            .await
            .map_err(|_| TransportError::ChannelClosed)?;

        // Await the matched reply.
        reply_rx.await.map_err(|_| TransportError::ChannelClosed)?
    }

    // ── Reconnect loop (task 4.2 + 4.3) ──────────────────────────────────────

    async fn reconnect_loop(
        url: String,
        policy: Option<BackoffPolicy>,
        on_event_tx: mpsc::Sender<MopidyEvent>,
        on_reply_tx: mpsc::Sender<JsonRpcMessage>,
        state_tx: mpsc::Sender<MopidyConnectionState>,
        cmd_rx: mpsc::Receiver<ClientCmd>,
        pending_replies: PendingReplies,
        shared_state: SharedState,
    ) {
        let policy = policy.unwrap_or_default();
        let mut attempt: u32 = 0;

        // Wrap receiver in Option so we can `take` ownership per-session and
        // put it back (from run_session's return value) for the next loop pass.
        let mut cmd_rx = Some(cmd_rx);

        // Initial state: Disconnected (task 4.3)
        let state_change = MopidyConnectionState::Disconnected;
        let _ = state_tx.send(state_change.clone()).await;
        if let Ok(mut shared) = shared_state.lock() {
            *shared = state_change.clone();
        }
        info!(state = ?state_change, "connection state transition" );

        loop {
            // ── State: Connecting (task 4.3) ──────────────
            let state_change = MopidyConnectionState::Connecting;
            let _ = state_tx.send(state_change.clone()).await;
            if let Ok(mut shared) = shared_state.lock() {
                *shared = state_change.clone();
            }
            info!(url = %url, attempt, state = ?state_change, "connecting to Mopidy WebSocket");

            match connect_ws(&url).await {
                Ok(ws_stream) => {
                    // ── State: Connected (task 4.3) ───────
                    let state_change = MopidyConnectionState::Connected;
                    let _ = state_tx.send(state_change.clone()).await;
                    if let Ok(mut shared) = shared_state.lock() {
                        *shared = state_change.clone();
                    }
                    info!(url = %url, state = ?state_change, "connected to Mopidy WebSocket");
                    attempt = 0; // reset on successful connection

                    let rx = cmd_rx
                        .take()
                        .expect("cmd_rx must be present after run_session returns it");

                    let returned_rx = run_session(
                        ws_stream,
                        &on_event_tx,
                        &on_reply_tx,
                        rx,
                        pending_replies.clone(),
                    )
                    .await;

                    // Put the receiver back for the next connection attempt.
                    cmd_rx.replace(returned_rx);
                }
                Err(e) => {
                    error!(error = %e, attempt, "connection failed — backing off");
                    let delay = policy.jitter_delay(attempt);

                    // ── State: BackingOff (task 4.3) ᐸ─────
                    let state_change = MopidyConnectionState::BackingOff { retry_in: delay };
                    let _ = state_tx.send(state_change.clone()).await;
                    if let Ok(mut shared) = shared_state.lock() {
                        *shared = state_change.clone();
                    }
                    info!(delay_ms = delay.as_millis(), state = ?state_change, "backing off before reconnect");
                    tokio::time::sleep(delay).await;

                    attempt += 1;
                }
            }

            // Connection ended — loop back and try again.
            debug!("session ended, scheduling reconnect");
        }
    }
}

// ── Internal helpers ────────────────────────────────────────────────────────

/// Establish a TCP + WebSocket connection to the given URL.
async fn connect_ws(
    url: &str,
) -> Result<WebSocketStream<MopidyStream>, TransportError> {
    let (stream, _response) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(TransportError::Tungstenite)?;
    debug!(url = %url, "WebSocket connected");
    Ok(stream)
}

/// Run a single WebSocket session: read messages, dispatch replies/events,
/// resolve pending typed-call receivers, and forward outbound writes from
/// `cmd_rx`. Ends when the connection drops.
async fn run_session(
    mut ws_stream: WebSocketStream<MopidyStream>,
    on_event_tx: &mpsc::Sender<MopidyEvent>,
    on_reply_tx: &mpsc::Sender<JsonRpcMessage>,
    mut cmd_rx: mpsc::Receiver<ClientCmd>,
    pending_replies: PendingReplies,
) -> mpsc::Receiver<ClientCmd> {
    use futures::{SinkExt, StreamExt};

    loop {
        tokio::select! {
            biased; // prefer reading incoming frames over sending out pending ones

            // Read next message from the WS stream.
            msg_result = ws_stream.next() => {
                match msg_result {
                    Some(Ok(ws_msg)) => {
                        let parsed = match JsonRpcMessage::from_message(ws_msg) {
                            Ok(p) => p,
                            Err(e) => {
                                warn!(error = %e, "failed to parse incoming message — skipping");
                                continue;
                            }
                        };

                        debug!(
                            jsonrpc = %parsed.jsonrpc,
                            has_id = parsed.request_id.is_some(),
                            has_method = parsed.method.is_some(),
                            "received JSON-RPC message",
                        );

                        if parsed.is_reply() {
                            // Resolve any pending typed-call oneshot for this ID.
                            if let Some(id) = parsed.request_id {
                                let mut map = pending_replies.lock().unwrap();
                                if let Some(tx) = map.remove(&id) {
                                    let _ = tx.send(Ok(parsed.clone()));
                                }
                            }
                            // Also forward through the reply channel for external consumers.
                            let _ = on_reply_tx.send(parsed).await;
                        } else if parsed.is_event() {
                            // Task 4.5: parse into typed event, log it, then forward.
                            if let Some(event) = parse_mopidy_event(&parsed) {
                                info!(event = ?event, "Mopidy event received");
                                let _ = on_event_tx.send(event).await;
                            }
                        }
                    }
                    Some(Err(e)) => {
                        error!(error = %e, "WebSocket read error — session ending");
                        break;
                    }
                    None => {
                        info!("WebSocket stream ended (EOF)");
                        break;
                    }
                }

                // Drain any queued outbound messages after processing an inbound frame.
                while let Ok(cmd) = cmd_rx.try_recv() {
                    let ClientCmd::SendMessage(msg) = cmd;
                    if let Err(e) = ws_stream.send(msg).await {
                        error!(error = %e, "WebSocket write failed");
                        break;
                    }
                }

                if cmd_rx.is_closed() {
                    info!("command channel closed — ending session");
                    break;
                }
            }

            // Handle outbound command. If no inbound frame is pending, we can still
            // flush an outgoing message.  This branch also fires when the rx is closed.
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(ClientCmd::SendMessage(msg)) => {
                        if let Err(e) = ws_stream.send(msg).await {
                            error!(error = %e, "WebSocket write failed");
                            break;
                        }
                        debug!("outbound message sent");
                    }
                    None => {
                        info!("command channel closed — ending session");
                        break;
                    }
                }
            }
        }
    }

    // Flush and close.
    let _ = ws_stream.close(None).await;

    cmd_rx
}

// ── Public convenience helpers ────────────────────────────────────────────────

/// Build the default [`BackoffPolicy`] (initial 500 ms, factor 2, cap 30 s, ± 20 % jitter).
pub fn default_backoff_policy() -> BackoffPolicy {
    BackoffPolicy::default()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── JSON-RPC framing (task 4.1) ────────────────────────────────────

    /// A `JsonRpcRequest::call` serializes to valid JSON-RPC 2.0.
    #[test]
    fn jsonrpc_request_serializes_correctly() {
        let req = JsonRpcRequest::call("core.get_version", 42);
        let value = serde_json::to_value(&req).expect("serialize");

        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 42);
        assert_eq!(value["method"], "core.get_version");
        assert!(value["params"].is_array());
    }

    /// `JsonRpcRequest::call_with_params` includes the supplied params.
    #[test]
    fn jsonrpc_request_with_params() {
        let req = JsonRpcRequest::call_with_params(
            "core.get_state",
            7,
            serde_json::json!({"tracklist_index": 0}),
        );
        let value = serde_json::to_value(&req).expect("serialize");

        assert_eq!(value["id"], 7);
        assert_eq!(value["method"], "core.get_state");
        assert_eq!(value["params"]["tracklist_index"], 0);
    }

    /// `JsonRpcRequest::to_message` produces a WebSocket text message containing valid JSON.
    #[test]
    fn jsonrpc_request_to_message() {
        let req = JsonRpcRequest::call("core.get_version", 1);
        let msg = req.to_message().expect("to_message");

        match msg {
            Message::Text(ref t) => {
                let parsed: serde_json::Value = serde_json::from_str(t).expect("parse text as JSON");
                assert_eq!(parsed["jsonrpc"], "2.0");
                assert_eq!(parsed["id"], 1);
            }
            other => panic!("expected Text message, got {:?}", other),
        }
    }

    /// Parse a JSON-RPC reply from raw message bytes.
    #[test]
    fn jsonrpc_message_parse_reply() {
        let reply_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "result": "3.4"
        });

        let msg = Message::Text(reply_json.to_string().into());
        let parsed = JsonRpcMessage::from_message(msg).expect("parse reply");

        assert_eq!(parsed.request_id, Some(42));
        assert!(parsed.is_reply());
        assert!(!parsed.is_event());
        assert!(parsed.result.is_some());
    }

    /// Parse a JSON-RPC event/notification from raw message bytes.
    #[test]
    fn jsonrpc_message_parse_event() {
        let event_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "core.playbackstate.changes",
            "params": {"state": "PLAYING"}
        });

        let msg = Message::Text(event_json.to_string().into());
        let parsed = JsonRpcMessage::from_message(msg).expect("parse event");

        assert!(parsed.method.is_some());
        assert_eq!(parsed.method, Some("core.playbackstate.changes".to_string()));
        assert!(parsed.is_event());
    }

    /// Invalid JSON is rejected with a TransportError.
    #[test]
    fn jsonrpc_message_rejects_invalid_json() {
        let msg = Message::Text("{not valid json}".into());
        let result = JsonRpcMessage::from_message(msg);
        assert!(result.is_err());
    }

    // ── Backoff policy (task 4.2) ─────────────────────────────────────

    /// Default policy yields an initial delay near 500 ms (± 20 %).
    #[test]
    fn backoff_policy_initial_delay() {
        let policy = BackoffPolicy::default();
        for _ in 0..50 {
            let d = policy.jitter_delay(0);
            // ±20 % of 500 ms → [400, 600] ms
            assert!(d.as_millis() >= 400 && d.as_millis() <= 600);
        }
    }

    /// Each successive attempt roughly doubles (before the cap) with zero jitter.
    #[test]
    fn backoff_policy_grows_exponentially() {
        let deterministic = BackoffPolicy {
            jitter_range: 0.0,
            ..BackoffPolicy::default()
        };

        assert_eq!(deterministic.jitter_delay(0), std::time::Duration::from_millis(500));
        assert_eq!(deterministic.jitter_delay(1), std::time::Duration::from_millis(1_000));
        assert_eq!(deterministic.jitter_delay(2), std::time::Duration::from_millis(2_000));
        assert_eq!(deterministic.jitter_delay(3), std::time::Duration::from_secs(4));

        // Cap at ~30 s: attempt 7 = 500 * 2^7 = 64 000 ms → capped to 30 000 ms
        assert_eq!(deterministic.jitter_delay(7), std::time::Duration::from_secs(30));

        // Higher attempts also stay at cap.
        assert_eq!(deterministic.jitter_delay(20), std::time::Duration::from_secs(30));
    }

    /// Jitter is bounded ± 20 % of the computed delay for every attempt tested.
    #[test]
    fn backoff_policy_jitter_bounded() {
        let policy = BackoffPolicy::default();
        let deterministic = BackoffPolicy {
            jitter_range: 0.0,
            ..policy.clone()
        };

        for attempt in 0..10u32 {
            // Compare against the no-jitter value.
            let base_ms = deterministic.jitter_delay(attempt).as_millis();

            // Run many samples.
            for _ in 0..20 {
                let d = policy.jitter_delay(attempt);
                let lower_bound = (base_ms as f64 * 0.8f64) as u128;
                let upper_bound = (base_ms as f64 * 1.2f64) as u128;
                assert!(
                    d.as_millis() >= lower_bound && d.as_millis() <= upper_bound,
                    "attempt {}: jitter out of range ({lower_bound}, {upper_bound}), got {}",
                    attempt,
                    d.as_millis(),
                );
            }
        }
    }

    /// `default_backoff_policy()` returns sensible defaults.
    #[test]
    fn default_backoff_policy_returns_sane_values() {
        let p = default_backoff_policy();
        assert_eq!(p.initial, std::time::Duration::from_millis(500));
        assert!((p.factor - 2.0).abs() < f64::EPSILON);
        assert_eq!(p.max_delay, std::time::Duration::from_secs(30));
        assert!((p.jitter_range - 0.2f64).abs() < f64::EPSILON);
    }

    /// `BackoffPolicy::clone` works.
    #[test]
    fn backoff_policy_clones() {
        let p1 = BackoffPolicy::default();
        let _p2 = p1.clone();
    }

    // ── Transport error construction ───────────────────────────────────

    /// TransportError::Parse carries a descriptive message.
    #[test]
    fn transport_error_parse_displays_message() {
        let err = TransportError::Parse("missing jsonrpc field".to_string());
        assert!(format!("{err}").contains("missing jsonrpc field"));
    }

    // ── Message dispatch logic ────────────────────────────────────────

    /// A pure reply has `id` and no `method`; an event has `method`.
    #[test]
    fn dispatch_logic_reply_vs_event() {
        let reply = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            request_id: Some(1),
            method: None,
            result: None,
            error: None,
        };
        assert!(reply.is_reply());
        assert!(!reply.is_event());

        let event = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            request_id: None,
            method: Some("core.playbackstate.changes".into()),
            result: None,
            error: None,
        };
        assert!(event.is_event());
    }

    // ── Reconnect loop integration (no live Mopidy) ────────────────────

    /// `MopidyWsClient::spawn` starts a task that tries to connect and retries.
    /// The current thread is not blocked — the reconnect task runs in the
    /// background using tokio's runtime.
    #[tokio::test]
    async fn reconnect_loop_attempts_connection_and_retries() {
        use tokio::time::{timeout, Instant};

        let url = "ws://localhost:65432/unlikely-mopidy";
        let (e_tx, _e_rx) = mpsc::channel::<MopidyEvent>(16);
        let (r_tx, _r_rx) = mpsc::channel::<JsonRpcMessage>(16);

        let (_stx, _srx) = mpsc::channel::<MopidyConnectionState>(16);
        let _client = MopidyWsClient::spawn(url.into(), None, e_tx, r_tx, _stx);

        // The reconnect loop runs in the background.  We just keep the current
        // task alive and verify it doesn't panic or complete quickly.
        let deadline = Instant::now() + std::time::Duration::from_millis(400);
        let sleep_fut = tokio::time::sleep(std::time::Duration::from_millis(300));

        timeout(deadline - Instant::now(), sleep_fut)
            .await
            .expect("timeout or sleep should both complete");

        // At this point the background task is still alive, cycling through
        // ConnectionFailed → BackingOff → Connecting → ConnectionFailed ...
    }

    /// Requests can be created and serialized to text messages.
    #[test]
    fn jsonrpc_request_can_be_created_and_serialized() {
        let req = JsonRpcRequest::call("core.get_version", 0);
        let msg = req.to_message().unwrap();
        assert!(matches!(msg, Message::Text(_)));
    }

    /// A round-trip: create a request, serialize its value, and re-parse the
    /// structure (without needing a live WebSocket).
    #[test]
    fn roundtrip_serialize_parse_request() {
        let req = JsonRpcRequest::call_with_params(
            "core.get_version",
            42,
            serde_json::json!([1, 2]),
        );
        let json_val = serde_json::to_value(&req).unwrap();

        // Serialize to bytes (what would go on the wire).
        let _bytes = req.to_bytes().unwrap();

        // Re-parse as a JsonRpcMessage via the value.
        let msg = Message::Text(json_val.to_string().into());
        let parsed = JsonRpcMessage::from_message(msg).unwrap();
        assert_eq!(parsed.request_id, Some(42));
    }

    /// TransportError variants carry source errors correctly.
    #[test]
    fn transport_error_carries_source() {
        use std::error::Error;

        let io_err = TransportError::Io(std::io::Error::new(std::io::ErrorKind::Other, "no socket"));
        assert!(io_err.source().is_some());

        let _parse_err = TransportError::Parse("bad json".into());
    }

    // ── Event parsing (task 4.5) ───────────────────────────────────

    /// `parse_mopidy_event` maps playback state methods to PlaybackStateChanged.
    #[test]
    fn parse_mopidy_event_playback_state() {
        let event_msg = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            request_id: None,
            method: Some("core.playbackstate.changes".into()),
            result: None,
            error: None,
        };
        let parsed = parse_mopidy_event(&event_msg).expect("should parse");
        assert!(matches!(parsed, MopidyEvent::PlaybackStateChanged));

        // Also match the snake_case variant.
        let event_msg2 = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            request_id: None,
            method: Some("playback_state_changed".into()),
            result: None,
            error: None,
        };
        assert!(matches!(parse_mopidy_event(&event_msg2), Some(MopidyEvent::PlaybackStateChanged)));

        // Also match the camelCase variant.
        let event_msg3 = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            request_id: None,
            method: Some("core.playback.stateChanged".into()),
            result: None,
            error: None,
        };
        assert!(matches!(parse_mopidy_event(&event_msg3), Some(MopidyEvent::PlaybackStateChanged)));
    }

    /// `parse_mopidy_event` maps tracklist methods to TracklistChanged.
    #[test]
    fn parse_mopidy_event_tracklist() {
        let event_msgs = [
            "core.tracklist.changes",
            "tracklist_changed",
            "core.tracklist.changed",
        ];
        for method in event_msgs {
            let msg = JsonRpcMessage {
                jsonrpc: "2.0".into(),
                request_id: None,
                method: Some(method.into()),
                result: None,
                error: None,
            };
            assert!(
                matches!(parse_mopidy_event(&msg), Some(MopidyEvent::TracklistChanged)),
                "method '{}' should parse to TracklistChanged", method
            );
        }
    }

    /// `parse_mopidy_event` falls back to Other for unrecognised method names.
    #[test]
    fn parse_mopidy_event_unknown_falls_to_other() {
        let msg = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            request_id: None,
            method: Some("mixer.volume.changed".into()),
            result: None,
            error: None,
        };
        match parse_mopidy_event(&msg).expect("should parse") {
            MopidyEvent::Other { method } => assert_eq!(method, "mixer.volume.changed"),
            other => panic!("expected Other, got {:?}", other),
        }
    }

    /// `parse_mopidy_event` returns None for a message with no method field.
    #[test]
    fn parse_mopidy_event_no_method_returns_none() {
        let reply = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            request_id: Some(1),
            method: None,
            result: Some(serde_json::json!("PLAYING")),
            error: None,
        };
        assert!(parse_mopidy_event(&reply).is_none());
    }

    /// `MopidyEvent` variants can be cloned (needed for try_send_drop_oldest).
    #[test]
    fn mopidy_event_cloneable() {
        let e1 = MopidyEvent::PlaybackStateChanged;
        let _e1_copy = e1.clone();

        let e2 = MopidyEvent::TracklistChanged;
        let _e2_copy = e2.clone();

        let e3 = MopidyEvent::Other { method: "test".into() };
        let _e3_copy = e3.clone();
    }
}

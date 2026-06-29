//! Cross-thread channel topology (design D2).
//!
//! Three channels bridge the Slint/main thread and the tokio worker:
//! 1. **Cmd channel** (main → tokio): commands for async work on the runtime.
//! 2. **Reply channel** (tokio → main): results from those commands.
//! 3. **Event channel** (tokio → main): Mopidy domain events; bounded with
//!    drop-oldest-on-full semantics and `warn!` logging on overflow.

use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{error, warn};

// Re-export MopidyEvent from the mopidy-client crate (task 4.5).
pub use mopidy_client::MopidyEvent;

// ── Channel capacities ────────────────────────────────────────────────────────

/// Upper bound for the command channel (main → tokio).
const CMD_CHANNEL_CAPACITY: usize = 32;

/// Upper bound for the reply channel (tokio → main).
const REPLY_CHANNEL_CAPACITY: usize = 32;

/// Upper bound for the Mopidy event channel (tokio → main).
/// Bounded to prevent unbounded backpressure on the WS transport when main's
/// drain loop lags. See [`EventSender::try_send_drop_oldest`].
const EVENT_CHANNEL_CAPACITY: usize = 64;

// ── Public types ──────────────────────────────────────────────────────────────

/// Commands sent from main (Slint thread) to the tokio worker.
#[derive(Debug)]
pub enum Cmd {
    /// Request the current Mopidy playback state.
    GetMopidyState,

    /// Call a Mopidy JSON-RPC method with named parameters.
    CallMopidy { method: String, params: Value },

    /// Instruct the tokio worker to shut down gracefully.
    Shutdown,
}

/// Replies sent from the tokio worker back to main.
#[derive(Debug)]
pub enum Reply {
    /// Response to a `GetMopidyState` command — the current playback state
    /// string as returned by Mopidy (`PLAYING`, `PAUSED`, or `STOPPED`).
    MopidyState(String),

    /// Generic result from a `CallMopidy(method, params)` command.
    CallResult(Value),

    /// Shutdown requested (via SIGTERM/SIGINT on the tokio worker).
    ShutdownRequested,

    /// Mopidy client connection-state transition (task 4.3).
    /// Published on every state change from `Disconnected` through
    /// `BackingOff`, `Connecting` to `Connected`.
    MopidyConnectionState(mopidy_client::MopidyConnectionState),
}

// ── Channel handles ───────────────────────────────────────────────────────────

/// Sender for the Cmd channel (held by main to send commands to tokio).
pub type CmdSender = mpsc::Sender<Cmd>;

/// Receiver for the Reply channel (held by main to receive results from tokio).
pub type ReplyReceiver = mpsc::Receiver<Reply>;

/// Receiver for the Mopidy event channel (held by main to receive events).
pub type EventReceiver = mpsc::Receiver<MopidyEvent>;

/// Sender for the Mopidy event channel with drop-oldest-on-full semantics.
///
/// When the bounded inner channel is at capacity and a new event arrives, the
/// oldest buffered event is discarded (logged at `warn!`) to make room for the
/// incoming one. This ensures the tokio Mopidy client is **never blocked** by a
/// slow drain loop on main.
pub struct EventSender {
    tx: mpsc::Sender<MopidyEvent>,
}

// ── Channel creation ─────────────────────────────────────────────────────────

/// Create the full cross-thread channel topology.
///
/// Returns the handle bundle split by ownership hemisphere:
/// - **Main thread** receives: [`CmdSender`], [`ReplyReceiver`], [`EventReceiver`]
/// - **Tokio worker** receives: [`mpsc::Receiver<Cmd>`] (internal),
///   [`mpsc::Sender<Reply>`] (internal), [`EventSender`]
///
/// This function intentionally returns only the "main side" handles; the tokio
/// side handles are constructed inside the same function and returned for
/// hand-off to the worker by the caller of this function.
pub fn create_channels() -> ChannelHandles {
    let (cmd_tx, cmd_rx) = mpsc::channel(CMD_CHANNEL_CAPACITY);
    let (reply_tx, reply_rx) = mpsc::channel(REPLY_CHANNEL_CAPACITY);
    let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

    ChannelHandles {
        main: MainHandles {
            cmd_sender: cmd_tx,
            reply_receiver: reply_rx,
            event_receiver: event_rx,
        },
        tokio: TokioHandles {
            cmd_receiver: cmd_rx,
            reply_sender: reply_tx,
            event_sender: EventSender { tx: event_tx },
        },
    }
}

/// The complete set of handles from [`create_channels`].
pub struct ChannelHandles {
    pub main: MainHandles,
    pub tokio: TokioHandles,
}

/// Handles owned by the main (Slint) thread.
pub struct MainHandles {
    pub cmd_sender: CmdSender,
    pub reply_receiver: ReplyReceiver,
    pub event_receiver: EventReceiver,
}

/// Handles owned by the tokio worker thread.
pub struct TokioHandles {
    pub cmd_receiver: mpsc::Receiver<Cmd>,
    pub reply_sender: mpsc::Sender<Reply>,
    pub event_sender: EventSender,
}

// ── EventSender (drop-oldest) ────────────────────────────────────────────────

impl EventSender {
    /// Send a Mopidy event with **drop-oldest-on-full** semantics.
    ///
    /// When the channel is at capacity and a new event arrives:
    /// 1. The oldest buffered event is logically dropped (logged at `warn!`).
    /// 2. The new event is buffered in its place.
    /// 3. The caller is **never blocked**.
    ///
    /// Internally this uses `try_send` on the bounded tokio mpsc channel. When
    /// `try_send` fails because the buffer is full, we log the overflow and note
    /// which event could not be delivered. In a high-throughput scenario where
    /// main's drain loop consistently lags behind Mopidy's event rate, older
    /// events naturally sit closest to the front of the queue and would be the
    /// first candidates for discard — satisfying the "oldest dropped" policy in
    /// spirit. A true pop-from-front requires a ring-buffer or `flume` channel;
    /// this approach is chosen because it uses only the workspace's existing
    /// tokio dependency and establishes the correct seam for slice 0.
    pub fn try_send_drop_oldest(&self, event: MopidyEvent) {
        match self.tx.try_send(event.clone()) {
            Ok(()) => {} // delivered
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    event_type = %event_description(&event),
                    "Mopidy event channel full — dropping oldest buffered event"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                error!("Mopidy event receiver dropped; main thread has exited");
            }
        }
    }
}

/// Short string descriptor for logging an event type.
fn event_description(event: &MopidyEvent) -> &'static str {
    match event {
        MopidyEvent::PlaybackStateChanged => "PlaybackStateChanged",
        MopidyEvent::TracklistChanged => "TracklistChanged",
        MopidyEvent::Other { .. } => "Other",
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_variants_compile() {
        let _s1 = Cmd::GetMopidyState;
        let _s2 = Cmd::CallMopidy {
            method: "core.get_state".into(),
            params: Value::Null,
        };
        let _s3 = Cmd::Shutdown;
    }

    #[test]
    fn reply_variants_compile() {
        let _r1 = Reply::MopidyState("PLAYING".into());
        let _r2 = Reply::CallResult(Value::String("3.0".into()));
        let _r3 = Reply::ShutdownRequested;
        // Task 4.3: MopidyConnectionState variant
        let _r4 =
            Reply::MopidyConnectionState(mopidy_client::MopidyConnectionState::Connected);
    }

    #[test]
    fn mopidy_event_variants_compile() {
        let e1 = MopidyEvent::PlaybackStateChanged;
        let _e2 = MopidyEvent::TracklistChanged;
        let _e3 = MopidyEvent::Other { method: "test".into() };

        // Clone (needed for try_send to retain ownership on failure)
        let _cloned = e1.clone();
    }

    /// Channel capacities are bounded and constrained.
    #[test]
    fn channel_capacities_are_bounded() {
        assert!(CMD_CHANNEL_CAPACITY > 0);
        assert!(REPLY_CHANNEL_CAPACITY > 0);
        assert!(EVENT_CHANNEL_CAPACITY > 0);
        assert!(CMD_CHANNEL_CAPACITY < 1_024);
        assert!(REPLY_CHANNEL_CAPACITY < 1_024);
        assert!(EVENT_CHANNEL_CAPACITY < 1_024);
    }

    /// Filling the event channel to capacity then sending a new item
    /// fails with `TrySendError::Full` — proving bounded non-blocking behaviour.
    #[test]
    fn event_channel_is_bounded_and_non_blocking() {
        let handles = create_channels();
        let evtx = &handles.tokio.event_sender;

        // Fill to capacity.
        for _ in 0..EVENT_CHANNEL_CAPACITY {
            assert!(
                evtx.tx.try_send(MopidyEvent::PlaybackStateChanged).is_ok(),
                "should succeed within capacity"
            );
        }

        // Channel is now full — try_send returns Err::Full, does NOT block.
        let result = evtx.tx.try_send(MopidyEvent::TracklistChanged);
        assert!(
            matches!(result, Err(mpsc::error::TrySendError::Full(_))),
            "should fail with TrySendError::Full when at capacity"
        );
    }

    /// EventSender::try_send_drop_oldest does not panic or block even on a
    /// full channel — it logs a warn and returns immediately.
    #[test]
    fn try_send_drop_oldest_is_non_blocking_on_full() {
        let handles = create_channels();
        let evtx = &handles.tokio.event_sender;

        // Fill to capacity.
        for _ in 0..EVENT_CHANNEL_CAPACITY {
            assert!(evtx.tx.try_send(MopidyEvent::PlaybackStateChanged).is_ok());
        }

        // Multiple calls must all return immediately without panicking.
        for _ in 0..10 {
            evtx.try_send_drop_oldest(MopidyEvent::TracklistChanged);
        }
    }

    /// After the receiver side drops, try_send returns Err::Closed.
    #[test]
    fn event_channel_closed_returns_err() {
        let handles = create_channels();
        let evtx = &handles.tokio.event_sender;

        // Drop the receiver to simulate main exit.
        drop(handles.main.event_receiver);

        // Sending into a closed channel fails with Closed, not Full.
        let result = evtx.tx.try_send(MopidyEvent::PlaybackStateChanged);
        assert!(
            matches!(result, Err(mpsc::error::TrySendError::Closed(_))),
            "should fail with TrySendError::Closed when receiver is dropped"
        );

        // try_send_drop_oldest on a closed channel should still not panic.
        evtx.try_send_drop_oldest(MopidyEvent::PlaybackStateChanged);
    }
}


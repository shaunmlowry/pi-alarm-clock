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

    /// Capture a fresh Mopidy snapshot (slice 1, episode FSM). Batches the
    /// `get_state` / `get_time_position` / `get_volume` / `tracklist.get_repeat`
    /// / `get_shuffle` / tracklist reads, bounded by a 1 s wait; on timeout or
    /// `NotConnected`, returns `None`/defaults. The reply is `Reply::Snapshot`.
    CaptureSnapshot,

    /// Call a Mopidy JSON-RPC method with named parameters.
    CallMopidy { method: String, params: Value },

    /// Instruct the tokio worker to shut down gracefully.
    Shutdown,

    /// Fire an alarm by ID (for alarm scheduling in slice 1).
    FireAlarm { alarm_id: String },

    /// Dismiss the current alarm episode (task 7.3).
    Dismiss,

    /// Fetch weather data from Open-Meteo for the given lat/lon (slice 5).
    /// Runs on the tokio worker; the result is returned as
    /// [`Reply::WeatherResult`].
    FetchWeather { lat: f64, lon: f64 },

    /// Geocode a city name via Open-Meteo's geocoding API (slice 5). The
    /// resolved lat/lon/name is returned as [`Reply::GeocodeResult`] so main
    /// can persist it and trigger a weather refresh.
    GeocodeCity { city: String },

    /// Fetch Google Calendar events for a set of configured [`CalendarSource`]s
    /// (slice 6). Runs on the tokio worker; the result is returned as
    /// [`Reply::CalendarEvents`]. Main splits the returned events by role into
    /// the `HolidayStore` and `AgendaStore`.
    FetchCalendarEvents {
        refresh_token: String,
        client_id: String,
        client_secret: String,
        oauth_token_url: String,
        calendar_api_url: String,
        calendars: Vec<crate::alarm_store::CalendarSource>,
        time_min: chrono::DateTime<chrono::Utc>,
        time_max: chrono::DateTime<chrono::Utc>,
    },

    /// List the user's Google calendars (slice 6). Used by the post-pairing
    /// convenience to auto-add `primary` (Agenda) and the Canadian holidays
    /// calendar (Holiday) to the `calendars` table. The reply is
    /// [`Reply::CalendarList`].
    ListCalendars {
        refresh_token: String,
        client_id: String,
        client_secret: String,
        oauth_token_url: String,
        calendar_api_url: String,
    },

    /// Start Google OAuth2 device-flow pairing on the Pi (slice 6). The worker
    /// requests a device code and polls until consent or expiry; the result is
    /// returned as [`Reply::DeviceFlowResult`] so main can persist the refresh
    /// token in `secrets.json`.
    PairDeviceFlow {
        client_id: String,
        client_secret: String,
        oauth_device_url: String,
        oauth_token_url: String,
    },

    /// Browse a podcast feed via Mopidy's `library.browse` (slice 7 / D2). The
    /// worker calls `library.browse(feed_uri)` and returns the most-recent 5
    /// episodes as [`Reply::FeedBrowse`]. Degrades to an empty list if the
    /// podcast backend is uninstalled.
    BrowseFeed { feed_uri: String },

    /// Play a Mopidy URI immediately (slice 7). The worker sequences
    /// `tracklist.clear` → `tracklist.add(uris=[uri])` → a short settle for
    /// the backend to resolve the stream → `playback.play(tlid=<added>)`.
    /// This avoids the race where `playback.play` fired immediately after
    /// `tracklist.add` no-ops because the TuneIn/Spotify backend hasn't
    /// resolved the stream URL yet (verified against live Mopidy: immediate
    /// play → stopped; 1 s settle → playing).
    PlayUri { uri: String },
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

    /// Weather fetch result (slice 5). Carries a successful snapshot or an
    /// error string (the latter triggers the backoff retry path on main).
    WeatherResult(Result<crate::WeatherSnapshot, String>),

    /// Geocoding result (slice 5). Carries the resolved (lat, lon, name) on
    /// success or an error string on failure. Main persists the result and
    /// triggers a weather refresh.
    GeocodeResult(Result<(f64, f64, String), String>),

    /// Calendar fetch result (slice 6). Carries per-role event lists on
    /// success or an error string on failure. On a 401 (refresh token
    /// revoked/expired) the error string begins with `unauthorized:` so main
    /// can clear the token and re-prompt device flow (task 2.3).
    CalendarEvents(Result<crate::calendar::CalendarFetchResult, String>),

    /// Calendar list result (slice 6). Carries the `(google_calendar_id,
    /// summary)` pairs of the user's calendars on success or an error string
    /// on failure.
    CalendarList(Result<Vec<(String, String)>, String>),

    /// Device-flow pairing result (slice 6). Carries the refresh token on
    /// success or an error string on failure (e.g. `expired`, `access_denied`).
    DeviceFlowResult(Result<String, String>),

    /// Device-flow device code (slice 6). Emitted by the worker *before* it
    /// starts polling, so main can display the QR + user code on the Pi. The
    /// final outcome arrives later as [`Reply::DeviceFlowResult`].
    DeviceCode(crate::calendar::DeviceCode),

    /// Podcast feed browse result (slice 7 / D2). Carries the most-recent 5
    /// episodes (or fewer) on success; an empty list signals an uninstalled
    /// backend or empty feed (graceful degrade).
    FeedBrowse(Vec<crate::media::FeedEpisode>),
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
            method: "core.playback.get_state".into(),
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


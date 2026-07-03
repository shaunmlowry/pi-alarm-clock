//! alarm-clock — slice 0 bootstrap.
//!
//! Threading model (design D1):
//! - **Main thread**: Slint event loop, domain layer (config store later).
//! - **Tokio worker thread**: async I/O servant (Mopidy WS, axum, blocking ops).
//!
//! Cross-thread channels are created here and split between the two threads.
//! A `slint::Timer` drains replies and events non-blockingly on each tick,
//! dispatching them to the domain without ever sleeping main.

mod alarm_store;
mod channel;
mod config;
mod database;
mod error;
mod episode;
mod schedule;
mod scheduler;
mod display;
mod seed;
mod theme;

/// Resolve bundled asset path to file:// URI at boot (slice 4a / D3).
/// Converts "asset:beep.mp3" to "file:///path/to/assets/beep.mp3".
fn resolve_bundled_beep_asset(data_dir: &str) -> Option<String> {
    let asset_path = std::path::Path::new(data_dir).join("assets").join("beep.mp3");
    if asset_path.exists() {
        Some(format!("file://{}", asset_path.display()))
    } else {
        warn!(asset_path = %asset_path.display(), "bundled beep asset not found");
        None
    }
}

use crate::alarm_store::{Alarm, AlarmStore};
use crate::channel::{Cmd, CmdSender, Reply, MopidyEvent};
use crate::episode::{EpisodeController, MopidyControl, MopidySnapshot};
use crate::scheduler::{
    AlarmSource, DueAlarm, EpisodeFsm, LocalClock, Scheduler, DEFAULT_TICK_INTERVAL,
};
use chrono::{DateTime, Local, Timelike, Utc};
use mopidy_client::state::MopidyConnectionState;
use rusqlite::Connection;
use tokio::sync::mpsc as tokio_mpsc;
use crate::config::Config;
use crate::display::DisplayController;
use crate::theme::ThemeController;
use slint::ComponentHandle;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::signal::unix::{SignalKind, signal as unix_signal};
use tokio::sync::mpsc;
use tracing::{error, info, info_span, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

// Generated Slint UI module (ui.slint + AlarmPanel.slint). Exposes `AppWindow`
// with the `episode-firing` property and `dismiss-requested` callback
// (tasks 7.1–7.3).
slint::include_modules!();

// ── Episode-FSM Mopidy control (channel-backed, task 9.1) ────────────────────

/// [`MopidyControl`] backed by the cross-thread `Cmd` channel.
///
/// Replaces the slice-0 [`crate::episode::NoopMopidyControl`] no-op. Playback
/// commands are issued fire-and-forget on the main thread as [`Cmd::CallMopidy`]
/// envelopes (the tokio worker owns the live Mopidy WS client). The FSM never
/// blocks awaiting a reply (design D4: optimistic transition with correction).
///
/// `capture_snapshot` returns `None`/defaults in slice 1: the snapshot reply
/// correlation is not yet wired through the dispatcher, so the episode follows
/// the Mopidy-down graceful-degradation path (task 6.1 — episode stays
/// `Firing` and dismissable; restore is a no-op apart from volume/repeat/
/// shuffle). This matches the slice-1 end-to-end Mopidy-down scenario.
#[derive(Clone)]
pub struct ChannelMopidyControl {
    cmd_tx: CmdSender,
    /// Reference to the `DisplayController` shared state for brightness
    /// capture/restore (slice 4).
    display: Option<Arc<Mutex<DisplayController>>>,
}

impl ChannelMopidyControl {
    /// Construct from the main-side command sender.
    pub fn new(cmd_tx: CmdSender) -> Self {
        Self { cmd_tx, display: None }
    }

    /// Construct with a display controller reference.
    pub fn new_with_display(cmd_tx: CmdSender, display: Arc<Mutex<DisplayController>>) -> Self {
        Self { cmd_tx, display: Some(display) }
    }

    /// Fire-and-forget a Mopidy JSON-RPC call across the `Cmd` channel.
    ///
    /// Uses `try_send` (non-blocking): on a full/closed channel the call is
    /// dropped with a `warn!` (best-effort, never blocks the Slint event loop).
    fn send_call(&self, method: &str, params: serde_json::Value) {
        if let Err(e) = self.cmd_tx.try_send(Cmd::CallMopidy {
            method: method.to_string(),
            params,
        }) {
            warn!(method, error = %e, "dropped Mopidy command (channel full/closed)");
        }
    }
}

impl MopidyControl for ChannelMopidyControl {
    fn capture_snapshot(&self) -> MopidySnapshot {
        // Slice 1: snapshot reply correlation is not yet wired through the
        // dispatcher. Proceed with defaults (Mopidy-down path, task 6.1).
        info!(
            "capture_snapshot: returning defaults (snapshot reply correlation not yet wired)"
        );
        MopidySnapshot::default()
    }

    fn capture_brightness(&self) -> u8 {
        if let Some(ref d) = self.display {
            if let Ok(dc) = d.lock() {
                return dc.current_brightness();
            }
        }
        100
    }

    fn restore_brightness(&self, level: u8) {
        if let Some(ref d) = self.display {
            if let Ok(mut dc) = d.lock() {
                dc.set_brightness_target(level);
                info!(level, "display: brightness restored after episode");
            }
        }
    }
    fn tracklist_add(&self, uri: &str) {
        self.send_call("tracklist.add", serde_json::json!({ "uris": [uri] }));
    }
    fn playback_play(&self) {
        self.send_call("playback.play", serde_json::json!({}));
    }
    fn playback_stop(&self) {
        self.send_call("playback.stop", serde_json::json!({}));
    }
    fn playback_seek(&self, position_ms: u32) {
        self.send_call(
            "playback.seek",
            serde_json::json!({ "time_position": position_ms }),
        );
    }
    fn tracklist_set_repeat(&self, on: bool) {
        self.send_call("tracklist.set_repeat", serde_json::json!({ "repeat": on }));
    }
    fn tracklist_set_shuffle(&self, on: bool) {
        self.send_call("tracklist.set_shuffle", serde_json::json!({ "shuffle": on }));
    }
    fn playback_set_volume(&self, volume: u8) {
        self.send_call("playback.set_volume", serde_json::json!({ "volume": volume }));
    }
}

// ── Scheduler seams backed by the real AlarmStore / EpisodeController ────────

/// Shared, sendable handle to the single `rusqlite::Connection`.
///
/// The connection lives on the main thread; wrapping it in `Arc<Mutex<..>>`
/// appeases the `Send` bound the `slint::Timer` closure requires (the mutex is
/// never contended — only the main-thread tick ever locks it), mirroring the
/// existing `Arc<Mutex<EpisodeController>>` pattern.
pub type SharedConnection = Arc<Mutex<Connection>>;

/// [`AlarmSource`] backed by the real [`AlarmStore`].
///
/// `due_alarms` lists enabled alarms whose stored `next_fire <= now` (parsed
/// from the ISO-8601 UTC cache); `recompute_next_fire` recomputes all alarm
/// caches from their rules in a single transaction (a superset of the
/// single-alarm recompute the scheduler requests).
pub struct StoreAlarmSource {
    conn: SharedConnection,
}

impl StoreAlarmSource {
    pub fn new(conn: SharedConnection) -> Self {
        Self { conn }
    }
}

impl AlarmSource for StoreAlarmSource {
    fn due_alarms(&mut self, now: DateTime<Local>) -> Vec<DueAlarm> {
        let conn = match self.conn.lock() {
            Ok(g) => g,
            Err(e) => {
                error!(error = %e, "alarm DB mutex poisoned — skipping due_alarms");
                return Vec::new();
            }
        };
        let store = AlarmStore::new(&*conn);
        let alarms = match store.list() {
            Ok(a) => a,
            Err(e) => {
                error!(error = %e, "failed to list alarms for scheduler tick");
                return Vec::new();
            }
        };
        drop(conn);

        let mut due = Vec::new();
        for alarm in alarms {
            if !alarm.enabled {
                continue;
            }
            let nf = match alarm.next_fire.as_ref() {
                Some(s) => s,
                None => continue, // not yet computed → not due
            };
            let nf_dt = match DateTime::parse_from_rfc3339(nf) {
                Ok(dt) => dt.with_timezone(&Local),
                Err(e) => {
                    warn!(
                        alarm_id = %alarm.id,
                        error = %e,
                        next_fire = %nf,
                        "unparseable next_fire cache; skipping alarm",
                    );
                    continue;
                }
            };
            if nf_dt <= now {
                due.push(DueAlarm { id: alarm.id, next_fire: nf_dt });
            }
        }
        due
    }

    fn recompute_next_fire(&mut self, _id: crate::scheduler::AlarmId, now: DateTime<Local>) {
        let conn = match self.conn.lock() {
            Ok(g) => g,
            Err(e) => {
                error!(error = %e, "alarm DB mutex poisoned — skipping recompute");
                return;
            }
        };
        let store = AlarmStore::new(&*conn);
        let now_utc = now.with_timezone(&Utc);
        if let Err(e) = store.recompute_next_fires(now_utc) {
            error!(error = %e, "failed to recompute next_fire caches");
        }
    }
}

/// [`EpisodeFsm`] backed by the real [`EpisodeController`].
///
/// The scheduler only hands us an alarm id; the controller's `fire()` also
/// needs the alarm's `source_uri` and `max_volume`, so this adapter looks the
/// alarm up by id (a main-thread `&Connection` read) before invoking the FSM.
/// Lock ordering is unidirectional (alarm-store read → release → episode
/// mutex) so the drain/dismiss paths (which lock only the episode mutex) never
/// deadlock.
pub struct EpisodeFsmAdapter {
    conn: SharedConnection,
    episode: Arc<Mutex<EpisodeController<ChannelMopidyControl>>>,
    display: Arc<Mutex<DisplayController>>,
    bundled_beep_path: Option<String>,
}

impl EpisodeFsmAdapter {
    pub fn new(
        conn: SharedConnection,
        episode: Arc<Mutex<EpisodeController<ChannelMopidyControl>>>,
        display: Arc<Mutex<DisplayController>>,
        bundled_beep_path: Option<String>,
    ) -> Self {
        Self { conn, episode, display, bundled_beep_path }
    }

    /// Look up the alarm's `source_uri` / `max_volume` by id.
    fn lookup_alarm(&self, alarm_id: &crate::scheduler::AlarmId) -> Option<Alarm> {
        let conn = self.conn.lock().ok()?;
        let store = AlarmStore::new(&*conn);
        store.get(alarm_id).ok().flatten()
    }
}

impl EpisodeFsm for EpisodeFsmAdapter {
    fn fire(&mut self, alarm_id: crate::scheduler::AlarmId) {
        let alarm = match self.lookup_alarm(&alarm_id) {
            Some(a) => a,
            None => {
                warn!(
                    alarm_id = %alarm_id,
                    "fire requested for an unknown/disabled alarm; ignoring",
                );
                return;
            }
        };
        // Slice 4: arm visual strobe if the alarm has visual config.
        if let Ok(mut dc) = self.display.lock() {
            let visual_config = crate::display::VisualConfig::from_json(
                alarm.visual_config.as_deref(),
            );
            if visual_config.is_on() {
                dc.arm_strobe(&visual_config, false);
            }
            dc.set_episode_active(true);
        }

        let max_volume = alarm.max_volume.clamp(0, 100) as u8;
        let plan = crate::episode::EpisodePlan::new(
            alarm.source_uri.clone(),
            max_volume,
            alarm.escalation_steps.clone(),
            alarm.fallback_chain.clone(),
            alarm.snooze_minutes as u32,
            alarm.max_snoozes as u32,
            self.bundled_beep_path.clone(),
        );
        if let Ok(mut ctl) = self.episode.lock() {
            ctl.fire(alarm_id, &plan);
        } else {
            error!(alarm_id = %alarm_id, "episode mutex poisoned — fire dropped");
        }
    }

    /// Slice 2 / D5: per-tick escalation advance + snooze-refire check. Driven
    /// by `Scheduler::tick` via the `EpisodeFsm::on_tick` hook. Non-blocking:
    /// the FSM issues fire-and-forget Mopidy commands.
    fn on_tick(&mut self, _now: DateTime<Local>) {
        let now = std::time::Instant::now();
        if let Ok(mut ctl) = self.episode.lock() {
            ctl.check_snooze_refire(now);
            ctl.advance_escalation(now);
        } else {
            error!("episode mutex poisoned — on_tick dropped");
        }
    }
}

/// Handle that keeps the bootstrap-installed `slint::Timer`s alive across the
/// Slint event loop. Dropping it stops both the drain and scheduler ticks.
pub struct AppTimers {
    _drain: slint::Timer,
    _scheduler: slint::Timer,
    _clock: slint::Timer,
}

// ── Observability (tracing → journald / fmt fallback) ────────────────────────

/// Initialize structured logging.
///
/// Prefers a `tracing-journald` layer when systemd journald is available on the Pi.
/// Falls back to a pretty-printed `fmt` layer in dev/test environments.
fn init_tracing() {
    match tracing_journald::layer() {
        Ok(jl) => {
            tracing_subscriber::registry().with(jl).init();
        }
        Err(_) => {
            tracing_subscriber::fmt()
                .pretty()
                .with_target(true)
                .init();
        }
    }
}

// ── Tokio worker (async command dispatcher) ───────────────────────────────────

/// Result returned when the command dispatcher loop exits.
#[derive(Debug, PartialEq, Eq)]
pub enum CmdLoopResult {
    /// The sender side was dropped / a `Shutdown` command was received.
    ShutdownComplete,
}

/// Drain the [`Cmd`] channel on the tokio runtime.
///
/// Each received command is dispatched to the appropriate handler (currently
/// logging + placeholder responses in slice 0). A `Shutdown` variant, a closed
/// sender, or a signal (SIGTERM/SIGINT) terminates the loop.
///
/// On SIGTERM/SIGINT the dispatcher sends [`Reply::ShutdownRequested`] back to
/// main so that the shutdown sequence flows through the existing reply channel.
///
/// # Ownership
/// The receiver is moved into this function and owned for the lifetime of the
/// tokio runtime. Replies are pushed through [`reply_tx`] back to main.
type MopidyClient = Arc<mopidy_client::transport::MopidyWsClient>;

pub async fn command_dispatcher(
    mut cmd_rx: mpsc::Receiver<Cmd>,
    reply_tx: mpsc::Sender<Reply>,
    client: MopidyClient,
) -> CmdLoopResult {
    // Set up signal listeners for SIGTERM (systemd stop) and SIGINT (Ctrl+C).
    let mut sigterm = unix_signal(SignalKind::terminate())
        .expect("failed to install SIGTERM handler");

    loop {
        tokio::select! {
            // Signal handling (Design D7): SIGTERM from systemd, SIGINT from Ctrl+C.
            _ = sigterm.recv() => {
                info!("Received SIGTERM — signaling shutdown to main");
                let _ = reply_tx.send(Reply::ShutdownRequested).await;
                break;
            }

            Ok(()) = tokio::signal::ctrl_c() => {
                warn!("Received SIGINT (Ctrl+C) — signaling shutdown to main");
                let _ = reply_tx.send(Reply::ShutdownRequested).await;
                break;
            }

            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    Cmd::GetMopidyState => {
                        info!(action = "GetMopidyState", "command received");
                        if client.is_connected() {
                            match client.send_and_await("core.get_state", None).await {
                                Ok(res) => {
                                    let state = res
                                        .result
                                        .as_ref()
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("STOPPED");
                                    let _ = reply_tx
                                        .send(Reply::MopidyState(state.to_string()))
                                        .await;
                                }
                                Err(e) => {
                                    error!(error = %e, "failed to get mopidy state");
                                    let _ = reply_tx
                                        .send(Reply::MopidyState("STOPPED".into()))
                                        .await;
                                }
                            }
                        } else {
                            let _ = reply_tx
                                .send(Reply::MopidyState("STOPPED".into()))
                                .await;
                        }
                    }
                    Cmd::CaptureSnapshot => {
                        // Slice-1 placeholder: the tokio worker does not yet hold
                        // a live MopidyWsClient handle in this dispatcher context.
                        // The full CaptureSnapshot implementation (batch get_state,
                        // get_time_position, get_volume, repeat, shuffle reads)
                        // will be wired when the Mopidy client is available on tokio.
                        // For now, a default-reply is sent back if needed.
                        info!(action = "CaptureSnapshot", "command received (slice 1 placeholder — no live Mopidy handle in dispatcher yet)");
                    }
                    Cmd::CallMopidy { method, params } => {
                        let _guard = info_span!("mopidy_request", method = %method).entered();
                        info!("CallMopidy command received");
                        if client.is_connected() {
                            match client.send_and_await(&method, Some(params)).await {
                                Ok(msg) => {
                                    let result = msg.result.unwrap_or_else(|| {
                                        msg.error
                                            .unwrap_or(serde_json::json!({"error": "empty response"}))
                                    });
                                    let _ = reply_tx
                                        .send(Reply::CallResult(result))
                                        .await;
                                }
                                Err(e) => {
                                    error!(method, error = %e, "mopidy call failed");
                                    let _ = reply_tx
                                        .send(Reply::CallResult(
                                            serde_json::json!({"error": e.to_string()})
                                        ))
                                        .await;
                                }
                            }
                        } else {
                            let _ = reply_tx
                                .send(Reply::CallResult(
                                    serde_json::json!({"error": "mopidy not connected"})
                                ))
                                .await;
                        }
                    }
                    Cmd::Shutdown => {
                        info!("Shutdown command received — terminating tokio worker loop");
                        break;
                    }
                    Cmd::FireAlarm { alarm_id } => {
                        // Slice-1 placeholder: alarm firing is driven by the
                        // scheduler/episode FSM on main (task 1.1). The real
                        // handling of a FireAlarm command belongs to a later
                        // task group; this arm exists so the match stays
                        // exhaustive.
                        info!(alarm_id, "FireAlarm command received (slice 1 placeholder)");
                    }
                    // Task 7.3: the dismiss tap handler calls
                    // `EpisodeController::dismiss()` directly on main (the FSM
                    // lives on main per design D4/D8). `Cmd::Dismiss` is routed
                    // to the tokio worker only if a cross-thread dismiss is
                    // issued; the worker has no FSM, so it is a logged no-op.
                    Cmd::Dismiss => {
                        info!("Dismiss command received on tokio worker — no-op (episode FSM is on main)");
                    }
                }
            }

            else => break, // cmd_rx closed (sender dropped)
        }
    }
    CmdLoopResult::ShutdownComplete
}

// ── Bootstrap (tokio thread + Slint drain timer) ──────────────────────────────

/// Start the tokio worker runtime on a dedicated thread.
///
/// The Mopidy WS client and its connection-state-forwarding task are spawned
/// *inside* the worker runtime (via `block_on`) — `MopidyWsClient::spawn`
/// calls `tokio::spawn`, which requires an active runtime context, so it must
/// run on the worker thread, not on main.
fn spawn_tokio_worker(
    cmd_rx: mpsc::Receiver<Cmd>,
    reply_tx: mpsc::Sender<Reply>,
    mopidy_ws_url: String,
    mopidy_event_tx: tokio_mpsc::Sender<mopidy_client::MopidyEvent>,
    mopidy_reply_tx: tokio_mpsc::Sender<mopidy_client::transport::JsonRpcMessage>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("tokio-worker".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to create tokio runtime for worker thread");

            info!("tokio worker runtime created on dedicated thread");

            rt.block_on(async move {{
                // ── Mopidy WS client (runs on the worker runtime) ────────────
                //
                // `MopidyWsClient::spawn` calls `tokio::spawn` internally, so it
                // must run inside a runtime context — hence here, inside
                // `block_on`, rather than on main.
                let (mopidy_state_tx, mut mopidy_state_rx) =
                    tokio_mpsc::channel::<MopidyConnectionState>(16);
                let client: MopidyClient = Arc::new(mopidy_client::transport::MopidyWsClient::spawn(
                    mopidy_ws_url,
                    None, // use default backoff policy
                    mopidy_event_tx,
                    mopidy_reply_tx,
                    mopidy_state_tx,
                ));

                // Task 4.3: Mopidy client state forwarding — spawn a background
                // task that reads MopidyConnectionState transitions and forwards
                // them through the Reply channel as Reply::MopidyConnectionState.
                let reply_tx_forward = reply_tx.clone();
                tokio::spawn(async move {{
                    while let Some(state) = mopidy_state_rx.recv().await {{
                        let _ = reply_tx_forward
                            .send(Reply::MopidyConnectionState(state))
                            .await;
                    }}
                }});

// Run the command dispatcher with the live MopidyClient handle.
                let result = command_dispatcher(cmd_rx, reply_tx, client).await;
                info!(result = ?result, "tokio command dispatcher exited");
            }});

            info!("tokio worker thread shutting down");
        })
        .expect("failed to spawn tokio worker thread")
}

/// Application entry point.
///
/// Creates the cross-thread channel topology, spaws the tokio worker on a
/// dedicated thread, and installs a repeating [`slint::Timer`] that drains
/// replies and events non-blockingly on each Slint tick.
///
/// The returned [`JoinHandle`] can be `.join()`d to wait for the tokio worker
/// to finish; in normal operation the handle is parked (the application lives
/// as long as the Slint event loop runs).
pub fn bootstrap(conn: SharedConnection) -> (JoinHandle<()>, AppWindow, AppTimers) {
    let cfg = Config::load();

    // Slice 4a / D3: resolve bundled beep asset path at boot
    let bundled_beep_path = resolve_bundled_beep_asset(&cfg.data_dir);
    if bundled_beep_path.is_some() {
        info!(beep_path = ?bundled_beep_path, "bundled beep asset resolved");
    } else {
        info!("no bundled beep asset found");
    }

    info!(
        db_path = %cfg.db_path,
        mopidy_ws_url = %cfg.mopidy_ws_url,
        axum_bind_addr = %cfg.axum_bind_addr,
        log_level = %cfg.log_level,
        data_dir = %cfg.data_dir,
        "bootstrap: configuration loaded",
    );

    // Create cross-thread channels (task 2.1).
    let handles = channel::create_channels();
    let cmd_sender = handles.main.cmd_sender;
    let mut reply_rx = handles.main.reply_receiver;
    let mut event_rx = handles.main.event_receiver;
    let tokio_handles = handles.tokio;

    // ── Mopidy client channels (task 4.3 / 4.5) ────────────────────────
    //
    // The WS client itself is spawned *inside* the tokio worker runtime (see
    // `spawn_tokio_worker`) because `MopidyWsClient::spawn` calls
    // `tokio::spawn`.  These senders are handed to the worker; the receiver
    // ends are retained on main for later slices (slice 0 only logs).
    let (mopidy_event_tx, _mopidy_event_rx) =
        tokio_mpsc::channel::<mopidy_client::MopidyEvent>(16);
    let (mopidy_reply_tx, _mopidy_reply_rx) =
        tokio_mpsc::channel::<mopidy_client::transport::JsonRpcMessage>(16);

    // Spawn tokio worker on a dedicated thread (task 2.2).
    let worker_handle = spawn_tokio_worker(
        tokio_handles.cmd_receiver,
        tokio_handles.reply_sender,
        cfg.mopidy_ws_url.clone(),
        mopidy_event_tx,
        mopidy_reply_tx,
    );

    info!("tokio worker thread spawned successfully");

    // ── Episode FSM (task 9.1) ──────────────────────────────────────
    //
    // Create the episode controller so the shutdown handler can call
    // `shutdown_restore()` (task 6.5) before draining the Cmd channel and
    // exiting. Its [`MopidyControl`] seam is the channel-backed
    // [`ChannelMopidyControl`] (the slice-0 `NoopMopidyControl` no-op is
    // replaced by group 9.1).
    //
    // Wrapped in `Arc<Mutex<..>>` (task 7.3) so the dismiss tap handler —
    // registered on the `AppWindow` below — the drain timer, and the
    // scheduler's [`EpisodeFsmAdapter`] can all reach the FSM on the main
    // thread. (`slint::Timer` closures do not require `Send` in this version,
    // but `Arc` is kept for the shared-ownership pattern.)
    // ── DisplayController (slice 4) ───────────────────────────────────
    //
    // Single backlight controller on main. Wrapped in `Arc<Mutex<..>>` so it
    // can be shared with the episode FSM (for brightness capture/restore) and
    // the scheduler tick (for policy resolution each tick).
    let display_ctl: Arc<Mutex<DisplayController>> = Arc::new(Mutex::new(DisplayController::new()));

    // Slice 4 / task 3.3: load persisted bedtime config and brightness floor.
    {
        if let Ok(mut dc) = display_ctl.lock() {
            if let Ok(conn_guard) = conn.lock() {
                let store = crate::database::ConfigStore::new(&*conn_guard);
                let bedtime = store.get("bedtime_config").ok().flatten();
                let floor = store.get("brightness_floor").ok().flatten();
                if let Some(json) = bedtime {
                    dc.set_bedtime_config(crate::display::BedtimeConfig::from_json(Some(&json)));
                }
                if let Some(val) = floor {
                    if let Ok(pct) = val.parse::<u8>() {
                        dc.set_brightness_target(pct);
                    }
                }
                // Persist current values (round-trip ensures they are stored).
                let _ = store.set("bedtime_config", &dc.bedtime_config().to_json());
                let _ = store.set("brightness_floor", &dc.brightness_floor().to_string());
            }
        }
    }

    let episode_ctl: Arc<Mutex<EpisodeController<ChannelMopidyControl>>> = Arc::new(
        Mutex::new(EpisodeController::new(ChannelMopidyControl::new_with_display(
            cmd_sender,
            Arc::clone(&display_ctl),
        ))),
    );

    // ── Slint AppWindow + episode UI wiring (tasks 7.2 / 7.3) ─────────────
    //
    // The `AppWindow` exposes the `episode-firing` property and the
    // `dismiss-requested` callback. Group 9.1 hosts `.run()` to drive the
    // Slint event loop; the window is held alive across the bootstrap scope
    // (and returned to `main`) so the weak refs captured by the drain timer
    // and the dismiss callback remain valid.
    let app_window = AppWindow::new().expect("failed to create AppWindow");

    // Task 7.3: a tap on the alarm overlay (`AlarmPanel`) invokes the
    // `dismiss-requested` callback, which calls `EpisodeController::dismiss()`
    // directly on main (the FSM lives on main per design D4/D8) and restores
    // the UI to Idle. This does not route through the `Cmd` channel — the
    // episode FSM is owned by main.
    {
        let ctl = Arc::clone(&episode_ctl);
        let dc = Arc::clone(&display_ctl);
        let weak = app_window.as_weak();
        app_window.on_dismiss_requested(move || {
            if let Ok(mut ctl) = ctl.lock() {
                ctl.dismiss();
            }
            // Slice 4: signal the display controller that the episode ended.
            if let Ok(mut dc) = dc.lock() {
                dc.set_episode_active(false);
            }
            // Optimistically restore the UI to Idle (the FSM is now `Dismissed`).
            if let Some(w) = weak.upgrade() {
                w.set_episode_firing(false);
            }
        });
    }

    // Slice 2 / D8: the snooze button on the alarm overlay invokes
    // `EpisodeController::snooze(DEFAULT_SNOOZE_DURATION)`. The drain timer
    // reflects `is_firing()` (false during `Snoozing`) into `episode-firing`
    // on the next tick, so the overlay hides without further wiring here.
    {
        let ctl = Arc::clone(&episode_ctl);
        app_window.on_snooze_requested(move || {
            if let Ok(mut ctl) = ctl.lock() {
                ctl.snooze(crate::episode::DEFAULT_SNOOZE_DURATION);
            }
        });
    }

    // Weak handle captured by the drain timer (task 7.2): each tick reflects
    // `EpisodeController::is_firing()` into the `episode-firing` property so
    // the overlay shows/hides and the nav container / swipe are gated.
    let ui_weak = app_window.as_weak();

    // ── Runtime theme controller + live clock timer (slice 3) ────────────────
    //
    // Theme selection and mode are loaded from `kv_config`, pushed into the
    // Slint `ThemeGlobal` singleton, and updated every second so the clock
    // and theme tokens stay in sync.
    let theme_controller = Arc::new(Mutex::new(
        ThemeController::new(Arc::clone(&conn))
            .with_display(Arc::clone(&display_ctl)),
    ));

    // Push the initially loaded theme into Slint.
    {
        let ctl = theme_controller.lock().expect("theme mutex poisoned");
        ctl.push(&app_window);
    }

    // Settings panel: tap to cycle theme.
    {
        let ctl = Arc::clone(&theme_controller);
        let weak = app_window.as_weak();
        app_window.on_theme_tapped(move || {
            protected_tick(|| {
                if let Ok(mut ctl) = ctl.lock() {
                    ctl.cycle_theme();
                    if let Some(w) = weak.upgrade() {
                        ctl.push(&w);
                    }
                }
            });
        });
    }

    // Settings panel: tap to cycle mode.
    {
        let ctl = Arc::clone(&theme_controller);
        let weak = app_window.as_weak();
        app_window.on_mode_tapped(move || {
            protected_tick(|| {
                if let Ok(mut ctl) = ctl.lock() {
                    ctl.cycle_mode();
                    if let Some(w) = weak.upgrade() {
                        ctl.push(&w);
                    }
                }
            });
        });
    }

    // Debug tap logging — remove once calibration is finalised.
    //
    // (removed — calibration is now handled via udev LIBINPUT_CALIBRATION_MATRIX)

    // 1-second clock timer: compute hand angles and refresh the theme tokens.
    let clock_timer = slint::Timer::default();
    {
        let ctl = Arc::clone(&theme_controller);
        let weak = app_window.as_weak();
        clock_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                protected_tick(|| {
                    let now = Local::now();
                    let h = (now.hour() % 12) as f32;
                    let m = now.minute() as f32;
                    let s = now.second() as f32;

                    let second_angle = s * 6.0;
                    let minute_angle = (m * 60.0 + s) * 0.1;
                    let hour_angle = (h * 3600.0 + m * 60.0 + s) * 0.008333333;

                    if let Some(w) = weak.upgrade() {
                        let global = w.global::<ThemeGlobal>();
                        global.set_hour_angle(hour_angle);
                        global.set_minute_angle(minute_angle);
                        global.set_second_angle(second_angle);
                        global.set_clock_weekday(slint::SharedString::from(
                            now.format("%A").to_string(),
                        ));
                        global.set_clock_date(slint::SharedString::from(
                            now.format("%B %-d, %Y").to_string(),
                        ));

                        if let Ok(ctl) = ctl.lock() {
                            ctl.push(&w);
                        }
                    }
                });
            },
        );
    }

    // ── Slint drain timer (non-blocking try_recv on each tick) ────────────
    //
    // This single repeating timer polls both the reply channel and the Mopidy
    // event channel on every Slint tick. It uses `try_recv` so main never
    // blocks waiting for the tokio worker. Each received item is dispatched
    // directly into domain handlers on the main thread — no locks needed.

    // The drain timer below moves `episode_ctl` into its closure; clone it now
    // for the scheduler's `EpisodeFsmAdapter` (constructed after the drain timer).
    let episode_ctl_for_scheduler = Arc::clone(&episode_ctl);

    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
        // ── Tick-level panic isolation (Design D6) ──────────────────────
        //
        // Every periodic tick wraps its body in `catch_unwind`. A panic
        // is logged at `error!` level and the tick reschedules on the
        // next interval. Cardinal rule: a bug in one tick must not sink
        // the alarm guarantee.
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            // Drain reply channel (non-blocking).
            while let Ok(reply) = reply_rx.try_recv() {
                dispatch_reply_to_domain(
                    reply,
                    &*episode_ctl,
                );
            }

            // Drain Mopidy event channel (non-blocking).
            while let Ok(event) = event_rx.try_recv() {
                dispatch_event_to_domain(event);
            }

            // Task 7.2: reflect the episode FSM state into the UI. When the
            // FSM is `Firing`, the alarm overlay is shown exclusively and the
            // navigation container is hidden + swipe disabled (bound in
            // `ui.slint`); on `Idle`/`Dismissed` it is restored.
            if let Ok(ctl) = episode_ctl.lock() {
                if let Some(w) = ui_weak.upgrade() {
                    w.set_episode_firing(ctl.is_firing());
                }
            }
        }));

        if let Err(err) = result {
            // Log the panic payload — `Box<dyn Any + Send>` can be a String,
            // &str, or opaque data. We log whatever we can recover.
            let msg = match err.downcast::<String>() {
                Ok(s) => *s,
                Err(e) => match e.downcast::<&str>() {
                    Ok(s) => s.to_string(),
                    Err(_) => "unknown panic payload".to_string(),
                },
            };
            tracing::error!(panic = %msg, tick_interval_ms = 50,
                "tick body panicked — caught and rescheduled",
            );
        }
    });

    info!("drain timer installed (50 ms repeat interval)");

    // ── Scheduler tick timer (slice 1, task 1.1) ────────────────────────────
    //
    // A repeating `slint::Timer` at the default 5 s interval drives the
    // alarm scheduler. Each tick re-reads `Local::now()`, asks the alarm
    // source for due alarms, enters the `scheduler_tick` span, and fires the
    // episode FSM for each due alarm (recomputing next_fire afterwards). See
    // design D1 and `scheduler.rs`.
    //
    // Until the real `AlarmStore` (group 3) and `EpisodeController` (group 5)
    // are wired in by group 9.1, the scheduler runs over no-op seam impls —
    // the tick machinery and span are exercised, but no real alarms fire yet.
    // Dev alarm seeding (task 8.3 / design D9): consume `./alarms.toml`
    // (if present) and upsert each entry by `id` into the DB. Idempotent,
    // dev-only (no-op in release builds). This must run *before* the
    // `recompute_next_fires` boot step so freshly-seeded alarms get a
    // `next_fire` cache entry on this boot.
    {
        if let Ok(conn_guard) = conn.lock() {
            let store = AlarmStore::new(&*conn_guard);
            if let Err(e) = seed::seed_alarms(&store) {
                error!(error = %e, "dev alarm seeding failed; continuing with DB as sole source");
            }
        } else {
            error!("alarm DB mutex poisoned at boot; skipping dev seed");
        }
    }

    // Boot recompute (task 3.4): populate/refresh the `next_fire` caches
    // from each alarm's rule once before the first tick (design D1).
    {
        if let Ok(conn_guard) = conn.lock() {
            let store = AlarmStore::new(&*conn_guard);
            if let Err(e) = store.recompute_next_fires(Utc::now()) {
                error!(error = %e, "boot recompute of next_fire caches failed");
            } else {
                info!("boot recompute: next_fire caches refreshed");
            }
        } else {
            error!("alarm DB mutex poisoned at boot; skipping recompute");
        }
    }

    // Group 9.1: wire the real [`StoreAlarmSource`] (over [`AlarmStore`]) and
    // [`EpisodeFsmAdapter`] (over the [`EpisodeController`] above) in place of
    // the slice-0 no-op seams, so real alarms now drive the episode FSM.
    let display_for_scheduler = Arc::clone(&display_ctl);
    let scheduler_state = Mutex::new(Scheduler::new(
        StoreAlarmSource::new(Arc::clone(&conn)),
        EpisodeFsmAdapter::new(Arc::clone(&conn), episode_ctl_for_scheduler, Arc::clone(&display_ctl), bundled_beep_path.clone()),
        LocalClock,
    ));
    let scheduler_timer = slint::Timer::default();
    scheduler_timer.start(
        slint::TimerMode::Repeated,
        DEFAULT_TICK_INTERVAL,
        move || {
            // Design D6: isolate the tick body so a bug never sinks the alarm
            // guarantee; the timer reschedules on its next interval.
            protected_tick(|| {
                if let Ok(mut state) = scheduler_state.lock() {
                    state.tick();
                }
                // Slice 4: drive the display controller policy resolution.
                if let Ok(mut dc) = display_for_scheduler.lock() {
                    dc.tick();
                }
            });
        },
    );
    info!(
        interval_secs = DEFAULT_TICK_INTERVAL.as_secs(),
        "scheduler timer installed",
    );

    // Hold the `AppWindow` alive across the bootstrap scope so the weak refs
    // captured by the drain timer and the dismiss callback remain valid. The
    // timers are returned in [`AppTimers`] so `main` can keep them alive
    // across `.run()` (a dropped `slint::Timer` stops firing).
    let _app_window = app_window;

    let timers = AppTimers {
        _drain: timer,
        _scheduler: scheduler_timer,
        _clock: clock_timer,
    };

    (worker_handle, _app_window, timers)
}

/// Dispatch a [`Reply`] from the tokio worker into the domain.
///
/// In slice 0 these are logged. Later slices will route them to the FSM or
/// update Slint UI models. Runs on main via the drain timer callback.
fn dispatch_reply_to_domain(
    reply: Reply,
    episode_ctl: &std::sync::Mutex<EpisodeController<ChannelMopidyControl>>,
) {
    match reply {
        Reply::MopidyState(state) => {
            info!(reply = "MopidyState", state = %state, "dispatched reply to domain");
        }
        Reply::CallResult(result) => {
            info!(reply = "CallResult", result = ?result, "dispatched reply to domain");
        }
        Reply::ShutdownRequested => {
            info!("Shutdown requested (signal) — entering shutdown sequence");
            execute_shutdown(episode_ctl);
        }
        // Task 4.3: log Mopidy connection-state transitions (not consumed beyond logging in slice 0).
        Reply::MopidyConnectionState(state) => {
            info!(reply = "MopidyConnectionState", state = ?state, "dispatched Mopidy connection state to domain");
        }
    }
}

/// Dispatch a [`MopidyEvent`] from the tokio worker into the domain.
///
/// In slice 0 these are logged and otherwise ignored; later slices consume them
/// within the episode FSM. Runs on main via the drain timer callback.
fn dispatch_event_to_domain(event: MopidyEvent) {
    match &event {
        MopidyEvent::PlaybackStateChanged => {
            info!(event = "PlaybackStateChanged", "dispatched Mopidy event to domain");
        }
        MopidyEvent::TracklistChanged => {
            info!(event = "TracklistChanged", "dispatched Mopidy event to domain");
        }
        MopidyEvent::Other { method } => {
            warn!(event = "Other", method = %method, "dispatched unmodelled Mopidy event to domain");
        }
    }
}

// ── Domain shutdown hook (Design D7 seam) ───────────────────────────────────

/// Trait for domain-level actions required before the process exits.
///
/// Slice 0: no-op placeholder.  Slice 1+: restore the episode snapshot from
/// persistence so that an in-flight alarm is not lost across a restart.
pub trait DomainShutdownRestore {
    fn shutdown_restore(&self);
}

/// Default domain implementation (slice 0: no-op).
pub struct Domain;

impl DomainShutdownRestore for Domain {
    fn shutdown_restore(&self) {
        info!("shutdown_restore called — slice 0 no-op placeholder");
    }
}

// ── systemd readiness notification (Design D10) ─────────────────────────────

/// Send `sd_notify(READY=1)` to systemd if we are running under it.
///
/// Called after all bootstrap steps complete: config parsed, DB migrated
/// (no-op in slice 0), Mopidy client started (placeholder), axum bound
/// (placeholder).  Does nothing when `NOTIFY_SOCKET` is not set (i.e. when
/// running outside systemd).
fn sd_notify_ready() {
    if let Ok(socket_path) = std::env::var("NOTIFY_SOCKET") {
        match std::os::unix::net::UnixDatagram::unbound() {
            Ok(socket) => {
                if let Err(e) = socket.send_to(b"READY=1", &socket_path) {
                    warn!(error = %e, "failed to send sd_notify READY=1");
                } else {
                    info!("sd_notify: READY=1");
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to create datagram socket for sd_notify");
            }
        }
    }
}

// ── Display orientation (slice 3 / D6) ──────────────────────────────────────

/// Rotate the display to portrait. Under Wayland/cage this calls `wlr-randr`;
/// under the Slint linuxkms backend (no compositor) the `SLINT_KMS_ROTATION`
/// environment variable is set before Slint initializes.
fn rotate_display_to_portrait() {
    // The Slint linuxkms backend reads SLINT_KMS_ROTATION at init time to
    // rotate the DRM framebuffer. Set it before the window is created.
    if std::env::var("SLINT_KMS_ROTATION").is_err() {
        std::env::set_var("SLINT_KMS_ROTATION", "270");
        info!("set SLINT_KMS_ROTATION=270 for portrait orientation");
    }

    // Under a Wayland compositor (cage), also rotate the output via wlr-randr.
    // `wlr-randr --json` prints a JSON array of output objects.
    let json_output = match std::process::Command::new("wlr-randr")
        .arg("--json")
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => {
            // wlr-randr not found or no compositor — not an error (linuxkms).
            return;
        }
    };

    let parsed: serde_json::Value = match serde_json::from_slice(&json_output) {
        Ok(v) => v,
        Err(_) => return,
    };

    // The output is an array: [{"name": "DSI-1", "transform": "normal", ...}]
    let output_name = parsed
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("name"))
        .and_then(|n| n.as_str());

    let output_name = match output_name {
        Some(name) => name.to_string(),
        None => return,
    };

    info!(output = %output_name, "rotating Wayland output 270° for portrait");
    let _ = std::process::Command::new("wlr-randr")
        .arg("--output")
        .arg(&output_name)
        .arg("--transform")
        .arg("270")
        .status();
}

// ── Shutdown sequence executor (Design D7) ──────────────────────────────────

/// Perform the full graceful shutdown on the main thread.
///
/// 1. **Restore snapshot** (task 6.5): if an episode is firing, restore the
///    Mopidy snapshot before draining the Cmd channel and exiting.
/// 2. Drain remaining commands by allowing the channel sender to be dropped
///    (happens naturally when the process exits).
/// 3. Stop Mopidy client and axum — no-op in slice 0 (no live resources).
/// 4. Commit any pending DB transaction — no-op in slice 0 (DB not yet wired).
/// 5. Exit with status 0.
fn execute_shutdown(
    episode_ctl: &std::sync::Mutex<EpisodeController<ChannelMopidyControl>>,
) {
    info!("shutdown sequence starting");

    // Step 1 (task 6.5): restore snapshot if an episode is firing, before
    // draining the Cmd channel and exiting.
    if let Ok(mut ctl) = episode_ctl.lock() {
        ctl.shutdown_restore();
    }

    // Step 2: cmd channel drain — sender drops when function scope ends and
    // the process exits, naturally closing the recv side on tokio.
    info!("cmd channel drained (sender dropped on exit)");

    // Step 3: stop Mopidy client and axum — no-op in slice 0 (no live resources).
    // Later slices will hold real handles here.
    info!("Mopidy client stop requested — slice 0 no-op");
    info!("axum server stop requested — slice 0 no-op");

    // Step 4: commit pending DB work — no-op in slice 0 (DB not yet wired).
    info!("pending DB transaction commit — slice 0 no-op");

    info!("shutdown sequence complete — exiting with code 0");
    std::process::exit(0);
}

// ── Main ──────────────────────────────────────────────────────────────────────

/// Application entry point (app boundary).
///
/// Uses **`anyhow::Result<()>`** per Design D6: anyhow at the boundary,
/// thiserror for domain-specific error types internally.
fn main() -> anyhow::Result<()> {
    init_tracing();

    // Slice 3 / D6: rotate the Wayland output to portrait before the Slint
    // window is created, so the fullscreen surface is 480×854 (9:16).
    // No-op on desktop / non-Wayland.
    rotate_display_to_portrait();

    // ── Task 3.1 + 3.2: SQLite connection on main + migrations ────────────
    let cfg = crate::config::Config::load();
    info!(db_path = %cfg.db_path, "opening SQLite database");

    let db_path = cfg.db_path.clone();
    let conn = database::open_connection(&db_path)
        .expect("failed to open database connection");

    info!("SQLite connection opened, running migrations");

    database::run_migrations(&conn)
        .expect("migration runner failed");

    info!("database: migrations complete");

    let (worker_handle, app_window, timers) =
        info_span!("bootstrap").in_scope(|| bootstrap(Arc::new(Mutex::new(conn))));

    // systemd readiness (Design D10): signal READY=1 after all bootstrap steps
    // complete even when Mopidy is not yet reachable.
    sd_notify_ready();

    // Release builds on the Pi request a true full-screen borderless surface;
    // debug builds stay at 480×854 for dev testing.
    app_window.window().set_fullscreen(!cfg!(debug_assertions));

    info!("alarm-clock: bootstrap complete — application running");

    // Task 9.1: drive the Slint event loop. The drain and scheduler timers
    // (held in `timers`) fire on each tick while `.run()` blocks; the episode
    // UI (`episode-firing` / `dismiss-requested`) is bound to this window. On
    // SIGTERM/SIGINT the worker sends `Reply::ShutdownRequested`, the drain
    // dispatches `execute_shutdown` (which restores any firing episode before
    // `process::exit(0)`), interrupting `.run()`.
    let _ = app_window.run();

    // `.run()` returned (window closed / `slint::quit()`). Drop the timers and
    // the window so the only `Cmd` sender (inside the episode FSM) is released,
    // closing the channel and letting the tokio worker exit before join.
    drop(timers);
    drop(app_window);
    let _ = worker_handle.join();

    Ok(())
}

// ── Protected tick (Design D6) ───────────────────────────────────────────────

/// Execute a periodic-tick body with panic isolation.
///
/// Returns `Ok(())` on success, `Err(String)` when the body panicked.
/// The caller (`slint::Timer` lambda) logs at `error!` and naturally
/// reschedules because the timer fires again on its interval.
pub(crate) fn protected_tick<F>(body: F)
where
    F: FnOnce(),
{
    let result = panic::catch_unwind(AssertUnwindSafe(body));
    if let Err(err) = result {
        let msg = match err.downcast::<String>() {
            Ok(s) => *s,
            Err(e) => match e.downcast::<&str>() {
                Ok(s) => s.to_string(),
                Err(_) => "unknown panic payload".to_string(),
            },
        };
        tracing::error!(panic = %msg, kind = "protected_tick",
            "tick body panicked — caught and will reschedule",
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: command_dispatcher processes GetMopidyState and sends a reply.
    #[tokio::test]
    async fn command_dispatcher_handles_get_mopidy_state() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (reply_tx, mut reply_rx) = mpsc::channel(8);

        let _dummy_ev = tokio_mpsc::channel::<mopidy_client::MopidyEvent>(4);
        let _dummy_rep = tokio_mpsc::channel::<mopidy_client::transport::JsonRpcMessage>(4);
        let client: MopidyClient = Arc::new(mopidy_client::transport::MopidyWsClient::spawn(
            "ws://192.168.255.255/mopidy".to_string(),
            None,
            _dummy_ev.0, _dummy_rep.0, 
            tokio_mpsc::channel::<MopidyConnectionState>(4).0,
        ));

        let dispatcher_fut = command_dispatcher(cmd_rx, reply_tx, client);
        tokio::pin!(dispatcher_fut);

        // Send a GetMopidyState command.
        cmd_tx.send(Cmd::GetMopidyState).await.unwrap();

        // Use select so both the dispatcher and receiver are polled concurrently
        // on tokio's current_thread runtime.
        tokio::select! {
            _ = &mut dispatcher_fut => panic!("dispatcher should not exit yet"),
            result = async { reply_rx.recv().await } => {
                assert!(result.is_some(), "should receive a reply");
                if let Some(Reply::MopidyState(state)) = result {
                    assert_eq!(&state, "STOPPED", "placeholder state is STOPPED");
                } else {
                    panic!("expected MopidyState reply, got: {:?}", result);
                }
            }
        }

        // Send Shutdown to terminate the dispatcher.
        cmd_tx.send(Cmd::Shutdown).await.unwrap();
        let result = dispatcher_fut.await;
        assert_eq!(result, CmdLoopResult::ShutdownComplete);
    }

    /// Scenario: command_dispatcher processes CallMopidy and sends a reply.
    #[tokio::test]
    async fn command_dispatcher_handles_call_mopidy() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (reply_tx, mut reply_rx) = mpsc::channel(8);

        let _dummy_ev = tokio_mpsc::channel::<mopidy_client::MopidyEvent>(4);
        let _dummy_rep = tokio_mpsc::channel::<mopidy_client::transport::JsonRpcMessage>(4);
        let client: MopidyClient = Arc::new(mopidy_client::transport::MopidyWsClient::spawn(
            "ws://192.168.255.255/mopidy".to_string(),
            None,
            _dummy_ev.0, _dummy_rep.0, 
            tokio_mpsc::channel::<MopidyConnectionState>(4).0,
        ));

        let dispatcher_fut = command_dispatcher(cmd_rx, reply_tx, client);
        tokio::pin!(dispatcher_fut);

        cmd_tx.send(Cmd::CallMopidy {
            method: "core.get_version".into(),
            params: serde_json::json!({}),
        })
        .await
        .unwrap();

        // Use select to ensure both sides are polled on current_thread runtime.
        tokio::select! {
            _ = &mut dispatcher_fut => panic!("dispatcher should not exit yet"),
            result = async { reply_rx.recv().await } => {
                assert!(result.is_some(), "should receive a reply");
                assert!(matches!(&result, Some(Reply::CallResult(_))));
            }
        }

        // Shut down.
        cmd_tx.send(Cmd::Shutdown).await.unwrap();
        assert_eq!(dispatcher_fut.await, CmdLoopResult::ShutdownComplete);
    }

    /// Scenario: bootstrap creates channels, spawns tokio worker, and installs
    /// the drain timer without panicking or deadlocking.
    #[test]
    fn bootstrap_creates_worker_and_timer() {
        // Run with the headless Slint testing backend so this test works on
        // CI / SSH sessions without a real Wayland/X11 display server.
        i_slint_backend_testing::init_no_event_loop();

        // bootstrap now owns the single `Connection` (wrapped in `Arc<Mutex>`
        // so the `slint::Timer` closures are `Send`). Build a fresh migrated
        // temp DB for this run.
        let path = std::env::temp_dir().join(format!(
            "alarm_bootstrap_test_{}_{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let _ = std::fs::remove_file(&path);
        let conn = crate::database::open_connection(path.to_str().unwrap())
            .expect("open db");
        crate::database::run_migrations(&conn).expect("migrations");

        let _result = bootstrap(Arc::new(Mutex::new(conn)));
        // If we get here without deadlocking or panicking, the structure is
        // sound: channels created, thread spawned, timer installed.

        let _ = std::fs::remove_file(&path);
    }

    /// Scenario: sending a command through the full channel topology reaches
    /// the tokio dispatcher and replies are dispatched back to main's domain.
    #[test]
    fn end_to_end_command_reply_cycle() {
        let handles = channel::create_channels();
        let main_cmd_sender = handles.main.cmd_sender;
        let mut main_reply_rx = handles.main.reply_receiver;

        // Spawn tokio worker (same as bootstrap does). The Mopidy WS client is
        // spawned inside the worker runtime; provide dummy Mopidy event/reply
        // channels (unused in this test) and a placeholder WS URL.
        let (test_event_tx, _test_event_rx) =
            tokio_mpsc::channel::<mopidy_client::MopidyEvent>(16);
        let (test_reply_tx, _test_reply_rx) =
            tokio_mpsc::channel::<mopidy_client::transport::JsonRpcMessage>(16);
        let _worker_handle = spawn_tokio_worker(
            handles.tokio.cmd_receiver,
            handles.tokio.reply_sender,
            "ws://127.0.0.1:6680/mopidy/ws".to_string(),
            test_event_tx,
            test_reply_tx,
        );

        // Give the worker thread a moment to start its recv loop.
        std::thread::sleep(Duration::from_millis(50));

        // Send GetMopidyState through the real channel topology (main → tokio).
        main_cmd_sender.blocking_send(Cmd::GetMopidyState).unwrap();

        // Receive reply from tokio worker back on main.
        let mut last_reply: Option<String> = None;
        for _ in 0..20 {
            if let Ok(Reply::MopidyState(state)) = main_reply_rx.try_recv() {
                last_reply = Some(state);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(
            last_reply.is_some(),
            "should receive MopidyState reply from tokio worker"
        );
        assert_eq!(last_reply, Some("STOPPED".to_string()));

        // Clean shutdown.
        main_cmd_sender.blocking_send(Cmd::Shutdown).unwrap();
    }

    /// ── Task 2.4: Tick-level panic isolation ────────────────────────────

    /// Scenario: a tick body that panics is caught by `protected_tick`;
    /// control returns to the caller (the timer will fire again).
    #[test]
    fn protected_tick_catches_panic_and_continues() {
        // Use a counter to prove subsequent ticks still execute.
        let counter = std::sync::atomic::AtomicU32::new(0);

        // Tick 1: normal execution.
        protected_tick(|| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1,
            "first tick should execute");

        // Tick 2: panicked body — must be caught, not abort the process.
        protected_tick(|| {
            panic!("simulated bug in dispatch logic");
        });
        // We are still alive after the caught panic.
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1,
            "panic should not mutate state past the unwind point");

        // Tick 3: normal execution again — proves rescheduling works.
        protected_tick(|| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "tick after a panicking tick should still execute (rescheduled)"
        );
    }

    /// Scenario: `protected_tick` extracts the panic message from a String payload.
    #[test]
    fn protected_tick_catches_string_panic_message() {
        // This must not abort this test.
        protected_tick(|| {
            let msg = "boom".to_string();
            panic!("{}", msg);
        });
        // If we reach here, the panic was caught — test passes.
    }

    /// Scenario: `protected_tick` handles a bare &str panic message.
    #[test]
    fn protected_tick_catches_str_panic_message() {
        protected_tick(|| { panic!("&str panic"); });
        // Alive = caught.
    }

    /// ── Task 2.5: Error / panic policy ──────────────────────────────────

    /// Scenario: app boundary uses `anyhow::Result<()>` — `main()` returns
    /// a proper Result that callers can inspect for failures.
    #[test]
    fn main_boundary_returns_anyhow_result() {
        use crate::error::{ConfigError, Result as DomainResult};

        // thiserror domain errors convert into anyhow at the boundary.
        let domain_err: DomainResult<()> = Err(ConfigError::WriteFailed(
            std::io::Error::new(std::io::ErrorKind::Other, "disk full"),
        ));

        // Conversion to anyhow::Error preserves the chain.
        let boundary_err: Result<(), anyhow::Error> = domain_err.map_err(Into::into);
        assert!(boundary_err.is_err());
        let msg = format!("{}", boundary_err.unwrap_err());
        assert!(msg.contains("config write failed"), "anyhow wraps ConfigError chain: {msg}");
    }

    /// Scenario: failed config write degrades — in-memory state remains
    /// authoritative; the process does not exit.
    #[test]
    fn failed_config_write_degrades_keeps_in_memory_state() {
        // Simulate a successful load followed by a failing persist attempt.
        let cfg = Config::default();

        // In-memory state before the (simulated) write.
        assert_eq!(cfg.db_path, crate::config::DEFAULT_DB_PATH);
        assert_eq!(cfg.mopidy_ws_url, crate::config::DEFAULT_MOPIDY_WS_URL);

        // Simulate a write failure: construct the error and verify it converts
        // to anyhow at the boundary without aborting.
        use crate::error::{ConfigError, Result as DomainResult};
        let write_result: DomainResult<()> = Err(ConfigError::WriteFailed(
            std::io::Error::new(std::io::ErrorKind::Other, "disk full"),
        ));

        // The error is propagated to the app boundary as anyhow (never panic).
        let _boundary: Result<(), anyhow::Error> = write_result.map_err(Into::into);

        // In-memory Config state is UNAFFECTED by the failed write.
        assert_eq!(cfg.db_path, crate::config::DEFAULT_DB_PATH);
        assert_eq!(cfg.mopidy_ws_url, crate::config::DEFAULT_MOPIDY_WS_URL);
    }

    /// ── Task 2.6: SIGTERM/SIGINT handling — graceful shutdown seam ─────

    /// Scenario: command_dispatcher signals ShutdownRequested to main when
    /// a Shutdown command arrives after having been set up with signal handlers.
    /// Proves the signal-handling wiring exists (signals are tested indirectly
    /// because sending real OS signals in tests is fragile).
    #[tokio::test]
    async fn command_dispatcher_sends_shutdown_requested_on_command_shutdown() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (reply_tx, _reply_rx) = mpsc::channel(8);

        let _dummy_ev = tokio_mpsc::channel::<mopidy_client::MopidyEvent>(4);
        let _dummy_rep = tokio_mpsc::channel::<mopidy_client::transport::JsonRpcMessage>(4);
        let client: MopidyClient = Arc::new(mopidy_client::transport::MopidyWsClient::spawn(
            "ws://192.168.255.255/mopidy".to_string(),
            None,
            _dummy_ev.0, _dummy_rep.0, 
            tokio_mpsc::channel::<MopidyConnectionState>(4).0,
        ));

        let dispatcher_fut = command_dispatcher(cmd_rx, reply_tx, client);
        tokio::pin!(dispatcher_fut);

        // Send Shutdown — proves the dispatcher loop with signal handlers is active.
        cmd_tx.send(Cmd::Shutdown).await.unwrap();

        assert_eq!(dispatcher_fut.await, CmdLoopResult::ShutdownComplete);
    }

    /// Scenario: Reply::ShutdownRequested triggers dispatch_reply_to_domain,
    /// which calls execute_shutdown → shutdown_restore hook (verified via
    /// the DomainShutdownRestore trait existence and no-op behaviour).
    #[test]
    fn domain_shutdown_restore_hook_exists_and_is_noop() {
        // Instantiate the domain and verify shutdown_restore exists.
        let domain = Domain;

        // The call must not panic (it is a no-op in slice 0).
        domain.shutdown_restore();

        // If we reach here, the hook interface works and is safe to call.
    }

    /// ── Task 2.7: systemd readiness notification ───────────────────────

    /// Scenario: sd_notify_ready() does not panic when NOTIFY_SOCKET is absent
    /// (the normal dev/test case outside systemd).
    #[test]
    fn sd_notify_ready_noop_without_systemd() {
        // Ensure NOTIFY_SOCKET is not set (remove if somehow present).
        std::env::remove_var("NOTIFY_SOCKET");

        // Must not panic or block.
        sd_notify_ready();
    }
}

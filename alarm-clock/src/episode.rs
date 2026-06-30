//! Episode FSM (slice 1, tasks 5.1–5.4).
//!
//! Design D4: a single-threaded state machine owned by main with states
//! `Idle → Firing → Dismissed`. The FSM is driven by the scheduler tick (fires),
//! the Mopidy reply/event drain (state changes), the UI (dismiss tap), and
//! shutdown (`shutdown_restore()`, task 6.4 — not in this group).
//!
//! - **`Idle`**: no episode active.
//! - **`Firing`**: a snapshot was captured at fire time and the alarm's
//!   `source_uri` is playing looping at `max_volume`. Holds the `alarm_id`
//!   and the captured [`MopidySnapshot`]. Only one episode is active at a
//!   time; the `episode` [`tracing`] span is entered on `fire()` and held
//!   entered for the whole episode lifetime.
//! - **`Dismissed`**: the snapshot has been restored (playback resumed/stopped,
//!   volume/repeat/shuffle restored). The `episode` span exits when restore
//!   completes.
//!
//! ## Non-blocking model
//!
//! Mopidy commands are issued through the [`MopidyControl`] seam. The seam's
//! `capture_snapshot` batches the `get_state` / `get_time_position` /
//! `get_volume` / `tracklist.get_repeat` / `get_shuffle` / tracklist reads and
//! is bounded by a 1 s wait; on timeout or `NotConnected` it returns
//! `None`/defaults (task 5.3 graceful-degradation path). The remaining
//! commands (`tracklist.add`, `playback.play`, `set_repeat`, `set_volume`,
//! `set_shuffle`, `stop`, `seek`) are fire-and-forget on the main thread — the
//! FSM transitions optimistically (task 5.5) and does not block awaiting a
//! reply. Replies arrive on the reply channel and are dispatched to the FSM on
//! the next tick; [`EpisodeController::on_command_failure`] is the correction
//! hook the drain calls when a reply indicates failure (logged). Slice 1's FSM
//! does not block the Slint event loop awaiting a reply.

use std::time::{Duration, Instant};

use tracing::{error, info, info_span};
use tracing::span::EnteredSpan;

use crate::scheduler::AlarmId;

use mopidy_client::MopidyConnectionState;
use mopidy_client::PlaybackState;

/// Upper bound for snapshot capture (task 5.3): a 1 s wait; on timeout or
/// `NotConnected`, proceed with `None`/defaults.
#[allow(dead_code)] // slice-1 seam; consumed by the Mopidy-backed impl (group 9.1).
pub const SNAPSHOT_CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Default timeout for `shutdown_restore()` (task 6.4): bounds the restore so a
/// hung Mopidy does not stall systemd's SIGTERM → SIGKILL window.
pub const SHUTDOWN_RESTORE_TIMEOUT: Duration = Duration::from_secs(2);

/// Default grace window for source-failure detection (task 6.2). When playback
/// goes to `Stopped` within this interval after fire, the episode is ended.
pub const DEFAULT_GRACE_WINDOW: Duration = Duration::from_secs(8);

// ── Snapshot (task 5.2) ──────────────────────────────────────────────────────

/// Snapshot of Mopidy playback state captured at fire time (task 5.2 / D5).
///
/// Captured fresh per fire. On Mopidy-down at fire time every field is
/// `None`/default (`uri = None`, `position_ms = 0`, `was_playing = false`,
/// `seekable = false`, `volume = 0`, `repeat = false`, `shuffle = false`) and
/// restore becomes a no-op apart from re-applying volume/repeat/shuffle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MopidySnapshot {
    /// Current tracklist URI (first track) if any.
    pub uri: Option<String>,
    /// Playback time position in milliseconds.
    pub position_ms: u32,
    /// Whether playback state was "playing" at capture.
    pub was_playing: bool,
    /// Whether the current track is seekable.
    pub seekable: bool,
    /// Volume 0..100.
    pub volume: u8,
    /// Tracklist repeat flag.
    pub repeat: bool,
    /// Tracklist shuffle flag.
    pub shuffle: bool,
}

// ── Mopidy control seam ──────────────────────────────────────────────────────

/// Mopidy command seam used by the episode FSM.
///
/// All methods are non-blocking fire-and-forget on main except
/// [`capture_snapshot`](MopidyControl::capture_snapshot), which batches the
/// `get_state` / `get_time_position` / `get_volume` / `tracklist.get_repeat` /
/// `get_shuffle` / tracklist reads and is bounded by
/// [`SNAPSHOT_CAPTURE_TIMEOUT`]; on timeout or `NotConnected` it returns
/// `None`/defaults.
///
/// The concrete implementation (group 9.1) talks to the Mopidy WS client over
/// the cross-thread command channel; the FSM itself never blocks the Slint
/// event loop awaiting a reply (optimistic transition, task 5.5).
pub trait MopidyControl {
    /// Capture a fresh [`MopidySnapshot`] (1 s bound; `None`/defaults on
    /// timeout / `NotConnected`).
    fn capture_snapshot(&self) -> MopidySnapshot;

    /// `tracklist.add([uri])`.
    fn tracklist_add(&self, uri: &str);
    /// `playback.play`.
    fn playback_play(&self);
    /// `playback.stop`.
    fn playback_stop(&self);
    /// `playback.seek(time_position)`.
    fn playback_seek(&self, position_ms: u32);
    /// `tracklist.set_repeat`.
    fn tracklist_set_repeat(&self, on: bool);
    /// `tracklist.set_shuffle` (alias `set_random`).
    fn tracklist_set_shuffle(&self, on: bool);
    /// `playback.set_volume` (clamped 0..100 by the impl).
    fn playback_set_volume(&self, volume: u8);
}

// ── FSM states (task 5.1) ────────────────────────────────────────────────────

/// Episode FSM states (task 5.1).
///
/// `Firing` holds the `alarm_id`, the captured [`MopidySnapshot`], and the
/// entered `episode` [`tracing::Span`] guard. The span is entered on `fire()`
/// and exits (guard dropped) on restore completion in `dismiss()`.
#[derive(Debug)]
pub enum EpisodeState {
    /// No episode active.
    Idle,
    /// An alarm is firing: snapshot captured, `source_uri` playing looping at
    /// `max_volume`.
    Firing {
        alarm_id: AlarmId,
        snapshot: MopidySnapshot,
        /// Monotonic time at which fire began (for grace-window checks).
        fire_time: Instant,
        /// Entered `episode` span guard; dropped on restore → span exits.
        _span: EnteredSpan,
    },
    /// The episode was dismissed and the snapshot restored.
    Dismissed,
}

impl EpisodeState {
    /// `true` when the FSM is in the `Firing` state.
    pub fn is_firing(&self) -> bool {
        matches!(self, EpisodeState::Firing { .. })
    }
}

impl Default for EpisodeState {
    fn default() -> Self {
        EpisodeState::Idle
    }
}

// ── EpisodeController (task 5.1) ──────────────────────────────────────────────

/// Single-threaded episode state machine owned by main (task 5.1 / design D4).
///
/// Generic over a [`MopidyControl`] seam so the FSM is fully unit-testable
/// with a mock backend; group 9.1 wires the concrete Mopidy-backed
/// implementation.
pub struct EpisodeController<C: MopidyControl> {
    control: C,
    state: EpisodeState,
}

impl<C: MopidyControl> EpisodeController<C> {
    /// Construct a controller in the `Idle` state with the given Mopidy seam.
    pub fn new(control: C) -> Self {
        Self {
            control,
            state: EpisodeState::Idle,
        }
    }

    /// Current state reference (for observability / UI wiring).
    pub fn state(&self) -> &EpisodeState {
        &self.state
    }

    /// `true` when an episode is currently firing.
    pub fn is_firing(&self) -> bool {
        self.state.is_firing()
    }

    /// Enter the `episode` span, capture a fresh Mopidy snapshot, and play the
    /// alarm's `source_uri` looping at `max_volume` (task 5.3).
    ///
    /// Begin a fresh episode from `Idle` or `Dismissed` (a restored state).
    /// A fire while already `Firing` is the second-alarm-during-episode case
    /// (task 5.6): slice 1 serializes by dismissing-and-restoring the current
    /// episode, then firing the queued alarm (no overlap). The commands issued
    /// here are fire-and-forget on main (task 5.5): the FSM transitions
    /// optimistically to `Firing` without awaiting a reply; the reply drain
    /// corrects state on failure via [`on_command_failure`](Self::on_command_failure).
    ///
    /// `source_uri` is the alarm's stored source URI; `max_volume` is the
    /// alarm's stored max volume (0..100).
    pub fn fire(&mut self, alarm_id: AlarmId, source_uri: &str, max_volume: u8) {
        if self.is_firing() {
            // Task 5.6: second-alarm serialization. Dismiss-and-restore the
            // current episode, then fall through to fire the queued alarm.
            info!(
                alarm_id,
                "second alarm while Firing — dismissing current episode then firing queued",
            );
            self.dismiss();
            // State is now `Dismissed` (restored); fall through to begin the
            // new episode below.
        }
        // From `Idle` or `Dismissed` (a restored state) a fresh episode begins.

        // Task 5.1: enter the `episode` span (held until restore).
        let span = info_span!(parent: None, "episode", alarm_id = alarm_id).entered();

        // Task 5.3: capture a fresh snapshot (1 s bound / NotConnected →
        // None/defaults handled inside the control seam).
        let snapshot = self.control.capture_snapshot();
        info!(?snapshot, "episode snapshot captured");

        // Task 5.3: play the alarm source looping at max_volume.
        //   tracklist.add(source_uri) + playback.play + tracklist.set_repeat(true)
        //   + playback.set_volume(max_volume)
        self.control.tracklist_add(source_uri);
        self.control.playback_play();
        self.control.tracklist_set_repeat(true);
        self.control.playback_set_volume(max_volume);

        self.state = EpisodeState::Firing {
            alarm_id,
            snapshot,
            fire_time: Instant::now(),
            _span: span,
        };
    }

    /// Transition `Firing → Dismissed` and restore the snapshot (task 5.4).
    ///
    /// Restore contract (D5):
    /// - `tracklist.set_repeat(snapshot.repeat)`
    /// - `tracklist.set_shuffle(snapshot.shuffle)`
    /// - `playback.set_volume(snapshot.volume)`
    /// - if `uri` is `Some` and `was_playing`: `tracklist.add(uri)` +
    ///   `playback.play` + seek `position_ms`
    /// - if `uri` is `Some` and not `was_playing`: `playback.stop`
    /// - if `uri` is `None` (Mopidy was down at capture): restore
    ///   volume/repeat/shuffle only.
    ///
    /// The `episode` span exits when restore completes (the entered-span guard
    /// is dropped at the end of this function).
    pub fn dismiss(&mut self) {
        // Extract the Firing payload, transitioning to Dismissed. The span guard
        // is moved out here but kept alive until the end of the function so the
        // span exits *after* restore completes (task 5.1: "exit on restore").
        let (snapshot, _span) = match std::mem::replace(&mut self.state, EpisodeState::Dismissed) {
            EpisodeState::Firing { snapshot, _span, .. } => (snapshot, _span),
            other => {
                // Not firing — nothing to restore. Restore the prior state.
                self.state = other;
                info!("dismiss() called while not Firing — no-op");
                return;
            }
        };

        // Task 5.4: restore the snapshot.
        info!(?snapshot, "restoring episode snapshot");

        self.control.tracklist_set_repeat(snapshot.repeat);
        self.control.tracklist_set_shuffle(snapshot.shuffle);
        self.control.playback_set_volume(snapshot.volume);

        match snapshot.uri.as_ref() {
            Some(uri) if snapshot.was_playing => {
                self.control.tracklist_add(uri);
                self.control.playback_play();
                if snapshot.seekable {
                    self.control.playback_seek(snapshot.position_ms);
                }
            }
            Some(_) => {
                // Was not playing → stop (leave it stopped).
                self.control.playback_stop();
            }
            None => {
                // Mopidy was down at capture: restore volume/repeat/shuffle only.
                info!("snapshot uri was None — restore limited to volume/repeat/shuffle");
            }
        }

        // `_span` drops here → the `episode` span exits on restore completion.
    }

    /// Borrow the captured snapshot, if currently `Firing`.
    pub fn snapshot(&self) -> Option<&MopidySnapshot> {
        match &self.state {
            EpisodeState::Firing { snapshot, .. } => Some(snapshot),
            _ => None,
        }
    }

    /// Correction hook for the optimistic-transition-with-correction pattern
    /// (task 5.5).
    ///
    /// The FSM issues Mopidy commands fire-and-forget and transitions
    /// optimistically to `Firing` without awaiting a reply. Replies arrive on
    /// the reply channel and are dispatched to the FSM on the next tick; the
    /// drain calls this method when a reply indicates failure (e.g.
    /// `NotConnected`, a JSON-RPC error, or a timeout).
    ///
    /// **Task 6.1** (Mopidy-down-at-fire): when Mopidy is disconnected at fire
    /// time the commands fail with `NotConnected`. This is logged but the
    /// episode *stays* `Firing` — best-effort, no dismiss. Only non-
    /// `NotConnected` failures trigger correction (dismiss-and-restore).
    /// Calling this while not `Firing` is a logged no-op.
    pub fn on_command_failure(&mut self, command: &str, error: &str) {
        info!(command, error, "mopidy command failure reply received");
        if self.is_firing() {
            // Task 6.1: Mopidy-down-at-fire — playback commands return
            // NotConnected; episode stays Firing (best-effort).
            if error == "NotConnected" {
                info!(
                    command,
                    error,
                    "mopidy not connected — playback best-effort, episode remains Firing",
                );
                return;
            }
            // Non-NotConnected failures: correct by dismissing-and-restoring.
            info!(
                command,
                error,
                "correcting FSM state: dismissing-and-restoring after command failure",
            );
            self.dismiss();
        } else {
            info!(
                command,
                error,
                "command failure reported while not Firing — no correction needed",
            );
        }
    }

    /// Grace-window source-failure detection (task 6.2 / design D10).
    ///
    /// When the Mopidy playback state transitions to `Stopped` within the
    /// grace window after fire, log at `error!` and dismiss-and-restore the
    /// episode. Slice 1 has no fallback chain; the user must re-arm.
    /// Calling this while not `Firing` is a logged no-op.
    pub fn on_playback_state_changed(&mut self, state: PlaybackState) {
        let now = Instant::now();
        match &self.state {
            EpisodeState::Firing { fire_time, .. } => {
                if now.duration_since(*fire_time) < DEFAULT_GRACE_WINDOW
                    && matches!(state, PlaybackState::Stopped)
                {
                    error!(
                        elapsed_ms = now.duration_since(*fire_time).as_millis(),
                        "source failed (stopped within grace window) — ending episode",
                    );
                    self.dismiss();
                }
            }
            _ => {
                info!(?state, "playback state change while not Firing — no-op");
            }
        }
    }

    /// Mid-episode Mopidy restart handling (task 6.3 / design D10).
    ///
    /// Connection-state transitions are logged at `info!`. The episode remains
    /// dismissable; no mid-episode re-issue in slice 1 (deferred to the
    /// fallback-chain slice). Calling this while not `Firing` is also logged.
    pub fn on_connection_state_change(
        &mut self,
        old_state: MopidyConnectionState,
        new_state: MopidyConnectionState,
    ) {
        if self.is_firing() {
            info!(
                from = ?old_state,
                to = ?new_state,
                "mopidy connection state transition during Firing episode — logging only (mid-episode re-issue is deferred)",
            );
        } else {
            info!(
                from = ?old_state,
                to = ?new_state,
                "mopidy connection state transition while not Firing",
            );
        }
    }

    /// Consume into the underlying control (for group 9.1 / shutdown wiring).
    #[allow(dead_code)] // consumed by group 9.1 wiring.
    pub fn into_control(self) -> C {
        self.control
    }

    /// Restore the Mopidy snapshot before process exit (task 6.4 / shutdown seam).
    ///
    /// If `Firing`, calls `dismiss()` to restore the snapshot (volume, repeat,
    /// shuffle, playback position) and transitions to `Dismissed`. The episode
    /// span exits on restore completion. If `Idle` or `Dismissed`, returns
    /// immediately as a no-op.
    ///
    /// The restore commands are bounded by [`SHUTDOWN_RESTORE_TIMEOUT`]; group
    /// 9.1 will wire the actual flush+timeout for the real Mopidy handle so a
    /// hung Mopidy worker does not stall systemd's SIGTERM → SIGKILL window.
    pub fn shutdown_restore(&mut self) {
        if self.is_firing() {
            info!("shutdown restore: episode is Firing, restoring snapshot before exit");
            self.dismiss(); // restores snapshot, transitions to Dismissed, drops span
        } else {
            info!("shutdown restore: not Firing — no-op");
        }
    }
}

// ── No-op seam (bootstrap placeholder) ────────────────────────────────────────

/// No-op [`MopidyControl`] used by bootstrap until group 9.1 wires the real
/// Mopidy-backed implementation.
#[derive(Default, Debug, Clone, Copy)]
pub struct NoopMopidyControl;

impl MopidyControl for NoopMopidyControl {
    fn capture_snapshot(&self) -> MopidySnapshot { MopidySnapshot::default() }
    fn tracklist_add(&self, _uri: &str) {}
    fn playback_play(&self) {}
    fn playback_stop(&self) {}
    fn playback_seek(&self, _position_ms: u32) {}
    fn tracklist_set_repeat(&self, _on: bool) {}
    fn tracklist_set_shuffle(&self, _on: bool) {}
    fn playback_set_volume(&self, _volume: u8) {}
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::Subscriber;
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
    use tracing_subscriber::util::SubscriberInitExt;

    /// Recording mock for [`MopidyControl`]. `capture_snapshot` returns the
    /// next snapshot in `snapshots` (advancing each call) so "fresh per fire"
    /// is testable.
    struct MockControl {
        calls: Arc<Mutex<Vec<String>>>,
        snapshots: Arc<Mutex<std::collections::VecDeque<MopidySnapshot>>>,
    }

    impl MockControl {
        fn new(snapshots: Vec<MopidySnapshot>) -> Self {
            let deque: std::collections::VecDeque<_> = snapshots.into();
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                snapshots: Arc::new(Mutex::new(deque)),
            }
        }

        fn record(&self, call: String) {
            self.calls.lock().unwrap().push(call);
        }
    }

    impl MopidyControl for MockControl {
        fn capture_snapshot(&self) -> MopidySnapshot {
            let mut q = self.snapshots.lock().unwrap();
            q.pop_front().unwrap_or_default()
        }
        fn tracklist_add(&self, uri: &str) {
            self.record(format!("add({})", uri));
        }
        fn playback_play(&self) {
            self.record("play".into());
        }
        fn playback_stop(&self) {
            self.record("stop".into());
        }
        fn playback_seek(&self, position_ms: u32) {
            self.record(format!("seek({})", position_ms));
        }
        fn tracklist_set_repeat(&self, on: bool) {
            self.record(format!("set_repeat({})", on));
        }
        fn tracklist_set_shuffle(&self, on: bool) {
            self.record(format!("set_shuffle({})", on));
        }
        fn playback_set_volume(&self, volume: u8) {
            self.record(format!("set_volume({})", volume));
        }
    }

    /// Convenience: thread the call log out via an Arc clone.
    fn mock(snapshots: Vec<MopidySnapshot>) -> (MockControl, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let ctrl = MockControl {
            calls: Arc::clone(&calls),
            snapshots: Arc::new(Mutex::new(snapshots.into_iter().collect())),
        };
        (ctrl, calls)
    }

    // ── Task 5.1 / scenario: Fire transitions Idle → Firing ───────────────

    /// Scenario: a fire while Idle transitions to `Firing`, captures a snapshot,
    /// plays `source_uri` with `repeat=true` at `max_volume`.
    #[test]
    fn fire_transitions_idle_to_firing() {
        let snap = MopidySnapshot {
            uri: Some("file:///music/track.mp3".into()),
            position_ms: 120_000,
            was_playing: true,
            seekable: true,
            volume: 25,
            repeat: false,
            shuffle: true,
        };
        let (ctrl, calls) = mock(vec![snap.clone()]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.fire(7, "file:///alarm/source.mp3", 80);

        assert!(fsm.is_firing());
        match fsm.state() {
            EpisodeState::Firing { alarm_id, snapshot, .. } => {
                assert_eq!(*alarm_id, 7);
                assert_eq!(*snapshot, snap, "snapshot should be captured into Firing");
            }
            other => panic!("expected Firing, got {:?}", other),
        }

        // Fire command sequence (task 5.3).
        let log = { calls.lock().unwrap().clone() };
        assert_eq!(
            log,
            vec![
                "add(file:///alarm/source.mp3)".to_string(),
                "play".into(),
                "set_repeat(true)".into(),
                "set_volume(80)".into(),
            ],
            "fire() should add source, play, set_repeat(true), set_volume(max)",
        );
    }

    // ── Task 5.4 / scenario: Dismiss transitions Firing → Dismissed ──────

    /// Scenario: dismiss while Firing transitions to Dismissed and restores the
    /// snapshot (position 120000, volume 25, shuffle on, repeat restored).
    #[test]
    fn dismiss_transitions_firing_to_dismissed_and_restores() {
        let snap = MopidySnapshot {
            uri: Some("file:///music/track.mp3".into()),
            position_ms: 120_000,
            was_playing: true,
            seekable: true,
            volume: 25,
            repeat: false,
            shuffle: true,
        };
        let (ctrl, calls) = mock(vec![snap.clone()]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.fire(1, "file:///alarm/source.mp3", 90);
        { calls.lock().unwrap().clear(); } // isolate restore calls

        fsm.dismiss();

        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
        assert_eq!(fsm.snapshot(), None);

        let log = { calls.lock().unwrap().clone() };
        assert_eq!(
            log,
            vec![
                "set_repeat(false)".to_string(), // pre-alarm repeat restored
                "set_shuffle(true)".to_string(),      // shuffle on
                "set_volume(25)".to_string(),          // volume 25
                "add(file:///music/track.mp3)".to_string(),
                "play".to_string(),
                "seek(120000)".to_string(),            // position 120000ms
            ],
            "dismiss() should restore repeat/shuffle/volume then resume+seek",
        );
    }

    /// Scenario: when `was_playing` is false but `uri` is Some, dismiss stops
    /// playback (and restores flags/volume).
    #[test]
    fn dismiss_stops_when_was_not_playing() {
        let snap = MopidySnapshot {
            uri: Some("file:///music/track.mp3".into()),
            position_ms: 5_000,
            was_playing: false,
            seekable: true,
            volume: 40,
            repeat: true,
            shuffle: false,
        };
        let (ctrl, calls) = mock(vec![snap]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.fire(2, "file:///alarm/source.mp3", 90);
        { calls.lock().unwrap().clear(); }

        fsm.dismiss();

        let log = { calls.lock().unwrap().clone() };
        assert_eq!(
            log,
            vec![
                "set_repeat(true)".to_string(),
                "set_shuffle(false)".to_string(),
                "set_volume(40)".to_string(),
                "stop".to_string(),
            ],
            "not-playing → stop after restoring flags/volume",
        );
    }

    // ── Task 5.3 / scenario: Snapshot with Mopidy down at fire time ──────

    /// Scenario: Mopidy disconnected at fire time → snapshot fields are
    /// None/defaults, the alarm still fires (playback commands issued), and
    /// restore is a no-op (volume/repeat/shuffle only, all defaults).
    #[test]
    fn fire_with_mopidy_down_uses_defaults_and_restore_is_noop() {
        // capture_snapshot returns the default (Mopidy-down) snapshot.
        let (ctrl, calls) = mock(vec![MopidySnapshot::default()]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.fire(3, "file:///alarm/source.mp3", 100);

        assert!(fsm.is_firing());
        let snap = fsm.snapshot().unwrap().clone();
        assert_eq!(snap.uri, None);
        assert_eq!(snap.position_ms, 0);
        assert!(!snap.was_playing);
        assert!(!snap.seekable);
        assert_eq!(snap.volume, 0);
        assert!(!snap.repeat);
        assert!(!snap.shuffle);

        // Alarm still fires the source at max_volume.
        let fire_log = { calls.lock().unwrap().clone() };
        assert_eq!(
            fire_log,
            vec![
                "add(file:///alarm/source.mp3)".to_string(),
                "play".to_string(),
                "set_repeat(true)".to_string(),
                "set_volume(100)".to_string(),
            ],
        );

        { calls.lock().unwrap().clear(); }
        fsm.dismiss();

        // Restore: volume/repeat/shuffle only (all defaults); no add/play/seek/stop.
        let restore_log = { calls.lock().unwrap().clone() };
        assert_eq!(
            restore_log,
            vec![
                "set_repeat(false)".to_string(),
                "set_shuffle(false)".to_string(),
                "set_volume(0)".to_string(),
            ],
            "Mopidy-down restore is volume/repeat/shuffle only",
        );
        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
    }

    // ── Task 5.2 / scenario: Fresh snapshot per fire ──────────────────────

    /// Scenario: an episode fires after a prior episode restored the user's
    /// (now advanced) session → the new snapshot reflects the current (advanced)
    /// Mopidy state, not the prior snapshot.
    #[test]
    fn snapshot_is_fresh_per_fire() {
        let first = MopidySnapshot {
            uri: Some("file:///a.mp3".into()),
            position_ms: 10_000,
            was_playing: true,
            seekable: true,
            volume: 10,
            repeat: false,
            shuffle: false,
        };
        // The user's session advanced between episodes: new track, position
        // 50_000, volume 30.
        let second = MopidySnapshot {
            uri: Some("file:///b.mp3".into()),
            position_ms: 50_000,
            was_playing: true,
            seekable: true,
            volume: 30,
            repeat: true,
            shuffle: true,
        };
        let (ctrl, calls) = mock(vec![first.clone(), second.clone()]);
        let mut fsm = EpisodeController::new(ctrl);

        // First episode.
        fsm.fire(1, "file:///alarm.mp3", 90);
        assert_eq!(fsm.snapshot().unwrap().clone(), first);
        fsm.dismiss();

        { calls.lock().unwrap().clear(); }

        // Second episode — snapshot reflects the advanced session.
        fsm.fire(2, "file:///alarm.mp3", 90);
        assert_eq!(
            fsm.snapshot().unwrap().clone(),
            second,
            "second fire captures a fresh (advanced) snapshot",
        );

        // Restore of the second episode uses the second snapshot's values.
        { calls.lock().unwrap().clear(); }
        fsm.dismiss();
        let restore_log = { calls.lock().unwrap().clone() };
        assert_eq!(
            restore_log,
            vec![
                "set_repeat(true)".to_string(),
                "set_shuffle(true)".to_string(),
                "set_volume(30)".to_string(),
                "add(file:///b.mp3)".to_string(),
                "play".to_string(),
                "seek(50000)".to_string(),
            ],
            "restore uses the fresh second snapshot",
        );
    }

    /// Scenario: dismiss while Idle is a safe no-op (does not transition to
    /// Dismissed spuriously or issue restore commands).
    #[test]
    fn dismiss_while_idle_is_noop() {
        let (ctrl, calls) = mock(vec![]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.dismiss();
        assert!(matches!(fsm.state(), EpisodeState::Idle));
        assert!({ calls.lock().unwrap().is_empty() });
    }

    /// Scenario: a `fire()` while already `Firing` dismisses-and-restores the
    /// current episode then fires the queued alarm (task 5.6: second-alarm
    /// serialization; no overlap).
    #[test]
    fn fire_while_firing_serializes() {
        let first = MopidySnapshot {
            uri: Some("file:///music/a.mp3".into()),
            position_ms: 1_000,
            was_playing: true,
            seekable: true,
            volume: 20,
            repeat: false,
            shuffle: false,
        };
        // Second capture reflects the (now advanced) session at the second fire.
        let second = MopidySnapshot {
            uri: Some("file:///music/b.mp3".into()),
            position_ms: 5_000,
            was_playing: true,
            seekable: true,
            volume: 50,
            repeat: true,
            shuffle: false,
        };
        let (ctrl, calls) = mock(vec![first.clone(), second.clone()]);
        let mut fsm = EpisodeController::new(ctrl);

        // First episode.
        fsm.fire(1, "file:///alarm1.mp3", 90);
        assert_eq!(fsm.snapshot().unwrap().clone(), first);
        { calls.lock().unwrap().clear(); }

        // Second fire mid-episode: dismiss-and-restore first, then fire queued.
        fsm.fire(2, "file:///alarm2.mp3", 80);

        // Still firing — but the queued alarm now.
        match fsm.state() {
            EpisodeState::Firing { alarm_id, snapshot, .. } => {
                assert_eq!(*alarm_id, 2, "queued alarm should now be firing");
                assert_eq!(snapshot.clone(), second, "fresh snapshot for queued fire");
            }
            other => panic!("expected Firing(queued), got {:?}", other),
        }

        // The recorded calls should be: restore of the first episode, then
        // the fire sequence of the second alarm.
        let log = { calls.lock().unwrap().clone() };
        assert_eq!(
            log,
            vec![
                // restore of the first episode (was_playing, seekable)
                "set_repeat(false)".to_string(),
                "set_shuffle(false)".to_string(),
                "set_volume(20)".to_string(),
                "add(file:///music/a.mp3)".to_string(),
                "play".to_string(),
                "seek(1000)".to_string(),
                // fire of the queued second alarm
                "add(file:///alarm2.mp3)".to_string(),
                "play".to_string(),
                "set_repeat(true)".to_string(),
                "set_volume(80)".to_string(),
            ],
            "second fire should dismiss-and-restore then fire the queued alarm",
        );
    }

    // ── Task 5.5 / scenario: optimistic transition with correction ───────

    /// Scenario: the reply drain reports a command failure while `Firing`; the
    /// FSM corrects by dismissing-and-restoring (logged), ending the episode
    /// without blocking the event loop.
    #[test]
    fn command_failure_corrects_firing_to_dismissed() {
        let snap = MopidySnapshot {
            uri: Some("file:///music/a.mp3".into()),
            position_ms: 1_000,
            was_playing: true,
            seekable: true,
            volume: 20,
            repeat: false,
            shuffle: false,
        };
        let (ctrl, calls) = mock(vec![snap.clone()]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.fire(1, "file:///alarm.mp3", 90);
        assert!(fsm.is_firing());
        { calls.lock().unwrap().clear(); }

        // The reply drain dispatches a failure (no blocking — called on tick).
        fsm.on_command_failure("playback.play", "RPCError: source unavailable");

        // Corrected: episode dismissed and restored.
        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
        assert_eq!(fsm.snapshot(), None);
        let log = { calls.lock().unwrap().clone() };
        assert_eq!(
            log,
            vec![
                "set_repeat(false)".to_string(),
                "set_shuffle(false)".to_string(),
                "set_volume(20)".to_string(),
                "add(file:///music/a.mp3)".to_string(),
                "play".to_string(),
                "seek(1000)".to_string(),
            ],
            "command failure while Firing should dismiss-and-restore",
        );
    }

    /// Scenario: a command failure reported while not `Firing` is a logged
    /// no-op (no spurious restore).
    #[test]
    fn command_failure_while_idle_is_noop() {
        let (ctrl, calls) = mock(vec![]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.on_command_failure("playback.play", "timeout");
        assert!(matches!(fsm.state(), EpisodeState::Idle));
        assert!({ calls.lock().unwrap().is_empty() });
    }

    // ── Task 6.1: Mopidy-down-at-fire — NotConnected stays Firing ────────

    /// Scenario: the reply drain reports a `NotConnected` failure while
    /// `Firing`; per task 6.1 the episode *remains* Firing (best-effort,
    /// no dismiss).
    #[test]
    fn not_connected_failure_stays_firing() {
        // Mopidy is down at fire time → snapshot is defaults.
        let (ctrl, calls) = mock(vec![MopidySnapshot::default()]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.fire(1, "file:///alarm.mp3", 90);
        assert!(fsm.is_firing());
        // Snapshot should be None/defaults (Mopidy-down capture).
        let snap = fsm.snapshot().unwrap().clone();
        assert_eq!(snap.uri, None);
        { calls.lock().unwrap().clear(); }

        // Playback commands failed with NotConnected — episode stays Firing.
        fsm.on_command_failure("playback.play", "NotConnected");
        fsm.on_command_failure("tracklist.add", "NotConnected");

        assert!(fsm.is_firing(), "episode should stay Firing on NotConnected error");
        // No restore calls issued (no dismiss).
        assert!(calls.lock().unwrap().is_empty(), "should not have restored");
    }

    /// Scenario: dismiss works normally when episode is Firing with a default
    /// snapshot (Mopidy-down path; task 6.1 + existing restore logic).
    #[test]
    fn mopidy_down_restore_is_noop_volume_repeat_shuffle_only() {
        let (ctrl, calls) = mock(vec![MopidySnapshot::default()]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.fire(1, "file:///alarm.mp3", 90);
        { calls.lock().unwrap().clear(); }

        // NotConnected failure stays Firing.
        fsm.on_command_failure("playback.play", "NotConnected");
        assert!(fsm.is_firing());

        { calls.lock().unwrap().clear(); }
        fsm.dismiss();

        // Restore: only volume/repeat/shuffle (all defaults → no-op content).
        let log = { calls.lock().unwrap().clone() };
        assert_eq!(
            log,
            vec![
                "set_repeat(false)".to_string(),
                "set_shuffle(false)".to_string(),
                "set_volume(0)".to_string(),
            ],
            "Mopidy-down restore is volume/repeat/shuffle only (no add/play/seek/stop)",
        );
        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
    }

    // ── Task 6.2: source-failure end-of-episode (grace window) ───────────

    /// Scenario: playback goes to Stopped within the grace window after fire;
    /// the episode is ended (dismiss-and-restore, error logged).
    #[test]
    fn source_failure_stops_within_grace_window_ends_episode() {
        let snap = MopidySnapshot {
            uri: Some("file:///music/a.mp3".into()),
            position_ms: 1_000,
            was_playing: true,
            seekable: true,
            volume: 20,
            repeat: false,
            shuffle: false,
        };
        let (ctrl, calls) = mock(vec![snap.clone()]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.fire(1, "file:///bad-source.mp3", 90);
        assert!(fsm.is_firing());
        { calls.lock().unwrap().clear(); }

        // Playback reports Stopped within the grace window → episode ends.
        fsm.on_playback_state_changed(PlaybackState::Stopped);

        assert!(matches!(fsm.state(), EpisodeState::Dismissed),
            "episode should be Dismissed after source failure in grace window");

        // Restore commands were issued (was_playing = true, seekable).
        let log = { calls.lock().unwrap().clone() };
        assert_eq!(
            log,
            vec![
                "set_repeat(false)".to_string(),
                "set_shuffle(false)".to_string(),
                "set_volume(20)".to_string(),
                "add(file:///music/a.mp3)".to_string(),
                "play".to_string(),
                "seek(1000)".to_string(),
            ],
            "source failure should dismiss-and-restore",
        );
    }

    /// Scenario: playback goes to Stopped AFTER the grace window has elapsed;
    /// no auto-dismiss occurs (user may manually dismiss later).
    #[test]
    fn stopped_after_grace_window_does_not_auto_dismiss() {
        let snap = MopidySnapshot::default();
        let (ctrl, _calls) = mock(vec![snap]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.fire(1, "file:///alarm.mp3", 90);
        assert!(fsm.is_firing());

        // Advance past the grace window by sleeping.
        std::thread::sleep(DEFAULT_GRACE_WINDOW + std::time::Duration::from_secs(1));

        // Playback goes Stopped after grace window — should NOT auto-dismiss.
        fsm.on_playback_state_changed(PlaybackState::Stopped);
        assert!(fsm.is_firing(),
            "episode should stay Firing when stopped outside grace window");
    }

    /// Scenario: PlaybackStateChanged to Playing within the grace window does
    /// NOT auto-dismiss (only Stopped triggers end-of-episode).
    #[test]
    fn playing_within_grace_window_does_not_dismiss() {
        let snap = MopidySnapshot::default();
        let (ctrl, _calls) = mock(vec![snap]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.fire(1, "file:///alarm.mp3", 90);
        assert!(fsm.is_firing());

        fsm.on_playback_state_changed(PlaybackState::Playing);
        assert!(fsm.is_firing(), "Playing state should not trigger dismiss");
    }

    // ── Task 6.3: mid-episode Mopidy restart handling ────────────────────

    /// Scenario: Mopidy restarts while an episode is Firing (Connected →
    /// BackingOff → Connected). The process does not crash, the episode
    /// remains dismissable, and no re-issue of playback occurs (logged only).
    #[test]
    fn mid_episode_mopidy_restart_stays_firing() {
        let snap = MopidySnapshot::default();
        let (ctrl, _calls) = mock(vec![snap]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.fire(1, "file:///alarm.mp3", 90);
        assert!(fsm.is_firing());

        // Mopidy disconnects mid-episode.
        fsm.on_connection_state_change(
            MopidyConnectionState::Connected,
            MopidyConnectionState::BackingOff {
                retry_in: std::time::Duration::from_secs(2),
            },
        );
        assert!(fsm.is_firing(), "should stay Firing during BackingOff");

        // Mopidy reconnects.
        fsm.on_connection_state_change(
            MopidyConnectionState::BackingOff {
                retry_in: std::time::Duration::from_secs(2),
            },
            MopidyConnectionState::Connected,
        );
        assert!(fsm.is_firing(), "should still stay Firing after reconnect");

        // Still dismissable via normal path.
        fsm.dismiss();
        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
    }

    /// Scenario: connection-state transition while not Firing (logged, no crash).
    #[test]
    fn connection_change_while_idle_does_not_crash() {
        let (ctrl, _calls) = mock(vec![]);
        let mut fsm = EpisodeController::new(ctrl);

        assert!(matches!(fsm.state(), EpisodeState::Idle));

        // Transition while Idle — should just log and not crash.
        fsm.on_connection_state_change(
            MopidyConnectionState::Disconnected,
            MopidyConnectionState::Connecting,
        );
        fsm.on_connection_state_change(
            MopidyConnectionState::Connecting,
            MopidyConnectionState::Connected,
        );

        assert!(matches!(fsm.state(), EpisodeState::Idle));
    }

    // ── Task 5.1: episode span entered on fire, exited on restore ────────

    /// Capturing layer that records enter/exit of the `episode` span.
    #[derive(Default)]
    struct SpanLifecycle {
        entered: Arc<Mutex<usize>>,
        exited: Arc<Mutex<usize>>,
    }

    impl<S: Subscriber> Layer<S> for SpanLifecycle {
        fn on_enter(&self, _id: &tracing::span::Id, _ctx: Context<'_, S>) {
            *self.entered.lock().unwrap() += 1;
        }
        fn on_exit(&self, _id: &tracing::span::Id, _ctx: Context<'_, S>) {
            *self.exited.lock().unwrap() += 1;
        }
    }

    /// Raise the global `MAX_LEVEL` gate (defaults to OFF). `set_default` alone
    /// does not raise it; a global default subscriber does. `try_init` succeeds
    /// once per process; we ignore the `Err` on subsequent calls.
    fn ensure_global_max_level() {
        let _ = tracing_subscriber::registry().try_init();
    }

    /// Scenario: `fire()` enters the `episode` span; `dismiss()` exits it on
    /// restore completion.
    #[test]
    fn episode_span_entered_on_fire_exited_on_restore() {
        let layer = SpanLifecycle::default();
        let entered = Arc::clone(&layer.entered);
        let exited = Arc::clone(&layer.exited);

        ensure_global_max_level();
        let _guard = tracing_subscriber::registry().with(layer).set_default();

        let (ctrl, _calls) = mock(vec![MopidySnapshot::default()]);
        let mut fsm = EpisodeController::new(ctrl);

        let before_enter = *entered.lock().unwrap();
        let before_exit = *exited.lock().unwrap();

        fsm.fire(42, "file:///alarm.mp3", 90);

        assert_eq!(
            *entered.lock().unwrap(),
            before_enter + 1,
            "fire() should enter the episode span",
        );
        assert_eq!(
            *exited.lock().unwrap(),
            before_exit,
            "episode span must NOT exit before restore",
        );

        fsm.dismiss();

        assert_eq!(
            *exited.lock().unwrap(),
            before_exit + 1,
            "dismiss() should exit the episode span on restore completion",
        );
    }

    // ── Task 6.4 / 6.6: shutdown_restore ────────────────────────────────

    /// Scenario: shutdown_restore while Firing restores the snapshot and
    /// transitions to Dismissed (Firing → restore issued).
    #[test]
    fn shutdown_restore_issues_restore_when_firing() {
        let snap = MopidySnapshot {
            uri: Some("file:///music/a.mp3".into()),
            position_ms: 1_000,
            was_playing: true,
            seekable: true,
            volume: 20,
            repeat: false,
            shuffle: false,
        };
        let (ctrl, calls) = mock(vec![snap]);
        let mut fsm = EpisodeController::new(ctrl);

        // Fire the alarm.
        fsm.fire(1, "file:///alarm.mp3", 90);
        assert!(fsm.is_firing());
        { calls.lock().unwrap().clear(); } // isolate restore commands

        // Shutdown is requested while Firing.
        fsm.shutdown_restore();

        // Episode should transition to Dismissed (snapshot restored).
        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
        assert_eq!(fsm.snapshot(), None, "snapshot consumed after restore");

        // Restore commands were issued (identical to dismiss() path).
        let log = { calls.lock().unwrap().clone() };
        assert_eq!(
            log,
            vec![
                "set_repeat(false)".to_string(),
                "set_shuffle(false)".to_string(),
                "set_volume(20)".to_string(),
                "add(file:///music/a.mp3)".to_string(),
                "play".to_string(),
                "seek(1000)".to_string(),
            ],
            "shutdown_restore should issue the full restore command set",
        );
    }

    /// Scenario: shutdown_restore while Idle is a no-op — no Mopidy commands
    /// are issued and state remains Idle (Idle → no-op).
    #[test]
    fn shutdown_restore_is_noop_when_idle() {
        let (ctrl, calls) = mock(vec![]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.shutdown_restore();

        assert!(matches!(fsm.state(), EpisodeState::Idle));
        assert!(
            calls.lock().unwrap().is_empty(),
            "no commands should be issued when Idle",
        );
    }

    /// Scenario: shutdown_restore while Dismissed is a no-op.
    #[test]
    fn shutdown_restore_is_noop_when_dismissed() {
        let snap = MopidySnapshot::default();
        let (ctrl, calls) = mock(vec![snap]);
        let mut fsm = EpisodeController::new(ctrl);

        // Fire then dismiss normally.
        fsm.fire(1, "file:///alarm.mp3", 90);
        fsm.dismiss();
        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
        { calls.lock().unwrap().clear(); }

        // shutdown_restore after dismiss should be no-op.
        fsm.shutdown_restore();

        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
        assert!(calls.lock().unwrap().is_empty(),
                "no commands issued when already Dismissed",
        );
    }

    /// Scenario: shutdown_restore exits promptly — completes well within the
    /// [`SHUTDOWN_RESTORE_TIMEOUT`] bound (timeout exits promptly).
    #[test]
    fn shutdown_restore_exits_promptly_within_timeout() {
        let snap = MopidySnapshot::default();
        let (ctrl, _calls) = mock(vec![snap]);
        let mut fsm = EpisodeController::new(ctrl);

        // Fire the alarm.
        fsm.fire(1, "file:///alarm.mp3", 90);

        // Measure wall-clock time of shutdown_restore.
        let start = Instant::now();
        fsm.shutdown_restore();
        let elapsed = start.elapsed();

        assert!(
            elapsed < SHUTDOWN_RESTORE_TIMEOUT,
            "shutdown_restore should complete within {:?}, elapsed: {:?}",
            SHUTDOWN_RESTORE_TIMEOUT, elapsed,
        );
        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
    }
}

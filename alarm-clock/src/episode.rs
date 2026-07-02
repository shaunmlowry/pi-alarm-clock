//! Episode FSM (slice 2: escalation, snooze, fallback chain).
//!
//! Design (slice 2 D4): a single-threaded state machine owned by main with
//! states `Idle → Escalating → Snoozing → Dismissed`. The FSM is driven by the
//! scheduler tick (`fire`, `advance_escalation`, `check_snooze_refire`), the
//! Mopidy reply/event drain (`on_playback_state_changed`,
//! `on_command_failure`, `on_connection_state_change`), the UI (dismiss tap,
//! snooze tap), and shutdown (`shutdown_restore()`).
//!
//! - **`Idle`**: no episode active.
//! - **`Escalating`**: a snapshot was captured at fire time and the alarm's
//!   `source_uri` is playing looping while the volume ramps through the
//!   alarm's `escalation_steps`. Holds the `alarm_id`, the captured
//!   [`MopidySnapshot`], the [`EpisodePlan`], two monotonic clocks
//!   (`fire_time` for escalation elapsed, `source_start` for the grace-window
//!   failure check), and the entered `episode` span guard.
//! - **`Snoozing`**: user media has been restored; escalation is suspended at
//!   the preserved `step_index`; the FSM waits until `snooze_until` to re-fire
//!   (`check_snooze_refire`). `is_firing()` is `false` (overlay hidden).
//! - **`Dismissed`**: the snapshot has been restored. The `episode` span exits.
//!
//! Only one episode is active at a time (`Escalating` or `Snoozing`); a second
//! alarm firing mid-episode dismisses-and-restores the current episode then
//! fires the queued alarm.
//!
//! ## Non-blocking model
//!
//! Mopidy commands are issued through the [`MopidyControl`] seam
//! fire-and-forget on the main thread — the FSM transitions optimistically
//! and does not block awaiting a reply. Replies arrive on the reply channel
//! and are dispatched to the FSM on the next tick; [`on_command_failure`] is
//! the correction hook the drain calls when a reply indicates failure.
//!
//! [`on_command_failure`]: EpisodeController::on_command_failure

use std::time::{Duration, Instant};

use tracing::{error, info, info_span};
use tracing::span::EnteredSpan;

use crate::alarm_store::EscalationStep;
use crate::scheduler::AlarmId;

use mopidy_client::MopidyConnectionState;
use mopidy_client::PlaybackState;

/// Upper bound for snapshot capture (slice 1): a 1 s wait; on timeout or
/// `NotConnected`, proceed with `None`/defaults.
#[allow(dead_code)] // consumed by the Mopidy-backed impl.
pub const SNAPSHOT_CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Default timeout for `shutdown_restore()`: bounds the restore so a hung
/// Mopidy does not stall systemd's SIGTERM → SIGKILL window.
pub const SHUTDOWN_RESTORE_TIMEOUT: Duration = Duration::from_secs(2);

/// Default grace window for source-failure detection (slice 1 / D10). When
/// playback goes to `Stopped` within this interval after the *current source*
/// began (`source_start`), the episode advances the fallback chain or ends.
pub const DEFAULT_GRACE_WINDOW: Duration = Duration::from_secs(8);

/// Default snooze duration (slice 2 / D6): 9 minutes, the PRD-typical value.
pub const DEFAULT_SNOOZE_DURATION: Duration = Duration::from_secs(9 * 60);

// ── Snapshot (slice 1) ───────────────────────────────────────────────────────

/// Snapshot of Mopidy playback state captured at fire time.
///
/// Captured fresh per fire. On Mopidy-down at fire time every field is
/// `None`/default and restore becomes a no-op apart from re-applying
/// volume/repeat/shuffle.
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

/// Mopidy command seam used by the episode FSM. Fire-and-forget on main except
/// [`capture_snapshot`](MopidyControl::capture_snapshot) (1 s bound).
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

// ── Episode plan (slice 2 / D4) ─────────────────────────────────────────────

/// Per-episode immutable data computed at fire time from the [`crate::alarm_store::Alarm`].
///
/// Bundled out of the [`EpisodeState`] variants so `Escalating`/`Snoozing`
/// share one copy and snooze preserves it wholesale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodePlan {
    /// Primary alarm source URI (always tried first).
    pub source_uri: String,
    /// Ordered backup source URIs (empty = no fallback; slice-1 behavior).
    pub fallback_chain: Vec<String>,
    /// Index into `fallback_chain` of the *currently playing* backup, or the
    /// sentinel `usize::MAX` while the primary is playing. Advanced on source
    /// failure.
    pub fallback_index: usize,
    /// Escalation steps (empty = fixed `max_volume`; slice-1 behavior).
    pub escalation_steps: Vec<EscalationStep>,
    /// Ceiling volume 0..=100; the fixed volume when `escalation_steps` is
    /// empty, and the hard cap otherwise.
    pub max_volume: u8,
}

/// Sentinel `fallback_index` meaning "the primary source is playing" (no
/// fallback advanced yet).
const PRIMARY: usize = usize::MAX;

impl EpisodePlan {
    /// Build a plan from the alarm's stored fields. `max_volume` is clamped to
    /// 0..=100. `escalation_steps` / `fallback_chain` default to empty when
    /// `None` (slice-1 behavior).
    pub fn new(
        source_uri: String,
        max_volume: u8,
        escalation_steps: Option<Vec<EscalationStep>>,
        fallback_chain: Option<Vec<String>>,
    ) -> Self {
        Self {
            source_uri,
            fallback_chain: fallback_chain.unwrap_or_default(),
            fallback_index: PRIMARY,
            escalation_steps: escalation_steps.unwrap_or_default(),
            max_volume: max_volume.clamp(0, 100),
        }
    }

    /// Convenience plan for the slice-1 path: fixed `max_volume`, no
    /// escalation, no fallback.
    pub fn simple(source_uri: impl Into<String>, max_volume: u8) -> Self {
        Self::new(source_uri.into(), max_volume, None, None)
    }

    /// The URI currently playing: the primary while `fallback_index == PRIMARY`,
    /// else `fallback_chain[fallback_index]`.
    fn current_uri(&self) -> &str {
        if self.fallback_index == PRIMARY {
            &self.source_uri
        } else {
            &self.fallback_chain[self.fallback_index]
        }
    }

    /// `true` when a further fallback source exists beyond `fallback_index`.
    fn has_next_fallback(&self) -> bool {
        // fallback_chain is 0-indexed; the next slot after `fallback_index`
        // (which is PRIMARY==usize::MAX for the primary) is
        // `fallback_index.wrapping_add(1)`.
        let next = self.fallback_index.wrapping_add(1);
        next < self.fallback_chain.len()
    }

    /// Index of the next fallback source, or `None` if the chain is exhausted.
    fn next_fallback_index(&self) -> Option<usize> {
        let next = self.fallback_index.wrapping_add(1);
        if next < self.fallback_chain.len() {
            Some(next)
        } else {
            None
        }
    }

    /// Volume for a given escalation `step_index`. For an empty-step plan this
    /// is always `max_volume` (the slice-1 fixed-volume path).
    fn volume_at(&self, step_index: usize) -> u8 {
        if let Some(step) = self.escalation_steps.get(step_index) {
            step.volume
        } else if step_index == 0 {
            self.max_volume
        } else {
            // step_index out of range with steps present — hold the last step.
            self.escalation_steps
                .last()
                .map(|s| s.volume)
                .unwrap_or(self.max_volume)
        }
    }

    /// The effective starting step index (0). The starting volume is
    /// `volume_at(0)` (the first step's volume, or `max_volume` when empty).
    fn start_volume(&self) -> u8 {
        self.volume_at(0)
    }
}

// ── FSM states (slice 2 / D4) ─────────────────────────────────────────────────

/// Episode FSM states.
///
/// `Escalating` and `Snoozing` both hold the `alarm_id`, the captured
/// [`MopidySnapshot`], the [`EpisodePlan`], and the entered `episode`
/// [`tracing::Span`] guard. The span is entered on `fire()` and held across
/// `Escalating ↔ Snoozing` transitions; it exits (guard dropped) on restore
/// completion in `dismiss()`/`shutdown_restore()`.
#[derive(Debug)]
pub enum EpisodeState {
    /// No episode active.
    Idle,
    /// An alarm is firing: snapshot captured, source playing looping, volume
    /// ramping through `escalation_steps`.
    Escalating {
        alarm_id: AlarmId,
        snapshot: MopidySnapshot,
        /// Original episode fire instant; drives escalation elapsed.
        fire_time: Instant,
        /// When the *current* source began; drives the grace-window failure
        /// check. Reset on fire and on each fallback advance.
        source_start: Instant,
        /// Current escalation step index.
        step_index: usize,
        plan: EpisodePlan,
        /// Entered `episode` span guard; moved across `Escalating ↔ Snoozing`.
        _span: EnteredSpan,
    },
    /// Escalation suspended; user media restored; waiting to re-fire at
    /// `snooze_until`.
    Snoozing {
        alarm_id: AlarmId,
        snapshot: MopidySnapshot,
        snooze_until: Instant,
        /// Preserved escalation step to resume from on re-fire.
        step_index: usize,
        plan: EpisodePlan,
        _span: EnteredSpan,
    },
    /// The episode was dismissed and the snapshot restored.
    Dismissed,
}

impl EpisodeState {
    /// `true` when the FSM is in the `Escalating` state (the alarm overlay is
    /// shown).
    pub fn is_firing(&self) -> bool {
        matches!(self, EpisodeState::Escalating { .. })
    }

    /// `true` when an episode is active (`Escalating` or `Snoozing`). Used by
    /// `dismiss()`/`shutdown_restore()` so a snoozed episode is still
    /// terminable.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            EpisodeState::Escalating { .. } | EpisodeState::Snoozing { .. }
        )
    }
}

impl Default for EpisodeState {
    fn default() -> Self {
        EpisodeState::Idle
    }
}

// ── EpisodeController ──────────────────────────────────────────────────────────

/// Single-threaded episode state machine owned by main. Generic over a
/// [`MopidyControl`] seam so the FSM is fully unit-testable with a mock
/// backend; `main.rs` wires the concrete channel-backed implementation.
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

    /// `true` when an episode is currently `Escalating` (overlay shown).
    pub fn is_firing(&self) -> bool {
        self.state.is_firing()
    }

    /// `true` when an episode is active (`Escalating` or `Snoozing`).
    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }

    /// Enter the `episode` span, capture a fresh Mopidy snapshot, and play the
    /// plan's source looping at the starting volume (step 0's volume, or
    /// `max_volume` when no escalation steps).
    ///
    /// Begin a fresh episode from `Idle` or `Dismissed`. A fire while already
    /// active (`Escalating`/`Snoozing`) is the second-alarm-during-episode
    /// case: dismiss-and-restore the current episode, then fire the queued
    /// alarm (no overlap).
    pub fn fire(&mut self, alarm_id: AlarmId, plan: &EpisodePlan) {
        if self.is_active() {
            info!(
                alarm_id,
                "second alarm while active — dismissing current episode then firing queued",
            );
            self.dismiss();
        }

        let span = info_span!(parent: None, "episode", alarm_id = alarm_id).entered();
        let snapshot = self.control.capture_snapshot();
        info!(?snapshot, "episode snapshot captured");

        let start_volume = plan.start_volume();
        self.control.tracklist_add(&plan.source_uri);
        self.control.playback_play();
        self.control.tracklist_set_repeat(true);
        self.control.playback_set_volume(start_volume);

        let now = Instant::now();
        self.state = EpisodeState::Escalating {
            alarm_id,
            snapshot,
            fire_time: now,
            source_start: now,
            step_index: 0,
            plan: plan.clone(),
            _span: span,
        };
    }

    /// Transition an active episode (`Escalating`/`Snoozing`) → `Dismissed`
    /// and restore the snapshot. A no-op (logged) when `Idle`/`Dismissed`.
    ///
    /// Restore contract (slice 1 D5): restore repeat/shuffle/volume, then
    /// resume the user's track (if `was_playing`) or stop. The `episode` span
    /// exits when restore completes (the guard is dropped at function end).
    pub fn dismiss(&mut self) {
        let (snapshot, _span) = match std::mem::replace(&mut self.state, EpisodeState::Dismissed) {
            EpisodeState::Escalating { snapshot, _span, .. } => (snapshot, _span),
            EpisodeState::Snoozing { snapshot, _span, .. } => (snapshot, _span),
            other => {
                self.state = other;
                info!("dismiss() called while not active — no-op");
                return;
            }
        };

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
                self.control.playback_stop();
            }
            None => {
                info!("snapshot uri was None — restore limited to volume/repeat/shuffle");
            }
        }
        // `_span` drops here → the `episode` span exits on restore completion.
    }

    /// Borrow the captured snapshot, if currently `Escalating`.
    pub fn snapshot(&self) -> Option<&MopidySnapshot> {
        match &self.state {
            EpisodeState::Escalating { snapshot, .. } => Some(snapshot),
            _ => None,
        }
    }

    /// Progressive volume escalation (slice 2 / D5). Computes the highest
    /// `step_index` whose `after_secs <= elapsed` since `fire_time` and, if it
    /// advanced, issues `playback.set_volume(step.volume)` (idempotent: no
    /// command when unchanged). For an empty-step plan this is a no-op (the
    /// volume stays at `max_volume`). A no-op when not `Escalating`.
    pub fn advance_escalation(&mut self, now: Instant) {
        let (new_index, volume, fire_time, control_needed) = match &self.state {
            EpisodeState::Escalating {
                fire_time,
                step_index,
                plan,
                ..
            } => {
                if plan.escalation_steps.is_empty() {
                    return; // slice-1 fixed volume
                }
                let elapsed = now.duration_since(*fire_time).as_secs();
                let mut target = *step_index;
                for (i, step) in plan.escalation_steps.iter().enumerate() {
                    if step.after_secs <= elapsed {
                        target = i;
                    } else {
                        break;
                    }
                }
                if target == *step_index {
                    return; // no change
                }
                (target, plan.volume_at(target), *fire_time, true)
            }
            _ => return,
        };

        if control_needed {
            if let EpisodeState::Escalating {
                step_index,
                plan: _,
                ..
            } = &mut self.state
            {
                *step_index = new_index;
                self.control.playback_set_volume(volume);
                info!(step_index = new_index, volume, "escalation advanced");
            }
            let _ = fire_time; // keep clippy happy; fire_time unchanged
        }
    }

    /// Snooze (slice 2 / D6). From `Escalating` only: transition to `Snoozing`,
    /// preserving `step_index`/`snapshot`/`plan`/span, set
    /// `snooze_until = now + duration`, and issue restore-playback commands so
    /// user media resumes. A no-op (logged) otherwise.
    pub fn snooze(&mut self, duration: Duration) {
        let (
            alarm_id,
            snapshot,
            step_index,
            plan,
            _span,
        ) = match std::mem::replace(&mut self.state, EpisodeState::Idle) {
            EpisodeState::Escalating {
                alarm_id,
                snapshot,
                step_index,
                plan,
                _span,
                ..
            } => (alarm_id, snapshot, step_index, plan, _span),
            other => {
                self.state = other;
                info!("snooze() called while not Escalating — no-op");
                return;
            }
        };

        info!(step_index, snooze_secs = duration.as_secs(), "snoozing episode — restoring user media");
        // Restore user media (resume snapshot playback) but keep the episode
        // active (do NOT drop the span/snapshot).
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
                self.control.playback_stop();
            }
            None => {
                info!("snooze restore: snapshot uri was None — volume/repeat/shuffle only");
            }
        }

        self.state = EpisodeState::Snoozing {
            alarm_id,
            snapshot,
            snooze_until: Instant::now() + duration,
            step_index,
            plan,
            _span,
        };
    }

    /// Snooze-refire check (slice 2 / D6). Called from the scheduler tick. If
    /// `Snoozing` and `now >= snooze_until`, transition back to `Escalating`:
    /// replay the source, set `fire_time` so elapsed places us at the preserved
    /// step, issue `set_volume(step.volume)`, reset `source_start`. A no-op
    /// otherwise.
    pub fn check_snooze_refire(&mut self, now: Instant) {
        let due = match &self.state {
            EpisodeState::Snoozing { snooze_until, .. } => *snooze_until <= now,
            _ => false,
        };
        if !due {
            return;
        }

        let (alarm_id, snapshot, step_index, mut plan, _span) =
            match std::mem::replace(&mut self.state, EpisodeState::Idle) {
                EpisodeState::Snoozing {
                    alarm_id,
                    snapshot,
                    step_index,
                    plan,
                    _span,
                    ..
                } => (alarm_id, snapshot, step_index, plan, _span),
                other => {
                    self.state = other;
                    return;
                }
            };

        info!(step_index, "snooze elapsed — re-firing episode, resuming escalation from preserved step");
        // Replay the alarm source.
        self.control.tracklist_add(&plan.source_uri);
        self.control.playback_play();
        self.control.tracklist_set_repeat(true);

        let resume_volume = plan.volume_at(step_index);
        self.control.playback_set_volume(resume_volume);

        // Set fire_time so elapsed maps exactly to the preserved step.
        let step_after = plan
            .escalation_steps
            .get(step_index)
            .map(|s| Duration::from_secs(s.after_secs))
            .unwrap_or(Duration::ZERO);
        let fire_time = now - step_after;

        // Re-fire resets to the primary source (fallback_index = PRIMARY),
        // source_start = now.
        plan.fallback_index = PRIMARY;
        self.state = EpisodeState::Escalating {
            alarm_id,
            snapshot,
            fire_time,
            source_start: now,
            step_index,
            plan,
            _span,
        };
    }

    /// Correction hook for the optimistic-transition-with-correction pattern
    /// (slice 1). The drain calls this when a reply indicates failure.
    ///
    /// `NotConnected` while active: episode stays active (best-effort). Other
    /// failures while active: correct by dismissing-and-restoring. A logged
    /// no-op when not active.
    pub fn on_command_failure(&mut self, command: &str, error: &str) {
        info!(command, error, "mopidy command failure reply received");
        if self.is_active() {
            if error == "NotConnected" {
                info!(
                    command,
                    error,
                    "mopidy not connected — playback best-effort, episode remains active",
                );
                return;
            }
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
                "command failure reported while not active — no correction needed",
            );
        }
    }

    /// Source-failure detection with fallback chain (slice 2 / D7).
    ///
    /// When Mopidy playback goes to `Stopped` within the grace window after
    /// `source_start` while `Escalating`, advance to the next fallback source
    /// (escalation uninterrupted) or, if the chain is exhausted, dismiss-and-
    /// restore. Calling this while not `Escalating` is a logged no-op.
    pub fn on_playback_state_changed(&mut self, state: PlaybackState) {
        let now = Instant::now();
        // Decide with a shared borrow, then act without holding it across
        // `self.dismiss()` (which moves the state out).
        enum Decision {
            Advance { next: usize, uri: String, volume: u8 },
            Exhausted,
        }
        let decision = match &self.state {
            EpisodeState::Escalating {
                source_start,
                plan,
                step_index,
                ..
            } => {
                if !matches!(state, PlaybackState::Stopped) {
                    return;
                }
                if now.duration_since(*source_start) >= DEFAULT_GRACE_WINDOW {
                    info!("stopped outside grace window — not a source-startup failure");
                    return;
                }
                match plan.next_fallback_index() {
                    Some(next) => Decision::Advance {
                        next,
                        uri: plan.fallback_chain[next].clone(),
                        volume: plan.volume_at(*step_index),
                    },
                    None => Decision::Exhausted,
                }
            }
            _ => {
                info!(?state, "playback state change while not Escalating — no-op");
                return;
            }
        };

        match decision {
            Decision::Advance { next, uri, volume } => {
                if let EpisodeState::Escalating {
                    source_start,
                    plan,
                    ..
                } = &mut self.state
                {
                    *source_start = now;
                    plan.fallback_index = next;
                }
                info!(
                    fallback_index = next,
                    uri = %uri,
                    "source failed within grace window — advancing fallback",
                );
                self.control.tracklist_add(&uri);
                self.control.playback_play();
                self.control.tracklist_set_repeat(true);
                self.control.playback_set_volume(volume);
            }
            Decision::Exhausted => {
                error!("source failed (chain exhausted) — ending episode");
                self.dismiss();
            }
        }
    }

    /// Mid-episode Mopidy restart handling. Connection-state transitions are
    /// logged at `info!`; no mid-episode re-issue. A logged no-op when not
    /// active.
    pub fn on_connection_state_change(
        &mut self,
        old_state: MopidyConnectionState,
        new_state: MopidyConnectionState,
    ) {
        if self.is_active() {
            info!(
                from = ?old_state,
                to = ?new_state,
                "mopidy connection state transition during active episode — logging only",
            );
        } else {
            info!(
                from = ?old_state,
                to = ?new_state,
                "mopidy connection state transition while not active",
            );
        }
    }

    /// Consume into the underlying control (for shutdown wiring).
    #[allow(dead_code)]
    pub fn into_control(self) -> C {
        self.control
    }

    /// Restore the Mopidy snapshot before process exit. If active, calls
    /// `dismiss()` to restore the snapshot and transitions to `Dismissed`.
    /// Bounded by [`SHUTDOWN_RESTORE_TIMEOUT`] in the wired impl.
    pub fn shutdown_restore(&mut self) {
        if self.is_active() {
            info!("shutdown restore: episode active, restoring snapshot before exit");
            self.dismiss();
        } else {
            info!("shutdown restore: not active — no-op");
        }
    }
}

// ── No-op seam (bootstrap placeholder) ────────────────────────────────────────

/// No-op [`MopidyControl`] used by bootstrap until the channel-backed
/// implementation is wired.
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
    /// next snapshot in `snapshots` (advancing each call).
    struct MockControl {
        calls: Arc<Mutex<Vec<String>>>,
        snapshots: Arc<Mutex<std::collections::VecDeque<MopidySnapshot>>>,
    }

    impl MockControl {
        fn new(snapshots: Vec<MopidySnapshot>) -> (Self, Arc<Mutex<Vec<String>>>) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let ctrl = Self {
                calls: Arc::clone(&calls),
                snapshots: Arc::new(Mutex::new(snapshots.into_iter().collect())),
            };
            (ctrl, calls)
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
        fn tracklist_add(&self, uri: &str) { self.record(format!("add({})", uri)); }
        fn playback_play(&self) { self.record("play".into()); }
        fn playback_stop(&self) { self.record("stop".into()); }
        fn playback_seek(&self, position_ms: u32) {
            self.record(format!("seek({})", position_ms));
        }
        fn tracklist_set_repeat(&self, on: bool) { self.record(format!("set_repeat({})", on)); }
        fn tracklist_set_shuffle(&self, on: bool) { self.record(format!("set_shuffle({})", on)); }
        fn playback_set_volume(&self, volume: u8) { self.record(format!("set_volume({})", volume)); }
    }

    /// Snapshot with sensible defaults for restore tests.
    fn snap_playing(uri: &str, pos: u32, vol: u8) -> MopidySnapshot {
        MopidySnapshot {
            uri: Some(uri.to_string()),
            position_ms: pos,
            was_playing: true,
            seekable: true,
            volume: vol,
            repeat: false,
            shuffle: false,
        }
    }

    fn steps(spec: &[(u64, u8)]) -> Vec<EscalationStep> {
        spec.iter()
            .map(|(s, v)| EscalationStep { after_secs: *s, volume: *v })
            .collect()
    }

    // ── Fire / dismiss basics ─────────────────────────────────────────────

    #[test]
    fn fire_transitions_idle_to_escalating_at_step0_volume() {
        let (ctrl, calls) = MockControl::new(vec![snap_playing("file:///music/a.mp3", 1000, 25)]);
        let mut fsm = EpisodeController::new(ctrl);
        let plan = EpisodePlan::new(
            "file:///alarm/source.mp3".to_string(),
            80,
            Some(steps(&[(0, 20), (60, 80)])),
            None,
        );

        fsm.fire("7".to_string(), &plan);

        assert!(fsm.is_firing());
        match fsm.state() {
            EpisodeState::Escalating { step_index, plan: p, .. } => {
                assert_eq!(*step_index, 0);
                assert_eq!(p.max_volume, 80);
                assert_eq!(p.escalation_steps.len(), 2);
            }
            other => panic!("expected Escalating, got {:?}", other),
        }
        let log = { calls.lock().unwrap().clone() };
        assert_eq!(
            log,
            vec![
                "add(file:///alarm/source.mp3)".to_string(),
                "play".into(),
                "set_repeat(true)".into(),
                "set_volume(20)".into(), // step 0 volume
            ],
        );
    }

    #[test]
    fn fire_with_no_steps_uses_max_volume() {
        let (ctrl, calls) = MockControl::new(vec![snap_playing("file:///a.mp3", 0, 25)]);
        let mut fsm = EpisodeController::new(ctrl);
        let plan = EpisodePlan::simple("file:///alarm.mp3", 40);

        fsm.fire("1".to_string(), &plan);
        let log = { calls.lock().unwrap().clone() };
        assert_eq!(
            log,
            vec![
                "add(file:///alarm.mp3)".to_string(),
                "play".into(),
                "set_repeat(true)".into(),
                "set_volume(40)".into(),
            ],
        );
    }

    #[test]
    fn dismiss_restores_snapshot_from_escalating() {
        let (ctrl, calls) = MockControl::new(vec![snap_playing("file:///music/a.mp3", 120000, 25)]);
        let mut fsm = EpisodeController::new(ctrl);
        fsm.fire("1".to_string(), &EpisodePlan::simple("file:///alarm.mp3", 90));
        { calls.lock().unwrap().clear(); }

        fsm.dismiss();
        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
        let log = { calls.lock().unwrap().clone() };
        assert_eq!(
            log,
            vec![
                "set_repeat(false)".to_string(),
                "set_shuffle(false)".to_string(),
                "set_volume(25)".to_string(),
                "add(file:///music/a.mp3)".to_string(),
                "play".to_string(),
                "seek(120000)".to_string(),
            ],
        );
    }

    // ── Escalation advance ───────────────────────────────────────────────

    #[test]
    fn advance_escalation_ramps_through_steps() {
        let (ctrl, calls) = MockControl::new(vec![MopidySnapshot::default()]);
        let mut fsm = EpisodeController::new(ctrl);
        let plan = EpisodePlan::new(
            "file:///alarm.mp3".to_string(),
            80,
            Some(steps(&[(0, 20), (30, 60), (60, 80)])),
            None,
        );
        fsm.fire("1".to_string(), &plan);
        { calls.lock().unwrap().clear(); }

        // Capture the fire_time from the state.
        let fire_time = match fsm.state() {
            EpisodeState::Escalating { fire_time, .. } => *fire_time,
            _ => unreachable!(),
        };

        // Tick before 30s — still step 0, no command.
        fsm.advance_escalation(fire_time + Duration::from_secs(10));
        assert!(calls.lock().unwrap().is_empty(), "no command before next step");

        // Tick at 31s — advance to step 1 (volume 60).
        fsm.advance_escalation(fire_time + Duration::from_secs(31));
        assert_eq!({ calls.lock().unwrap().clone() }, vec!["set_volume(60)".to_string()]);
        let idx = match fsm.state() {
            EpisodeState::Escalating { step_index, .. } => *step_index,
            _ => unreachable!(),
        };
        assert_eq!(idx, 1);

        // Another tick at 31s — idempotent, no new command.
        { calls.lock().unwrap().clear(); }
        fsm.advance_escalation(fire_time + Duration::from_secs(31));
        assert!(calls.lock().unwrap().is_empty(), "idempotent advance");

        // Tick at 65s — advance to step 2 (volume 80).
        fsm.advance_escalation(fire_time + Duration::from_secs(65));
        assert_eq!({ calls.lock().unwrap().clone() }, vec!["set_volume(80)".to_string()]);
    }

    #[test]
    fn advance_escalation_is_noop_with_no_steps() {
        let (ctrl, calls) = MockControl::new(vec![MopidySnapshot::default()]);
        let mut fsm = EpisodeController::new(ctrl);
        fsm.fire("1".to_string(), &EpisodePlan::simple("file:///alarm.mp3", 40));
        { calls.lock().unwrap().clear(); }

        let fire_time = match fsm.state() {
            EpisodeState::Escalating { fire_time, .. } => *fire_time,
            _ => unreachable!(),
        };
        fsm.advance_escalation(fire_time + Duration::from_secs(120));
        assert!(calls.lock().unwrap().is_empty(), "no escalation without steps");
    }

    #[test]
    fn advance_escalation_is_noop_when_idle() {
        let (ctrl, calls) = MockControl::new(vec![]);
        let mut fsm = EpisodeController::new(ctrl);
        fsm.advance_escalation(Instant::now());
        assert!(calls.lock().unwrap().is_empty());
    }

    // ── Fallback chain ───────────────────────────────────────────────────

    #[test]
    fn source_failure_advances_to_first_fallback_uninterrupted() {
        let (ctrl, calls) = MockControl::new(vec![MopidySnapshot::default()]);
        let mut fsm = EpisodeController::new(ctrl);
        let plan = EpisodePlan::new(
            "file:///bad.mp3".to_string(),
            80,
            Some(steps(&[(0, 20), (30, 60)])),
            Some(vec!["spotify:backup1".to_string(), "file:///beep.mp3".to_string()]),
        );
        fsm.fire("1".to_string(), &plan);
        // We are at step 0 / volume 20 (no time has elapsed).
        { calls.lock().unwrap().clear(); }

        // Playback goes Stopped within the grace window → advance to backup1.
        fsm.on_playback_state_changed(PlaybackState::Stopped);

        let log = { calls.lock().unwrap().clone() };
        assert_eq!(
            log,
            vec![
                "add(spotify:backup1)".to_string(),
                "play".into(),
                "set_repeat(true)".into(),
                "set_volume(20)".into(), // current step volume preserved
            ],
        );
        match fsm.state() {
            EpisodeState::Escalating {
                plan, step_index, ..
            } => {
                assert_eq!(*step_index, 0, "escalation step unchanged across fallback");
                assert_eq!(plan.fallback_index, 0);
            }
            other => panic!("expected Escalating, got {:?}", other),
        }
    }

    #[test]
    fn final_fallback_failure_ends_episode() {
        let (ctrl, _calls) = MockControl::new(vec![snap_playing("file:///music/a.mp3", 1000, 20)]);
        let mut fsm = EpisodeController::new(ctrl);
        // Chain with exactly one backup; after it fails, the episode ends.
        let plan = EpisodePlan::new(
            "file:///bad.mp3".to_string(),
            80,
            None,
            Some(vec!["spotify:only_backup".to_string()]),
        );
        fsm.fire("1".to_string(), &plan);

        // First failure → advance to the only backup.
        fsm.on_playback_state_changed(PlaybackState::Stopped);
        assert!(fsm.is_firing(), "still firing on first backup");

        // Second failure → chain exhausted → episode ends.
        fsm.on_playback_state_changed(PlaybackState::Stopped);
        assert!(matches!(fsm.state(), EpisodeState::Dismissed),
            "episode should be Dismissed after the chain is exhausted");
    }

    #[test]
    fn stopped_outside_grace_window_does_not_advance() {
        let (ctrl, _calls) = MockControl::new(vec![MopidySnapshot::default()]);
        let mut fsm = EpisodeController::new(ctrl);
        let plan = EpisodePlan::new(
            "file:///alarm.mp3".to_string(),
            80,
            None,
            Some(vec!["spotify:backup".to_string()]),
        );
        fsm.fire("1".to_string(), &plan);

        // Advance the source_start past the grace window by sleeping.
        std::thread::sleep(DEFAULT_GRACE_WINDOW + Duration::from_secs(1));
        fsm.on_playback_state_changed(PlaybackState::Stopped);
        assert!(fsm.is_firing(), "stopped outside grace window does not advance chain");
    }

    // ── Snooze ────────────────────────────────────────────────────────────

    #[test]
    fn snooze_preserves_step_and_restores_user_media() {
        let (ctrl, calls) = MockControl::new(vec![snap_playing("file:///music/a.mp3", 5000, 30)]);
        let mut fsm = EpisodeController::new(ctrl);
        let plan = EpisodePlan::new(
            "file:///alarm.mp3".to_string(),
            80,
            Some(steps(&[(0, 20), (30, 60), (60, 80)])),
            None,
        );
        fsm.fire("1".to_string(), &plan);

        // Advance escalation to step 2 (volume 80) before snoozing.
        let fire_time = match fsm.state() {
            EpisodeState::Escalating { fire_time, .. } => *fire_time,
            _ => unreachable!(),
        };
        fsm.advance_escalation(fire_time + Duration::from_secs(65));
        assert_eq!(match fsm.state() { EpisodeState::Escalating { step_index, .. } => *step_index, _ => unreachable!() }, 2);

        { calls.lock().unwrap().clear(); }
        fsm.snooze(DEFAULT_SNOOZE_DURATION);

        assert!(!fsm.is_firing(), "overlay hidden during Snoozing");
        assert!(fsm.is_active(), "episode still active during Snoozing");
        match fsm.state() {
            EpisodeState::Snoozing { step_index, .. } => assert_eq!(*step_index, 2),
            other => panic!("expected Snoozing, got {:?}", other),
        }
        // Restore-playback commands were issued (resume user media).
        let log = { calls.lock().unwrap().clone() };
        assert!(log.contains(&"add(file:///music/a.mp3)".to_string()));
        assert!(log.contains(&"set_volume(30)".to_string())); // snapshot volume restored
    }

    #[test]
    fn snooze_refire_resumes_from_preserved_step() {
        let (ctrl, calls) = MockControl::new(vec![snap_playing("file:///music/a.mp3", 5000, 30)]);
        let mut fsm = EpisodeController::new(ctrl);
        let plan = EpisodePlan::new(
            "file:///alarm.mp3".to_string(),
            80,
            Some(steps(&[(0, 20), (30, 60), (60, 80)])),
            None,
        );
        fsm.fire("1".to_string(), &plan);
        let fire_time = match fsm.state() {
            EpisodeState::Escalating { fire_time, .. } => *fire_time,
            _ => unreachable!(),
        };
        // Advance to step 2.
        fsm.advance_escalation(fire_time + Duration::from_secs(65));
        fsm.snooze(Duration::from_millis(10));
        { calls.lock().unwrap().clear(); }

        // Wait for snooze_until to pass, then tick.
        std::thread::sleep(Duration::from_millis(20));
        fsm.check_snooze_refire(Instant::now());

        assert!(fsm.is_firing(), "re-fire returns to Escalating");
        match fsm.state() {
            EpisodeState::Escalating { step_index, .. } => assert_eq!(*step_index, 2,
                "escalation resumes from preserved step, not step 0"),
            other => panic!("expected Escalating, got {:?}", other),
        }
        // Re-fire issued the alarm source + step-2 volume (80).
        let log = { calls.lock().unwrap().clone() };
        assert!(log.contains(&"add(file:///alarm.mp3)".to_string()));
        assert!(log.contains(&"set_volume(80)".to_string()));
    }

    #[test]
    fn snooze_while_idle_is_noop() {
        let (ctrl, calls) = MockControl::new(vec![]);
        let mut fsm = EpisodeController::new(ctrl);
        fsm.snooze(DEFAULT_SNOOZE_DURATION);
        assert!(matches!(fsm.state(), EpisodeState::Idle));
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn dismiss_from_snoozing_cancels_snooze() {
        let (ctrl, _calls) = MockControl::new(vec![snap_playing("file:///music/a.mp3", 1000, 20)]);
        let mut fsm = EpisodeController::new(ctrl);
        fsm.fire("1".to_string(), &EpisodePlan::simple("file:///alarm.mp3", 90));
        fsm.snooze(DEFAULT_SNOOZE_DURATION);
        assert!(matches!(fsm.state(), EpisodeState::Snoozing { .. }));

        fsm.dismiss();
        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
    }

    // ── Second-alarm serialization ───────────────────────────────────────

    #[test]
    fn fire_while_active_serializes() {
        let first = snap_playing("file:///music/a.mp3", 1000, 20);
        let second = snap_playing("file:///music/b.mp3", 5000, 50);
        let (ctrl, calls) = MockControl::new(vec![first, second]);
        let mut fsm = EpisodeController::new(ctrl);

        fsm.fire("1".to_string(), &EpisodePlan::simple("file:///alarm1.mp3", 90));
        { calls.lock().unwrap().clear(); }
        fsm.fire("2".to_string(), &EpisodePlan::simple("file:///alarm2.mp3", 80));

        match fsm.state() {
            EpisodeState::Escalating { alarm_id, .. } => assert_eq!(*alarm_id, "2".to_string()),
            other => panic!("expected Escalating(queued), got {:?}", other),
        }
        let log = { calls.lock().unwrap().clone() };
        // Restore of the first episode, then fire of the second.
        assert!(log.contains(&"add(file:///music/a.mp3)".to_string()));
        assert!(log.contains(&"add(file:///alarm2.mp3)".to_string()));
    }

    // ── Command failure correction ───────────────────────────────────────

    #[test]
    fn not_connected_failure_stays_active() {
        let (ctrl, calls) = MockControl::new(vec![MopidySnapshot::default()]);
        let mut fsm = EpisodeController::new(ctrl);
        fsm.fire("1".to_string(), &EpisodePlan::simple("file:///alarm.mp3", 90));
        { calls.lock().unwrap().clear(); }

        fsm.on_command_failure("playback.play", "NotConnected");
        assert!(fsm.is_active(), "episode stays active on NotConnected");
        assert!(calls.lock().unwrap().is_empty(), "no restore issued");
    }

    #[test]
    fn other_command_failure_dismisses() {
        let (ctrl, _calls) = MockControl::new(vec![snap_playing("file:///music/a.mp3", 1000, 20)]);
        let mut fsm = EpisodeController::new(ctrl);
        fsm.fire("1".to_string(), &EpisodePlan::simple("file:///alarm.mp3", 90));
        fsm.on_command_failure("playback.play", "RPCError: bad source");
        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
    }

    // ── Shutdown restore ──────────────────────────────────────────────────

    #[test]
    fn shutdown_restore_from_escalating() {
        let (ctrl, _calls) = MockControl::new(vec![snap_playing("file:///music/a.mp3", 1000, 20)]);
        let mut fsm = EpisodeController::new(ctrl);
        fsm.fire("1".to_string(), &EpisodePlan::simple("file:///alarm.mp3", 90));
        fsm.shutdown_restore();
        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
    }

    #[test]
    fn shutdown_restore_from_snoozing() {
        let (ctrl, _calls) = MockControl::new(vec![snap_playing("file:///music/a.mp3", 1000, 20)]);
        let mut fsm = EpisodeController::new(ctrl);
        fsm.fire("1".to_string(), &EpisodePlan::simple("file:///alarm.mp3", 90));
        fsm.snooze(DEFAULT_SNOOZE_DURATION);
        fsm.shutdown_restore();
        assert!(matches!(fsm.state(), EpisodeState::Dismissed));
    }

    #[test]
    fn shutdown_restore_idle_is_noop() {
        let (ctrl, calls) = MockControl::new(vec![]);
        let mut fsm = EpisodeController::new(ctrl);
        fsm.shutdown_restore();
        assert!(matches!(fsm.state(), EpisodeState::Idle));
        assert!(calls.lock().unwrap().is_empty());
    }

    // ── Episode span lifecycle ───────────────────────────────────────────

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
    fn ensure_global_max_level() {
        let _ = tracing_subscriber::registry().try_init();
    }

    #[test]
    fn episode_span_held_across_snooze_and_exits_on_dismiss() {
        let layer = SpanLifecycle::default();
        let entered = Arc::clone(&layer.entered);
        let exited = Arc::clone(&layer.exited);
        ensure_global_max_level();
        let _guard = tracing_subscriber::registry().with(layer).set_default();

        let (ctrl, _calls) = MockControl::new(vec![MopidySnapshot::default()]);
        let mut fsm = EpisodeController::new(ctrl);

        let before_enter = *entered.lock().unwrap();
        let before_exit = *exited.lock().unwrap();

        fsm.fire("42".to_string(), &EpisodePlan::simple("file:///alarm.mp3", 90));
        assert_eq!(*entered.lock().unwrap(), before_enter + 1);
        assert_eq!(*exited.lock().unwrap(), before_exit, "span must not exit on fire");

        // Snooze: span stays entered (held across Escalating ↔ Snoozing).
        fsm.snooze(DEFAULT_SNOOZE_DURATION);
        assert_eq!(*exited.lock().unwrap(), before_exit, "span must not exit on snooze");

        // Dismiss: span exits on restore completion.
        fsm.dismiss();
        assert_eq!(*exited.lock().unwrap(), before_exit + 1, "span exits on dismiss");
    }

    // ── EpisodePlan helpers ──────────────────────────────────────────────

    #[test]
    fn plan_volume_at_uses_steps_then_max() {
        let plan = EpisodePlan::new("u".to_string(), 80, Some(steps(&[(0, 20), (30, 60)])), None);
        assert_eq!(plan.volume_at(0), 20);
        assert_eq!(plan.volume_at(1), 60);
        // No-steps plan returns max_volume.
        let simple = EpisodePlan::simple("u", 40);
        assert_eq!(simple.volume_at(0), 40);
    }

    #[test]
    fn plan_has_next_fallback_logic() {
        let plan = EpisodePlan::new("p".to_string(), 80, None, Some(vec!["a".into(), "b".into()]));
        assert!(plan.has_next_fallback(), "primary → first fallback exists");
        assert_eq!(plan.next_fallback_index(), Some(0));

        let empty = EpisodePlan::simple("p", 80);
        assert!(!empty.has_next_fallback());
        assert_eq!(empty.next_fallback_index(), None);
    }
}

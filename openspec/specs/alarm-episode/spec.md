# alarm-episode Specification

## Purpose
TBD - created by archiving change slice-1-alarm-scheduling-and-episode. Update Purpose after archive.
## Requirements
### Requirement: Episode FSM with Idle, Firing, Dismissed states
The application SHALL own a single-threaded episode state machine on the main thread with states `Idle`, `Escalating` (capturing a snapshot, playing the alarm source, ramping volume through `escalation_steps`), `Snoozing` (user media restored, escalation suspended at a preserved step, waiting to re-fire), and `Dismissed` (restoring the snapshot). Only one episode SHALL be active at a time (`Escalating` or `Snoozing`); a second alarm firing mid-episode SHALL dismiss-and-restore the current episode then fire the queued alarm. The `episode` span SHALL be entered on `fire()` and held across `Escalating ↔ Snoozing` transitions; it SHALL exit only on restore completion in `dismiss()`/`shutdown_restore()`. `is_firing()` SHALL return `true` only while `Escalating`; `is_active()` SHALL return `true` while `Escalating` or `Snoozing`.

#### Scenario: Fire transitions Idle to Escalating
- **WHEN** the scheduler fires an alarm while the episode FSM is `Idle` and the alarm has `escalation_steps = [{after_secs=0, volume=20}, {after_secs=60, volume=80}]`
- **THEN** the FSM transitions to `Escalating`, captures a Mopidy snapshot, plays the alarm's `source_uri` with `repeat=true` at volume 20 (step 0), records `fire_time` and `source_start` as now, and the alarm episode UI is shown

#### Scenario: Fire with no escalation_steps plays at fixed max_volume
- **WHEN** an alarm with `escalation_steps = None` (or empty) and `max_volume = 40` fires
- **THEN** the FSM transitions to `Escalating`, plays the source at volume 40, and the volume never ramps (slice-1 behavior preserved)

#### Scenario: Second alarm during Escalating serializes
- **WHEN** a second alarm fires while the FSM is `Escalating`
- **THEN** the current episode is dismissed and restored, then the queued alarm fires (no overlap)

#### Scenario: Dismiss from Snoozing cancels the snooze
- **WHEN** a dismiss is invoked while the FSM is `Snoozing`
- **THEN** the FSM transitions to `Dismissed`, the pending snooze re-fire is cancelled, and the snapshot is restored

#### Scenario: Alarm fires without blocking the Slint event loop
- **WHEN** the episode FSM issues Mopidy commands (play, set_volume, set_repeat, tracklist_add)
- **THEN** the commands are sent over the command channel and the FSM does not block the Slint event loop awaiting replies; replies are drained on the next tick and correct the FSM state if needed

### Requirement: Mopidy snapshot capture and restore
The episode FSM SHALL capture a fresh Mopidy snapshot at fire time containing `{ uri, position_ms, was_playing, seekable, volume, repeat, shuffle }` and SHALL restore it on dismiss or shutdown. The snapshot SHALL be fresh per fire.

#### Scenario: Snapshot restored on dismiss
- **WHEN** the user was playing a track at position 120000ms volume 25 with shuffle on, an alarm fires, and is dismissed
- **THEN** Mopidy resumes the user's track at position 120000ms, volume 25, shuffle on, repeat restored to its pre-alarm value

#### Scenario: Snapshot with Mopidy down at fire time
- **WHEN** Mopidy is disconnected at fire time
- **THEN** the snapshot fields are `None`/defaults, the alarm episode still fires (audio silently fails, logged), and restore is a no-op (no hang, no crash)

#### Scenario: Fresh snapshot per fire
- **WHEN** an episode fires after a prior episode restored the user's (now advanced) session
- **THEN** the new snapshot reflects the current (advanced) Mopidy state, not the prior snapshot

### Requirement: Fixed-volume playback, no escalation
Slice 1 SHALL play the alarm source at the alarm's `max_volume` (a fixed value, default 40) for the duration of the episode. Escalation (volume ramp) is deferred to a later slice and SHALL NOT be implemented in slice 1.

#### Scenario: Alarm plays at fixed max_volume
- **WHEN** an alarm with `max_volume=40` fires
- **THEN** Mopidy volume is set to 40 and remains at 40 for the duration of the episode (no ramp)

#### Scenario: Pre-alarm volume restored on dismiss
- **WHEN** an alarm fires (setting volume to `max_volume`) and is dismissed
- **THEN** the user's pre-alarm volume (captured in the snapshot) is restored

### Requirement: Graceful degradation on source failure without fallback chain
Slice 1 SHALL NOT implement the fallback chain (deferred). When the alarm source fails to play (Mopidy connected but source fails within the grace window), the episode SHALL log the failure at `error!` and end the episode (dismiss-and-restore) after the grace window. The episode SHALL NOT hang or crash.

#### Scenario: Bad source URI ends episode after grace window
- **WHEN** an alarm fires with a source URI that Mopidy cannot play and playback goes to stopped within the grace window (default 8s)
- **THEN** the failure is logged at `error!`, the episode is dismissed (snapshot restored), and the user must re-arm the alarm

#### Scenario: Mopidy restart mid-episode does not crash
- **WHEN** Mopidy restarts while an episode is `Firing`
- **THEN** the connection-state signal transitions are logged, the episode remains dismissable, and the process does not crash (mid-episode re-issue of playback is a known limitation, not implemented in slice 1)

### Requirement: Alarm episode UI with tap-anywhere-to-dismiss
During a `Firing` episode, the alarm episode UI SHALL be shown exclusively above the navigation container, hiding the normal panels and disabling panel-swipe. The alarm UI SHALL show the clock face and SHALL dismiss the episode on a tap anywhere on the screen. No snooze button SHALL be present in slice 1 (snooze is deferred).

#### Scenario: Alarm UI hides normal panels during Firing
- **WHEN** the episode FSM transitions to `Firing`
- **THEN** the alarm episode UI is shown, the navigation container is hidden, and panel-swipe is disabled

#### Scenario: Tap anywhere dismisses
- **WHEN** the user taps anywhere on the alarm episode UI during `Firing`
- **THEN** a `Dismiss` command is sent to the episode FSM and the episode transitions to `Dismissed`

#### Scenario: No snooze control present
- **WHEN** the alarm episode UI is shown
- **THEN** no snooze button or snooze affordance is present (snooze is a later slice)

### Requirement: Shutdown restores snapshot before exit
The `shutdown_restore()` hook (a no-op in slice 0) SHALL be implemented by the episode controller to restore the snapshot before the process exits on SIGTERM/SIGINT mid-episode. The restore SHALL be bounded by a timeout (default 2s) so a hung Mopidy does not stall shutdown.

#### Scenario: SIGTERM mid-episode restores snapshot
- **WHEN** the process receives SIGTERM while an episode is `Firing`
- **THEN** the snapshot is restored (Mopidy volume/repeat/shuffle/tracklist resumed) before the process exits with code 0

#### Scenario: SIGTERM mid-episode with Mopidy down exits promptly
- **WHEN** the process receives SIGTERM while an episode is `Firing` and Mopidy is disconnected
- **THEN** the restore is a no-op and the process exits within the timeout (no indefinite wait)

### Requirement: Dev alarm seeding via TOML
Slice 1 SHALL consume a dev `alarms.toml` file (if present at the dev path) at boot, parsing it into `Alarm` records and upserting them into the database idempotently. The seeding SHALL be dev-only (logged with a "dev seed" marker) and SHALL be absent in production (the database is the sole source). This exists so slice 1 is end-to-end testable without a web UI.

#### Scenario: Dev seed upserts alarms at boot
- **WHEN** a dev `alarms.toml` is present at boot and contains two alarm entries
- **THEN** both alarms are upserted into the database by `id` (re-running boot does not duplicate them) and an `info!` log entry records the dev seeding

#### Scenario: Absent seed file is not an error
- **WHEN** the dev `alarms.toml` is absent at boot
- **THEN** no error is logged and the database is the sole source of alarms

### Requirement: Progressive volume escalation
While `Escalating`, the FSM SHALL advance the volume through the alarm's `escalation_steps` on each scheduler tick. The current step is the highest step whose `after_secs` is less than or equal to the elapsed time since `fire_time`. When the current step index advances, the FSM SHALL issue `playback.set_volume(step.volume)` (fire-and-forget). Escalation SHALL NOT reset across source fallbacks and SHALL hold at the final step's volume until dismiss, snooze, or fallback.

#### Scenario: Volume ramps through steps as time elapses
- **WHEN** an alarm fires with steps `[{0s,20},{30s,60},{60s,80}]` and 31 s have elapsed since `fire_time`
- **THEN** the FSM has issued `set_volume(20)` at fire, `set_volume(60)` once elapsed crossed 30 s, and issues no further `set_volume` until elapsed crosses 60 s

#### Scenario: Escalation does not re-issue the same volume
- **WHEN** two consecutive scheduler ticks both compute the same `step_index`
- **THEN** no `set_volume` command is issued on the second tick (idempotent advance)

#### Scenario: Escalation continues uninterrupted across a fallback
- **WHEN** the FSM advances to fallback source 1 at elapsed 45 s (step index for 30 s) and the next tick runs at 50 s
- **THEN** the `fire_time` and `step_index` are unchanged across the fallback and the volume continues from the current step (no reset to step 0)

### Requirement: Multi-stage source fallback chain
When the alarm's primary `source_uri` fails to play (Mopidy connected but playback goes to `Stopped` within the grace window after the current source began), the FSM SHALL advance to the next URI in the alarm's `fallback_chain`. On advance, the FSM SHALL issue `tracklist.add(next_uri)`, `playback.play`, `tracklist.set_repeat(true)`, and `playback.set_volume(current_step_volume)`, reset `source_start` to now, and increment `fallback_index`. `fire_time` and `step_index` SHALL be unchanged (escalation uninterrupted). When the chain is exhausted (no more fallbacks), the FSM SHALL log the failure at `error!` and dismiss-and-restore the episode. An alarm with no `fallback_chain` (empty/`None`) SHALL end the episode on the first source failure (slice-1 behavior).

#### Scenario: Primary source failure advances to first fallback
- **WHEN** an alarm with `fallback_chain = ["spotify:backup1", "file:///beep.mp3"]` is `Escalating` and playback goes to `Stopped` within the grace window after `source_start`
- **THEN** the FSM issues `tracklist.add("spotify:backup1")`, `play`, `set_repeat(true)`, `set_volume(<current step volume>)`, increments `fallback_index` to 0, resets `source_start`, and remains `Escalating` with `fire_time`/`step_index` unchanged

#### Scenario: Final fallback failure ends the episode
- **WHEN** the last fallback in the chain goes to `Stopped` within the grace window
- **THEN** the FSM logs `error!`, dismisses-and-restores the episode, and the user must re-arm the alarm

#### Scenario: Stopped outside the grace window does not advance the chain
- **WHEN** playback goes to `Stopped` after the grace window has elapsed since `source_start`
- **THEN** the FSM does NOT advance the fallback chain (the failure is not a source-startup failure)

### Requirement: Snooze suspends escalation and resumes from the preserved step
A snooze invoked while `Escalating` SHALL transition the FSM to `Snoozing`, preserving the current `step_index`, the `snapshot`, the `EpisodePlan`, and the `episode` span. The FSM SHALL set `snooze_until = now + duration` and issue the restore-playback commands (resume snapshot playback, restore repeat/shuffle/volume) so user media resumes. While `Snoozing`, `is_firing()` SHALL be `false` (the alarm overlay is hidden). On the first scheduler tick where `now >= snooze_until`, the FSM SHALL transition back to `Escalating`: re-issue `tracklist.add(source_uri)`, `playback.play`, `tracklist.set_repeat(true)`, set `fire_time = now - steps[step_index].after_secs`, issue `set_volume(steps[step_index].volume)`, and reset `source_start = now`. Escalation SHALL resume from the preserved `step_index`, not from step 0. A snooze invoked while not `Escalating` SHALL be a logged no-op.

#### Scenario: Snooze preserves step and restores user media
- **WHEN** an alarm is `Escalating` at step index 2 (volume 80) and snooze is invoked with a 9-minute duration
- **THEN** the FSM transitions to `Snoozing` with `step_index = 2`, resumes the user's snapshot playback, hides the alarm overlay, and sets `snooze_until` to 9 minutes from now

#### Scenario: Snooze re-fire resumes escalation from the preserved step
- **WHEN** the FSM is `Snoozing` at `step_index = 2` and a scheduler tick observes `now >= snooze_until`
- **THEN** the FSM transitions to `Escalating`, replays the alarm source, sets `fire_time` so elapsed is at the step-2 boundary, issues `set_volume(80)`, and subsequent escalation advances from step 2 (not step 0)

#### Scenario: Snooze while Idle or Snoozing is a no-op
- **WHEN** snooze is invoked while the FSM is `Idle`, `Snoozing`, or `Dismissed`
- **THEN** the FSM logs the no-op and does not change state

### Requirement: Alarm episode UI with tap-to-dismiss and snooze
During an `Escalating` episode, the alarm episode UI SHALL be shown exclusively above the navigation container, hiding the normal panels and disabling panel-swipe. The alarm UI SHALL show the clock face, SHALL dismiss the episode on a tap anywhere on the screen, and SHALL provide a Snooze button that invokes snooze. The Snooze button's tap SHALL NOT also trigger dismiss (propagation is stopped). During `Snoozing`, `Idle`, and `Dismissed`, the alarm overlay SHALL be hidden (snooze re-fire re-shows it).

#### Scenario: Alarm UI hides normal panels during Escalating
- **WHEN** the episode FSM transitions to `Escalating`
- **THEN** the alarm episode UI is shown, the navigation container is hidden, and panel-swipe is disabled

#### Scenario: Tap anywhere dismisses
- **WHEN** the user taps anywhere on the alarm episode UI outside the Snooze button during `Escalating`
- **THEN** a `Dismiss` command is sent to the episode FSM and the episode transitions to `Dismissed`

#### Scenario: Snooze button invokes snooze without dismissing
- **WHEN** the user taps the Snooze button during `Escalating`
- **THEN** a snooze is invoked (FSM transitions to `Snoozing`) and the dismiss handler is NOT triggered

#### Scenario: Alarm overlay hidden during Snoozing
- **WHEN** the FSM transitions to `Snoozing`
- **THEN** the alarm overlay is hidden and the normal Clock panel is visible until the snooze re-fires


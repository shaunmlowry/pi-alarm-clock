## ADDED Requirements

### Requirement: Episode FSM with Idle, Firing, Dismissed states
The application SHALL own a single-threaded episode state machine on the main thread with states `Idle`, `Firing` (capturing a snapshot, playing the alarm source looping at `max_volume`), and `Dismissed` (restoring the snapshot). Only one episode SHALL be active at a time; a second alarm firing mid-episode SHALL dismiss-and-restore the current episode then fire the queued alarm. The `episode` span (defined in slice 0, unused) SHALL be entered on `fire()` and exited on restore completion.

#### Scenario: Fire transitions Idle to Firing
- **WHEN** the scheduler fires an alarm while the episode FSM is `Idle`
- **THEN** the FSM transitions to `Firing`, captures a Mopidy snapshot, plays the alarm's `source_uri` with `repeat=true` at `max_volume`, and the alarm episode UI is shown

#### Scenario: Dismiss transitions Firing to Dismissed and restores
- **WHEN** a dismiss is invoked while the FSM is `Firing`
- **THEN** the FSM transitions to `Dismissed`, restores the Mopidy snapshot, and the alarm episode UI is hidden

#### Scenario: Second alarm during Firing serializes
- **WHEN** a second alarm fires while the FSM is `Firing`
- **THEN** the current episode is dismissed and restored, then the queued alarm fires (no overlap)

#### Scenario: Alarm fires without blocking the Slint event loop
- **WHEN** the episode FSM issues Mopidy commands (play, set_volume, set_repeat)
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

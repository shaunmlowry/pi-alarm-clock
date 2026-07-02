## ADDED Requirements

### Requirement: Scheduler tick span active
The `scheduler_tick` span (defined in slice 0 as unused) SHALL be entered on each scheduler tick with the tick's work instrumented under it. The span SHALL carry structured fields (e.g. `alarms_evaluated`, `fired`) to journald.

#### Scenario: Scheduler tick emits span to journald
- **WHEN** the scheduler tick fires
- **THEN** a `scheduler_tick` span is entered and `journalctl` shows the span with its fields for that tick

### Requirement: Episode span active during firing
The `episode` span (defined in slice 0 as unused) SHALL be entered on `fire()` and exited on restore completion (dismiss or shutdown restore). The span SHALL carry structured fields (e.g. `alarm_id`, `source_uri`) to journald.

#### Scenario: Episode span covers fire to restore
- **WHEN** an alarm fires and is later dismissed
- **THEN** a single `episode` span (with `alarm_id` field) covers the period from `fire()` to restore completion in `journalctl`

### Requirement: shutdown_restore implemented by episode controller
The `shutdown_restore()` hook (a no-op in slice 0) SHALL be implemented by the episode controller to restore the Mopidy snapshot before the process exits on SIGTERM/SIGINT mid-episode. The shutdown handler SHALL call this hook before draining the command channel and exiting. The restore SHALL be bounded by a timeout (default 2s) so a hung Mopidy does not stall systemd's shutdown window.

#### Scenario: shutdown_restore restores snapshot then exits
- **WHEN** the process receives SIGTERM while an episode is `Firing`
- **THEN** the episode controller restores the snapshot (volume/repeat/shuffle/tracklist resumed), bounded by the timeout, and the process exits with code 0

#### Scenario: shutdown_restore is a no-op when no episode active
- **WHEN** the process receives SIGTERM while the episode FSM is `Idle`
- **THEN** `shutdown_restore()` returns immediately (no Mopidy commands issued) and the process exits promptly

## 1. Scheduler tick & spans

- [ ] 1.1 Add the `scheduler` module on main: a `slint::Timer`-driven tick (default 5s interval) that enters the `scheduler_tick` span, calls `AlarmStore::due_alarms(now)`, and invokes the episode FSM's `fire()` for each due alarm; recompute the alarm's `next_fire` after firing.
- [ ] 1.2 Implement the missed-alarm-on-boot policy: if `now > next_fire` for an alarm at the first tick (boot catch-up), do NOT fire; advance `next_fire` to the next occurrence after `now` and log `info!` with the skip.
- [ ] 1.3 Activate the `scheduler_tick` span with structured fields (`alarms_evaluated`, `fired`); verify a tick emits the span to journald/logs.
- [ ] 1.4 Unit-test the tick: a due alarm fires; a not-yet-due alarm does not; `Local::now()` is re-read each tick (mock the clock).

## 2. Schedule & next-fire computation

- [x] 2.1 Add dependencies `rrule`, `chrono-tz`, and make `chrono` explicit in `alarm-clock/Cargo.toml`.
- [x] 2.2 Implement the `Schedule` struct wrapping an `rrule` + `time_local` + `timezone`, with `next_fire(after: DateTime<Tz>) -> Option<DateTime<Tz>>` evaluating in the alarm's stored IANA timezone.
- [x] 2.3 Implement the preset → RRULE mapping: `Once` (no rrule), `Daily` (`FREQ=DAILY`), `Weekdays` (`FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR`), `Weekends` (`FREQ=WEEKLY;BYDAY=SA,SU`), `Specific-days` (`FREQ=WEEKLY;BYDAY=<selected>`). Accept (parse) complex RRULE but do not construct it.
- [x] 2.4 Unit-test `next_fire` across a DST spring-forward and fall-back boundary (daily alarm fires at the same wall-clock time on both sides).
- [x] 2.5 Unit-test a `Once` alarm returns its `once_at` time then `None`; a `Weekdays` alarm skips Sat/Sun.

## 3. Alarm data model & persistence

- [x] 3.1 Write migration `v2`: create the `alarms` table (columns per design D3); bump `user_version` to `2`; leave `v1`'s `schema_meta`/`kv_config` untouched.
- [x] 3.2 Verify migration `v2` is idempotent (starting at `user_version=2` skips re-application; `alarms` intact).
- [x] 3.3 Implement the `Alarm` model (serde struct matching the columns) and `AlarmStore` (owned by main, `&Connection`) with `list()`, `get(id)`, `upsert(alarm)`, `delete(id)`, `set_enabled(id, bool)`.
- [x] 3.4 Implement `AlarmStore::recompute_next_fires(now)` that recomputes `next_fire` from each alarm's rule and writes it back in a single transaction.
- [x] 3.5 Verify each `AlarmStore` mutation is a single transaction (rollback on partial failure; `error!` logged; in-memory state authoritative). Unit-test upsert idempotency by `id`.

## 4. Mopidy playback method surface

- [x] 4.1 Add typed wrappers (request struct + `call` + typed reply, slice-0 shape) for `playback.play(uri)`, `playback.pause`, `playback.resume`, `playback.stop`.
- [x] 4.2 Add typed wrappers for `playback.set_volume` (clamp 0..100), `playback.get_state` (`PlaybackState` enum: Playing/Paused/Stopped), `playback.get_time_position` (`u32` ms).
- [x] 4.3 Add typed wrappers for `tracklist.add(uris)`, `tracklist.set_repeat(bool)`, `tracklist.set_shuffle(bool)` (alias `set_random` if the Mopidy version exposes only that).
- [x] 4.4 Implement `MopidyClientError::NotConnected` (`thiserror` variant): typed calls return this error immediately (no hang) when the client is `Disconnected`/`BackingOff`.
- [x] 4.5 Unit-test the typed wrappers (serialization shape + reply deserialization) against fixture JSON-RPC payloads; unit-test the `NotConnected` path.

## 5. Episode FSM: snapshot, fire, restore

- [x] 5.1 Define the `EpisodeController` on main with states `Idle`, `Firing` (holding `alarm_id` + `snapshot`), `Dismissed`; enter the `episode` span on `fire()`, exit on restore.
- [x] 5.2 Define `MopidySnapshot { uri, position_ms, was_playing, seekable, volume, repeat, shuffle }`.
- [x] 5.3 Implement `fire()`: capture the snapshot (batch the `get_state`/`get_time_position`/`get_volume`/`tracklist.get_repeat`/`get_shuffle`/tracklist reads; bound by a 1s wait, proceed with `None`/defaults on timeout or `NotConnected`)); then `tracklist.add(source_uri)` + `playback.play` + `tracklist.set_repeat(true)` + `playback.set_volume(max_volume)`.
- [x] 5.4 Implement `dismiss()`: transition to `Dismissed`, restore the snapshot (`set_repeat`, `set_shuffle`, `set_volume`; if `uri` Some and `was_playing`: `tracklist.add` + `play` + seek `position_ms`; if not playing: `stop`; if `uri` None: restore volume/repeat/shuffle only).
- [x] 5.5 Implement the optimistic-transition-with-correction pattern: the FSM does not block awaiting replies; the reply drain corrects FSM state on failure (logged).
- [x] 5.6 Implement second-alarm serialization: a fire while `Firing` dismisses-and-restores the current episode then fires the queued alarm.
- [x] 5.7 Unit-test the FSM: fire → Firing; dismiss → Dismissed → restore issued; snapshot fresh per fire.

## 6. Graceful degradation & shutdown restore

- [x] 6.1 Implement the Mopidy-down-at-fire path: snapshot is `None`/defaults; playback commands return `NotConnected` (logged); episode stays `Firing` and dismissable; restore is a no-op.
- [x] 6.2 Implement source-failure end-of-episode: if playback goes to `stopped`/tracklist-ended within the grace window (default 8s), log `error!`, dismiss-and-restore, end the episode (no fallback chain in slice 1).
- [x] 6.3 Implement mid-episode Mopidy restart handling: log connection-state transitions; do not crash; episode stays dismissable (no mid-episode re-issue in slice 1).
- [x] 6.4 Implement `shutdown_restore()` on `EpisodeController`: if `Firing`, restore the snapshot before exit; if `Idle`, no-op. Bound the restore by a 2s timeout.
- [x] 6.5 Wire the shutdown handler (slice 0) to call `EpisodeController::shutdown_restore()` before draining the Cmd channel and exiting.
- [x] 6.6 Unit-test `shutdown_restore()`: `Firing` → restore issued; `Idle` → no-op; timeout exits promptly.

## 7. Alarm episode UI

- [x] 7.1 Create `AlarmPanel.slint` rendered above the navigation container when the episode is `Firing`: shows the clock face (reusing the Clock panel's theme-seam properties, hardcoded values) and is tap-anywhere-to-dismiss; no snooze button.
- [x] 7.2 Expose the episode state (`Firing`/`Idle`) to Slint; on `Firing`, hide the navigation container and disable panel-swipe; on `Idle`, restore.
- [x] 7.3 Wire the dismiss tap handler to send a `Dismiss` command to the `EpisodeController` on main.
- [x] 7.4 Verify (cargo check / manual): during `Firing`, normal panels are hidden, swipe is disabled, and a tap dismisses; no snooze affordance is present.

## 8. Dev alarm seeding

- [ ] 8.1 Define the dev `alarms.toml` schema (serde struct mirroring `Alarm`: `id`, `enabled`, `name`, `preset`, `days`, `time`, `timezone`, `source_uri`, `max_volume`, `once_at`) and a sample file committed for dev.
- [ ] 8.2 Implement the boot seeding: if the dev `alarms.toml` is present, parse and upsert each alarm by `id` (idempotent); log `info!` with a "dev seed" marker. If absent, no error (database is the sole source).
- [ ] 8.3 Verify seeding is idempotent (re-running boot does not duplicate alarms) and dev-only (production path skips it).

## 9. Integration & acceptance

- [x] 9.1 Wire the scheduler tick, episode FSM, and alarm episode UI into `main.rs` (replacing slice-0 no-ops); ensure `cargo check --workspace` and `cargo test --workspace` pass.
- [ ] 9.2 End-to-end (dev Mopidy with a playable URI): seed a daily alarm due in ~1 minute; verify it fires, plays the source looping at `max_volume`, and on tap dismisses and restores the prior Mopidy session (track, position, volume, repeat, shuffle).
- [ ] 9.3 End-to-end Mopidy-down: stop Mopidy before the fire time; verify the alarm still fires (audio silently fails, logged), the UI is dismissable, and the process does not hang or crash.
- [ ] 9.4 End-to-end shutdown mid-episode: fire an alarm, `systemctl stop` (or SIGTERM); verify the snapshot is restored before exit (exit code 0, restore logged).
- [x] 9.5 End-to-end DST: seed a daily alarm across a DST boundary (mock or real); verify it fires at the same wall-clock local time on both sides.
- [ ] 9.6 Verify on the Pi: `journalctl` shows `scheduler_tick` and `episode` spans with structured fields; `user_version=2` after migration; `alarms` table round-trips.

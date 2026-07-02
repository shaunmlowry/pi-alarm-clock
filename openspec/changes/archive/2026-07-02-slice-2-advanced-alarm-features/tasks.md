## 1. Data model & persistence (alarm-clock/src/alarm_store.rs, alarm-clock/src/database.rs)

- [x] 1.1 Add `EscalationStep { after_secs: u64, volume: u8 }` (serde struct) and `escalation_steps: Option<Vec<EscalationStep>>` + `fallback_chain: Option<Vec<String>>` fields to `Alarm` in `alarm_store.rs`.
- [x] 1.2 Add migration `v3` in `database.rs`: `ALTER TABLE alarms ADD COLUMN escalation_steps TEXT` and `ADD COLUMN fallback_chain TEXT` in a single transaction; bump `user_version` to `3`.
- [x] 1.3 Update `AlarmStore::list`/`get`/`upsert` (and `row_to_alarm`) to round-trip the two new columns (JSON via `serde_json`); sort `escalation_steps` ascending by `after_secs` on write.
- [x] 1.4 Unit-test: upsert+read-back of an alarm with escalation+fallback preserves order/values; a slice-1 alarm round-trips as `None`; store re-sorts unsorted steps.
- [x] 1.5 Unit-test migration `v3`: fresh DB reaches `user_version=3` with the new columns; a `v2` DB with existing rows upgrades preserving data (new columns NULL); idempotent at `v3`.

## 2. Episode FSM — Escalating/Snoozing states (alarm-clock/src/episode.rs)

- [x] 2.1 Replace `Firing` with `Escalating` and add `Snoozing` in `EpisodeState`; introduce `EpisodePlan { source_uri, fallback_chain, fallback_index, escalation_steps, max_volume }` carried in both active states.
- [x] 2.2 Add a second `Instant` clock `source_start` (reset on fire and on each fallback advance) to `Escalating`, distinct from `fire_time` (escalation elapsed). Keep `is_firing()` true only for `Escalating`; add `is_active()` true for `Escalating` or `Snoozing`.
- [x] 2.3 Refactor `fire()` to build an `EpisodePlan` from the alarm's `source_uri`/`fallback_chain`/`escalation_steps`/`max_volume`, capture the snapshot, play the source at step 0's volume (or `max_volume` if no steps), and enter `Escalating`.
- [x] 2.4 Implement `advance_escalation(now: Instant)`: compute the highest `step_index` whose `after_secs <= now - fire_time`; if it advanced, issue `playback_set_volume(step.volume)`; idempotent (no command when unchanged). Hold at the final step.
- [x] 2.5 Implement `snooze(duration: Duration)`: from `Escalating` only, transition to `Snoozing` preserving `step_index`/`snapshot`/`plan`/span, set `snooze_until = now + duration`, and issue restore-playback commands (resume snapshot, restore repeat/shuffle/volume). No-op (logged) otherwise.
- [x] 2.6 Implement `check_snooze_refire(now: Instant)`: if `Snoozing` and `now >= snooze_until`, transition back to `Escalating`, replay source (`tracklist.add`+`play`+`set_repeat(true)`), set `fire_time = now - steps[step_index].after_secs`, issue `set_volume(steps[step_index].volume)`, reset `source_start`.
- [x] 2.7 Update `dismiss()` and `shutdown_restore()` to restore from `Escalating` **or** `Snoozing` (cancel pending snooze); keep the `episode` span exit on restore completion.
- [x] 2.8 Update `on_playback_state_changed(Stopped)` for `Escalating`: within the grace window of `source_start`, advance `fallback_index` if a fallback remains (re-issue `tracklist.add`+`play`+`set_repeat(true)`+`set_volume(current_step_volume)`, reset `source_start`, keep `fire_time`/`step_index`); else dismiss-and-restore (chain exhausted).
- [x] 2.9 Update `on_command_failure` and `on_connection_state_change` to apply to `Escalating`/`Snoozing` (NotConnected stays best-effort active; non-NotConnected corrects by dismiss).
- [x] 2.10 Unit-test: fire→Escalating at step 0; escalation ramp across ticks (idempotent); no-steps alarm stays at `max_volume`; second-alarm serialization; dismiss from Snoozing cancels snooze.
- [x] 2.11 Unit-test: snooze preserves `step_index`, restores user media, hides overlay; re-fire resumes from preserved step (not step 0); snooze while Idle/Snoozing is a no-op.
- [x] 2.12 Unit-test: primary-source failure advances to fallback with `fire_time`/`step_index` unchanged; final-fallback failure ends episode; stopped outside grace window does not advance.

## 3. Scheduler tick integration (alarm-clock/src/scheduler.rs, alarm-clock/src/main.rs)

- [x] 3.1 Add `fn on_tick(&mut self, now: DateTime<Local>)` with a default no-op to the `EpisodeFsm` trait; `NoopEpisodeFsm` and the scheduler-test mock use the default.
- [x] 3.2 Call `self.fsm.on_tick(now)` in `Scheduler::tick` after the due-alarm fire loop.
- [x] 3.3 Implement `EpisodeFsmAdapter::on_tick` (main.rs): lock the episode controller, call `advance_escalation(Instant::now())` and `check_snooze_refire(Instant::now())`.
- [x] 3.4 Update `EpisodeFsmAdapter::fire` to read the alarm's new `escalation_steps`/`fallback_chain`/`max_volume` and pass them into `EpisodeController::fire` (extend the `fire` signature).
- [x] 3.5 Unit-test: a scheduler tick with an `Escalating` mock FSM calls `on_tick`; a tick with `NoopEpisodeFsm` is a no-op; `on_tick` while Idle issues no commands.

## 4. Mopidy seam — no new surface (verify only)

- [x] 4.1 Confirm `PlaybackApi::playback_set_volume` / `playback_get_state` exist in `mopidy-client/src/methods.rs` (slice 1) and that `ChannelMopidyControl` in `main.rs` already implements every seam method escalation/fallback/snooze requires (`playback_set_volume`, `tracklist_add`, `playback_play`, `tracklist_set_repeat`, `tracklist_set_shuffle`, `playback_stop`, `playback_seek`, `capture_snapshot`). No new transport code. Document this in the module doc-comment.

## 5. Alarm episode UI — snooze affordance (alarm-clock/AlarmPanel.slint, alarm-clock/ui.slint)

- [x] 5.1 Add a Snooze button (Rectangle+Text+TouchArea) to `AlarmPanel.slint`; it emits a new `snooze-requested` callback and stops propagation so the dismiss TouchArea beneath does not also fire.
- [x] 5.2 Add `callback snooze-requested()` to `AppWindow` in `ui.slint`; forward from `AlarmPanel`; keep `AlarmPanel` visibility gated on `episode-firing` (true only during `Escalating`).
- [x] 5.3 In `main.rs`, wire `app_window.on_snooze_requested` to `episode_ctl.lock().snooze(DEFAULT_SNOOZE_DURATION)` (9 min constant); the drain timer reflects `is_firing()` (false during `Snoozing`) into `episode-firing`.

## 6. Dev seed — escalation_steps & fallback_chain (alarm-clock/src/seed.rs, alarms.toml)

- [x] 6.1 Add optional `escalation_steps: Option<Vec<EscalationStep>>` and `fallback_chain: Option<Vec<String>>` to `SeedAlarm`; map them into the `Alarm` row in `to_alarm()`.
- [x] 6.2 Add a dev seed entry in `alarms.toml` exercising `escalation_steps` + `fallback_chain` (and leave existing entries unchanged).
- [x] 6.3 Unit-test: a seed entry with the new fields upserts and round-trips; a seed entry without them seeds as `None`; idempotent re-seed.

## 7. End-to-end build & regression

- [x] 7.1 `cargo build` succeeds for the workspace; `cargo test` for `alarm-clock` and `mopidy-client` is green (slice-1 tests still pass).
- [x] 7.2 Verify the slice-1 fixed-volume / no-fallback / no-snooze path is unchanged when an alarm has `escalation_steps=None` and `fallback_chain=None`.

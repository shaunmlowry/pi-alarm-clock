## 1. Persistence (alarm-clock/src/database.rs, alarm_store.rs)

- [ ] 1.1 Migration `v5`: `ALTER TABLE alarms ADD COLUMN snooze_minutes INTEGER NOT NULL DEFAULT 10`; `ADD COLUMN max_snoozes INTEGER NOT NULL DEFAULT 3`; bump `user_version` to 5.
- [ ] 1.2 Round-trip `snooze_minutes`/`max_snoozes` in `Alarm`/`AlarmStore`/`row_to_alarm`.
- [ ] 1.3 Unit-test: v5 migration (fresh + upgrade + idempotent); snooze fields round-trip; defaults on upgrade.

## 2. Episode FSM (alarm-clock/src/episode.rs)

- [ ] 2.1 Add `snooze_count: u32` to `Escalating`/`Snoozing` (reset on `fire`); `snooze(duration, max)` increments and refuses at cap.
- [ ] 2.2 Bundled beep: resolve asset path at boot; `EpisodePlan::new` appends it as the final fallback.
- [ ] 2.3 Re-fire path: re-capture snapshot, replay primary, reset `fallback_index`/`source_start`, keep preserved `step_index`/`fire_time` adjustment.
- [ ] 2.4 Chain-exhaustion: call `DisplayController::force_full_strobe()` (slice 4) instead of `dismiss()`; stay `Escalating` until dismiss.
- [ ] 2.5 Unit-test: per-alarm duration; snooze-hidden-at-cap (expose `snooze_exhausted()`); re-fire fresh snapshot + reset chain + preserved step; bundled beep appended; chain-exhaustion forces visual.

## 3. UI & wiring (AlarmPanel.slint, main.rs, seed.rs)

- [ ] 3.1 Hide the Snooze button when `snooze_count >= max_snoozes` (expose a `snooze-available` property from the FSM via the drain timer).
- [ ] 3.2 Pass per-alarm `snooze_minutes`/`max_snoozes` into the `EpisodePlan`/`snooze` call from `EpisodeFsmAdapter`.
- [ ] 3.3 Add `snooze_minutes`/`max_snoozes` to dev `alarms.toml` `SeedAlarm` (optional, defaults applied).
- [ ] 3.4 `cargo build` + `cargo test` green; slice 0–4 tests unaffected.

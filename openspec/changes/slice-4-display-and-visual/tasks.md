## 1. DisplayController & precedence (alarm-clock/src/display.rs)

- [ ] 1.1 Implement `DisplayController` on main owning sysfs `brightness`/`bl_power` (boot-time discovery; no-op fallback if absent).
- [ ] 1.2 Implement `DisplayPolicy` precedence resolution (`Strobe > BedtimeOff > Override > Dynamic`); only the winner writes hardware.
- [ ] 1.3 Implement bedtime windows (weekday/weekend, cross-midnight `end < start` inference), `bl_power` off, wake-on-touch 10 s idle timer (touch-reset, Settings/quick-controls suspend/re-arm).
- [ ] 1.4 Implement alarm-suspends-bedtime → on dismiss power off + arm 10 s grace.
- [ ] 1.5 Implement dynamic brightness target + ~120 s interpolator (default 60% until weather slice).
- [ ] 1.6 Implement user override (30 min timeout, doesn't defeat bedtime).
- [ ] 1.7 Unit-test: precedence (strobe masks bedtime; override doesn't defeat bedtime); cross-midnight window; wake-timer reset/suspend/re-arm; interpolator ramp.

## 2. Visual alarm (alarm-clock/src/visual.rs, episode.rs)

- [ ] 2.1 Define `VisualConfig` (`Off` | `On { brightness, pulse_period, color }`) with serde.
- [ ] 2.2 Implement `Strobe` state in `DisplayController` (floor/ceil/period, 10 s `Pending`→`Active` on arm, `brightness`-only modulation).
- [ ] 2.3 Implement forced full-brightness terminal fallback (`force_full`).
- [ ] 2.4 Extend `MopidySnapshot` with `backlight_level: u8`; capture on `fire`, restore on `dismiss`/`shutdown_restore`.
- [ ] 2.5 Wire episode FSM → `DisplayController` (arm strobe on fire when `On`; cancel on dismiss; force_full on chain exhaustion).
- [ ] 2.6 Unit-test: 10 s delay; chain-exhaustion forces full strobe; backlight restored on dismiss/shutdown.

## 3. Persistence (alarm-clock/src/database.rs, alarm_store.rs, config.rs)

- [ ] 3.1 Migration `v4`: `ALTER TABLE alarms ADD COLUMN visual_config TEXT`; bump `user_version` to 4.
- [ ] 3.2 Round-trip `visual_config` JSON in `Alarm`/`AlarmStore` (None = Off).
- [ ] 3.3 Persist bedtime windows + brightness floor in `kv_config`.
- [ ] 3.4 Unit-test: v4 migration (fresh + upgrade + idempotent); visual_config round-trip; bedtime persistence.

## 4. Wiring & verification (alarm-clock/src/main.rs)

- [ ] 4.1 Construct `DisplayController`, wire into scheduler tick + episode + touch path (wake-on-touch).
- [ ] 4.2 Resolve `Follow-Bedtime` theme mode against `DisplayController::is_bedtime(now)` (replaces slice-3 heuristic).
- [ ] 4.3 `cargo build` + `cargo test` green; slice 0–3 tests unaffected.

## 5. Systemd / hardware doc

- [ ] 5.1 Document the sysfs backlight path expectation and `fake-hwclock` interaction in README.

## Context

The PRD mandates a display that is never wrongly lit and never silently fails an alarm. Slices 0–2 have no display control and slice 2's fallback chain ends in a silent dismiss. Slice 4 introduces one backlight controller with a precedence stack over four writers, bedtime power management with wake-on-touch, dynamic brightness, and the visual-alarm strobe that is the terminal fallback.

## Goals / Non-Goals

**Goals:** single `DisplayController` owning sysfs; precedence stack; bedtime (weekday/weekend, cross-midnight, wake-on-touch 10 s, alarm-suspends-bedtime); dynamic brightness from `shortwave_radiation` (default 60% until weather slice); user override (30 min, doesn't defeat bedtime); `VisualConfig` strobe 10 s after fire; forced full-brightness terminal fallback; `backlight_level` in the snapshot.

**Non-Goals:** gradual pre-bedtime dimming (v2); follow-ambient (v2); permanent brightness offset (v2); the quick-controls overlay UI (slice 7 — this slice defines the brightness-slider contract); per-weekday bedtime (v2).

## Decisions

### D1. One DisplayController on main, precedence-resolved each tick
A `DisplayController` on main owns the sysfs handles and, each scheduler tick (5 s) + on event (touch, fire, dismiss), computes the effective `DisplayPolicy` from four inputs ranked: `Strobe > BedtimeOff > Override > Dynamic`. Only the winner writes hardware. The FSM never touches sysfs — it calls `DisplayController::arm_strobe()`/`cancel_strobe()`/`force_full()`. This single-writer design prevents the "screen on at 3am" risk the PRD flags as subtle.

**Rationale.** Multiple writers racing the same sysfs files is exactly the bug class the PRD warns about. One owner + precedence is the simplest provably-correct model.

### D2. bl_power vs brightness split
`bl_power` (binary on/off) for bedtime power state and wake transitions; `brightness` (0..max) for strobe/override/dynamic. The PRD forbids strobing `bl_power` (hardware wear, visible flicker artifacts). Strobe toggles `brightness` between floor and `visual_brightness` while `bl_power` stays on.

### D3. Wake-on-touch idle timer as a DisplayController state
The 10 s wake timer is a `DisplayController` state (`Wake { deadline: Instant }`), not a separate timer. Touches reset `deadline`; Settings/quick-controls suspend by setting a `paused` flag; exit re-arms. Alarm-during-bedtime transitions to a `Suspended` state that holds `bl_power` on for the episode, then `Off → Wake{+10s}` on dismiss.

### D4. Dynamic brightness interpolation
The 30-min weather tick sets a *target* brightness; a per-tick (5 s) interpolator ramps the *actual* brightness toward the target over ~120 s (24 ticks). Strobe (0.5 s steps) writes directly, bypassing the interpolator (precedence). Until the weather slice lands, target = 60%.

### D5. Visual strobe scheduling
Strobe is a `DisplayController` state `Strobe { floor, ceil, period, started: Instant }`. The 10 s delay is a pending state armed on `fire`; the controller's tick transitions `Strobe::Pending` → `Strobe::Active` at 10 s. Forced-terminal sets `ceil = max_brightness` and overrides `VisualConfig::Off`. Strobe cancels on dismiss/restore.

### D6. Snapshot extension
`MopidySnapshot` gains `backlight_level: u8`. Captured on `fire` (read from `DisplayController`), restored on `dismiss`/`shutdown_restore`. Migration `v4` adds `visual_config` JSON column.

## Risks / Trade-offs

- **[sysfs backlight path varies across Pi/HAT setups]** → discovery at boot (glob `/sys/class/backlight/*/brightness`); fall back to a no-op controller if none found (logged).
- **[Strobe at 1 Hz could trigger photosensitivity]** → PRD mandates it as the safety net; `pulse_period` is per-alarm-configurable. Documented risk.
- **[5 s tick granularity for wake-timer expiry]** → wake expiry can be up to 5 s late; acceptable (display simply stays on a few extra seconds). The touch path re-reads `Instant::now()` precisely.
- **[Dynamic brightness interpolation vs strobe contention]** → precedence resolves it; strobe writes bypass the interpolator so no ramp-up latency during an alarm.

## Migration Plan

Additive: migration `v4` (nullable `visual_config`); new `display.rs`/`visual.rs`; `kv_config` entries for bedtime/floor. Old alarms get `VisualConfig::Off` (slice-2 behavior). No rollback.

## Open Questions

- Should `bl_power` discovery also handle the JustBoom Amp's separate ALSA mixer? (Out of scope — amp volume is Mopidy's mixer, not the display backlight.)
- Is 1 Hz strobe the right default, or should the bundled terminal fallback strobe faster? (Defaulting to `pulse_period` per-alarm; forced-terminal uses the same period at 100%.)

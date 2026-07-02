# Slice 4: Display Policies & Visual Alarms

## Why

The PRD specifies an appliance display that is never wrongly lit (no "screen on at 3am") and never silently fails an alarm (visual strobe is the terminal fallback). Slices 0–2 have no display control at all — the screen is always on at full brightness, and slice 2's fallback chain ends in a silent dismiss when exhausted. This slice introduces a single backlight controller owning four competing writers under an explicit precedence stack, plus the visual-alarm strobe that closes the silent-failure gap (and unblocks the slice-4a fallback-terminal correction).

## What Changes

- **Backlight controller (one owner on main).** A `DisplayController` on main owns the sysfs backlight (`brightness`, `bl_power`) and computes the effective policy each tick from four inputs by precedence: (1) visual-alarm strobe, (2) bedtime off, (3) user override, (4) dynamic brightness. Only the topmost active policy drives the hardware; lower policies are masked. `bl_power` is used only for state transitions (bedtime off, wake); strobing uses `brightness` only (never `bl_power`).
- **Bedtime (display power).** Global weekday/weekend split — two `(start, end)` wall-clock `Time` windows, cross-midnight inferred when `end < start`. During bedtime, `bl_power` off (true power-down). Wake-on-touch: any touch powers on for a 10 s idle timer (reset by further touches); entering settings or the quick-controls overlay suspends the timer; exiting re-arms. An alarm firing during bedtime suspends bedtime for the episode; on dismiss, power off immediately but arm the 10 s wake-on-touch grace.
- **Dynamic brightness (idle default).** Input: Open-Meteo `shortwave_radiation` (W/m²) from the weather slice (slice 5) on the 30-min tick. Perceptual curve (gamma ~0.5) with configurable floor (default 10%) and ceiling (100%). Transitions interpolated over ~120 s. Until the weather slice lands, dynamic brightness defaults to a fixed 60%.
- **User brightness override.** Temporary, via the swipe-up quick-controls overlay's brightness slider (slice 7). 30-min timeout, then reverts to auto. An override does NOT defeat bedtime-off (precedence).
- **Visual alarms.** `VisualConfig: Off | On { brightness, pulse_period, color }`. When `On`, the clock UI stays rendered and its brightness is flashed between a floor and `visual_brightness` at `pulse_period` (default 1 s), activating **10 s after fire** (audio first). Forced-visual at full brightness is the terminal fallback when the audio chain exhausts (closes the silent-failure gap). The episode snapshot is extended to include `backlight_level`, restored on dismiss.
- **Persistence.** Bedtime windows, `VisualConfig` per-alarm, brightness floor, and theme-mode `Follow-Bedtime` resolution are persisted via `ConfigStore` / the alarms table.

## Non-goals

- Gradual pre-bedtime theme *dimming* (v2; this slice does hard light/dark per the Follow-Bedtime mode).
- "Follow-ambient" theme mode (v2).
- Permanent brightness offset (v2).
- The swipe-up quick-controls overlay itself (slice 7) — this slice defines the brightness-slider contract the overlay will host.
- Per-weekday bedtime windows (v2; weekday/weekend split only).

## Capabilities

### New Capabilities
- `display-policy`: backlight controller, precedence stack, bedtime windows, wake-on-touch, dynamic brightness, user override.
- `visual-alarm`: `VisualConfig`, 10 s delay, brightness strobe, terminal full-brightness fallback.

### Modified Capabilities
- `alarm-episode`: episode snapshot extended with `backlight_level`; `shutdown_restore`/`dismiss` restore backlight; visual strobe started/stopped on fire/dismiss when `VisualConfig::On`.
- `persistence`: bedtime windows, brightness floor, per-alarm `visual_config`.

## Impact

- **New code:** `alarm-clock/src/display.rs` (`DisplayController`, `DisplayPolicy`, precedence resolution); `alarm-clock/src/visual.rs` (strobe scheduling); sysfs backlight I/O helpers.
- **Modified code:** `alarm-clock/src/episode.rs` (snapshot field, strobe start/stop, restore backlight), `alarm-clock/src/alarm_store.rs` (`visual_config` column), `alarm-clock/src/main.rs` (wire `DisplayController` into the tick + episode), `alarm-clock/src/config.rs` (bedtime/floor settings).
- **Dependency:** the `Follow-Bedtime` theme mode (slice 3) resolves against this slice's bedtime window.

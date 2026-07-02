# Slice 4a: Alarm PRD Alignment

## Why

Slice 2 built snooze and fallback from *its* proposal, but the PRD's model is richer and differs in three concrete ways: (1) snooze duration/cap are **per-alarm** (`snooze_minutes` default 10, `max_snoozes` default 3, 0 = disabled/hide button), not a global constant; (2) snooze re-fire starts from the **primary source with a fresh snapshot and a reset chain**, not the preserved snapshot/step; (3) the fallback chain's terminal element is a **bundled local beep**, and chain exhaustion triggers the **visual alarm at full brightness** (slice 4) rather than a silent dismiss. Slice 4a reconciles the built alarm with the PRD now that visual alarms exist (slice 4 dependency).

## What Changes

- **Per-alarm snooze.** `snooze_minutes` and `max_snoozes` columns on `Alarm` (migration `v5`). `EpisodeController::snooze` takes the per-alarm duration; the controller tracks a snooze count and hides/disables snooze at the cap.
- **Snooze re-fire resets to primary + fresh snapshot.** On re-fire, the FSM re-captures the snapshot (the restored session has advanced), replays the primary source, and resets the fallback chain to the primary. Escalation clock remains alarm-active-time-based (pauses during snooze, resumes from last value, never resets to step 0) — slice 2's clock-preservation is retained, but the *source/snapshot* reset per the PRD.
- **Bundled beep terminal fallback.** A bundled local beep file is always appended as the last element of every alarm's effective fallback chain. Chain exhaustion (beep also fails) requests the forced full-brightness visual (slice 4) instead of a silent dismiss.
- **Snooze hidden at cap.** When `max_snoozes` is reached, the snooze button is hidden on the alarm overlay; only dismiss remains.

## Non-goals

- Event-derived alarms (v2).
- Per-snooze escalating duration (PRD keeps `snooze_minutes` fixed per alarm).
- Changing the escalation-clock semantics (already PRD-aligned in slice 2).

## Capabilities

### Modified Capabilities
- `alarm-episode`: per-alarm snooze duration/cap, re-fire-from-primary + fresh snapshot, snooze-hidden-at-cap, bundled-beep terminal + forced-visual fallback.
- `persistence`: `snooze_minutes`/`max_snoozes` columns (migration `v5`); bundled beep path config.

## Impact

- **Modified code:** `alarm-clock/src/episode.rs` (snooze duration/count, re-fire path, terminal fallback), `alarm-clock/src/alarm_store.rs` + `database.rs` (migration `v5`, two columns), `alarm-clock/AlarmPanel.slint` (hide snooze at cap), `alarm-clock/src/main.rs` (pass per-alarm snooze fields, bundled beep path).
- **New asset:** a bundled beep audio file (e.g. `alarm-clock/assets/beep.mp3`), path resolved at boot.
- **Depends on:** slice 4 (forced-visual terminal fallback).

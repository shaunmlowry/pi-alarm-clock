## Context

Slice 2 faithfully implemented snooze + fallback per *its* proposal, but the PRD's model differs in three ways that slice 2 could not satisfy (visual alarms didn't exist yet, and the proposal deliberately scoped snooze to a global constant). Slice 4a reconciles the built alarm with the PRD now that slice 4's forced-visual terminal fallback exists.

## Goals / Non-Goals

**Goals:** per-alarm `snooze_minutes`/`max_snoozes`; snooze-hidden-at-cap; re-fire-from-primary + fresh snapshot + reset chain (escalation clock still preserved); bundled-beep terminal; chain-exhaustion → forced visual.

**Non-Goals:** event-derived alarms (v2); per-snooze escalating duration; changing the escalation-clock semantics (already PRD-aligned); a custom beep per alarm.

## Decisions

### D1. Snooze count is episode-scoped, not alarm-scoped
The `Escalating`/`Snoozing` state carries a `snooze_count: u32` reset to 0 on `fire`. `snooze()` increments it; the UI reads `snooze_count >= max_snoozes` to hide the button. Persisting a lifetime snooze count is not required (PRD: "after the cap, snooze is hidden" — per-episode).

### D2. Re-fire: fresh snapshot + primary source + reset chain, but preserved escalation clock
On `check_snooze_refire`: re-call `capture_snapshot()` (replacing the held snapshot), replay `source_uri`, set `fallback_index = PRIMARY`, reset `source_start = now`. **Keep** `fire_time` adjusted to the preserved step (slice 2's mechanism) so escalation resumes from step N. This honors both the PRD's "fresh snapshot + primary + reset chain" and "escalation clock never resets to step 0."

### D3. Bundled beep appended at plan construction
`EpisodePlan::new` appends the compiled beep path (`const BUNDLED_BEEP: &str = "asset:beep.mp3"` resolved to a file:// URI at boot) as the final element of `fallback_chain` if not already present. The beep is a real Mopidy `file://` source, so the existing fallback-advance logic handles it uniformly.

### D4. Terminal fallback delegates to slice 4
Chain exhaustion (beep fails) calls `DisplayController::force_full_strobe()` (slice 4) instead of `dismiss()`. The episode stays `Escalating` (visual strobing) until the user dismisses. This is the PRD's "silent failure is never acceptable."

## Risks / Trade-offs

- **[Bundled beep asset must ship with the binary]** → embed via `include_bytes!` and materialize to the data dir at boot, or rely on a known install path. Task 2.2 resolves the mechanism.
- **[Re-capturing a snapshot on re-fire requires Mopidy connected]** → if Mopidy is down at re-fire, the snapshot is defaults (slice-1 graceful degradation); the alarm still fires (audio best-effort) and visual still strobes.

## Migration Plan

Migration `v5` (additive, defaults). Bundled-beep asset added to the install. No rollback.

## Open Questions

- Should `max_snoozes` be per-alarm or also globally configurable? PRD says per-alarm; global default is the alarm's default, not a separate config.

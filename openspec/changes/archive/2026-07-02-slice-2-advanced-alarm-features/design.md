## Context

Slice 1 delivered the core episode FSM: `Idle → Firing(snapshot, play source looping at fixed `max_volume`) → Dismissed(restore)`, driven by a 5 s scheduler tick on main, with a tap-anywhere-to-dismiss alarm overlay and `shutdown_restore()`. Source failure within an 8 s grace window ends the episode (no fallback). Snooze is absent.

Slice 2 adds the **intelligence layer** over that core: progressive volume escalation, a multi-stage source fallback chain, and snooze. These are pure episode-FSM and data-model concerns — they reuse slice 1's Mopidy seam (`MopidyControl`), scheduler tick, channel topology, and snapshot/restore machinery without changing the threading model or the single-`Connection`-on-main rule.

The as-built code differs from the placeholder `tasks.md` that shipped with this change. The real modules are:
- `alarm-clock/src/episode.rs` — the `EpisodeController<C: MopidyControl>` FSM (not `episode_controller.rs` / `episode_manager.rs` / `models.rs`).
- `alarm-clock/src/alarm_store.rs` — the `Alarm` model + `AlarmStore` (not `models.rs`).
- `alarm-clock/src/scheduler.rs` — `Scheduler<S,F,C>` with the `EpisodeFsm` trait seam.
- `alarm-clock/src/main.rs` — `ChannelMopidyControl`, `EpisodeFsmAdapter`, drain + scheduler timers, UI wiring.
- `alarm-clock/src/seed.rs` — dev `alarms.toml` seeding.
- `alarm-clock/AlarmPanel.slint` + `alarm-clock/ui.slint` — the alarm overlay UI (not `ui/main.slint`).
- `mopidy-client/src/methods.rs` — already provides `playback_set_volume`, `playback_get_state`, `tracklist_add`, etc. via the `PlaybackApi`/`TracklistApi` traits. **No new transport work is required** — slice 1 already built the wrapper surface the placeholder tasks asked for.

This design is written against the as-built files.

## Goals / Non-Goals

**Goals:**
- Progressive volume escalation through per-alarm `escalation_steps`, advancing on the scheduler tick, uninterrupted across fallbacks.
- Multi-stage source fallback chain: on primary-source failure within the grace window, automatically advance to the next backup source without dropping the episode or resetting escalation.
- Snooze: suspend escalation at step *N*, restore user media, and on re-fire resume escalation from step *N* (not reset).
- Persist the new alarm fields (`escalation_steps`, `fallback_chain`) via migration `v3` (non-destructive; NULL = slice-1 behavior).
- Extend the alarm overlay with a Snooze affordance.
- Preserve every slice-1 guarantee: optimistic non-blocking Mopidy commands, snapshot capture/restore, `shutdown_restore()`, tick-level panic isolation, single-threaded DB access.

**Non-Goals:**
- Calendar/holiday suppression (S3+).
- Visual alarms / brightness strobe (needs display policy).
- Alarm editing UI on Pi or web (the web slice owns the builder; slice 2 extends dev `alarms.toml` seeding only).
- Auto-generation of escalation steps when none are configured (absent → fixed `max_volume`, exactly slice 1).
- Per-source retry/backoff inside the fallback chain (one attempt per fallback slot; the chain is exhaustive).
- Snooze count limiting / "snooze too many times" policy (slice 2 allows unlimited snooze; a max-snooze count is a later slice).

## Decisions

### D1. Escalation data model — `EscalationStep { after_secs, volume }`, JSON column

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationStep {
    /// Seconds elapsed since the episode fire instant at which this step's
    /// volume takes effect.
    pub after_secs: u64,
    /// Volume 0..=100 to apply from this step onward (clamped).
    pub volume: u8,
}
```

- `Alarm.escalation_steps: Option<Vec<EscalationStep>>`. `None` or empty → fixed `max_volume` (slice-1 behavior, bit-for-bit). When present, the steps **must** be sorted ascending by `after_secs`; the store validates and re-sorts on write. Step 0 conventionally has `after_secs = 0` (the initial volume).
- The final step's volume is the ceiling; `max_volume` is retained as the hard cap and the slice-1 fixed-volume fallback. If the last step's volume is less than `max_volume`, `max_volume` still wins as the cap only when no steps are configured — with steps, the ramp is authoritative.
- Stored as a JSON text column `escalation_steps` (nullable). JSON keeps the column additive (no schema per-step table) and round-trips through `serde`.

**Rationale.** A flat sorted list is the simplest model that supports the PRD's "increasing urgency over time" without a separate scheduler per step. `after_secs` (not `Duration`) serializes cleanly to JSON and compares as integers. Storing JSON in a TEXT column matches the existing `next_fire`/`once_at` "text column, parse at the boundary" pattern.

**Alternatives.** A separate `escalation_steps` table (normalized) was rejected: it adds a join for every `list()`/`get()` and a multi-statement transaction for every upsert, for a list that is always read whole and never queried. A CSV column was rejected: no nested typing, fiddly escaping.

### D2. Fallback chain data model — `fallback_chain: Vec<String>`, JSON column

- `Alarm.fallback_chain: Option<Vec<String>>` — ordered backup source URIs. The alarm's `source_uri` is the **primary** (always tried first); entries in `fallback_chain` are backups tried in order. `None`/empty → no fallback (slice-1 behavior: end episode on source failure).
- Stored as a JSON text column `fallback_chain` (nullable).

**Rationale.** A URI list is all the FSM needs; it does not select fallbacks by policy, just by order. Keeping `source_uri` as the primary (rather than treating it as `fallback_chain[0]`) preserves the slice-1 invariant that every alarm has a non-null `source_uri` and lets the model round-trip alarms authored before slice 2.

### D3. Migration `v3` — additive `ALTER TABLE`, NULL = slice-1 behavior

```sql
ALTER TABLE alarms ADD COLUMN escalation_steps TEXT;  -- nullable JSON
ALTER TABLE alarms ADD COLUMN fallback_chain TEXT;    -- nullable JSON array
```

Bump `user_version` to `3`. Non-destructive: existing rows get `NULL` for both columns, which the model deserializes as `None` → fixed `max_volume`, no fallback (identical to slice 1). Applied inside a single transaction with the `user_version` bump, mirroring `v1`/`v2`.

**Rollback.** Not supported (schema migrations are forward-only on the Pi). The additive columns are harmless to older code paths that do not read them.

### D4. Episode FSM states — `Escalating` + `Snoozing` replace `Firing`

```rust
pub enum EpisodeState {
    Idle,
    Escalating {
        alarm_id: AlarmId,
        snapshot: MopidySnapshot,
        fire_time: Instant,      // original episode fire; drives escalation elapsed
        source_start: Instant,   // when the *current* source began; drives grace window
        step_index: usize,       // current escalation step
        plan: EpisodePlan,       // source_uri, fallback_chain, fallback_index, steps, max_volume
        _span: EnteredSpan,
    },
    Snoozing {
        alarm_id: AlarmId,
        snapshot: MopidySnapshot,
        snooze_until: Instant,   // re-fire deadline
        step_index: usize,       // preserved escalation step
        plan: EpisodePlan,
        _span: EnteredSpan,
    },
    Dismissed,
}
```

`EpisodePlan` bundles the per-episode immutable data computed at fire time from the `Alarm`: `{ source_uri, fallback_chain: Vec<String>, fallback_index: usize, escalation_steps: Vec<EscalationStep>, max_volume: u8 }`.

- **`is_firing()`** (drives the alarm overlay / `episode-firing` property) returns `true` **only** for `Escalating`. During `Snoozing` the overlay is hidden and user media is restored.
- **`is_active()`** (new) returns `true` for `Escalating` **or** `Snoozing` — used by `shutdown_restore()` and the dismiss handler so a snoozed episode is still terminable.
- The `episode` span is entered on `fire()` and held across `Escalating ↔ Snoozing` transitions; it exits only on `dismiss()`/`shutdown_restore()` (restore completion).

**Rationale.** Splitting `Firing` into `Escalating`/`Snoozing` makes the "user media restored vs. alarm playing" distinction explicit in the type system, so the drain timer's `is_firing()` → overlay binding cannot accidentally show the overlay during snooze. `EpisodePlan` factors the alarm-derived data out of the state enum so `Escalating`/`Snoozing` share one copy and snooze preserves it wholesale.

**Two clocks.** `fire_time` (original fire) drives escalation elapsed so volume keeps climbing across fallbacks and snooze. `source_start` (reset on each fallback advance) drives the grace-window failure check, so a fallback that immediately stops is detected without waiting out the original 8 s window.

### D5. Escalation advance — driven by the scheduler tick (5 s granularity)

- New method `EpisodeController::advance_escalation(now: Instant)`. Computes `elapsed = now - fire_time`. Finds the highest `step_index` whose `after_secs <= elapsed`. If it advanced, issues `control.playback_set_volume(step.volume)` (fire-and-forget, optimistic). Idempotent: no command when `step_index` is unchanged.
- The `Scheduler::tick` calls a new `EpisodeFsm::on_tick(now)` hook after its fire loop. `EpisodeFsmAdapter::on_tick` locks the episode controller and calls `advance_escalation(now)` **plus** `check_snooze_refire(now)` (D6). `NoopEpisodeFsm` and the test mock get a default no-op `on_tick`.
- 5 s granularity is acceptable: escalation steps are coarse (tens of seconds, per PRD), and the scheduler tick already runs on main and already locks the episode. A dedicated sub-second timer would issue `set_volume` far more often than the ear can perceive and would add a third timer to manage.

**Rationale.** Reusing the scheduler tick avoids a new timer and keeps all "time-based episode progress" in one place (firing, escalation advance, snooze refire). The `EpisodeFsm` trait gains a default-method `on_tick` so the existing mock/no-op seams compile unchanged.

**Alternatives.** A 1 s `slint::Timer` dedicated to escalation was rejected: doubles the lock-acquisitions on the episode mutex for no perceptible gain (steps are ≥10 s apart in practice). Driving escalation off the 50 ms drain timer was rejected: would issue `set_volume` 20×/s.

### D6. Snooze — suspend at step *N*, restore user media, resume from step *N*

- `EpisodeController::snooze(duration: Duration)`. Valid only in `Escalating`; a no-op (logged) otherwise. Transitions `Escalating → Snoozing`, preserving `step_index`, `snapshot`, `plan`, and the span. Sets `snooze_until = now + duration`. Issues the **restore-playback** commands (resume snapshot playback + restore repeat/shuffle/volume) so user media resumes — but does **not** drop the span or transition to `Dismissed`.
- `check_snooze_refire(now: Instant)` (called from `on_tick`): if `Snoozing` and `now >= snooze_until`, transition back to `Escalating`: re-issue `tracklist.add(source_uri) + playback.play + tracklist.set_repeat(true)`, set `fire_time = now - steps[step_index].after_secs` (so elapsed places us exactly at the preserved step), issue `set_volume(steps[step_index].volume)`, reset `source_start = now`. The overlay re-appears (`is_firing()` → true).
- `DEFAULT_SNOOZE_DURATION = 9 min` (PRD-typical). The UI snooze button uses this constant; no per-alarm snooze duration in slice 2.
- `dismiss()` works from `Escalating` **or** `Snoozing` (cancels a pending snooze). `shutdown_restore()` likewise restores from either active state.

**Rationale.** Preserving `step_index` and setting `fire_time = now - steps[step_index].after_secs` makes escalation "resume from step *N*" literally: the next `advance_escalation` sees elapsed at the step boundary and climbs from there, rather than restarting at step 0 (which the PRD forbids) or jumping to max (which would skip the ramp). Restoring user media during snooze reuses the existing restore command set without a new code path.

**Alternatives.** "Reset escalation to step 0 on re-fire" — explicitly rejected by the proposal. "Continue the wall-clock ramp during snooze" — rejected: user media is playing during snooze, so volume must not climb then.

### D7. Fallback chain — advance on source failure, escalation uninterrupted

- `on_playback_state_changed(Stopped)` while `Escalating` and `now - source_start < GRACE_WINDOW`:
  - If `plan.fallback_index + 1 < plan.fallback_chain.len()`: increment `fallback_index`, issue `tracklist.add(next_uri) + playback.play + tracklist.set_repeat(true) + playback.set_volume(current_step_volume)`, reset `source_start = now`. `fire_time` and `step_index` are **unchanged** — escalation continues uninterrupted across the fallback (acceptance criterion 3). Log at `info!`.
  - Else (no more fallbacks): log `error!`, `dismiss()` (end episode, restore). This is the slice-1 failure path, reached only when the chain is exhausted.
- `fallback_chain` empty/`None` → slice-1 behavior (dismiss on first failure).

**Rationale.** Resetting only `source_start` (not `fire_time`) cleanly satisfies "escalation continues uninterrupted across all fallbacks" while still detecting a fallback that fails immediately. Re-issuing the current step's volume on fallback advance prevents a momentary dip to the snapshot volume.

### D8. UI — Snooze button on the alarm overlay

- `AlarmPanel.slint`: add a Snooze `Rectangle`+`Text`+`TouchArea` in the lower region. The snooze `TouchArea` emits a new `snooze-requested` callback **and** stops propagation so the tap-anywhere-to-dismiss `TouchArea` beneath does not also fire. The dismiss TouchArea remains full-screen beneath the button.
- `ui.slint` `AppWindow`: add `callback snooze-requested();`, forward from `AlarmPanel`, and gate `AlarmPanel` visibility on `episode-firing` (already true only during `Escalating`, so the snooze button is hidden during `Snoozing`/`Idle`/`Dismissed`).
- `main.rs`: wire `app_window.on_snooze_requested` to `episode_ctl.lock().snooze(DEFAULT_SNOOZE_DURATION)` and optimistically let the drain timer flip `episode-firing` to false (the FSM is now `Snoozing`).

**Rationale.** A dedicated button (not a gesture) is unambiguous on a touch-only device and matches the PRD's "Snooze" gesture language. Keeping it inside `AlarmPanel` (shown only when `episode-firing`) means snooze is only reachable while the alarm is actively escalating — never during snooze.

### D9. Mopidy seam — no new methods; reuse slice-1 surface

The placeholder `tasks.md` asked for `set_volume`/`get_state` wrappers in `mopidy-client/src/transport.rs`. These already exist: `PlaybackApi::playback_set_volume` and `playback_get_state` in `mopidy-client/src/methods.rs` (slice 1, tasks 4.2/4.4). The episode FSM does not call the raw client — it goes through the `MopidyControl` seam (`ChannelMopidyControl` in `main.rs`), which already implements `playback_set_volume`, `tracklist_add`, `playback_play`, `tracklist_set_repeat`, `tracklist_set_shuffle`, `playback_stop`, `playback_seek`, `capture_snapshot`. **Slice 2 adds no new seam methods and no new transport wrappers.** Escalation, fallback, and snooze are expressed entirely through existing seam calls.

### D10. Dev seed — optional `escalation_steps` / `fallback_chain` in `alarms.toml`

`SeedAlarm` gains two optional fields, serialized straight to the `Alarm` row:
```toml
escalation_steps = [{ after_secs = 0, volume = 20 }, { after_secs = 30, volume = 60 }, { after_secs = 60, volume = 80 }]
fallback_chain = ["spotify:track:backup1", "file:///beep.mp3"]
```
Absent → `None` (slice-1 behavior). The seed path remains dev-only and idempotent.

## Risks / Trade-offs

- **[5 s escalation granularity can overshoot a step boundary by up to 5 s]** → acceptable per PRD (steps are tens of seconds apart); the step is applied on the first tick at/after its `after_secs`, never skipped (advance finds the highest applicable step).
- **[Snooze re-fire fires `tracklist.add` while user media is still playing]** → the snapshot restore during snooze already resumed user media; re-fire replaces the tracklist (same as the initial fire). This mirrors slice-1's "second alarm during Firing" serialize path and is safe.
- **[Fallback advances forever if every source immediately stops]** → bounded by `fallback_chain.len()`; when exhausted, the episode ends (dismiss). No infinite loop.
- **[Migration `v3` on a DB with existing alarms]** → additive `ALTER TABLE` + `NULL`; old rows deserialize to `None`/slice-1 behavior. Verified by an idempotency test mirroring the `v2` test.
- **[Optimistic `set_volume` during escalation can race a Mopidy restart]** → the existing `on_command_failure` / `NotConnected` correction path applies (episode stays `Firing`/`Escalating`, best-effort). No new correction logic.
- **[Two `Instant` clocks in `Escalating` (`fire_time`, `source_start`) is easy to misuse]** → mitigated by unit tests covering both the escalation-advance and grace-window-failure paths independently.

## Migration Plan

1. Ship migration `v3` (additive) — existing deployments keep working with NULL columns.
2. The episode FSM, scheduler tick, and UI ship together in one build (no partial rollout).
3. Dev `alarms.toml` may carry the new optional fields immediately; old seed files are unchanged (absent fields → `None`).
4. No rollback path (forward-only migrations on the Pi). A failed build simply is not deployed.

## Open Questions

- Should snooze duration be per-alarm (stored) or global (config)? Slice 2 uses a global `DEFAULT_SNOOZE_DURATION = 9 min`; per-alarm is deferred to the web slice.
- Should escalation auto-stop at the last step or hold? Slice 2 holds at the last step's volume until dismiss/snooze/fallback (no auto-dismiss on reaching max).

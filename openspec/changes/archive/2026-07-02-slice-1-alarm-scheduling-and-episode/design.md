## Context

Slice 0 established the architectural spine: Slint on main, tokio on a worker thread, the domain layer owned by main, a single `rusqlite::Connection` touched only from main, a reconnecting Mopidy WebSocket client with typed minimal surface and a connection-state signal, SQLite migrations + `ConfigStore`, structured logging with `scheduler_tick`/`episode` spans defined but unused, and a `shutdown_restore()` hook that is a no-op. Slice 0 rendered a placeholder Clock panel and proved the round-trips (config, Mopidy `get_version`, `sd_notify READY=1`).

Slice 1 is the first user-facing behavior: an alarm fires at the scheduled time, plays its audio source through Mopidy (looping), and restores the user's prior Mopidy state on dismiss. This exercises the riskiest state machine in the system — the episode FSM, interleaved with async Mopidy replies marshalled back to main — and turns two slice-0 no-ops (`scheduler_tick` span, `episode` span, `shutdown_restore()`) into real behavior.

The PRD specifies the full alarm feature (escalation, snooze, visual, fallback chain, holiday suppression). Slice 1 deliberately delivers the **core episode** only: schedule → fire → snapshot → play (fixed volume, loop) → dismiss-restore. Escalation, snooze, visual, fallback chain, and UI editing are deferred to later slices. Slice 1's value is a proven fire/restore path; its acceptance bar is "an alarm fires, plays, and restores."

Slice 0's open questions resolved here: the scheduler tick model (interval-with-recompute), the `shutdown_restore()` seam shape (method on a `Domain`/episode trait), and `.slint` files vs inline (slice 0 chose `.slint`; slice 1 follows).

## Goals / Non-Goals

**Goals:**
- Resolve the scheduler tick model (slice 0 open question) and implement it.
- Persist alarms in SQLite (migration `v2`) with single-threaded CRUD on main.
- Compute next-fire from `rrule` + `chrono-tz`, wall-clock local, DST-correct.
- Implement the episode FSM on main: `Idle → Firing(snapshot, play, loop) → Dismissed(restore)`.
- Capture and restore the Mopidy snapshot (the PRD's core behavioral guarantee).
- Implement `shutdown_restore()` for real (restore snapshot on SIGTERM mid-episode).
- Expand the Mopidy typed surface to the playback/tracklist methods the episode needs.
- Render the alarm episode UI (clock + tap-anywhere-to-dismiss; no snooze).
- Allow alarms to be seeded for end-to-end testing without a web UI.

**Non-Goals:**
- Escalation / volume ramp (slice 2). Slice 1 plays at fixed `max_volume`.
- Snooze (slice 2). Slice 1 dismiss is the only episode terminator.
- Visual alarms / brightness strobe (later; needs display policy).
- Fallback chain + bundled beep (later). Slice 1 plays one source; on failure, logs and ends.
- Holiday suppression (later; needs calendar integration).
- Alarm editing UI on Pi or web (later). Slice 1 seeds via DB/dev-config.
- Daily-data, media, settings panels (later slices).
- Quick-controls overlay (later).

## Decisions

### D1. Scheduler tick: interval with recompute (resolves slice 0 open question)

A `slint::Timer` on main fires at a fixed interval (e.g. every 5s) and re-derives the next fire time from `Local::now()` each tick. When `now >= next_fire` for an enabled alarm, the alarm fires. Next-fire is recomputed on rule change, on fire (advance to next occurrence), and on DST boundary (detected by the tick observing the local offset changing).

**Rationale.** The alternative — arming a point-in-time timer for `next_fire - now` — is precise but fragile on a device with no RTC: an NTP correction or `fake-hwclock` jump can fire the timer wrong (early) or never (if the clock jumps past the fire time and the timer doesn't notice). An interval tick that re-reads `Local::now()` is robust to clock jumps: a missed alarm fires on the next tick after the clock becomes correct (slightly late, which is acceptable); an early jump doesn't fire an alarm that isn't due. The 5s granularity is well below the PRD's human-perceptible alarm-timing bar. The recompute cost (`rrule` evaluation for a handful of alarms) is negligible per tick.

**Edge case: catch-up on boot.** If the device was off across an alarm's fire time, the tick on boot sees `now >= next_fire`. Slice 1's policy: **do not fire missed alarms** — advance `next_fire` to the next occurrence after `now` and log. The PRD does not require missed-alarm playback; firing a stale alarm at boot (e.g. 3am alarm, powered on at 9am) is worse than skipping it. This policy is revisited if a later slice adds "fire if missed within N minutes."

**Alternatives considered.**
- *Point-in-time timer (`tokio::time::sleep_until(next_fire)`):* rejected as fragile to clock jumps and harder to reason about across DST.
- *1s tick:* no perceptible benefit over 5s; doubles per-tick cost for no gain.

### D2. Schedule representation: `rrule` + `chrono-tz`, wall-clock local

- Each alarm stores a wall-clock-local `time` (HH:MM:SS in the configured timezone) and an `rrule` string (RFC 5545). For a `Once` alarm, the `rrule` is absent/empty and the `time` is a full `DateTime<Tz>`.
- `Schedule::next_fire(after: DateTime<Tz>) -> Option<DateTime<Tz>>` evaluates the `rrule` filtered to the local `time`, returning the next occurrence strictly after `after`. Uses the `rrule` crate for evaluation and `chrono-tz` for the timezone.
- **Times are stored as wall-clock local** (per PRD); next-fire is derived from the rule + `Local::now()`, not stored authoritatively. A cached `next_fire` column is an optimization recomputed on boot, rule change, and fire — but the source of truth is the rule.
- **Presets** (Once, Daily, Weekdays, Weekends, Specific-days) map to RRULE strings:
  - Once → no rrule
  - Daily → `FREQ=DAILY`
  - Weekdays → `FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR`
  - Weekends → `FREQ=WEEKLY;BYDAY=SA,SU`
  - Specific-days → `FREQ=WEEKLY;BYDAY=<selected>`
- Complex RRULE (COUNT, UNTIL, BYSETPOS, INTERVAL>1) is accepted (stored/parsed) but only preset-generated rules are constructed in slice 1. The full builder is web-only (later slice).

**Rationale.** The PRD mandates full RFC 5545 RRULE behind a `Schedule` with `next_fire`, and explicitly offloads calendar RRULE expansion to Google (not this crate). `rrule` + `chrono-tz` is the standard Rust stack for this; wall-clock-local storage is mandated by the PRD so DST is handled at compute time, not storage time.

### D3. Alarm data model & persistence: migration `v2`, `AlarmStore` on main

Migration `v2` (non-destructive; `v1`'s `schema_meta`/`kv_config` untouched):

```sql
CREATE TABLE alarms (
    id           TEXT PRIMARY KEY,           -- UUID v4 string
    enabled      INTEGER NOT NULL DEFAULT 1,  -- 0/1 boolean
    name         TEXT NOT NULL,
    time_local   TEXT NOT NULL,               -- wall-clock local "HH:MM:SS"
    timezone     TEXT NOT NULL,               -- IANA tz name, e.g. "America/Edmonton"
    rrule        TEXT,                        -- nullable; NULL = Once alarm
    once_at      TEXT,                        -- nullable; for Once: full ISO8601 local DateTime
    source_uri   TEXT NOT NULL,              -- Mopidy URI to play
    max_volume    INTEGER NOT NULL DEFAULT 40, -- 0..100
    next_fire    TEXT,                        -- cached ISO8601 UTC; derived, recomputed
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
```

`AlarmStore` (owned by main, holds `&Connection`) provides `list()`, `get(id)`, `upsert(alarm)`, `delete(id)`, `set_enabled(id, bool)`, `recompute_next_fires(now)`. Each mutation is a single transaction (slice 0's atomic-write policy). `next_fire` is recomputed in `recompute_next_fires` (called on boot, on rule change, after a fire) and written back; the column is an optimization — the FSM re-derives from the rule on the tick to avoid drift if the cache is stale.

**Rationale.** Same single-threaded, no-mutex model as `ConfigStore`. UUIDs as text (no `uuid` crate dependency required if generated simply; if needed, `uuid` is added). `next_fire` stored in UTC for comparison simplicity (`now` is converted to UTC for the comparison), but the *rule* is the source of truth. The `timezone` column exists because the configured local tz at alarm-creation time should be the tz used for evaluation (a user who moves the device keeps alarms firing at their original local time until edited) — though slice 1 has no edit UI, the column is laid out for that future.

### D4. Episode FSM: `Idle → Firing → Dismissed`, single-threaded on main

```
   ┌────────┐  fire(alarm)   ┌──────────────────────┐  dismiss()  ┌───────────┐
   │  Idle  │ ─────────────▶ │ Firing               │ ──────────▶ │ Dismissed │
   └────────┘                 │  · snapshot captured │             │  · restore │
        ▲                     │  · play(source_uri)  │             └─────┬─────┘
        │                     │  · repeat=true      │                   │
        └─────────────────────┘  · volume=max_volume│                   │ restore done
                                                   ▼                   ▼
                                                 (snapshot restored)
```

- The FSM is a struct owned by main (`EpisodeController`), driven by: the scheduler tick (fires), the Mopidy reply/event drain (state changes, tracklist-ended), the UI (dismiss tap), and shutdown (`shutdown_restore()`).
- **`Firing` state holds the snapshot and the alarm id.** Only one episode at a time (a second alarm firing mid-episode is queued: slice 1 policy is to dismiss-and-restore the current episode then fire the queued one — simplest correct behavior; the PRD's "multiple simultaneous alarms" semantics are not specified, so slice 1 serializes).
- **Transitions are functions on main**, never blocking. Mopidy commands are sent over the Cmd channel; replies arrive on the reply channel and are dispatched to the FSM on the tick. The FSM does not block on a reply — it transitions optimistically (`Firing → AwaitingPlaybackConfirmation`) and corrects on the reply (or on a timeout).
- The existing `episode` span (slice 0, unused) is entered on `fire()` and exited on `dismiss()`/restore.

**Rationale.** Single-threaded FSM is the load-bearing decision from slice 0 (D1). The optimistic-transition-with-correction pattern avoids blocking the Slint event loop and matches the non-blocking reply-drain model.

### D5. Snapshot capture & restore contract

The snapshot is a struct captured **at fire time** from Mopidy state:

```rust
struct MopidySnapshot {
    uri: Option<String>,        // current tracklist uri (first track)
    position_ms: u32,           // playback time position
    was_playing: bool,          // playback state was "playing"
    seekable: bool,
    volume: u8,                 // 0..100
    repeat: bool,
    shuffle: bool,
}
```

- **Capture:** `fire()` issues (over the channel, awaited via the reply drain) `playback.get_state`, `playback.get_time_position`, `playback.get_volume`, `tracklist.get_repeat`, `tracklist.get_shuffle`, `tracklist.get_random`, and the current tracklist. The snapshot is stored on the `Firing` state. If Mopidy is disconnected at fire time, the snapshot fields are `None`/defaults and the episode plays the alarm source anyway (slice 1 policy: the alarm must fire even if Mopidy is down — playback silently fails, logged; restore becomes a no-op). The full terminal-fallback (visual) is a later slice.
- **Restore:** `dismiss()` (or `shutdown_restore()`) issues: `tracklist.set_repeat(snapshot.repeat)`, `tracklist.set_shuffle(snapshot.shuffle)`, `playback.set_volume(snapshot.volume)`, then if `snapshot.uri` is Some and `was_playing`: `tracklist.add(uri)` + `playback.play` + seek to `position_ms`; if `was_playing` false: `playback.stop` (leave it stopped). If `snapshot.uri` is None (Mopidy was down at capture): restore volume/repeat/shuffle only.
- **Snapshot is fresh per fire** (per PRD). A re-fire (snooze, slice 2) re-captures. Slice 1 has one fire per episode so this is straightforward.

**Rationale.** This is the PRD's core behavioral guarantee and the riskiest part of the slice. The contract is exhaustive (capture everything that affects playback, restore exactly) so a later slice adding snooze/escalation doesn't redefine it. The "snapshot with Mopidy-down" path is slice 1's graceful-degradation: the alarm fires (audio may fail), restore is a no-op — no hang, no crash.

### D6. `shutdown_restore()` seam: method on the episode controller

Slice 0's `shutdown_restore()` was a no-op on a domain hook. Slice 1 implements it as a method on `EpisodeController`:

```rust
impl EpisodeController {
    fn shutdown_restore(&mut self, mopidy: &mut MopidyHandle) {
        if let Some(state) = self.firing_state.take() {
            // restore the snapshot synchronously on the shutdown path
            self.restore_snapshot(state.snapshot, mopidy);
        }
    }
}
```

The shutdown handler (slice 0) calls this **before** draining the Cmd channel and exiting. Because the snapshot restore issues Mopidy commands, and Mopidy is on the tokio worker, the shutdown path sends the commands and waits for the tokio worker to flush them (with a short timeout, e.g. 2s) before exiting. If Mopidy is down, the restore is a no-op and exit is immediate.

**Rationale.** The PRD requires "snapshot-restore before exit" on shutdown. A method on the controller (rather than a separate `ShutdownCoordinator` trait) is simpler and sufficient for one episode; if multiple restorable subsystems exist later, refactor then. The timeout bounds shutdown so a hung Mopidy doesn't stall systemd's `SIGTERM` → `SIGKILL` window.

### D7. Mopidy method expansion: mechanical extension of slice 0's typed surface

Slice 0's typed surface (`core.get_version`, `core.get_state`) established the request-struct / `call` / typed-reply shape. Slice 1 adds the playback/tracklist methods the episode needs:

- `playback.play(uri: Option<String>)`
- `playback.pause`
- `playback.resume`
- `playback.stop`
- `playback.set_volume(volume: u8)`
- `playback.get_state -> PlaybackState`
- `playback.get_time_position -> u32`
- `tracklist.add(uris: Vec<String>)`
- `tracklist.set_repeat(bool)`
- `tracklist.set_shuffle(bool)` (also covers `set_random` — Mopidy's `random` and `shuffle` are aliased; slice 1 uses `shuffle` naming per PRD)

Each is a request struct serializing to the JSON-RPC `params`, a `call` on the client, and a typed reply deserialized via `serde`. The existing connection-state signal and event channel are unchanged.

**Rationale.** The contribution is the surface, not novel methods. Following slice 0's shape keeps it mechanical and reviewable.

### D8. Alarm episode UI: overlay panel above the navigation container

- A new Slint panel (`AlarmPanel.slint`) rendered **above** the navigation container when an episode is `Firing`. The navigation container (slice 0) is hidden (opacity 0 / not drawn), swipe is disabled.
- The alarm panel shows: the clock face (reusing the Clock panel's theme-seam properties — hardcoded values, same as slice 0), and is **tap-anywhere-to-dismiss** (per PRD). No snooze button in slice 1.
- The panel is driven by the episode FSM state (a `Firing`/`Idle` property exposed to Slint). The dismiss tap handler sends a `Dismiss` command to the FSM on main.
- On `Dismissed → restore done`, the panel hides and the navigation container (Clock panel) reappears.

**Rationale.** Stacking above the nav container (rather than as a 5th navigable panel) matches the PRD's "normal panels hidden; alarm UI shown exclusively; panel-swipe disabled." Reusing the Clock's theme properties keeps the theme seam consistent (slice 1 still doesn't build the token system; the alarm clock face uses the same hardcoded feeder).

### D9. Alarm seeding: dev `alarms.toml` consumed at boot

Slice 1 has no alarm-editing UI (web or Pi). To be end-to-end testable, a dev `alarms.toml` at a known path is consumed at boot **if present**: parsed into `Alarm` records and upserted into the DB. In production this file is absent and the DB is the sole source (seeding happens via the future web slice).

```toml
[[alarms]]
id = "test-morning"
enabled = true
name = "Morning test"
preset = "Daily"          # or "Once", "Weekdays", "Weekends", "Specific-days"
days = ["Mo","Fr"]         # only for Specific-days
time = "07:30:00"
timezone = "America/Edmonton"
source_uri = "spotify:track:..."
max_volume = 40
once_at = "2026-07-01T07:30:00"  # only for Once
```

The seeding is **idempotent** (upsert by `id`) and **dev-only** (logged at `info!` with a "dev seed" marker; the production path skips it). It is not a replacement for the web config — it exists so slice 1 can be validated end-to-end.

**Rationale.** Without a seeding path, slice 1 cannot be acceptance-tested (no way to create an alarm). Direct DB insertion is brittle; a TOML file parsed by the same serde model as the future web API is cleaner and reuses the alarm model. Marked dev-only so it's not mistaken for a feature.

### D10. Mopidy-down / source-failure behavior in slice 1: graceful degradation, no fallback chain

- **Mopidy disconnected at fire time:** the alarm fires (FSM enters `Firing`), the snapshot is empty (None), playback commands are sent and silently fail (the client logs the failure; replies indicate "not connected"). The episode stays `Firing` (so the UI shows the alarm) until dismiss. Restore is a no-op.
- **Mopidy connected but source fails to play (e.g. bad URI):** slice 1 has no fallback chain (deferred). The episode logs the failure at `error!` and **ends the episode** (transitions to `Dismissed` with restore) after a short grace window (reusing the PRD's 8s heuristic conceptually, though the full fallback chain is later). The user must re-arm.
- **Mopidy restarts mid-episode:** the connection-state signal transitions `Connected → BackingOff → Connected`. Slice 1's FSM does **not** mid-episode re-issue playback (that's the mid-episode-restart logic the fallback-chain slice handles); it logs and waits. If still `Firing` when Mopidy returns, the user dismisses normally. This is a known limitation — slice 1's episode is not resilient to mid-episode Mopidy restart; the later fallback-chain slice adds re-issue logic.

**Rationale.** Slice 1 must not hang or crash on Mopidy-down (the PRD's "alarms fire offline" guarantee's floor). The full fallback chain (terminal visual, beep, re-issue) is a later slice; slice 1's contract is "the episode fires and is dismissable, audio best-effort."

## Risks / Trade-offs

- **[Snapshot capture latency]** Capturing the snapshot requires several Mopidy `get_*` calls (state, position, volume, repeat, shuffle, tracklist). Issued concurrently over the channel they complete in one round-trip, but if Mopidy is slow, `fire()` is delayed. → Mitigation: issue all snapshot reads as a batch (fan-out, await all via the reply drain); bound the wait (e.g. 1s) and proceed with partial/empty snapshot if it expires. The alarm still fires on time.
- **[Optimistic FSM transitions can desync]** Transitioning to `Firing` before `playback.play` confirms can leave the FSM in a state that disagrees with Mopidy (e.g. play failed). → Mitigation: the reply drain corrects the FSM state on failure; a `Firing → AwaitingPlayback → Firing/Failed` sub-state tracks confirmation. The UI shows `Firing` regardless (the alarm is "on" from the user's view even if audio is still starting).
- **[Mid-episode Mopidy restart not handled]** Slice 1's episode doesn't re-issue playback if Mopidy restarts mid-episode (D10). → Mitigation: documented limitation; the episode stays dismissable. The fallback-chain slice adds re-issue.
- **[`rrule` crate correctness across DST]** The `rrule` crate's DST handling must be verified for our wall-clock-local model. → Mitigation: unit tests for DST-boundary alarms (spring-forward, fall-back) are in the spec scenarios; if the crate misbehaves, wrap with explicit `chrono-tz` adjustment.
- **[Scheduler tick granularity]** A 5s tick means an alarm can fire up to 5s late. → Mitigation: acceptable per PRD's human-perceptible bar; documented.
- **[Snapshot restore on shutdown races SIGKILL]** If Mopidy is slow and systemd's `TimeoutStopSec` (default 90s) is hit, SIGKILL kills the process mid-restore. → Mitigation: the 2s restore timeout (D6) bounds our wait; systemd's default is generous. Document the recommended `TimeoutStopSec`.
- **[Dev seed file could be mistaken for a feature]** → Mitigation: logged with a "dev seed" marker; production path skips it; clearly documented as dev-only in the spec.
- **[No way to create alarms in production]** Slice 1 ships no alarm UI. The slice is acceptance-testable via dev seed only; production use requires the later web/Pi config slice. → Mitigation: explicit non-goal; the slice's value is the proven fire/restore path, not alarm management.

## Open Questions

- **Tick interval (5s vs 1s).** Lean 5s; revisit if late-firing is perceptible in testing. No effect on slice 1 design.
- **Multiple simultaneous alarms policy.** Slice 1 serializes (dismiss-and-restore current, fire queued). The PRD doesn't specify; a later slice may allow overlap. Captured here so slice 1's serialization is a known decision, not an accident.
- **`set_shuffle` vs `set_random`.** Mopidy exposes both (aliased). Slice 1 uses `shuffle` naming (PRD-consistent); if Mopidy versions differ, alias in the client. Verify against the test Mopidy in task work.
- **Snapshot read batching.** Whether the Mopidy client supports a batched/multi-call or whether slice 1 issues N parallel `call`s and awaits all. Decide at task time; the typed surface supports either.

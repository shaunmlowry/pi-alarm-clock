## Why

Slice 0 proved the architecture but ships nothing user-facing. Slice 1 delivers the alarm clock's core promise for the first time: an alarm fires at its scheduled time, plays its audio source through Mopidy (looping), and restores the user's prior Mopidy state on dismiss. This is the riskiest state machine in the system (snapshot → play → restore, interleaved with async Mopidy replies) and the path whose seams slice 0 deliberately left as no-ops (`shutdown_restore()`, the `episode` span, the `scheduler_tick` span). Building it now validates the threading model with real behavior and unblocks every later alarm feature (escalation, snooze, visual, fallback chain).

## What Changes

- **Scheduler tick (resolves slice 0 open question).** An interval tick on the main thread re-derives the next fire time from `Local::now()` each tick, recomputes on rule change and on DST boundary. Chosen over point-in-time timers because it is robust to NTP/`fake-hwclock` clock jumps — a missed alarm is a correctness bug, a slightly-late alarm is acceptable. The existing `scheduler_tick` span (defined in slice 0, unused) becomes active.
- **Alarm data model.** SQLite migration `v2` adds an `alarms` table storing `id`, `enabled`, `name`, `rrule` (RFC 5545 string), `time` (wall-clock local), `source_uri`, `max_volume`, plus a derived `next_fire` cached column recomputed on boot/rule-change/fire. CRUD lives on main as a `AlarmStore` over the single `Connection` (same single-threaded, no-mutex model as `ConfigStore`).
- **Schedule / next-fire computation.** A `Schedule` wrapper over the `rrule` crate with `next_fire(after: DateTime<Tz>) -> Option<DateTime<Tz>>`, using `chrono-tz` for DST-correct local time. Times stored as wall-clock local; next-fire is derived, not stored-authoritatively (the cached column is an optimization, recomputed from the rule). Presets (Once, Daily, Weekdays, Weekends, Specific-days) map to RRULE strings; complex RRULE is accepted but only preset-generated rules are constructed in slice 1 (full builder is web-only, a later slice).
- **Episode FSM on main.** A single-threaded state machine owned by main: `Idle → Firing(snapshot, play, loop) → Dismissed(restore)`. The fire path captures a fresh snapshot of Mopidy state (`{uri, position_ms, was_playing, seekable, volume, repeat, shuffle}`), preempts Mopidy, sets `repeat=true`, plays the alarm's `source_uri` at `max_volume` (fixed — escalation is a later slice). Dismiss restores the snapshot. The existing `episode` span (defined in slice 0, unused) becomes active.
- **Snapshot capture & restore.** The snapshot is the PRD's core guarantee: whatever the user was playing before the alarm resumes on dismiss. Snapshot is fresh per fire. On `shutdown_restore()` (SIGTERM mid-episode), the snapshot is restored before exit — the no-op seam from slice 0 becomes real.
- **Mopidy method expansion.** Adds typed wrappers for the methods the episode needs: `playback.play(uri)`, `playback.pause`, `playback.resume`, `playback.stop`, `playback.set_volume`, `playback.get_state`, `playback.get_time_position`, `tracklist.add`, `tracklist.set_repeat`, `tracklist.set_shuffle`, `tracklist.get_random`/`set_random`. Extends slice 0's typed surface mechanically (same request/reply shape).
- **Alarm episode UI.** During a firing episode, the normal panels are hidden and an alarm screen is shown exclusively (clock + tap-anywhere-to-dismiss). Panel-swipe disabled. No snooze button yet (snooze is a later slice). The alarm UI is a new Slint panel stacked above the navigation container.
- **Alarm seeding without web UI.** Slice 1 has no web config (later slice) and no Pi alarm-editing UI (later slice). Alarms are seeded via a dev `alarms.toml` (or direct DB insert) consumed at boot, so the slice is end-to-end testable without the web. This seeding path is dev-only and replaced by the web/alarm-config slice.

### Non-goals (deferred to later slices)
- Escalation steps / volume ramp (slice 2; slice 1 uses fixed `max_volume`).
- Snooze (slice 2; slice 1 dismiss is the only way to end an episode).
- Visual alarms / brightness strobe (later slice; needs display policy).
- Fallback chain + bundled beep (later slice; slice 1 plays one source, logs on failure and ends).
- Holiday suppression (needs calendar integration, later slice).
- Alarm editing UI on Pi or web (later slice; slice 1 seeds alarms via DB/dev-config).
- Daily-data, media, and settings panels (later slices).
- Quick-controls overlay (later slice).

## Capabilities

### New Capabilities
- `alarm-scheduling`: the scheduler tick, alarm data model & persistence, `Schedule`/`next_fire` over `rrule` + `chrono-tz`, RRULE presets, next-fire caching/recompute.
- `alarm-episode`: the single-threaded episode FSM (fire/snapshot/play/restore), snapshot capture & restore, `shutdown_restore()` implementation, fixed-volume playback (no escalation), the alarm episode UI (tap-anywhere-to-dismiss, no snooze).
- `mopidy-playback`: typed Mopidy method surface expanded to playback/tracklist control needed by the episode (play, pause, resume, stop, set_volume, get_state, get_time_position, tracklist.add/set_repeat/set_shuffle).

### Modified Capabilities
- `process-runtime`: the `scheduler_tick` and `episode` spans (defined-but-unused in slice 0) become active; the `shutdown_restore()` hook (no-op in slice 0) is implemented by the episode FSM to restore the snapshot on SIGTERM mid-episode.
- `persistence`: migration `v2` adds the `alarms` table; `AlarmStore` CRUD over the single `Connection`, same single-threaded/no-mutex model as `ConfigStore`.

## Impact

- **New code:** `alarm-clock` crate gains `scheduler`, `alarm_store`, `schedule` (rrule wrapper), `episode` (FSM + snapshot) modules, and the alarm episode Slint panel. The `mopidy-client` crate gains the playback/tracklist method wrappers.
- **New dependencies:** `rrule`, `chrono-tz`, `chrono` (already pulled by tokio/slint transitively but made explicit), `serde` for the alarm model. Possibly `toml` for the dev seed file (already a dependency).
- **Modified code:** `main.rs` wires the scheduler tick into the existing Slint timer drain; the shutdown handler calls the episode's `shutdown_restore()` instead of the slice-0 no-op; the `scheduler_tick`/`episode` spans are entered for real work.
- **Database:** migration `v2` (non-destructive; `v1`'s `schema_meta`/`kv_config` untouched).
- **Runtime:** a Mopidy instance with at least one playable URI is required to validate the fire/restore path end-to-end (slice 0 only needed `core.get_version`). Alarms must fire offline (Mopidy-down case: episode logs and ends without playback — the full terminal-fallback is a later slice, but slice 1 must not hang or crash).
- **First user-facing behavior:** an alarm fires at its scheduled time, plays audio, and restores prior state on dismiss. This is the slice's acceptance bar.

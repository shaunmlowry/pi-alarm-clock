## Context

Greenfield repo. The PRD specifies a single Rust process that is "the brain" — owning alarm state, scheduling, persistence, calendar, weather, display policy, and an embedded web server — with Slint (touch UI) and axum (web config) as two front-ends over one in-memory domain layer and one SQLite store, plus a Mopidy WebSocket backend as a playback servant. Mopidy is supervised by systemd separately and may restart independently.

Slice 0 exists to make the architectural spine real and to establish the seams every later slice extends. It deliberately ships no user-facing feature; its deliverable is a running process that boots, integrates with systemd, reaches Mopidy, persists to SQLite, and renders a placeholder clock — and does all of that under a threading/ownership model that will not have to be reversed later.

The PRD does not specify where the domain layer lives (which thread owns it), how Slint's event loop coexists with tokio, how the SQLite `Connection` is shared, or the error/observability/shutdown conventions. These are decided here because they cascade into every later slice.

## Goals / Non-Goals

**Goals:**
- Establish and prove the threading model: Slint on main, tokio on a worker thread, domain layer owned by main, async I/O as a servant.
- Establish cross-thread communication shapes (command/reply channels, `invoke_from_event_loop` marshalling) that later slices extend without redesign.
- Establish persistence (SQLite WAL + migrations + `ConfigStore`) reachable only from main, with no mutex/pool.
- Establish the Mopidy client: reconnecting WS JSON-RPC, typed minimal surface, event channel, and **connection-state as a domain signal**.
- Establish observability (tracing → journald), error/panic policy, graceful-shutdown seam, bootstrap config, systemd `Type=notify`.
- Render a placeholder Clock panel with the multi-panel navigation scaffold and a reserved theme seam.

**Non-Goals:**
- Alarm scheduling, episode FSM, escalation, snooze, visual alarms (slice 1+).
- Fallback chain, bundled beep (slice 1+).
- Display policies, bedtime, dynamic brightness (later).
- Calendar, weather, media transport, favorites (later).
- Web config endpoints, pairing, TLS (later) — axum binds, serves nothing.
- Theming token system, second theme (later).
- The scheduler tick / next-fire computation (slice 1) — see Open Questions.

## Decisions

### D1. Threading model: Slint on main, tokio on a worker thread, domain on main

```
   MAIN THREAD (owns domain)              WORKER: TOKIO RUNTIME
   ┌───────────────────────────────┐      ┌────────────────────────────┐
   │ Slint event loop              │      │ async tasks:              │
   │ Domain layer                  │      │   • mopidy-client WS      │
   │  · ConfigStore (rusqlite)     │ req  │   • axum server           │
   │  · (scheduler, FSM — later)  │─────▶│   • blocking I/O          │
   │ Slint UI models               │      │                            │
   │                               │ ◀────│ reply (via                │
   │ invoke_from_event_loop ◀──┐   │      │  invoke_from_event_loop)  │
   └────────────────────────────┤───┘      └────────────────────────────┘
            ▲                   │
            │                   │
            └─── replies marshalled back to main ────┘
```

**Rationale.** Three considerations converge on this layout:

1. **DB concurrency disappears.** `rusqlite::Connection` is `Send` but not `Sync`. If the domain lived on the tokio thread (where axum and Mopidy both touch it), we'd need `Mutex<Connection>` or a pool and would have to reason about WAL + mutex interaction. With the domain on main, the `Connection` lives on main, only main touches it — **no mutex, no pool, single-threaded**. The web reaches the store by sending a command over the channel; main reads the DB and replies. Config mutations are rare and fast, so the channel hop is effectively free.

2. **The episode FSM (slice 1+) is single-threaded.** The alarm fire path is the gnarliest state machine in the system (snapshot → play → escalate → snooze-refire → dismiss-restore, interleaved with Mopidy events). Driving it from two threads (timer ticks + async Mopidy replies) is a bug farm. Under this model both feeds land on main: the `slint::Timer` ticks on main, and Mopidy replies arrive on main via the reply channel marshalled with `slint::api::invoke_from_event_loop`. One thread owns the FSM. No locks.

3. **axum becomes a trivial servant.** The PRD scopes the web to config-only for v1 and explicitly "rare" by the user's call. A web handler builds a command, sends it over the channel, awaits the reply. The domain cannot be reentered from the web side — every access is serialized on main.

**Alternatives considered.**
- *Domain on tokio (A.2):* forces every UI state read through a channel — bad for a touch-primary surface that redraws continuously; also forces the DB mutex. Rejected.
- *One tokio runtime, Slint driven by a `slint::Timer` that pumps tokio tasks:* conflates the two event models and makes ownership of the domain ambiguous. Rejected.

### D2. Cross-thread channel topology

Two typed channels, both directions, owned by main:

- **`Cmd` channel (main → tokio):** an `mpsc` of `Cmd` enums (`GetMopidyState`, `CallMopidy(method, params)`, …). The tokio side drains it.
- **`Reply` channel (tokio → main):** an `mpsc` of `Reply` enums. The tokio side sends replies here; **main does not block on this channel** — instead, replies are drained by a `slint::Timer` that polls the channel non-blockingly on each Slint tick, and any work that needs to run "when the reply arrives" is encoded in the `Reply` itself (a callback-style enum variant or a tagged request id matched against pending work on main).

This avoids the anti-pattern of blocking the Slint event loop waiting on a channel. The non-blocking poll is cheap (one `try_recv` per tick) and matches Slint's idiomatic "timer-driven" update model.

**Mopidy event channel** is a separate `mpsc<Events::Mopidy>` flowing tokio → main, drained by the same timer. Events (state changed, tracklist ended) are delivered to the domain on main; in slice 0 they are logged and otherwise ignored, establishing the seam.

### D3. SQLite: WAL, migrations, single-connection on main

- **WAL mode** set on every connection open (`pragma journal_mode=WAL`), plus `synchronous=NORMAL` (WAL + NORMAL is durable across application crashes; FULL is unnecessary given atomic-write-on-mutation).
- **One `Connection`**, owned by main, held for the process lifetime. No pool, no `Mutex`. Migrations run on startup inside this connection.
- **Migration framework:** a minimal `user_version` pragma-based runner (no heavyweight dep). Each migration is a `&str` of SQL applied in order; `user_version` is bumped. Slice 0 ships migration `v1` creating a `schema_meta` table and a trivial `kv_config` table (proving the round-trip with one persisted value, e.g. a "last boot" timestamp).
- **Atomic writes:** every config mutation is a single transaction (`BEGIN … COMMIT`). For multi-statement mutations, all statements live in one transaction. This satisfies the PRD's "synchronous atomic write on every config mutation."

### D4. Mopidy client: reconnecting WS, typed surface, connection-state signal

- **Transport:** `tokio-tungstenite` WebSocket to Mopidy's `mopidy/http` + `mopidy/json` frontend (default `ws://localhost:6680/mopidy/ws`).
- **Protocol:** JSON-RPC 2.0. Outgoing: typed request structs serialized to `{jsonrpc, id, method, params}`. Incoming: dispatch on `id` (reply) vs presence of `method` (event).
- **Reconnect/backoff:** exponential backoff with jitter, capped (e.g. initial 500ms, factor 2, cap 30s, ±20% jitter), retrying **indefinitely** — the app does not block on Mopidy and alarms must fire without it (slice 1). State transitions: `Disconnected → BackingOff → Connecting → Connected`, with the current state published as a domain signal.
- **Typed minimal surface (slice 0):** `core.get_version` and `core.get_state`. The wrapper is structured so adding methods is mechanical (a request struct, a `call` method, a typed reply) — the *shape* is the contribution, not the method count.
- **Event channel:** the client parses incoming JSON-RPC events into an `enum MopidyEvent` and forwards to the event channel. Slice 0 logs every event; slice 1+ consumes them.
- **Connection-state signal:** `MopidyConnectionState { Disconnected, BackingOff(retry_in), Connecting, Connected }` is published on every transition via the reply channel. **Nothing consumes it in slice 0** — it exists so slice 1's fallback chain ("degrade to terminal visual if Mopidy is down") and mid-episode-restart logic have a signal to branch on. This is the seam most likely to be retrofitted badly if skipped.

### D5. Observability: tracing → journald

- `tracing` with `#[instrument]` at module/function boundaries; spans at process boundaries: `bootstrap`, `mopidy_request{method}`, `scheduler_tick` (named now, unused), `episode` (named now, unused).
- `tracing-subscriber` with a `fmt` layer fallback + `tracing-journald` layer when available. Structured fields (alarm_id, mopidy_state, etc.) carry through to `journalctl -o json`.
- **Why journald:** systemd-native, free structured fields, queryable, already where `Type=notify` logs go. Retrofitting spans into every slice later is annoying; doing it once at slice 0 is cheap.

### D6. Error & panic policy

- **`anyhow`** at the app boundary (`main`, bootstrap). **`thiserror`** for domain error types (e.g. `ConfigError`, `MopidyClientError`).
- **Tick-level `catch_unwind`:** the scheduler/system tick (slice 0: a no-op periodic tick proving the pattern) wraps its body in `std::panic::catch_unwind`. A panic is logged at `error!` and the tick reschedules. **Cardinal rule:** a bug in one tick must not sink the alarm guarantee. Established now so every later slice follows it.
- **Failed config writes degrade, not panic:** a write that fails is logged loudly (`error!`) and the in-memory state remains authoritative. The process does not exit. (The PRD's snapshot/restore guarantees are about *behavioral* failure; this is about *code-level* failure.)

### D7. Graceful shutdown seam

- `tokio::signal::ctrl_c` + a systemd `SIGTERM` listener on the tokio worker, communicating "shutdown requested" to main via the reply channel.
- Main, on receiving shutdown: drains the Cmd channel, asks the tokio side to drop the Mopidy client and stop axum, commits any pending DB transaction, and exits.
- **Seam for slice 1+:** the shutdown handler calls a trait method `shutdown_restore()` on the domain (slice 0's implementation is a no-op). Slice 1's episode FSM implements `shutdown_restore()` to restore the snapshot before exiting — the same restore path as dismiss, minus UI. **This seam must exist in slice 0** so slice 1 doesn't have to restructure shutdown to reach the episode.

### D8. Bootstrap config: tiny `config.toml` + compiled defaults

- A `config.toml` at a known path (e.g. `/etc/alarm-clock/config.toml` on the Pi, `./config.toml` for dev), parsed with `serde` + `toml`.
- Fields: `db_path`, `mopidy_ws_url`, `axum_bind_addr`, `log_level`, `data_dir`. Each has a compiled default; the file overrides.
- **Why a file and not SQLite:** the app needs these values *before* the DB exists. This is the unavoidable bootstrap layer the PRD omits.

### D9. UI shell: one panel, navigation scaffold, reserved theme seam

- **Slint application** with the Slint build macro (`slint::slint!` or `.slint` files compiled in). Vertical 9:16.
- **Navigation scaffold:** a horizontal panel container that supports swipe-between-panels with hard stops (no wraparound). Slice 0 ships **one** panel (Clock); the scaffold exists so slice 1+ adds panels without touching the navigation code.
- **Clock panel:** renders a placeholder clock face. Exposes theme-relevant properties (`clock_color`, `font_family`, …) fed by **hardcoded values** in slice 0. This **reserves the theme seam**: slice "theming" swaps the hardcoded feeder for the token system without rewriting the Clock component.
- **No text input, no scrolling** (per PRD). Vertical gestures reserved for future quick-controls overlay (slice 0 does not implement the overlay, just reserves the gesture space by not consuming vertical swipes on the Clock panel).

### D10. systemd readiness

- `Type=notify`. App calls `sd_notify(READY=1)` after: bootstrap config parsed + DB migrated + Mopidy client started (not necessarily connected) + axum bound + Slint event loop about to run.
- `Restart=on-failure` with `StartLimitBurst`/`StartLimitIntervalSec` backoff.
- `After=network-online.target` + `Wants=` it; `After=mopidy.service` + `Wants=` it. App does **not** block on either.
- **No `WatchdogSec` in slice 0** (deferred; the alarm guarantee doesn't depend on it and wiring `sd_notify(WATCHDOG=1)` into the Slint tick is a separate decision).

## Risks / Trade-offs

- **[Slint timer-poll model under load]** The non-blocking `try_recv` poll per Slint tick is cheap, but if the Slint event loop ever stalls (long synchronous work on main), Mopidy replies/events back up in the channel. → Mitigation: keep main-thread work short; any non-trivial synchronous work (e.g. a heavy DB migration) is dispatched to tokio and reported back via the reply channel. Slice 0's migrations are tiny.
- **[Channel backpressure]** `mpsc` channels are bounded; a flood of Mopidy events could block the tokio sender. → Mitigation: bound the event channel and drop+log oldest on overflow (events are informational; the FSM re-derives state from `get_state` when it matters). Slice 0 establishes the bounded channel + drop-oldest policy.
- **[rusqlite on main blocks the event loop]** A slow query blocks Slint. → Mitigation: slice 0 queries are trivial; the policy is "if a query might be slow, it doesn't belong on main" — but the single-threaded model means we accept this trade-off for the simplicity of no mutex. Revisit only if a real slow query appears (none anticipated for v1's data volume).
- **[tokio runtime lifetime]** The worker runtime must outlive the axum server and Mopidy client, and must shut down cleanly on SIGTERM. → Mitigation: the runtime is built in `main` before the Slint loop and dropped after it exits; shutdown coordination via D7.
- **[Mopidy connection-state signal unused in slice 0]** Shipping an unconsumed signal risks it being wrong-shaped when slice 1 needs it. → Mitigation: the signal is modeled on the *client's* lifecycle states (`Disconnected/BackingOff/Connecting/Connected`), which are intrinsic to the client and unlikely to change; slice 1 consumes, it doesn't redefine.
- **[Reserved theme seam could be mis-shaped]** Hardcoding a couple of properties now and guessing wrong means slice "theming" refactors the Clock anyway. → Mitigation: keep the exposed properties minimal and obvious (`color`, `font_family`); a theme is overwhelmingly colors and fonts, so the bet is low-risk.
- **[No real feature to validate the spine]** Slice 0 ships nothing user-facing, so "does it work?" is harder to demonstrate. → Mitigation: slice 0's acceptance is the tasks' verification steps (log lines, journald entries, Mopidy `get_version` round-trip, persisted `kv_config` round-trip, `systemctl status` showing `active (running)` with `READY=1`). Slice 1 is the first user-facing validation.

## Open Questions

- **Scheduler tick model (deferred to slice 1's design).** Interval-with-recompute-on-tick (robust to NTP/`fake-hwclock` clock jumps, simpler) vs point-in-time timer armed for `next_fire - now` (precise, but a clock jump can fire it wrong or never). Lean: interval tick that re-derives `next_fire` from `Local::now()` each time, recompute on rule change and on DST boundary. Captured here so slice 1 doesn't re-litigate; the choice does not affect slice 0 because slice 0 has no scheduler.
- **`Shutdown_restore()` seam shape.** Should it be a method on a `Domain` trait, or a dedicated `ShutdownCoordinator` that the episode FSM registers itself with? Decide in slice 1 when the FSM exists; slice 0 only needs the shutdown handler to call *something* that is currently a no-op.
- **Slint `.slint` files vs inline `slint::slint!`.** Pure tooling/ergonomics; decide at task time. Lean: `.slint` files for anything non-trivial, for editor support.

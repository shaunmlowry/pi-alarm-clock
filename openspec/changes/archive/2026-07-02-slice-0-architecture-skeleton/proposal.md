## Why

The PRD describes a large appliance (alarms, media, calendar, weather, web config, theming) built on a single Rust process with two front-ends over one domain layer. Before any user-facing feature can be built reliably, the architectural spine must exist and be proven: how Slint and tokio coexist, where the domain layer lives, how SQLite is reached, how Mopidy is spoken to, how the process integrates with systemd, and how observability/error/shutdown are handled. Slice 0 establishes that spine and the seams every later slice extends — it ships nothing user-facing on purpose.

## What Changes

- **Process architecture made real.** Single Rust process: Slint event loop on the main thread owning the domain layer; a dedicated tokio runtime on a worker thread acting as an async servant (Mopidy WS, axum, blocking I/O). Cross-thread communication via typed command/reply channels marshalled back to main with `slint::api::invoke_from_event_loop`.
- **Domain layer on main (single-threaded).** Scheduler, episode FSM (when introduced), and `ConfigStore` live on main. SQLite `Connection` is touched only from main — no `Mutex`, no pool. The web (axum) reaches the domain by sending a command over the channel and awaiting the reply.
- **SQLite persistence established.** `rusqlite` with WAL mode, a migration framework, and synchronous atomic writes on every config mutation. Slice 0 ships a minimal schema + one trivial persisted value to prove the round-trip; the real schema (alarms, favorites, calendars) is added by later slices.
- **Mopidy client skeleton.** One reconnecting WebSocket JSON-RPC client (`mopidy-client` crate) with exponential backoff + jitter, a typed minimal surface (`core.get_version`, `core.get_state`), and an event channel. Exposes **Mopidy connection state** as a domain signal (connected/disconnected/backoff) even though nothing consumes it in slice 0 — the fallback chain and mid-episode restart logic in later slices depend on it.
- **UI shell + clock panel.** Slint application rendering a single **Clock panel** (placeholder clock face) with the multi-panel navigation scaffold in place (horizontal swipe, no wraparound) even though only one panel exists. The clock exposes theme-relevant properties (colors, font) fed by hardcoded values, reserving the theme seam without building the token system.
- **systemd integration.** `Type=notify` unit; app signals readiness after bootstrap config loaded + DB migrated + Mopidy client started (not necessarily connected) + axum bound. `Restart=on-failure` with backoff. `After/Wants` on `network-online.target` and `mopidy.service` without blocking on either.
- **Observability.** `tracing` + `tracing-subscriber` to journald (structured). Span conventions established at the process boundaries (bootstrap, scheduler tick, Mopidy request, episode — the last named now even if unused).
- **Error & panic policy.** `anyhow` at app boundaries, `thiserror` for domain error types. Scheduler/system ticks wrap work in `catch_unwind` so a panic in one tick cannot sink the process. Failed config writes degrade (log loudly, keep in-memory state) rather than panic.
- **Graceful shutdown seam.** SIGTERM handling wired at the process level. Slice 0 restores nothing (no episode exists), but the shutdown path is structured so a later slice can query "is there an active episode?" and run the snapshot-restore path before exiting.
- **Bootstrap config.** A tiny `config.toml` (with compiled defaults) for the pre-DB settings the app needs to boot: DB path, Mopidy WS URL, axum bind addr/port, log level, data dir.

### Non-goals (deferred to later slices)
- Alarm scheduling, episode FSM, escalation, snooze, visual alarms (slice 1+).
- Fallback chain, bundled beep (slice 1+).
- Display policies, bedtime, dynamic brightness (later slice).
- Calendar, weather, full media transport, favorites (later slices).
- Web config UI / pairing / TLS (later slice) — slice 0's axum binds but serves no config endpoints.
- Theming token system and second theme (later slice).
- Live media control from the web (v2 per PRD).

## Capabilities

### New Capabilities
- `process-runtime`: process architecture (Slint+tokio coexistence, domain-on-main threading model), bootstrap config, logging/observability, error & panic policy, graceful shutdown seam, systemd integration.
- `persistence`: SQLite store (WAL, migrations, atomic writes), `ConfigStore` abstraction reachable only from main.
- `mopidy-client`: reconnecting WebSocket JSON-RPC client, typed minimal surface, event channel, connection-state signal.
- `ui-shell`: Slint application, multi-panel navigation scaffold, Clock panel with theme seam reserved.

### Modified Capabilities
<!-- None — greenfield repo, no existing specs. -->

## Impact

- **New code:** Rust workspace with crates for the app binary and `mopidy-client` (and internal modules for process-runtime, persistence, ui-shell). `Cargo.toml`, `config.toml` defaults, systemd unit file.
- **New dependencies:** `slint`, `tokio`, `axum`, `rusqlite`, `tracing`, `tracing-subscriber`, `tracing-journald`, `anyhow`, `thiserror`, `serde`/`toml`, a WebSocket client crate (e.g. `tokio-tungstenite`), `serde_json`, `slint` build tooling.
- **Runtime:** runs on the target Pi 5; needs Mopidy reachable over its WS frontend for the client to connect (but does not require it to boot).
- **No user-facing behavior** in slice 0 — the screen shows a placeholder clock; no alarms, no media control, no web config. This is intentional: the value is the proven architecture and the seams, not features.

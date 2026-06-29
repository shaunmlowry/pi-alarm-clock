## 1. Workspace & bootstrap

- [x] 1.1 Create the Cargo workspace: a binary crate (`alarm-clock`) and a library crate (`mopidy-client`). Pin a Rust edition and toolchain.
- [x] 1.2 Add core dependencies: `tokio`, `slint`, `axum`, `rusqlite`, `tracing`, `tracing-subscriber`, `tracing-journald`, `anyhow`, `thiserror`, `serde`, `toml`, `tokio-tungstenite`, `serde_json`. Add the Slint build tooling (`.slint` files or `slint::slint!`).
- [x] 1.3 Define the `Config` struct (`db_path`, `mopidy_ws_url`, `axum_bind_addr`, `log_level`, `data_dir`) with `serde` defaults, parse from `config.toml` (path: `/etc/alarm-clock/config.toml` in release, `./config.toml` in dev), and fall back to compiled defaults on missing file/fields.
- [x] 1.4 Create a sample `config.toml` committed for dev.

## 2. Process runtime

- [x] 2.1 Define the `Cmd` and `Reply` enums and the `MopidyEvent` enum. Create bounded `mpsc` channels in both directions; the event channel uses drop-oldest-on-full with a `warn!` log.
- [x] 2.2 Build the tokio worker runtime on a dedicated thread; establish the non-blocking `try_recv` drain timer on the Slint tick that dispatches replies/events to the domain on main.
- [x] 2.3 Install `tracing` with a `tracing-journald` layer when available and a `fmt` fallback otherwise; define the `bootstrap`, `mopidy_request{method}`, `scheduler_tick`, and `episode` spans (the latter two unused).
- [x] 2.4 Implement the tick-level `catch_unwind` wrapper for periodic ticks: a panic is logged at `error!` and the tick reschedules.
- [x] 2.5 Implement the error/panic policy: `anyhow` at the app boundary, `thiserror` for domain errors; failed config writes degrade (log `error!`, keep in-memory state, do not exit).
- [x] 2.6 Implement `SIGTERM`/`SIGINT` handling on the tokio worker that signals shutdown to main; main drains the Cmd channel, stops the Mopidy client and axum, commits pending DB work, calls the domain's `shutdown_restore()` hook (no-op in slice 0), and exits 0.
- [x] 2.7 Wire `sd_notify(READY=1)` after bootstrap config parsed + DB migrated + Mopidy client started + axum bound (graceful when Mopidy is not yet connected).

## 3. Persistence

- [x] 3.1 Open a single `rusqlite::Connection` on main; set `PRAGMA journal_mode=WAL` and `PRAGMA synchronous=NORMAL`.
- [x] 3.2 Implement the `user_version`-based migration runner; migrations applied in order, skipped when `user_version` is already at the target.
- [ ] 3.3 Write migration `v1`: create `schema_meta` and `kv_config(key TEXT PRIMARY KEY, value TEXT NOT NULL)`; bump `user_version` to `1`.
- [ ] 3.4 Implement `ConfigStore` (owned by main) with `get(key)` / `set(key, value)` over `kv_config`, each as a single transaction; multi-statement mutations roll back on partial failure.
- [ ] 3.5 Verify the round-trip: write `("last_boot", "<iso8601>")`, read it back, assert equality.

## 4. Mopidy client (`mopidy-client` crate)

- [ ] 4.1 Implement the WebSocket transport with `tokio-tungstenite` to the configured `mopidy_ws_url`; implement JSON-RPC 2.0 framing (`{jsonrpc, id, method, params}`).
- [ ] 4.2 Implement the reconnect loop with exponential backoff + jitter (initial ~500ms, factor ~2, cap ~30s, ±20%), retrying indefinitely; do not block application boot.
- [ ] 4.3 Define `MopidyConnectionState { Disconnected, BackingOff { retry_in }, Connecting, Connected }` and publish it on every transition via the reply channel; log each transition.
- [ ] 4.4 Implement typed wrappers for `core.get_version` and `core.get_state` (request struct + `call` + typed reply); structure so adding methods is mechanical.
- [ ] 4.5 Implement event parsing: incoming messages with a `method` field are parsed into `enum MopidyEvent` and forwarded to the bounded event channel; log every event.
- [ ] 4.6 Verify end-to-end against a running Mopidy: `core.get_version` round-trips a typed reply; `core.get_state` returns a typed state; disconnecting/restarting Mopidy transitions through backoff to `Connected`.

## 5. UI shell

- [ ] 5.1 Create the Slint application (`.slint` file) in vertical 9:16 orientation, touch-only, no text input, no scrolling; run the Slint event loop on main.
- [ ] 5.2 Implement the horizontal panel navigation container with hard stops at both ends (no wraparound); ship exactly one panel.
- [ ] 5.3 Ensure vertical gestures on the Clock panel are not consumed by navigation (reserved for the future quick-controls overlay).
- [ ] 5.4 Implement the Clock panel rendering a placeholder clock face; expose theme-relevant properties (`clock_color`, `font_family`) fed by hardcoded values as the theme seam.

## 6. Integration & acceptance

- [ ] 6.1 Author the systemd unit file (`Type=notify`, `Restart=on-failure` with backoff, `After/Wants=network-online.target`, `After/Wants=mopidy.service`, no blocking on either).
- [ ] 6.2 Verify on the Pi: `systemctl start` brings the unit to `active (running)` with `READY=1` even when Mopidy is down.
- [ ] 6.3 Verify on the Pi: the screen shows the placeholder Clock panel in vertical orientation.
- [ ] 6.4 Verify on the Pi: `journalctl` shows structured entries with span context and fields; Mopidy state transitions and events are logged.
- [ ] 6.5 Verify on the Pi: reboot the device — the database migration is idempotent (`user_version=1`, no re-migration), `ConfigStore` round-trips, and the unit reaches `READY=1`.
- [ ] 6.6 Verify graceful shutdown: `systemctl stop` causes the process to exit 0 with a clean shutdown log sequence (Mopidy client stopped, axum stopped, DB committed, `shutdown_restore()` invoked).

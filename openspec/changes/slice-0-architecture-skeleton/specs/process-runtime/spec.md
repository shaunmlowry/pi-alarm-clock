## ADDED Requirements

### Requirement: Single Rust process with two-thread architecture
The application SHALL run as a single Rust process containing the Slint event loop on the main thread and a dedicated tokio runtime on a worker thread. The domain layer (configuration store, and in later slices the scheduler and episode FSM) SHALL be owned by the main thread. The tokio worker thread SHALL act as an async servant for Mopidy WebSocket I/O, the axum server, and blocking operations.

#### Scenario: Main thread owns the domain
- **WHEN** the process has finished bootstrapping
- **THEN** the Slint event loop is running on the main thread and the `rusqlite::Connection` is only ever accessed from the main thread (no `Mutex`, no connection pool)

#### Scenario: Tokio worker services async I/O
- **WHEN** the main thread needs to perform an async operation (e.g. call Mopidy)
- **THEN** it SHALL send a typed command over a command channel to the tokio worker and receive a reply over a reply channel marshalled back to the main thread via `slint::api::invoke_from_event_loop`

### Requirement: Non-blocking reply consumption on main
The main thread SHALL NOT block the Slint event loop waiting on the reply or event channels. Replies and events SHALL be drained non-blockingly by a Slint timer on each tick.

#### Scenario: Reply poll does not stall the UI
- **WHEN** the Slint event loop is running
- **THEN** a timer on each tick performs a non-blocking `try_recv` on the reply and event channels and dispatches any received items to the domain, without sleeping

### Requirement: Bounded event channel with drop-oldest
The Mopidy event channel SHALL be bounded. When the channel is full and a new event arrives, the oldest event SHALL be dropped and logged at `warn!` level.

#### Scenario: Flood of events does not block the Mopidy client
- **WHEN** the Mopidy client receives events faster than main drains them and the channel is at capacity
- **THEN** the oldest event is dropped, a `warn!` log entry is emitted, and the Mopidy client is not blocked from continuing

### Requirement: Bootstrap configuration via TOML file
The application SHALL parse a `config.toml` file (at `/etc/alarm-clock/config.toml` in production, `./config.toml` in development) using `serde` + `toml`. The file SHALL override compiled defaults for: `db_path`, `mopidy_ws_url`, `axum_bind_addr`, `log_level`, `data_dir`. Missing fields SHALL fall back to compiled defaults; a missing file SHALL not prevent boot (defaults used).

#### Scenario: Missing config file uses defaults
- **WHEN** `config.toml` does not exist at the resolved path
- **THEN** the application boots using compiled defaults for every field

#### Scenario: Partial override
- **WHEN** `config.toml` specifies `db_path` and `log_level` but omits the other fields
- **THEN** the application uses the file's values for those two fields and compiled defaults for the rest

### Requirement: Structured logging to journald
The application SHALL use `tracing` with structured spans and fields. A `tracing-journald` layer SHALL be installed when journald is available; a `fmt` fallback layer SHALL be installed otherwise. Spans SHALL exist at process boundaries: `bootstrap`, `mopidy_request{method}`, `scheduler_tick` (defined now, unused), and `episode` (defined now, unused).

#### Scenario: Logs appear in journald
- **WHEN** the application is running on the Pi under systemd
- **THEN** `journalctl` for the unit shows structured log entries with span context and fields

### Requirement: Tick-level panic isolation
Periodic ticks on the main thread SHALL wrap their body in `std::panic::catch_unwind`. A panic in one tick SHALL be logged at `error!` level and the tick SHALL reschedule on its next interval; the process SHALL NOT exit.

#### Scenario: Panic in a tick does not kill the process
- **WHEN** a periodic tick body panics
- **THEN** the panic is caught, an `error!` entry is logged, the process continues, and the next tick fires on schedule

### Requirement: Failed config writes degrade, not panic
A failed configuration write to SQLite SHALL be logged at `error!` level and the in-memory state SHALL remain authoritative. The process SHALL NOT exit on a failed write.

#### Scenario: Disk-full write failure
- **WHEN** a config mutation transaction fails to commit (e.g. disk full)
- **THEN** an `error!` entry is logged, the in-memory state is unchanged, and the process continues running

### Requirement: Graceful shutdown seam
The application SHALL handle `SIGTERM` (and `SIGINT`) by coordinating shutdown: the tokio worker is asked to stop the Mopidy client and axum, any pending DB transaction is committed, and the process exits. The shutdown path SHALL call a `shutdown_restore()` hook on the domain whose slice-0 implementation is a no-op, so later slices can implement snapshot-restore before exit.

#### Scenario: SIGTERM triggers clean exit
- **WHEN** the process receives `SIGTERM`
- **THEN** the Mopidy client is stopped, axum is stopped, pending DB work is committed, the domain's `shutdown_restore()` hook is invoked (no-op in slice 0), and the process exits with code 0

### Requirement: systemd Type=notify readiness
The application SHALL run under a systemd unit of `Type=notify` and SHALL call `sd_notify(READY=1)` after bootstrap config is parsed, the database is migrated, the Mopidy client is started (not necessarily connected), and axum is bound. The unit SHALL `Wants=` and `After=` `network-online.target` and `mopidy.service` without blocking on either. The unit SHALL use `Restart=on-failure` with backoff.

#### Scenario: Ready signal after successful bootstrap
- **WHEN** bootstrap completes (config parsed, DB migrated, Mopidy client started, axum bound) even if Mopidy is not yet connected
- **THEN** `sd_notify(READY=1)` is sent and systemd marks the unit `active (running)`

#### Scenario: Mopidy-down boot is still ready
- **WHEN** Mopidy is not reachable at boot time
- **THEN** the application still reaches `READY=1` (the Mopidy client retries in the background)

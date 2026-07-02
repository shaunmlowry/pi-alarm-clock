# persistence Specification

## Purpose
TBD - created by archiving change slice-0-architecture-skeleton. Update Purpose after archive.
## Requirements
### Requirement: SQLite store with WAL mode
The application SHALL persist state in a single SQLite database via `rusqlite`. The connection SHALL be opened with `PRAGMA journal_mode=WAL` and `PRAGMA synchronous=NORMAL`. Exactly one `Connection` SHALL exist, owned by the main thread for the process lifetime; no `Mutex` or connection pool SHALL be used.

#### Scenario: WAL mode active
- **WHEN** the database is opened
- **THEN** `PRAGMA journal_mode` returns `wal` and `PRAGMA synchronous` returns `NORMAL`

#### Scenario: Single connection on main
- **WHEN** the process is running
- **THEN** exactly one `rusqlite::Connection` exists and it is only accessed from the main thread

### Requirement: Versioned migrations on startup
The application SHALL run database migrations on startup using the `user_version` pragma. Migrations SHALL be applied in order; each migration bumps `user_version`. Migrations SHALL be idempotent-safe only in the sense that they are skipped when `user_version` is already at or above the migration's version. Slice 0 SHALL ship migration `v1` creating a `schema_meta` table and a `kv_config(key TEXT PRIMARY KEY, value TEXT NOT NULL)` table.

#### Scenario: Fresh database migrates to v1
- **WHEN** the application boots against a fresh (nonexistent) database file
- **THEN** migration `v1` is applied, `schema_meta` and `kv_config` tables exist, and `user_version` is `1`

#### Scenario: Already-migrated database skips migrations
- **WHEN** the application boots against a database whose `user_version` is `1`
- **THEN** no migrations are applied and startup proceeds

### Requirement: ConfigStore abstraction on main
The application SHALL expose a `ConfigStore` abstraction (owned by main) as the sole read/write path to persisted configuration. `ConfigStore` SHALL support a key-value read/write over `kv_config` in slice 0. All `ConfigStore` operations SHALL execute on the main thread.

#### Scenario: Round-trip a value
- **WHEN** `ConfigStore` writes `("last_boot", "<iso8601>")` and then reads `("last_boot")`
- **THEN** the read returns the value that was written

### Requirement: Atomic config mutations
Every configuration mutation SHALL be a single SQLite transaction (`BEGIN … COMMIT`). Multi-statement mutations SHALL place all statements within one transaction. A mutation that fails partway SHALL roll back, leaving the database unchanged.

#### Scenario: Partial failure rolls back
- **WHEN** a mutation writes two rows and the second statement errors
- **THEN** the transaction rolls back and neither row is persisted

### Requirement: Migration v2 adds alarms table
The application SHALL ship migration `v2` (applied after slice 0's `v1`) that creates an `alarms` table with columns: `id` (TEXT PRIMARY KEY), `enabled` (INTEGER 0/1), `name` (TEXT), `time_local` (TEXT "HH:MM:SS"), `timezone` (TEXT IANA name), `rrule` (TEXT nullable), `once_at` (TEXT nullable ISO8601 local), `source_uri` (TEXT), `max_volume` (INTEGER 0..100, default 40), `next_fire` (TEXT nullable ISO8601 UTC, derived cache), `created_at` (TEXT), `updated_at` (TEXT). The migration SHALL be non-destructive (slice 0's `schema_meta` and `kv_config` SHALL be untouched) and SHALL bump `user_version` to `2`.

#### Scenario: Migration v2 creates alarms table
- **WHEN** the application starts on a database at `user_version=1`
- **THEN** migration `v2` runs, the `alarms` table is created, `user_version` becomes `2`, and `schema_meta`/`kv_config` are unchanged

#### Scenario: Migration v2 is idempotent
- **WHEN** the application starts on a database already at `user_version=2`
- **THEN** migration `v2` is skipped (no re-application) and `alarms` is intact

### Requirement: AlarmStore CRUD on main with atomic writes
The application SHALL provide an `AlarmStore` (owned by the main thread, holding a reference to the single `rusqlite::Connection`) with `list()`, `get(id)`, `upsert(alarm)`, `delete(id)`, `set_enabled(id, bool)`, and `recompute_next_fires(now)`. Each mutation SHALL be a single transaction (slice 0's atomic-write policy); multi-statement mutations SHALL roll back on partial failure. The `Connection` SHALL be touched only from the main thread (no `Mutex`, no pool), consistent with slice 0's `ConfigStore` model.

#### Scenario: Upsert and read back an alarm
- **WHEN** an `Alarm` is upserted by `id` and then `get(id)` is called
- **THEN** the read returns the alarm with all fields equal to the upserted values

#### Scenario: Upsert is idempotent by id
- **WHEN** the same alarm (same `id`) is upserted twice
- **THEN** only one row exists for that `id` (the second upsert updates, not inserts)

#### Scenario: set_enabled flips the flag in one transaction
- **WHEN** `set_enabled(id, false)` is called
- **THEN** the `enabled` column for that `id` becomes `0` and the change is committed atomically

#### Scenario: Failed mutation rolls back
- **WHEN** a multi-statement mutation fails partway (e.g. disk full)
- **THEN** the transaction rolls back, no partial change is persisted, an `error!` is logged, and the in-memory state remains authoritative

### Requirement: next_fire caching recomputed from the rule
The `next_fire` column SHALL be a derived cache recomputed by `recompute_next_fires(now)` on boot, on rule change, and after a fire. The rule (`rrule`/`once_at` + `time_local` + `timezone`) SHALL remain the source of truth; the FSM SHALL re-derive next-fire from the rule on the tick rather than trusting the cache blindly.

#### Scenario: next_fire cache populated on boot
- **WHEN** the application boots with alarms in the database
- **THEN** `recompute_next_fires(now)` populates `next_fire` for each enabled alarm from its rule

#### Scenario: next_fire recomputed after a fire
- **WHEN** an alarm fires
- **THEN** its `next_fire` cache is recomputed to its next future occurrence (or set NULL/disabled for a `Once` alarm)

### Requirement: Escalation steps and fallback chain persisted on the Alarm
The `Alarm` model SHALL carry two optional fields: `escalation_steps: Option<Vec<EscalationStep>>` and `fallback_chain: Option<Vec<String>>`. An `EscalationStep` SHALL consist of `after_secs: u64` (seconds elapsed since fire at which the step's volume takes effect) and `volume: u8` (0..=100). `escalation_steps` SHALL be sorted ascending by `after_secs` by the store on write. `fallback_chain` is an ordered list of backup source URIs; the alarm's `source_uri` is the primary (always tried first) and is not duplicated into the chain. `None` or empty for either field SHALL preserve slice-1 behavior (fixed `max_volume`; no fallback). The `AlarmStore` SHALL round-trip both fields through SQLite without loss.

#### Scenario: Upsert and read back an alarm with escalation and fallback
- **WHEN** an alarm with `escalation_steps = [{after_secs=0,volume=20},{after_secs=60,volume=80}]` and `fallback_chain = ["spotify:backup","file:///beep.mp3"]` is upserted and read back
- **THEN** both fields are preserved exactly (order and values)

#### Scenario: Alarm with no escalation/fallback round-trips as None
- **WHEN** an alarm authored without the new fields (a slice-1 alarm) is upserted and read back
- **THEN** `escalation_steps` and `fallback_chain` are both `None` and the alarm behaves as in slice 1

#### Scenario: Store re-sorts escalation steps by after_secs on write
- **WHEN** an alarm is upserted with `escalation_steps` in non-ascending order
- **THEN** the store sorts them ascending by `after_secs` before persisting, and a subsequent read returns the sorted order

### Requirement: Migration v3 adds escalation_steps and fallback_chain columns
Migration `v3` SHALL add nullable `escalation_steps TEXT` and `fallback_chain TEXT` columns to the `alarms` table and bump `user_version` to `3`, applied inside a single transaction. The migration SHALL be non-destructive: existing rows SHALL receive `NULL` for both columns (deserializing to `None` = slice-1 behavior), and `v1`/`v2` schema/data SHALL be untouched. The migration SHALL be idempotent (starting at `user_version >= 3` skips re-application).

#### Scenario: Fresh database migrates to v3 with the new columns
- **WHEN** a fresh database is opened and migrations are run
- **THEN** `user_version` is `3`, the `alarms` table has `escalation_steps` and `fallback_chain` columns, and existing `v1`/`v2` tables are intact

#### Scenario: Existing v2 database upgrades to v3 preserving data
- **WHEN** a database at `user_version = 2` with existing alarm rows is migrated to `v3`
- **THEN** all existing alarm rows survive, their `escalation_steps` and `fallback_chain` columns are `NULL`, and `user_version` becomes `3`

#### Scenario: v3 migration is idempotent
- **WHEN** migrations are run on a database already at `user_version = 3`
- **THEN** no work is done, `user_version` remains `3`, and existing data is unchanged

### Requirement: Dev seed supports optional escalation_steps and fallback_chain
The dev `alarms.toml` seed format SHALL accept optional `escalation_steps` (array of `{after_secs, volume}` tables) and `fallback_chain` (array of strings) on each `[[alarms]]` entry. Absent fields SHALL seed as `None`. The seed path SHALL remain dev-only and idempotent.

#### Scenario: Seed entry with escalation and fallback is upserted
- **WHEN** a dev `alarms.toml` entry includes `escalation_steps` and `fallback_chain`
- **THEN** the upserted alarm carries both fields and they round-trip through the store

#### Scenario: Seed entry without the new fields seeds as None
- **WHEN** a dev `alarms.toml` entry omits `escalation_steps` and `fallback_chain`
- **THEN** the upserted alarm has both fields as `None` (slice-1 behavior)


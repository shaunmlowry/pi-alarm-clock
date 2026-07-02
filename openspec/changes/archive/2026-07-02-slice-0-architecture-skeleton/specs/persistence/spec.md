## ADDED Requirements

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

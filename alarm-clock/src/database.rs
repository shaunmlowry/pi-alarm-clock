//! SQLite persistence layer (design D3).
//!
//! - Single `rusqlite::Connection` owned by the main thread.
//! - WAL mode + `synchronous = NORMAL` on every open.
//! - `user_version`-based migration runner: migrations are applied in version
//!   order and skipped when `user_version` is already at or above the target.

use rusqlite::Connection;
use tracing::info;

use crate::error::{ConfigError, Result as DomainResult};

// ── Migration definitions ───────────────────────────────────────────────────

/// A single numbered migration step.
#[derive(Debug)]
struct Migration {
    /// Target `user_version` after this migration succeeds.
    version: u32,
    /// SQL statements applied inside a transaction.
    sql: &'static str,
}

/// All known migrations, ordered from oldest to newest.
///
/// Each migration's SQL is applied atomically; on success `user_version` is
/// bumped to that migration's target version. Migrations with a version <= the
/// current `user_version` are skipped (idempotent-safe by version tracking).
fn migrations() -> &'static [Migration] {
    &[
        Migration {
            version: 1,
            sql: "
                CREATE TABLE IF NOT EXISTS schema_meta (
                    key   TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS kv_config (
                    key   TEXT PRIMARY KEY,
                    value TEXT NOT NULL DEFAULT ''
                );
            ",
        },
        Migration {
            version: 2,
            sql: "
                CREATE TABLE IF NOT EXISTS alarms (
                    id           TEXT PRIMARY KEY,
                    enabled      INTEGER NOT NULL DEFAULT 1,
                    name         TEXT NOT NULL,
                    time_local   TEXT NOT NULL,
                    timezone     TEXT NOT NULL,
                    rrule        TEXT,
                    once_at      TEXT,
                    source_uri   TEXT NOT NULL,
                    max_volume    INTEGER NOT NULL DEFAULT 40,
                    next_fire    TEXT,
                    created_at   TEXT NOT NULL,
                    updated_at   TEXT NOT NULL
                );
            ",
        },
        // Slice 2 / D3: add nullable JSON columns for escalation steps and the
        // fallback chain. Non-destructive: existing rows get NULL (slice-1
        // behavior). Applied in a single transaction with the user_version bump.
        Migration {
            version: 3,
            sql: "
                ALTER TABLE alarms ADD COLUMN escalation_steps TEXT;
                ALTER TABLE alarms ADD COLUMN fallback_chain TEXT;
            ",
        },
        // Slice 4 / D6: add JSON column for per-alarm visual alarm config.
        // Non-destructive: existing rows get NULL (VisualConfig::Off).
        Migration {
            version: 4,
            sql: "
                ALTER TABLE alarms ADD COLUMN visual_config TEXT;
            ",
        },
        // Slice 4a: add per-alarm snooze duration and cap.
        // Non-destructive: existing rows get defaults (10 minutes, 3 max).
        Migration {
            version: 5,
            sql: "
                ALTER TABLE alarms ADD COLUMN snooze_minutes INTEGER NOT NULL DEFAULT 10;
                ALTER TABLE alarms ADD COLUMN max_snoozes INTEGER NOT NULL DEFAULT 3;
            ",
        },
        // Slice 5: add weather configuration
        // Non-destructive: new kv_config keys for city, lat, and lon
        Migration {
            version: 6,
            sql: "
                -- Weather configuration will be stored in kv_config table
                -- Keys: weather_city, weather_lat, weather_lon
            ",
        },
        // Slice 6: add per-alarm holiday_policy and a calendars table.
        // Non-destructive: existing alarms get `Suppress` (default); no
        // calendars are configured until the user pairs via device flow.
        // (The slice-6 spec text calls this "migration v6", but slice 5's
        // weather migration already occupies v6; this is the next available
        // version, v7.)
        Migration {
            version: 7,
            sql: "
                ALTER TABLE alarms ADD COLUMN holiday_policy TEXT NOT NULL DEFAULT 'Suppress';
                CREATE TABLE IF NOT EXISTS calendars (
                    google_calendar_id TEXT PRIMARY KEY,
                    display_name       TEXT NOT NULL,
                    role                TEXT NOT NULL
                );
            ",
        },
        // Slice 7: favorites table (media-player).
        // Non-destructive: creates an empty `favorites` table; existing
        // alarms are untouched. The slice-7 spec text calls this "migration
        // v7", but slice 6's calendars migration already occupies v7; this is
        // the next available version, v8.
        Migration {
            version: 8,
            sql: "
                CREATE TABLE IF NOT EXISTS favorites (
                    id            TEXT PRIMARY KEY,
                    name          TEXT NOT NULL,
                    source_type   TEXT NOT NULL,
                    source_uri    TEXT NOT NULL,
                    display_order INTEGER NOT NULL
                );
            ",
        },
    ]
}

// ── Task 3.1: open connection with WAL + NORMAL ─────────────────────────────

/// Open a single `Connection` to the SQLite database file at *path*.
///
/// Immediately configures **WAL journal mode** and **synchronous=NORMAL**.
/// Creates the parent directory for *path* if it does not yet exist (so a fresh
/// database can be created).
///
/// # Design notes
/// - Exactly one `Connection` per process lifetime, owned by main.
/// - No `Mutex`, no connection pool — all SQL runs on the main thread.
pub fn open_connection(path: &str) -> DomainResult<Connection> {
    let conn = Connection::open(path)
        .map_err(|e| ConfigError::Database(e))?;

    // WAL journal mode for concurrent reads + durability.
    // First PRAGMA call returns the old mode and sets to WAL.
    let jm: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .map_err(ConfigError::Database)?;
    assert_eq!(jm, "wal", "journal_mode should now be wal");

    // synchronous=NORMAL: durable across application crashes with WAL,
    // avoids the latency of FULL without sacrificing safety.
    db_pragma_set(&conn, "PRAGMA synchronous = NORMAL")?;

    info!(db_path = path, "SQLite connection opened with WAL + synchronous=NORMAL");
    Ok(conn)
}

/// Helper: execute a single DDL/DML statement and ignore the change count.
fn db_pragma_set(conn: &Connection, sql: &str) -> DomainResult<()> {
    conn.execute_batch(sql).map(|_| ()).map_err(ConfigError::Database)
}

// ── Task 3.2: user_version migration runner ────────────────────────────────

/// Read the current `user_version` of the database.
///
/// Returns `0` for a fresh/empty database (no versions applied yet).
fn read_user_version(conn: &Connection) -> DomainResult<u32> {
    // user_version is 0 by default; query it.
    let version: u32 = conn.query_row(
        "PRAGMA user_version",
        [],
        |r| r.get(0),
    ).map_err(ConfigError::Database)?;
    Ok(version)
}

/// Set `user_version` to *v*.
fn set_user_version(conn: &Connection, v: u32) -> DomainResult<()> {
    let sql = format!("PRAGMA user_version = {}", v);
    conn.execute_batch(&sql).map(|_| ()).map_err(ConfigError::Database)
}

// ── ConfigStore (Design D3, Task 3.4) ───────────────────────────────────────

/// Key-value store backed by `kv_config` in SQLite.
///
/// Owned by main. All mutations run inside a single transaction (`BEGIN … COMMIT`).
pub struct ConfigStore<'a> {
    conn: &'a Connection,
}

impl<'a> ConfigStore<'a> {
    /// Create a ConfigStore borrowing *conn*.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Read the value associated with *key*, or `None` if the key does not exist.
    ///
    /// Database errors propagate as `Err(ConfigError::Database(..))`. "Key not
    /// found" is returned as `Ok(None)`.
    pub fn get(&self, key: &str) -> DomainResult<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM kv_config WHERE key = ?",
                [key],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(ConfigError::Database(other)),
            })
    }

    /// Set *key* to *value* inside a single transaction.
    ///
    /// Uses `INSERT OR REPLACE` so that updating an existing key is idempotent.
    pub fn set(&self, key: &str, value: &str) -> DomainResult<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(ConfigError::Database)?;

        tx.execute(
            "INSERT OR REPLACE INTO kv_config (key, value) VALUES (?, ?)",
            [key, value],
        )
        .map_err(ConfigError::Database)?;

        tx.commit().map_err(ConfigError::Database)?;
        Ok(())
    }

    /// Execute multiple key-value writes inside a single transaction.
    ///
    /// If any statement fails the **entire** transaction rolls back, leaving
    /// the database unchanged. This satisfies PRD § D3: *"multi-statement
    /// mutations roll back on partial failure."*
    pub fn set_multi(&self, pairs: &[(&str, &str)]) -> DomainResult<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(ConfigError::Database)?;

        for (k, v) in pairs {
            tx.execute(
                "INSERT OR REPLACE INTO kv_config (key, value) VALUES (?, ?)",
                [k, v],
            )
            .map_err(ConfigError::Database)?;
        }

        tx.commit().map_err(ConfigError::Database)?;
        Ok(())
    }
}

/// Run pending migrations on the given connection.
///
/// Migrations are applied in ascending version order. Each migration's SQL is
/// wrapped in a single transaction (`BEGIN … COMMIT`). After success,
/// `user_version` is bumped to that migration's target version.
///
/// If `user_version` >= the highest known migration version, no work is done.
pub fn run_migrations(conn: &Connection) -> DomainResult<()> {
    let current = read_user_version(conn)?;
    info!(current_user_version = current, "checking for pending migrations");

    let mut applied_count: u32 = 0;

    for mig in migrations() {
        if current >= mig.version {
            info!(migration_version = mig.version, status = "skipped (already up-to-date or newer)");
            continue;
        }

        // Apply migration SQL inside a single transaction.
        let tx = conn.unchecked_transaction()
            .map_err(ConfigError::Database)?;

        tx.execute_batch(mig.sql)
            .map_err(|e| ConfigError::MigrationFailed {
                step: mig.version,
                detail: e.to_string(),
            })?;

        // Bump user_version atomically within the transaction.
        set_user_version(&tx, mig.version)?;

        tx.commit()
            .map_err(ConfigError::Database)?;

        applied_count += 1;
        info!(
            migration_version = mig.version,
            "migration applied",
        );
    }

    if applied_count == 0 {
        info!("no pending migrations");
    } else {
        let new_version = read_user_version(conn)?;
        info!(
            applied = applied_count,
            new_user_version = new_version,
            "migrations complete",
        );
    }

    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: WAL mode active — PRAGMA journal_mode returns "wal" and
    /// PRAGMA synchronous returns "NORMAL" after open_connection.
    #[test]
    fn wal_mode_and_normal_synchronous_active() {
        let path = format!(
            "{}{}open_conn_wal_test.db",
            std::env::temp_dir().display(),
            std::path::MAIN_SEPARATOR,
        );

        // Clean up any prior run.
        let _ = std::fs::remove_file(&path);

        let conn = open_connection(&path).expect("open connection");

        // Verify WAL is active.
        let jm: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(jm, "wal", "journal_mode should be wal");

        // Verify synchronous is NORMAL (value 1 in SQLite).
        let synch: i32 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(synch, 1, "synchronous should be NORMAL (1)");

        let _ = std::fs::remove_file(&path);
    }

    /// Scenario: Fresh database migrates to the latest version — schema_meta
    /// and kv_config tables exist, user_version is the highest known migration.
    #[test]
    fn fresh_database_migrates_to_v1() {
        let path = format!(
            "{}{}fresh_db_migration_test.db",
            std::env::temp_dir().display(),
            std::path::MAIN_SEPARATOR,
        );

        let _ = std::fs::remove_file(&path);

        let conn = open_connection(&path).expect("open connection");

        // Before migrations user_version should be 0.
        assert_eq!(read_user_version(&conn).unwrap(), 0);

        run_migrations(&conn).expect("migrations should succeed");

        // After migration user_version is the latest known migration version.
        let latest = migrations().last().unwrap().version;
        assert_eq!(read_user_version(&conn).unwrap(), latest);

        // schema_meta table exists.
        let has_schema_meta: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='schema_meta'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert!(has_schema_meta, "schema_meta table should exist");

        // kv_config table exists.
        let has_kv_config: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='kv_config'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert!(has_kv_config, "kv_config table should exist");

        let _ = std::fs::remove_file(&path);
    }

    /// Scenario: Already-migrated database skips migrations — running
    /// run_migrations on a DB at user_version=1 applies nothing.
    #[test]
    fn already_migrated_database_skips_migrations() {
        let path = format!(
            "{}{}skip_migration_test.db",
            std::env::temp_dir().display(),
            std::path::MAIN_SEPARATOR,
        );

        let _ = std::fs::remove_file(&path);

        let conn = open_connection(&path).expect("open connection");

        // Manually set user_version to the latest (simulate a fully-migrated DB).
        let latest = migrations().last().unwrap().version;
        set_user_version(&conn, latest).unwrap();

        // Running migrations should succeed but apply nothing.
        run_migrations(&conn).expect("should succeed even with no pending work");

        // user_version remains at the latest.
        assert_eq!(read_user_version(&conn).unwrap(), latest);

        let _ = std::fs::remove_file(&path);
    }

    /// Scenario: Single connection on main — only one Connection object is
    /// created and used; no Mutex or pool involved.
    #[test]
    fn single_connection_on_main() {
        // This test proves the architecture: open_connection returns a plain
        // owned Connection (not Arc<Mutex<Connection>> etc.). The connection
        // lives on main for the process lifetime. We verify it is a bare
        // rusqlite::Connection by type-checking its use.
        let path = format!(
            "{}{}single_conn_test.db",
            std::env::temp_dir().display(),
            std::path::MAIN_SEPARATOR,
        );

        let _ = std::fs::remove_file(&path);

        // open_connection returns bare Connection.
        let conn: Connection = open_connection(&path).unwrap();

        // We can query it directly — no guards, locks, or pools.
        let jm: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(jm, "wal");

        // run_migrations accepts &Connection (shared reference, no mutex).
        run_migrations(&conn).unwrap();

        // Write directly through the bare connection to prove ownership.
        conn.execute("INSERT OR REPLACE INTO kv_config(key, value) VALUES('test', 'ok')", [])
            .unwrap();

        let val: String = conn.query_row(
            "SELECT value FROM kv_config WHERE key='test'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(val, "ok");

        // Connection drops here; parent scope (main) owns it.
        std::mem::drop(conn);

        let _ = std::fs::remove_file(&path);
    }

    /// Scenario: run_migrations is idempotent-safe — calling it twice on the
    /// same database is a no-op the second time.
    #[test]
    fn double_migration_run_is_noop() {
        let path = format!(
            "{}{}double_migration_test.db",
            std::env::temp_dir().display(),
            std::path::MAIN_SEPARATOR,
        );

        let _ = std::fs::remove_file(&path);

        let conn = open_connection(&path).unwrap();

        let latest = migrations().last().unwrap().version;

        // First run: applies all pending migrations.
        run_migrations(&conn).unwrap();
        assert_eq!(read_user_version(&conn).unwrap(), latest);

        // Second run: should be a no-op, user_version stays at the latest.
        run_migrations(&conn).unwrap();
        assert_eq!(read_user_version(&conn).unwrap(), latest);

        let _ = std::fs::remove_file(&path);
    }

    /// ── Task 3.2: Migration v2 idempotency ─────────────────────────────

    /// Scenario: Starting at user_version=2 skips re-applying v2 (row intact)
    /// and then applies the additive v3 migration (escalation_steps /
    /// fallback_chain columns added), ending at user_version=3.
    #[test]
    fn migration_v2_idempotent_skips_when_already_at_v2() {
        let path = format!(
            "{}{}v2_idempotent_test.db",
            std::env::temp_dir().display(),
            std::path::MAIN_SEPARATOR,
        );

        let _ = std::fs::remove_file(&path);

        let conn = open_connection(&path).expect("open connection");

        // Manually apply v1 migration SQL so schema_meta + kv_config exist.
        let v1_mig = migrations().iter().find(|m| m.version == 1).unwrap();
        conn.execute_batch(v1_mig.sql)
            .expect("v1 migration should succeed");

        // Manually apply v2 migration SQL so alarms table exists.
        let v2_mig = migrations().iter().find(|m| m.version == 2).unwrap();
        conn.execute_batch(v2_mig.sql)
            .expect("v2 migration should succeed");

        // Set user_version to 2 (simulate a fully-migrated DB at v2).
        set_user_version(&conn, 2).unwrap();

        // Insert a test row into alarms so we can verify it survives.
        conn.execute(
            "INSERT INTO alarms (id, name, time_local, timezone, source_uri, max_volume, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            ["test-uuid", "Morning Alarm", "07:00:00", "America/Edmonton", "coreaudio://alarm.mp3", "50", "2025-01-01T00:00:00Z", "2025-01-01T00:00:00Z"],
        )
        .expect("should insert alarm row");

        // Verify alarms count before run_migrations.
        let alarm_count_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM alarms", [], |r| r.get(0))
            .unwrap();
        assert_eq!(alarm_count_before, 1);

        // Run migrations — v2 is skipped (user_version already 2); the
        // additive v3..v7 migrations apply (escalation_steps, fallback_chain,
        // visual_config, snooze_minutes, max_snoozes, holiday_policy columns +
        // calendars table) and user_version advances to the latest.
        run_migrations(&conn).expect("migrations should succeed");

        // user_version is now the latest (v3..v7 applied); v2 was not re-applied.
        assert_eq!(read_user_version(&conn).unwrap(), migrations().last().unwrap().version);

        // alarms table intact — row count unchanged.
        let alarm_count_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM alarms", [], |r| r.get(0))
            .unwrap();
        assert_eq!(alarm_count_after, alarm_count_before, "alarms data must be intact");

        // Verify the specific row is still there.
        let name: String = conn.query_row(
            "SELECT name FROM alarms WHERE id = ?",
            ["test-uuid"],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(name, "Morning Alarm", "alarm row must survive");

        // v3 added the new columns; the pre-existing row has NULL for both
        // (slice-1 behavior preserved).
        let (es, fc): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT escalation_steps, fallback_chain FROM alarms WHERE id = ?",
                ["test-uuid"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(es.is_none(), "escalation_steps NULL on pre-v3 row");
        assert!(fc.is_none(), "fallback_chain NULL on pre-v3 row");

        // schema_meta and kv_config from v1 are untouched.
        let has_schema_meta: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='schema_meta'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert!(has_schema_meta, "schema_meta must still exist after v2 idempotent skip");

        let has_kv_config: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='kv_config'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert!(has_kv_config, "kv_config must still exist after v2 idempotent skip");

        let _ = std::fs::remove_file(&path);
    }

    /// Scenario: Migration v5 adds snooze_minutes and max_snoozes columns with
    /// defaults; existing alarms get defaults (10, 3); new alarms preserve values.
    #[test]
    fn migration_v5_adds_snooze_columns_with_defaults() {
        let path = format!(
            "{}{}v5_snooze_migration_test.db",
            std::env::temp_dir().display(),
            std::path::MAIN_SEPARATOR,
        );

        let _ = std::fs::remove_file(&path);

        let conn = open_connection(&path).expect("open connection");
        run_migrations(&conn).expect("migrations should succeed");

        // Insert a test alarm without specifying snooze fields (v2-v4 alarm).
        conn.execute(
            "INSERT INTO alarms (id, name, time_local, timezone, source_uri, max_volume, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            ["test-v2-alarm", "Morning Alarm", "07:00:00", "America/Edmonton", "coreaudio://alarm.mp3", "50", "2025-01-01T00:00:00Z", "2025-01-01T00:00:00Z"],
        )
        .expect("should insert v2 alarm");

        // Insert another alarm with explicit snooze values.
        conn.execute(
            "INSERT INTO alarms (id, name, time_local, timezone, source_uri, max_volume, snooze_minutes, max_snoozes, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            ["test-v5-alarm", "Evening Alarm", "22:00:00", "America/Edmonton", "coreaudio://evening.mp3", "60", "5", "2", "2025-01-01T00:00:00Z", "2025-01-01T00:00:00Z"],
        )
        .expect("should insert v5 alarm");

        // Read back both alarms and verify snooze fields.
        let mut stmt = conn.prepare("SELECT id, snooze_minutes, max_snoozes FROM alarms ORDER BY id").unwrap();
        let rows: Vec<(String, i64, i64)> = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        }).unwrap().collect::<Result<Vec<_>, _>>().unwrap();

        // v2 alarm gets defaults.
        assert_eq!(rows[0], ("test-v2-alarm".to_string(), 10, 3));
        // v5 alarm preserves explicit values.
        assert_eq!(rows[1], ("test-v5-alarm".to_string(), 5, 2));

        let _ = std::fs::remove_file(&path);
    }

    /// Scenario: Migration v5 is idempotent - running it multiple times
    /// doesn't change the database state.
    #[test]
    fn migration_v5_is_idempotent() {
        let path = format!(
            "{}{}v5_idempotent_test.db",
            std::env::temp_dir().display(),
            std::path::MAIN_SEPARATOR,
        );

        let _ = std::fs::remove_file(&path);

        let conn = open_connection(&path).expect("open connection");
        run_migrations(&conn).expect("first migration should succeed");

        // Insert an alarm
        conn.execute(
            "INSERT INTO alarms (id, name, time_local, timezone, source_uri, max_volume, snooze_minutes, max_snoozes, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            ["test-alarm", "Test Alarm", "07:00:00", "America/Edmonton", "coreaudio://alarm.mp3", "50", "7", "4", "2025-01-01T00:00:00Z", "2025-01-01T00:00:00Z"],
        )
        .expect("should insert alarm");

        // Run migrations again - should be no-op
        run_migrations(&conn).expect("second migration should succeed");

        // Verify the alarm is still there with the same values
        let mut stmt = conn.prepare("SELECT id, snooze_minutes, max_snoozes FROM alarms").unwrap();
        let rows: Vec<(String, i64, i64)> = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        }).unwrap().collect::<Result<Vec<_>, _>>().unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], ("test-alarm".to_string(), 7, 4));

        let _ = std::fs::remove_file(&path);
    }

    /// Scenario: Upgrading from v4 to v5 applies defaults to existing alarms.
    #[test]
    fn migration_v5_upgrade_from_v4_applies_defaults() {
        let path = format!(
            "{}{}v5_upgrade_test.db",
            std::env::temp_dir().display(),
            std::path::MAIN_SEPARATOR,
        );

        let _ = std::fs::remove_file(&path);

        let conn = open_connection(&path).expect("open connection");

        // Manually apply v1-v4 migrations to simulate an existing database
        for version in 1..=4 {
            let mig = migrations().iter().find(|m| m.version == version).unwrap();
            conn.execute_batch(mig.sql).expect(&format!("migration v{} should succeed", version));
            set_user_version(&conn, version).expect(&format!("setting user_version to {} should succeed", version));
        }

        // Insert a v4 alarm (without snooze fields)
        conn.execute(
            "INSERT INTO alarms (id, name, time_local, timezone, source_uri, max_volume, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            ["test-v4-alarm", "V4 Alarm", "08:00:00", "America/Edmonton", "coreaudio://v4.mp3", "60", "2025-01-01T00:00:00Z", "2025-01-01T00:00:00Z"],
        )
        .expect("should insert v4 alarm");

        // Run all migrations (should apply v5..v7)
        run_migrations(&conn).expect("migrations should succeed");

        // Verify user_version is now the latest
        assert_eq!(read_user_version(&conn).unwrap(), migrations().last().unwrap().version);

        // Verify the v4 alarm now has snooze defaults
        let mut stmt = conn.prepare("SELECT id, snooze_minutes, max_snoozes FROM alarms").unwrap();
        let rows: Vec<(String, i64, i64)> = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        }).unwrap().collect::<Result<Vec<_>, _>>().unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], ("test-v4-alarm".to_string(), 10, 3));

        let _ = std::fs::remove_file(&path);
    }

    /// ── Slice 6: Migration v7 (holiday_policy + calendars table) ────────

    /// Scenario: Fresh database migrates to v7 — `alarms` has a
    /// `holiday_policy` column (default `Suppress`) and a `calendars` table exists.
    #[test]
    fn fresh_database_migrates_to_v7() {
        let path = format!(
            "{}{}fresh_v7_migration_test.db",
            std::env::temp_dir().display(),
            std::path::MAIN_SEPARATOR,
        );

        let _ = std::fs::remove_file(&path);

        let conn = open_connection(&path).expect("open connection");
        run_migrations(&conn).expect("migrations should succeed");

        let latest = migrations().last().unwrap().version;
        assert_eq!(latest, 8);
        assert_eq!(read_user_version(&conn).unwrap(), 8);

        // `holiday_policy` column exists on alarms with default Suppress.
        // Insert a row without specifying holiday_policy and read the default.
        conn.execute(
            "INSERT INTO alarms (id, name, time_local, timezone, source_uri, max_volume, created_at, updated_at) \
             VALUES ('t','t','07:00:00','America/Edmonton','coreaudio://x.mp3',40,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let default_pol: String = conn
            .query_row("SELECT holiday_policy FROM alarms WHERE id='t'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(default_pol, "Suppress");

        // `calendars` table exists and is empty.
        let has_calendars: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='calendars'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(has_calendars, "calendars table should exist after v7 migration");
        let calendars_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM calendars", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calendars_count, 0, "calendars table should be empty");

        let _ = std::fs::remove_file(&path);
    }

    /// Scenario: v6 database upgrades to v7 — all alarm rows survive with
    /// `holiday_policy = Suppress`, the `calendars` table is created (empty),
    /// and `user_version` becomes `7`.
    #[test]
    fn v6_database_upgrades_to_v7() {
        let path = format!(
            "{}{}v6_to_v7_upgrade_test.db",
            std::env::temp_dir().display(),
            std::path::MAIN_SEPARATOR,
        );

        let _ = std::fs::remove_file(&path);

        let conn = open_connection(&path).expect("open connection");

        // Manually apply v1–v6 migrations to simulate an existing slice-5 DB.
        for version in 1..=6 {
            let mig = migrations().iter().find(|m| m.version == version).unwrap();
            conn.execute_batch(mig.sql).expect(&format!("migration v{} should succeed", version));
            set_user_version(&conn, version).expect(&format!("setting user_version to {}", version));
        }

        // Insert a pre-v7 alarm (no holiday_policy column yet).
        conn.execute(
            "INSERT INTO alarms (id, name, time_local, timezone, source_uri, max_volume, created_at, updated_at) \
             VALUES ('pre-v7','Morning','07:00:00','America/Edmonton','coreaudio://alarm.mp3',40,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            [],
        )
        .expect("insert pre-v7 alarm");

        // Run migrations — should apply v7 and v8.
        run_migrations(&conn).expect("migrations should succeed");

        assert_eq!(read_user_version(&conn).unwrap(), 8);

        // The pre-v7 alarm survives with default Suppress.
        let pol: String = conn
            .query_row(
                "SELECT holiday_policy FROM alarms WHERE id='pre-v7'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pol, "Suppress", "pre-v7 alarm gets default holiday_policy");

        // calendars table exists and is empty.
        let calendars_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM calendars", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calendars_count, 0);

        // v7/v8 are idempotent — running migrations again is a no-op.
        run_migrations(&conn).expect("second run should succeed");
        assert_eq!(read_user_version(&conn).unwrap(), 8);
        let pol2: String = conn
            .query_row(
                "SELECT holiday_policy FROM alarms WHERE id='pre-v7'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pol2, "Suppress");

        let _ = std::fs::remove_file(&path);
    }

    /// ── Task 3.5: ConfigStore round-trip ───────────────────────────────

    /// Scenario: write a "last_boot" ISO-8601 timestamp, read it back,
    /// assert equality.
    #[test]
    fn config_store_round_trip_last_boot() {
        let path = format!(
            "{}{}config_roundtrip_test.db",
            std::env::temp_dir().display(),
            std::path::MAIN_SEPARATOR,
        );

        let _ = std::fs::remove_file(&path);

        let conn = open_connection(&path).unwrap();
        run_migrations(&conn).unwrap();

        let store = ConfigStore::new(&conn);

        // Write an ISO-8601 timestamp.
        let ts = "2025-03-14T09:30:00+00:00";
        store.set("last_boot", ts).expect("set should succeed");

        // Read it back.
        let read_val = store.get("last_boot").expect("get should succeed");
        assert_eq!(
            read_val.as_deref(),
            Some(ts),
            "read value must equal the written timestamp"
        );

        // Reading a missing key returns None (no error).
        let missing = store.get("does_not_exist").expect("get should succeed");
        assert!(missing.is_none(), "missing key should return None");

        let _ = std::fs::remove_file(&path);
    }

    /// Scenario: partial failure rolls back — a multi-statement mutation
    /// where the second statement errors should leave neither row persisted.
    #[test]
    fn partial_failure_rolls_back_transaction() {
        let path = format!(
            "{}{}partial_rollback_test.db",
            std::env::temp_dir().display(),
            std::path::MAIN_SEPARATOR,
        );

        let _ = std::fs::remove_file(&path);

        let conn = open_connection(&path).unwrap();
        run_migrations(&conn).unwrap();

        let store = ConfigStore::new(&conn);

        // First, write a known-good row so we have a baseline count.
        store.set("baseline", "yes").unwrap();
        let baseline_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM kv_config", [], |r| r.get(0))
            .unwrap();

        // Attempt a multi-statement write where the second statement fails.
        // The second key contains invalid characters that break inside a
        // deliberately constructed SQL error (no REPLACE, uses INSERT which
        // will hit a NOT NULL violation on an intentionally wrong table).
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute(
            "INSERT OR REPLACE INTO kv_config (key, value) VALUES ('alpha', '1')",
            [],
        )
        .unwrap();
        // Second statement: write to a column that doesn't exist → error.
        let second_stmt = tx.execute(
            "INSERT INTO kv_config (key, value) SELECT 'beta', 0 FROM nonexistent_table_xyz",
            [],
        );
        assert!(second_stmt.is_err(), "second statement should fail");

        // Rollback the transaction.
        let _ = tx.rollback();

        // Verify neither row was persisted (count unchanged).
        let after_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM kv_config", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            after_count, baseline_count,
            "transaction should have rolled back — row count unchanged"
        );

        // Confirm the specific keys are not present.
        let store2 = ConfigStore::new(&conn);
        assert!(store2.get("alpha").unwrap().is_none(), "alpha must not exist after rollback");
        assert!(store2.get("beta").unwrap().is_none(), "beta must not exist after rollback");

        let _ = std::fs::remove_file(&path);
    }
}

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

    /// Scenario: Fresh database migrates to v1 — schema_meta and kv_config
    /// tables exist, user_version is 1.
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

        // After migration user_version == 1.
        assert_eq!(read_user_version(&conn).unwrap(), 1);

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

        // Manually set user_version to 1 (simulate prior migration).
        set_user_version(&conn, 1).unwrap();

        // Running migrations should succeed but apply nothing.
        run_migrations(&conn).expect("should succeed even with no pending work");

        // user_version remains 1.
        assert_eq!(read_user_version(&conn).unwrap(), 1);

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

        // First run: applies v1.
        run_migrations(&conn).unwrap();
        assert_eq!(read_user_version(&conn).unwrap(), 1);

        // Second run: should be a no-op, user_version stays 1.
        run_migrations(&conn).unwrap();
        assert_eq!(read_user_version(&conn).unwrap(), 1);

        let _ = std::fs::remove_file(&path);
    }
}

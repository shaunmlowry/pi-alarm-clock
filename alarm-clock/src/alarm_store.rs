//! Alarm data model & persistence (design D3, tasks 3.3–3.5).
//!
//! [`AlarmStore`] is owned by the main thread and borrows the single
//! [`rusqlite::Connection`] (the same model as [`crate::database::ConfigStore`]).
//! Every mutation runs inside a single transaction; a multi-statement mutation
//! rolls back on partial failure and logs `error!`.
//!
//! `next_fire` is a **derived cache**: [`AlarmStore::recompute_next_fires`]
//! re-derives it from each alarm's rule (`rrule`/`once_at` + `time_local` +
//! `timezone`) and writes it back. The rule remains the source of truth — the
//! scheduler re-derives next-fire on the tick rather than trusting the cache
//! blindly (design D3).

use chrono::{DateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::error::{ConfigError, Result};
use crate::schedule::Schedule;

// ── Alarm model ─────────────────────────────────────────────────────────────

/// A persisted alarm row, mirroring the `alarms` table (migration `v2`).
///
/// Field order and types match the SQL columns exactly so the row can be
/// round-tripped through `upsert` → `get`/`list` without loss.
///
/// - `enabled` is stored as a SQLite `INTEGER` (`0`/`1`); the model exposes a
///   `bool` and the store performs the conversion at the SQL boundary.
/// - `next_fire` is a derived cache (ISO-8601 UTC), recomputed by
///   [`AlarmStore::recompute_next_fires`]; it is **not** authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alarm {
    /// UUID v4 string primary key.
    pub id: String,
    /// Whether the alarm is armed (`true`) or disabled (`false`).
    pub enabled: bool,
    /// Human-readable label.
    pub name: String,
    /// Wall-clock local fire time, `HH:MM:SS`.
    pub time_local: String,
    /// IANA timezone name, e.g. `America/Edmonton`.
    pub timezone: String,
    /// RFC 5545 RRULE body; `None` for a `Once` alarm.
    pub rrule: Option<String>,
    /// For a `Once` alarm: full ISO-8601 local DateTime. `None` otherwise.
    pub once_at: Option<String>,
    /// Mopidy URI to play when the alarm fires.
    pub source_uri: String,
    /// Ceiling volume for the episode, `0..=100`.
    pub max_volume: i64,
    /// Cached next fire time (ISO-8601 UTC); derived, may be stale.
    pub next_fire: Option<String>,
    /// Creation timestamp (ISO-8601).
    pub created_at: String,
    /// Last-update timestamp (ISO-8601).
    pub updated_at: String,
}

// ── AlarmStore ──────────────────────────────────────────────────────────────

/// Alarm persistence, owned by main, borrowing the single `Connection`.
///
/// All mutations ([`upsert`], [`delete`], [`set_enabled`],
/// [`recompute_next_fires`]) run inside a single transaction. A multi-statement
/// mutation that fails partway rolls back, logs `error!`, and leaves the
/// database unchanged — the persisted state remains authoritative.
///
/// [`upsert`]: AlarmStore::upsert
/// [`delete`]: AlarmStore::delete
/// [`set_enabled`]: AlarmStore::set_enabled
/// [`recompute_next_fires`]: AlarmStore::recompute_next_fires
pub struct AlarmStore<'a> {
    conn: &'a Connection,
}

impl<'a> AlarmStore<'a> {
    /// Create an `AlarmStore` borrowing *conn*.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// List all alarms, ordered by `id` for stable iteration.
    pub fn list(&self) -> Result<Vec<Alarm>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, enabled, name, time_local, timezone, rrule, once_at, \
                 source_uri, max_volume, next_fire, created_at, updated_at \
                 FROM alarms ORDER BY id",
            )
            .map_err(ConfigError::Database)?;

        let rows = stmt
            .query_map([], row_to_alarm)
            .map_err(ConfigError::Database)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(ConfigError::Database)?;
        Ok(rows)
    }

    /// Read a single alarm by `id`, or `None` if no such row exists.
    pub fn get(&self, id: &str) -> Result<Option<Alarm>> {
        self.conn
            .query_row(
                "SELECT id, enabled, name, time_local, timezone, rrule, once_at, \
                 source_uri, max_volume, next_fire, created_at, updated_at \
                 FROM alarms WHERE id = ?",
                [id],
                row_to_alarm,
            )
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(ConfigError::Database(other)),
            })
    }

    /// Insert or update an alarm by `id` in a single transaction.
    ///
    /// Uses `INSERT OR REPLACE` so a second upsert of the same `id` updates the
    /// existing row (idempotent by `id`) rather than inserting a duplicate.
    pub fn upsert(&self, alarm: &Alarm) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(ConfigError::Database)?;

        let enabled_i: i64 = alarm.enabled.into();
        if let Err(e) = tx.execute(
            "INSERT OR REPLACE INTO alarms \
             (id, enabled, name, time_local, timezone, rrule, once_at, \
              source_uri, max_volume, next_fire, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                alarm.id,
                enabled_i,
                alarm.name,
                alarm.time_local,
                alarm.timezone,
                alarm.rrule,
                alarm.once_at,
                alarm.source_uri,
                alarm.max_volume,
                alarm.next_fire,
                alarm.created_at,
                alarm.updated_at,
            ],
        ) {
            error!(alarm_id = %alarm.id, error = %e, "alarm upsert failed; rolling back");
            // Drop `tx` without committing → rolls back.
            let _ = tx.rollback();
            return Err(ConfigError::Database(e));
        }

        tx.commit().map_err(ConfigError::Database)?;
        Ok(())
    }

    /// Delete an alarm by `id` in a single transaction.
    ///
    /// Returns `Ok(true)` if a row was deleted, `Ok(false)` if the `id` was
    /// not present.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(ConfigError::Database)?;

        let removed = match tx.execute("DELETE FROM alarms WHERE id = ?", [id]) {
            Ok(n) => n,
            Err(e) => {
                error!(alarm_id = id, error = %e, "alarm delete failed; rolling back");
                let _ = tx.rollback();
                return Err(ConfigError::Database(e));
            }
        };

        tx.commit().map_err(ConfigError::Database)?;
        Ok(removed > 0)
    }

    /// Flip the `enabled` flag for an alarm in a single transaction.
    ///
    /// Returns `Ok(true)` if the row was updated, `Ok(false)` if the `id` was
    /// not present.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(ConfigError::Database)?;

        let enabled_i: i64 = enabled.into();
        let updated = match tx.execute(
            "UPDATE alarms SET enabled = ?, updated_at = ? WHERE id = ?",
            params![enabled_i, iso_now(), id],
        ) {
            Ok(n) => n,
            Err(e) => {
                error!(alarm_id = id, enabled, error = %e, "set_enabled failed; rolling back");
                let _ = tx.rollback();
                return Err(ConfigError::Database(e));
            }
        };

        tx.commit().map_err(ConfigError::Database)?;
        Ok(updated > 0)
    }

    /// Recompute the `next_fire` cache for every alarm from its rule and write
    /// all updates back in a single transaction (task 3.4).
    ///
    /// Called on boot, on rule change, and after a fire. The rule
    /// (`rrule`/`once_at` + `time_local` + `timezone`) is the source of truth;
    /// `next_fire` is only an optimization.
    ///
    /// - Disabled alarms have their `next_fire` set to `NULL`.
    /// - An alarm whose rule cannot be parsed/evaluated keeps its cache cleared
    ///   (`NULL`) and an `error!` is logged, but the transaction is not aborted
    ///   — a single malformed alarm must not prevent the rest from recomputing.
    pub fn recompute_next_fires(&self, now: DateTime<Utc>) -> Result<()> {
        let alarms = self.list()?;

        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(ConfigError::Database)?;

        for alarm in &alarms {
            let next_fire = if !alarm.enabled {
                None
            } else {
                match compute_next_fire(alarm, now) {
                    Ok(Some(dt)) => Some(dt.to_rfc3339()),
                    Ok(None) => None,
                    Err(e) => {
                        error!(
                            alarm_id = %alarm.id,
                            error = %e,
                            "failed to recompute next_fire from rule; clearing cache",
                        );
                        None
                    }
                }
            };

            if let Err(e) = tx.execute(
                "UPDATE alarms SET next_fire = ? WHERE id = ?",
                params![next_fire, alarm.id],
            ) {
                error!(alarm_id = %alarm.id, error = %e, "recompute_next_fires write failed; rolling back");
                let _ = tx.rollback();
                return Err(ConfigError::Database(e));
            }
        }

        tx.commit().map_err(ConfigError::Database)?;
        Ok(())
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Map a `rusqlite::Row` to an [`Alarm`].
fn row_to_alarm(row: &rusqlite::Row<'_>) -> rusqlite::Result<Alarm> {
    let enabled_i: i64 = row.get("enabled")?;
    Ok(Alarm {
        id: row.get("id")?,
        enabled: enabled_i != 0,
        name: row.get("name")?,
        time_local: row.get("time_local")?,
        timezone: row.get("timezone")?,
        rrule: row.get("rrule")?,
        once_at: row.get("once_at")?,
        source_uri: row.get("source_uri")?,
        max_volume: row.get("max_volume")?,
        next_fire: row.get("next_fire")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// Compute the next fire time for an alarm strictly after `now`, evaluated in
/// the alarm's stored IANA timezone (task 3.4 derivation).
fn compute_next_fire(alarm: &Alarm, now: DateTime<Utc>) -> Result<Option<DateTime<Tz>>> {
    let tz: Tz = alarm
        .timezone
        .parse()
        .map_err(|e| ConfigError::Database(rusqlite::Error::ToSqlConversionFailure(
            format!("invalid timezone {:?}: {}", alarm.timezone, e).into(),
        )))?;

    // `Once` alarm: no rrule, `once_at` is authoritative.
    if alarm.rrule.is_none() {
        let once_at = alarm.once_at.as_ref().ok_or_else(|| {
            ConfigError::Database(rusqlite::Error::ToSqlConversionFailure(
                format!("alarm {} has neither rrule nor once_at", alarm.id).into(),
            ))
        })?;
        let dt = parse_once_at(once_at, tz)?;
        let sched = Schedule::once(dt, tz);
        let after = now.with_timezone(&tz);
        return Ok(sched.next_fire(after));
    }

    // Recurring alarm: build the schedule from its RRULE + time_local + tz.
    let time_local = NaiveTime::parse_from_str(&alarm.time_local, "%H:%M:%S").map_err(|e| {
        ConfigError::Database(rusqlite::Error::ToSqlConversionFailure(
            format!("invalid time_local {:?}: {}", alarm.time_local, e).into(),
        ))
    })?;
    let rrule_body = alarm.rrule.clone();
    let sched = Schedule::recurring(rrule_body, time_local, tz).map_err(|e| {
        ConfigError::Database(rusqlite::Error::ToSqlConversionFailure(
            format!("invalid rrule for alarm {}: {}", alarm.id, e).into(),
        ))
    })?;
    let after = now.with_timezone(&tz);
    Ok(sched.next_fire(after))
}

/// Parse a stored `once_at` ISO-8601 string into a `DateTime<Tz>`.
///
/// Accepts either an offset-bearing RFC-3339 timestamp (converted into the
/// alarm's stored timezone) or a naive local datetime interpreted in the
/// alarm's stored timezone.
fn parse_once_at(s: &str, tz: Tz) -> Result<DateTime<Tz>> {
    // First try a full RFC-3339 timestamp (with offset).
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&tz));
    }
    // Then try a naive local datetime in the alarm's stored timezone.
    if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        if let Some(dt) = tz.from_local_datetime(&ndt).single() {
            return Ok(dt);
        }
    }
    Err(ConfigError::Database(rusqlite::Error::ToSqlConversionFailure(
        format!("invalid once_at {:?}: not RFC-3339 or naive local", s).into(),
    )))
}

/// Current UTC time as an ISO-8601 string (for `updated_at` bumps).
fn iso_now() -> String {
    Utc::now().to_rfc3339()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{open_connection, run_migrations};

    /// Build a fresh, migrated in-memory-backed temp DB and return its
    /// `AlarmStore`.
    fn fresh_store() -> (std::path::PathBuf, AlarmStore<'static>) {
        // We leak the Connection so the store can borrow it for `'static` in
        // tests; tests are short-lived. Each call gets a unique DB file so
        // parallel test runs do not collide.
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "alarm_store_test_{}_{}_{}.db",
            std::process::id(),
            n,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let _ = std::fs::remove_file(&path);
        let conn = open_connection(path.to_str().unwrap()).unwrap();
        run_migrations(&conn).unwrap();
        let conn: &'static Connection = Box::leak(Box::new(conn));
        (path, AlarmStore::new(conn))
    }

    fn cleanup(path: std::path::PathBuf) {
        let _ = std::fs::remove_file(&path);
    }

    fn sample_alarm(id: &str) -> Alarm {
        Alarm {
            id: id.to_string(),
            enabled: true,
            name: "Morning".to_string(),
            time_local: "07:30:00".to_string(),
            timezone: "America/Edmonton".to_string(),
            rrule: Some("FREQ=DAILY".to_string()),
            once_at: None,
            source_uri: "coreaudio://alarm.mp3".to_string(),
            max_volume: 40,
            next_fire: None,
            created_at: "2026-01-01T00:00:00+00:00".to_string(),
            updated_at: "2026-01-01T00:00:00+00:00".to_string(),
        }
    }

    /// Scenario: upsert and read back an alarm — all fields equal.
    #[test]
    fn upsert_and_read_back() {
        let (path, store) = fresh_store();
        let a = sample_alarm("alarm-1");
        store.upsert(&a).expect("upsert should succeed");

        let got = store.get("alarm-1").expect("get should succeed").expect("row present");
        assert_eq!(got, a, "round-trip must preserve all fields");

        cleanup(path);
    }

    /// Scenario: upsert is idempotent by id — two upserts of the same id leave
    /// exactly one row, the second updating rather than inserting.
    #[test]
    fn upsert_is_idempotent_by_id() {
        let (path, store) = fresh_store();
        let mut a = sample_alarm("alarm-2");
        store.upsert(&a).expect("first upsert");

        // Mutate a non-id field and upsert again.
        a.name = "Evening".to_string();
        a.max_volume = 60;
        store.upsert(&a).expect("second upsert");

        // Exactly one row for this id.
        let all = store.list().expect("list");
        let matching: Vec<_> = all.iter().filter(|x| x.id == "alarm-2").collect();
        assert_eq!(matching.len(), 1, "only one row per id");

        // The row reflects the second upsert.
        let got = store.get("alarm-2").unwrap().unwrap();
        assert_eq!(got.name, "Evening");
        assert_eq!(got.max_volume, 60);

        // Total row count is 1, not 2.
        assert_eq!(all.len(), 1, "no duplicate rows");

        cleanup(path);
    }

    /// Scenario: set_enabled flips the flag in one transaction.
    #[test]
    fn set_enabled_flips_flag() {
        let (path, store) = fresh_store();
        let a = sample_alarm("alarm-3");
        store.upsert(&a).unwrap();

        assert!(store.set_enabled("alarm-3", false).unwrap(), "row updated");
        let got = store.get("alarm-3").unwrap().unwrap();
        assert!(!got.enabled, "enabled should now be false");
        assert_ne!(got.updated_at, a.updated_at, "updated_at should bump");

        // Flip back.
        store.set_enabled("alarm-3", true).unwrap();
        let got = store.get("alarm-3").unwrap().unwrap();
        assert!(got.enabled, "enabled should be true again");

        // Unknown id → Ok(false), no row created.
        assert!(!store.set_enabled("nope", true).unwrap(), "unknown id → false");

        cleanup(path);
    }

    /// Scenario: delete removes the row and returns whether it existed.
    #[test]
    fn delete_removes_row() {
        let (path, store) = fresh_store();
        store.upsert(&sample_alarm("alarm-4")).unwrap();

        assert!(store.delete("alarm-4").unwrap(), "should report deleted");
        assert!(store.get("alarm-4").unwrap().is_none(), "row gone");
        assert!(!store.delete("alarm-4").unwrap(), "second delete → false");

        cleanup(path);
    }

    /// Scenario: next_fire cache populated on boot — a daily alarm's cache is
    /// filled from its rule, in UTC.
    #[test]
    fn recompute_next_fires_populates_cache_on_boot() {
        let (path, store) = fresh_store();
        let a = sample_alarm("alarm-5"); // daily 07:30 America/Edmonton
        store.upsert(&a).unwrap();

        let now = Utc::now();
        store.recompute_next_fires(now).expect("recompute");

        let got = store.get("alarm-5").unwrap().unwrap();
        let nf = got.next_fire.expect("next_fire should be populated");
        // Parseable as RFC-3339, in UTC, strictly after now.
        let dt = DateTime::parse_from_rfc3339(&nf).unwrap().with_timezone(&Utc);
        assert!(dt > now, "next_fire must be in the future");

        // Wall-clock in Edmonton should be 07:30 the next day.
        let edmonton = dt.with_timezone(&chrono_tz::America::Edmonton);
        assert_eq!(edmonton.format("%H:%M:%S").to_string(), "07:30:00");

        cleanup(path);
    }

    /// Scenario: a `Once` alarm's next_fire is its `once_at` before firing and
    /// `None` (cleared) once it is in the past — verified by recomputing after.
    #[test]
    fn recompute_next_fires_for_once_alarm() {
        let (path, store) = fresh_store();
        let mut a = sample_alarm("alarm-6");
        a.rrule = None;
        // once_at one day in the future, in the alarm's tz.
        let tz: Tz = "America/Edmonton".parse().unwrap();
        let future = Utc::now().with_timezone(&tz)
            + chrono::Duration::days(1);
        a.once_at = Some(future.format("%Y-%m-%dT%H:%M:%S").to_string());
        store.upsert(&a).unwrap();

        let now = Utc::now();
        store.recompute_next_fires(now).unwrap();
        let got = store.get("alarm-6").unwrap().unwrap();
        assert!(got.next_fire.is_some(), "future Once alarm has a next_fire");

        // Recompute with `now` after the once_at → cache cleared.
        let after = future.with_timezone(&Utc) + chrono::Duration::seconds(1);
        store.recompute_next_fires(after).unwrap();
        let got = store.get("alarm-6").unwrap().unwrap();
        assert!(got.next_fire.is_none(), "past Once alarm cache is cleared");

        cleanup(path);
    }

    /// Scenario: disabled alarms have their next_fire cleared on recompute.
    #[test]
    fn recompute_clears_disabled_alarms() {
        let (path, store) = fresh_store();
        let mut a = sample_alarm("alarm-7");
        a.next_fire = Some("2026-01-01T07:30:00+00:00".to_string());
        a.enabled = false;
        store.upsert(&a).unwrap();

        store.recompute_next_fires(Utc::now()).unwrap();
        let got = store.get("alarm-7").unwrap().unwrap();
        assert!(got.next_fire.is_none(), "disabled alarm cache cleared");

        cleanup(path);
    }

    /// Scenario: a malformed rule for one alarm does not abort the transaction
    /// — the rest still recompute; the malformed one's cache is cleared.
    #[test]
    fn recompute_skips_malformed_rule_without_aborting() {
        let (path, store) = fresh_store();

        let good = sample_alarm("alarm-good");
        let mut bad = sample_alarm("alarm-bad");
        bad.rrule = Some("FREQ=NOPE".to_string());
        store.upsert(&good).unwrap();
        store.upsert(&bad).unwrap();

        store.recompute_next_fires(Utc::now()).expect("should not abort");

        let good_got = store.get("alarm-good").unwrap().unwrap();
        assert!(good_got.next_fire.is_some(), "good alarm recomputed");
        let bad_got = store.get("alarm-bad").unwrap().unwrap();
        assert!(bad_got.next_fire.is_none(), "bad alarm cache cleared");

        cleanup(path);
    }

    /// Scenario: failed mutation rolls back — a recompute whose UPDATE fails
    /// partway leaves prior writes uncommitted. We simulate failure by dropping
    /// the underlying table mid-transaction via a second connection is not
    /// possible (single-connection model); instead we verify the rollback
    /// contract directly: a transaction that errors does not persist partial
    /// state, mirroring ConfigStore's rollback test.
    #[test]
    fn failed_mutation_rolls_back() {
        let (path, store) = fresh_store();
        let a = sample_alarm("alarm-rb");
        store.upsert(&a).unwrap();

        // Drive an upsert into a failure: rename the alarms table so the
        // INSERT OR REPLACE inside upsert fails. The transaction must roll
        // back and the original row must remain intact.
        store
            .conn
            .execute("ALTER TABLE alarms RENAME TO alarms_hidden", [])
            .unwrap();

        let res = store.upsert(&a);
        assert!(res.is_err(), "upsert against missing table should fail");

        // Restore the table and confirm the original row survived (the failed
        // upsert's transaction rolled back; the prior committed row is intact).
        store
            .conn
            .execute("ALTER TABLE alarms_hidden RENAME TO alarms", [])
            .unwrap();
        let got = store.get("alarm-rb").unwrap().expect("original row intact");
        assert_eq!(got.id, "alarm-rb");

        cleanup(path);
    }
}

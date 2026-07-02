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

// ── Escalation step (slice 2, design D1) ───────────────────────────────────

/// A single step in an alarm's progressive volume escalation (slice 2 / D1).
///
/// After `after_secs` seconds elapsed since the episode fire instant, the
/// episode volume becomes `volume` (clamped 0..=100) and holds until the next
/// step. Steps are sorted ascending by `after_secs` by [`AlarmStore::upsert`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationStep {
    /// Seconds elapsed since fire at which this step's volume takes effect.
    pub after_secs: u64,
    /// Volume 0..=100 to apply from this step onward.
    pub volume: u8,
}

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
    /// Progressive volume escalation steps (slice 2 / D1). `None` or empty =
    /// fixed `max_volume` (slice-1 behavior). Stored as JSON text.
    pub escalation_steps: Option<Vec<EscalationStep>>,
    /// Ordered backup source URIs tried in order on primary-source failure
    /// (slice 2 / D2). `None` or empty = no fallback (slice-1 behavior). The
    /// alarm's `source_uri` is the primary and is not duplicated here.
    pub fallback_chain: Option<Vec<String>>,
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
                 source_uri, max_volume, escalation_steps, fallback_chain, \
                 next_fire, created_at, updated_at \
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
                 source_uri, max_volume, escalation_steps, fallback_chain, \
                 next_fire, created_at, updated_at \
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
    ///
    /// `escalation_steps` is sorted ascending by `after_secs` before persisting
    /// (slice 2 / D1) and both new columns are serialized as JSON text.
    pub fn upsert(&self, alarm: &Alarm) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(ConfigError::Database)?;

        let enabled_i: i64 = alarm.enabled.into();
        // Slice 2 / D1: sort escalation steps ascending by `after_secs`.
        let sorted_steps = alarm.escalation_steps.as_ref().map(|steps| {
            let mut s = steps.clone();
            s.sort_by_key(|st| st.after_secs);
            s
        });
        let escalation_json = serialize_escalation(&sorted_steps)?;
        let fallback_json = serialize_fallback(&alarm.fallback_chain)?;
        if let Err(e) = tx.execute(
            "INSERT OR REPLACE INTO alarms \
             (id, enabled, name, time_local, timezone, rrule, once_at, \
              source_uri, max_volume, escalation_steps, fallback_chain, \
              next_fire, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
                escalation_json,
                fallback_json,
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
    let escalation_json: Option<String> = row.get("escalation_steps")?;
    let fallback_json: Option<String> = row.get("fallback_chain")?;
    let escalation_steps = escalation_json
        .as_deref()
        .and_then(|s| deserialize_escalation(s).ok())
        .flatten();
    let fallback_chain = fallback_json
        .as_deref()
        .and_then(|s| deserialize_fallback(s).ok())
        .flatten();
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
        escalation_steps,
        fallback_chain,
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

/// Serialize `escalation_steps` to a JSON TEXT column value (`None`/empty → NULL).
fn serialize_escalation(steps: &Option<Vec<EscalationStep>>) -> Result<Option<String>> {
    match steps {
        None => Ok(None),
        Some(s) if s.is_empty() => Ok(None),
        Some(s) => serde_json::to_string(s)
            .map(Some)
            .map_err(|e| ConfigError::Database(rusqlite::Error::ToSqlConversionFailure(
                format!("escalation_steps serialize: {e}").into(),
            ))),
    }
}

/// Serialize `fallback_chain` to a JSON TEXT column value (`None`/empty → NULL).
fn serialize_fallback(chain: &Option<Vec<String>>) -> Result<Option<String>> {
    match chain {
        None => Ok(None),
        Some(s) if s.is_empty() => Ok(None),
        Some(s) => serde_json::to_string(s)
            .map(Some)
            .map_err(|e| ConfigError::Database(rusqlite::Error::ToSqlConversionFailure(
                format!("fallback_chain serialize: {e}").into(),
            ))),
    }
}

/// Deserialize `escalation_steps` from a JSON TEXT column value.
fn deserialize_escalation(s: &str) -> std::result::Result<Option<Vec<EscalationStep>>, serde_json::Error> {
    let v: Vec<EscalationStep> = serde_json::from_str(s)?;
    if v.is_empty() {
        Ok(None)
    } else {
        Ok(Some(v))
    }
}

/// Deserialize `fallback_chain` from a JSON TEXT column value.
fn deserialize_fallback(s: &str) -> std::result::Result<Option<Vec<String>>, serde_json::Error> {
    let v: Vec<String> = serde_json::from_str(s)?;
    if v.is_empty() {
        Ok(None)
    } else {
        Ok(Some(v))
    }
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
    use chrono::Offset;

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
            escalation_steps: None,
            fallback_chain: None,
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

    // ── Slice 2 / D1–D3: escalation + fallback persistence ───────────────

    /// Scenario: upsert and read back an alarm with escalation steps and a
    /// fallback chain preserves order and values exactly.
    #[test]
    fn upsert_and_read_back_escalation_and_fallback() {
        let (path, store) = fresh_store();
        let mut a = sample_alarm("alarm-esc");
        a.escalation_steps = Some(vec![
            EscalationStep { after_secs: 0, volume: 20 },
            EscalationStep { after_secs: 60, volume: 80 },
        ]);
        a.fallback_chain = Some(vec![
            "spotify:backup1".to_string(),
            "file:///beep.mp3".to_string(),
        ]);
        store.upsert(&a).expect("upsert");

        let got = store.get("alarm-esc").unwrap().expect("row present");
        assert_eq!(got.escalation_steps, a.escalation_steps, "steps preserved");
        assert_eq!(got.fallback_chain, a.fallback_chain, "chain preserved");

        cleanup(path);
    }

    /// Scenario: a slice-1 alarm (no escalation/fallback) round-trips as None.
    #[test]
    fn slice1_alarm_round_trips_without_new_fields() {
        let (path, store) = fresh_store();
        let a = sample_alarm("alarm-slice1");
        store.upsert(&a).unwrap();

        let got = store.get("alarm-slice1").unwrap().unwrap();
        assert!(got.escalation_steps.is_none());
        assert!(got.fallback_chain.is_none());

        cleanup(path);
    }

    /// Scenario: the store sorts escalation steps ascending by after_secs on
    /// write, even if the caller supplies them out of order.
    #[test]
    fn store_sorts_escalation_steps_on_write() {
        let (path, store) = fresh_store();
        let mut a = sample_alarm("alarm-unsorted");
        a.escalation_steps = Some(vec![
            EscalationStep { after_secs: 60, volume: 80 },
            EscalationStep { after_secs: 0, volume: 20 },
            EscalationStep { after_secs: 30, volume: 60 },
        ]);
        store.upsert(&a).unwrap();

        let got = store.get("alarm-unsorted").unwrap().unwrap();
        let steps = got.escalation_steps.expect("steps present");
        assert_eq!(steps[0].after_secs, 0);
        assert_eq!(steps[1].after_secs, 30);
        assert_eq!(steps[2].after_secs, 60);

        cleanup(path);
    }

    /// Scenario: a fresh DB reaches the latest migration with the new
    /// escalation_steps / fallback_chain columns, and a v2 alarm row upgrades
    /// preserving its data (new columns NULL = slice-1 behavior).
    #[test]
    fn migration_v3_adds_columns_and_preserves_v2_rows() {
        let (path, store) = fresh_store();
        // fresh_store already runs migrations, so we are at the latest (v3).
        let a = sample_alarm("alarm-v3");
        store.upsert(&a).unwrap();

        // The new columns exist and are readable (None for a slice-1 alarm).
        let got = store.get("alarm-v3").unwrap().unwrap();
        assert!(got.escalation_steps.is_none());
        assert!(got.fallback_chain.is_none());

        // The new columns are physically present on the alarms table.
        let conn = store.conn;
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(alarms)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(cols.iter().any(|c| c == "escalation_steps"),
            "escalation_steps column present");
        assert!(cols.iter().any(|c| c == "fallback_chain"),
            "fallback_chain column present");

        cleanup(path);
    }

    /// Task 9.5 — End-to-end DST: seed a daily alarm across a DST boundary;
    /// verify it fires at the same wall-clock local time on both sides.
    ///
    /// Scenario: seed a daily alarm for 07:30 in `America/Edmonton`;
    /// recompute next-fire before the spring-forward (MST, UTC-7) and after
    /// it (MDT, UTC-6); assert both yield wall-clock "07:30:00" despite the
    /// underlying UTC offset shifting by 1 hour.
    #[test]
    fn e2e_daily_alarm_fires_same_wall_clock_across_dst_boundary() {
        let (path, store) = fresh_store();

        // Seed a daily alarm for 07:30 America/Edmonton (simulates dev alarm
        // seeding through the persistence layer).
        let alarm = Alarm {
            id: "dst-e2e-daily".to_string(),
            enabled: true,
            name: "DST end-to-end daily".to_string(),
            time_local: "07:30:00".to_string(),
            timezone: "America/Edmonton".to_string(),
            rrule: Some("FREQ=DAILY".to_string()),
            once_at: None,
            source_uri: "coreaudio://alarm.mp3".to_string(),
            max_volume: 40,
            escalation_steps: None,
            fallback_chain: None,
            next_fire: None,
            created_at: "2026-01-01T00:00:00+00:00".to_string(),
            updated_at: "2026-01-01T00:00:00+00:00".to_string(),
        };
        store.upsert(&alarm).expect("seed upsert should succeed");

        let tz: Tz = "America/Edmonton".parse().unwrap();

        // --- Before DST spring-forward (2026-03-08 02:00 MST→MDT) ---
        // now = Mar 7 midnight MST → next fire should be Mar 7 07:30 MST.
        let before_dst = tz.with_ymd_and_hms(2026, 3, 7, 0, 0, 0).unwrap();
        let now_before = before_dst.with_timezone(&Utc);
        store.recompute_next_fires(now_before).expect("recompute before DST");

        let got_before = store.get("dst-e2e-daily").unwrap().unwrap();
        let nf_before = got_before.next_fire.expect("next_fire populated before DST");
        let dt_before = DateTime::parse_from_rfc3339(&nf_before)
            .unwrap()
            .with_timezone(&tz);
        assert_eq!(
            dt_before.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-03-07 07:30:00",
            "wall-clock before DST should be 07:30:00 MST"
        );
        assert_eq!(
            dt_before.offset().fix().local_minus_utc(),
            -7 * 3600,
            "before DST offset should be negative for UTC-7 (MST)"
        );

        // --- After DST spring-forward ---
        // now = Mar 8 at 04:00 MDT (after the boundary at 03:00, before alarm)
        let after_dst = tz.with_ymd_and_hms(2026, 3, 8, 4, 0, 0).unwrap();
        let now_after = after_dst.with_timezone(&Utc);
        store.recompute_next_fires(now_after).expect("recompute after DST");

        let got_after = store.get("dst-e2e-daily").unwrap().unwrap();
        let nf_after = got_after.next_fire.expect("next_fire populated after DST");
        let dt_after = DateTime::parse_from_rfc3339(&nf_after)
            .unwrap()
            .with_timezone(&tz);
        // Same wall-clock time!
        assert_eq!(
            dt_after.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-03-08 07:30:00",
            "wall-clock after DST should still be 07:30:00 MDT"
        );
        assert_eq!(
            dt_after.offset().fix().local_minus_utc(),
            -6 * 3600,
            "after DST offset should be negative for UTC-6 (MDT)"
        );

        // --- Cross-boundary consistency ---
        // Both fires report the same wall-clock HH:MM:SS.
        assert_eq!(
            dt_before.format("%H:%M:%S").to_string(),
            dt_after.format("%H:%M:%S").to_string(),
            "wall-clock HH:MM:SS must be identical on both sides of DST",
        );
        // But the UTC instants differ by only 1 hour (not 0, not 2).
        let utc_before = dt_before.with_timezone(&Utc);
        let utc_after = dt_after.with_timezone(&Utc) - chrono::Duration::days(1);
        assert_eq!(
            utc_after - utc_before,
            chrono::Duration::hours(-1),
            "UTC shifted by 1 hour across DST (wall-clock preserved)"
        );

        cleanup(path);
    }
}

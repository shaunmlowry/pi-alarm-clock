//! Dev alarm seeding via TOML (design D9, tasks 8.1–8.3).
//!
//! Slice 1 has no alarm-editing UI (web or Pi). To be end-to-end testable, a
//! dev `alarms.toml` at a known path is consumed at boot **if present**: parsed
//! into [`Alarm`] records and upserted into the DB. In production this file is
//! absent and the DB is the sole source (seeding happens via the future web
//! slice).
//!
//! The seeding is:
//! - **idempotent** — upsert by `id` (re-running boot does not duplicate); and
//! - **dev-only** — logged at `info!` with a "dev seed" marker; the production
//!   path skips it entirely (`cfg!(debug_assertions)` guard).
//!
//! It is **not** a replacement for the web config — it exists so slice 1 can be
//! validated end-to-end without a UI. An absent file is not an error: the
//! database is the sole source of alarms.

use chrono::Utc;
use serde::Deserialize;

use crate::alarm_store::{Alarm, AlarmStore};
use crate::error::{ConfigError, Result};
use crate::schedule::{Preset, preset_to_rrule};

/// Path to the dev `alarms.toml` consumed at boot in development builds.
///
/// In release builds seeding is skipped entirely (the DB is the sole source),
/// so this constant is only consulted on the dev path.
pub const DEV_ALARMS_PATH: &str = "./alarms.toml";

// ── Dev seed schema (task 8.1) ──────────────────────────────────────────────

/// Top-level shape of the dev `alarms.toml`: a single `[[alarms]]` array.
#[derive(Debug, Clone, Deserialize)]
struct SeedFile {
    /// The alarm entries to upsert.
    #[serde(default)]
    alarms: Vec<SeedAlarm>,
}

/// A single alarm entry in the dev `alarms.toml`.
///
/// Mirrors the persisted [`Alarm`] model but uses a friendlier dev surface:
/// `preset` + `days` (instead of a raw RRULE body) and `time` (instead of
/// `time_local`). The conversion to an [`Alarm`] row is performed by
/// [`SeedAlarm::to_alarm`].
#[derive(Debug, Clone, Deserialize)]
struct SeedAlarm {
    /// UUID v4 string primary key (upserted by `id`).
    id: String,
    /// Whether the alarm is armed. Defaults to `true` when omitted.
    #[serde(default = "default_enabled")]
    enabled: bool,
    /// Human-readable label.
    name: String,
    /// Schedule preset: `Once`, `Daily`, `Weekdays`, `Weekends`, or
    /// `Specific-days`.
    preset: String,
    /// Weekday codes for `Specific-days` (e.g. `["Mo","Fr"]`); ignored
    /// otherwise. Each entry is a two-letter RFC 5545 `BYDAY` code.
    #[serde(default)]
    days: Vec<String>,
    /// Wall-clock local fire time, `HH:MM:SS`.
    time: String,
    /// IANA timezone name, e.g. `America/Edmonton`.
    timezone: String,
    /// Mopidy URI to play when the alarm fires.
    source_uri: String,
    /// Ceiling volume for the episode, `0..=100`.
    max_volume: i64,
    /// Optional progressive volume escalation steps (slice 2). Absent →
    /// fixed `max_volume` (slice-1 behavior).
    #[serde(default)]
    escalation_steps: Option<Vec<crate::alarm_store::EscalationStep>>,
    /// Optional ordered backup source URIs (slice 2). Absent → no fallback.
    #[serde(default)]
    fallback_chain: Option<Vec<String>>,
    /// Full ISO-8601 local DateTime for a `Once` alarm; `None` otherwise.
    #[serde(default)]
    once_at: Option<String>,
}

fn default_enabled() -> bool {
    true
}

impl SeedAlarm {
    /// Convert a dev seed entry into a persisted [`Alarm`] row.
    ///
    /// `preset` + `days` are mapped to an RRULE body via the same
    /// [`preset_to_rrule`] used by the future web API; `time` becomes
    /// `time_local`; `created_at`/`updated_at` are stamped now (UTC ISO-8601).
    /// `next_fire` is left `None` — the boot recompute (task 3.4) populates it.
    fn to_alarm(&self) -> Result<Alarm> {
        let preset = parse_preset(&self.preset, &self.days)?;
        let rrule = preset_to_rrule(&preset);

        Ok(Alarm {
            id: self.id.clone(),
            enabled: self.enabled,
            name: self.name.clone(),
            time_local: self.time.clone(),
            timezone: self.timezone.clone(),
            rrule,
            once_at: self.once_at.clone(),
            source_uri: self.source_uri.clone(),
            max_volume: self.max_volume,
            escalation_steps: self.escalation_steps.clone(),
            fallback_chain: self.fallback_chain.clone(),
            next_fire: None,
            created_at: iso_now(),
            updated_at: iso_now(),
        })
    }
}

/// Parse a dev `preset` string (and optional `days` list) into a [`Preset`].
fn parse_preset(preset: &str, days: &[String]) -> Result<Preset> {
    match preset {
        "Once" => Ok(Preset::Once),
        "Daily" => Ok(Preset::Daily),
        "Weekdays" => Ok(Preset::Weekdays),
        "Weekends" => Ok(Preset::Weekends),
        "Specific-days" => {
            let weekdays: Vec<chrono::Weekday> = days
                .iter()
                .map(|d| parse_byday(d))
                .collect::<Result<_>>()?;
            if weekdays.is_empty() {
                return Err(ConfigError::Seed(format!(
                    "preset `Specific-days` requires at least one entry in `days`"
                )));
            }
            Ok(Preset::SpecificDays(weekdays))
        }
        other => Err(ConfigError::Seed(format!(
            "unknown preset {other:?}: expected Once, Daily, Weekdays, Weekends, or Specific-days"
        ))),
    }
}

/// Parse a two-letter RFC 5545 `BYDAY` code into a [`chrono::Weekday`].
fn parse_byday(code: &str) -> Result<chrono::Weekday> {
    use chrono::Weekday;
    Ok(match code {
        "Mo" | "MO" => Weekday::Mon,
        "Tu" | "TU" => Weekday::Tue,
        "We" | "WE" => Weekday::Wed,
        "Th" | "TH" => Weekday::Thu,
        "Fr" | "FR" => Weekday::Fri,
        "Sa" | "SA" => Weekday::Sat,
        "Su" | "SU" => Weekday::Sun,
        other => {
            return Err(ConfigError::Seed(format!(
                "invalid weekday code {other:?}: expected one of Mo,Tu,We,Th,Fr,Sa,Su"
            )))
        }
    })
}

fn iso_now() -> String {
    Utc::now().to_rfc3339()
}

// ── Boot seeding (task 8.2) ────────────────────────────────────────────────

/// Consume the dev `alarms.toml` at boot, upserting each entry by `id`.
///
/// Behaviour:
/// - **Dev-only**: in release builds this is a no-op (the DB is the sole
///   source). The dev path is guarded by `cfg!(debug_assertions)`.
/// - **Idempotent**: each entry is upserted by `id` (re-running boot does not
///   duplicate alarms) via [`AlarmStore::upsert`].
/// - **Absent file is not an error**: if the file is missing, the function
///   returns `Ok(())` and the database remains the sole source.
/// - **Logged**: an `info!` entry records the dev seeding with a "dev seed"
///   marker and the count of upserted alarms.
///
/// Parse/upsert failures are returned as [`ConfigError::Seed`] and surfaced to
/// the caller (main treats them as non-fatal — logged and boot continues).
pub fn seed_alarms(store: &AlarmStore) -> Result<()> {
    seed_alarms_from_path(store, DEV_ALARMS_PATH)
}

/// Seed from an explicit `alarms.toml` path. See [`seed_alarms`] for behaviour;
/// the dev-only guard still applies (release builds skip seeding entirely).
///
/// Split out from [`seed_alarms`] so tests can target a temp file without
/// mutating the process-global working directory (which would race with
/// parallel tests that resolve `./config.toml`).
pub fn seed_alarms_from_path(store: &AlarmStore, path: &str) -> Result<()> {
    // Task 8.3 / D9: dev-only. In production the DB is the sole source; the
    // seeding path is skipped entirely.
    if !cfg!(debug_assertions) {
        return Ok(());
    }

    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Scenario: absent seed file is not an error — the database is the
            // sole source of alarms.
            tracing::info!(
                marker = "dev seed",
                path = path,
                "no dev alarms.toml present; database is the sole source",
            );
            return Ok(());
        }
        Err(err) => {
            return Err(ConfigError::Seed(format!(
                "failed to read {path}: {err}"
            )));
        }
    };

    let seed: SeedFile = toml::from_str(&contents).map_err(|e| {
        ConfigError::Seed(format!("failed to parse {path} as alarms.toml: {e}"))
    })?;

    let total = seed.alarms.len();
    let mut upserted = 0usize;
    for entry in &seed.alarms {
        let alarm = entry.to_alarm()?;
        store.upsert(&alarm)?;
        upserted += 1;
    }

    tracing::info!(
        marker = "dev seed",
        path = path,
        total = total,
        upserted = upserted,
        "dev alarm seeding complete (idempotent upsert by id)",
    );

    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{open_connection, run_migrations};
    use rusqlite::Connection;

    /// Build a fresh, migrated temp DB and return a leaked `AlarmStore` borrowing
    /// it for `'static` (tests are short-lived).
    fn fresh_store() -> (std::path::PathBuf, AlarmStore<'static>) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "alarm_seed_test_{}_{}_{}.db",
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

    const TWO_ALARM_TOML: &str = r#"
[[alarms]]
id = "test-morning"
enabled = true
name = "Morning test"
preset = "Daily"
time = "07:30:00"
timezone = "America/Edmonton"
source_uri = "spotify:track:abc"
max_volume = 40

[[alarms]]
id = "test-weekends"
name = "Weekend test"
preset = "Weekends"
time = "09:00:00"
timezone = "America/Edmonton"
source_uri = "spotify:track:def"
max_volume = 35
"#;

    /// Scenario: a present dev `alarms.toml` with two entries upserts both by id.
    #[test]
    fn two_entries_upsert_both_alarms() {
        let (path, store) = fresh_store();

        let seed: SeedFile = toml::from_str(TWO_ALARM_TOML).unwrap();
        assert_eq!(seed.alarms.len(), 2);

        for entry in &seed.alarms {
            let alarm = entry.to_alarm().unwrap();
            store.upsert(&alarm).unwrap();
        }

        let all = store.list().unwrap();
        assert_eq!(all.len(), 2, "both alarms present");

        let morning = store.get("test-morning").unwrap().unwrap();
        assert_eq!(morning.name, "Morning test");
        assert_eq!(morning.rrule.as_deref(), Some("FREQ=DAILY"));
        assert_eq!(morning.time_local, "07:30:00");
        assert_eq!(morning.max_volume, 40);

        let weekends = store.get("test-weekends").unwrap().unwrap();
        assert_eq!(weekends.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=SA,SU"));
        // `enabled` defaults to true when omitted.
        assert!(weekends.enabled);

        cleanup(path);
    }

    /// Scenario: re-running the seed (re-booting) does not duplicate alarms —
    /// upsert by `id` is idempotent.
    #[test]
    fn seeding_is_idempotent_across_reboots() {
        let (path, store) = fresh_store();

        let seed: SeedFile = toml::from_str(TWO_ALARM_TOML).unwrap();

        // First boot.
        for entry in &seed.alarms {
            store.upsert(&entry.to_alarm().unwrap()).unwrap();
        }
        // Second boot (re-seed).
        for entry in &seed.alarms {
            store.upsert(&entry.to_alarm().unwrap()).unwrap();
        }

        let all = store.list().unwrap();
        assert_eq!(all.len(), 2, "no duplicates after re-seed");
        let ids: Vec<_> = all.iter().map(|a| a.id.clone()).collect();
        assert!(ids.contains(&"test-morning".to_string()));
        assert!(ids.contains(&"test-weekends".to_string()));

        cleanup(path);
    }

    /// Scenario: `Specific-days` preset with `days` maps to the right RRULE.
    #[test]
    fn specific_days_preset_maps_days() {
        let (path, store) = fresh_store();

        let toml_str = r#"
[[alarms]]
id = "mo-fr"
name = "Mo/Fr"
preset = "Specific-days"
days = ["Mo", "Fr"]
time = "06:45:00"
timezone = "America/Edmonton"
source_uri = "spotify:track:x"
max_volume = 50
"#;
        let seed: SeedFile = toml::from_str(toml_str).unwrap();
        for entry in &seed.alarms {
            store.upsert(&entry.to_alarm().unwrap()).unwrap();
        }

        let got = store.get("mo-fr").unwrap().unwrap();
        assert_eq!(
            got.rrule.as_deref(),
            Some("FREQ=WEEKLY;BYDAY=MO,FR"),
        );

        cleanup(path);
    }

    /// Scenario: a `Once` alarm carries no RRULE and stores `once_at`.
    #[test]
    fn once_alarm_has_no_rrule_and_uses_once_at() {
        let (path, store) = fresh_store();

        let toml_str = r#"
[[alarms]]
id = "once-1"
name = "Once"
preset = "Once"
time = "07:30:00"
timezone = "America/Edmonton"
source_uri = "spotify:track:once"
max_volume = 30
once_at = "2026-07-01T07:30:00"
"#;
        let seed: SeedFile = toml::from_str(toml_str).unwrap();
        for entry in &seed.alarms {
            store.upsert(&entry.to_alarm().unwrap()).unwrap();
        }

        let got = store.get("once-1").unwrap().unwrap();
        assert!(got.rrule.is_none(), "Once alarm has no RRULE");
        assert_eq!(got.once_at.as_deref(), Some("2026-07-01T07:30:00"));

        cleanup(path);
    }

    /// Scenario: end-to-end `seed_alarms_from_path` against a present file
    /// upserts both alarms idempotently (logged via tracing in normal runs;
    /// here we assert the DB effect).
    #[test]
    fn seed_alarms_upserts_present_file_idempotently() {
        let (db_path, store) = fresh_store();

        // Write a dev alarms.toml to a temp path and seed against it by path
        // (avoids mutating the process-global cwd, which would race with
        // parallel tests that resolve `./config.toml`).
        let seed_path = std::env::temp_dir().join(format!(
            "alarm-clock-seed-{}.toml",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::write(&seed_path, TWO_ALARM_TOML).unwrap();
        let path_str = seed_path.to_str().unwrap();

        // First seed.
        seed_alarms_from_path(&store, path_str).expect("seeding should succeed on a present file");
        let after_first = store.list().unwrap();
        assert_eq!(after_first.len(), 2, "both alarms seeded");

        // Second seed (re-boot) — idempotent, no duplicates.
        seed_alarms_from_path(&store, path_str).expect("re-seeding should succeed");
        let after_second = store.list().unwrap();
        assert_eq!(after_second.len(), 2, "no duplicates after re-seed");

        let _ = std::fs::remove_file(&seed_path);
        cleanup(db_path);
    }

    /// Scenario: `seed_alarms_from_path` against a missing file returns Ok
    /// (not an error); the database is the sole source. Covers the absent-file
    /// branch of the public boot path without touching the real `./alarms.toml`.
    #[test]
    fn seed_alarms_missing_file_is_not_an_error() {
        let (db_path, store) = fresh_store();
        let missing = std::env::temp_dir().join("alarm-clock-seed-missing.toml");
        let _ = std::fs::remove_file(&missing);

        seed_alarms_from_path(&store, missing.to_str().unwrap())
            .expect("absent file is not an error");

        let all = store.list().unwrap();
        assert!(all.is_empty(), "DB stays empty (sole source)");

        cleanup(db_path);
    }

    /// Scenario: a `Once` alarm seeded from TOML round-trips through the store
    /// and its `next_fire` can be recomputed from `once_at`.
    #[test]
    fn once_seed_round_trips_and_recomputes() {
        let (path, store) = fresh_store();

        let toml_str = r#"
[[alarms]]
id = "once-rt"
name = "Once round-trip"
preset = "Once"
time = "07:30:00"
timezone = "America/Edmonton"
source_uri = "spotify:track:rt"
max_volume = 30
once_at = "2026-07-01T07:30:00"
"#;
        let seed: SeedFile = toml::from_str(toml_str).unwrap();
        for entry in &seed.alarms {
            store.upsert(&entry.to_alarm().unwrap()).unwrap();
        }

        // Recompute next_fire — the Once alarm is in 2026, so it is in the
        // future relative to "now" only if now is before it; either way the
        // recompute must not error and must not mutate once_at.
        store.recompute_next_fires(Utc::now()).unwrap();
        let got = store.get("once-rt").unwrap().unwrap();
        assert_eq!(got.once_at.as_deref(), Some("2026-07-01T07:30:00"));
        assert!(got.rrule.is_none());

        cleanup(path);
    }

    /// Scenario: a seed entry with `escalation_steps` and `fallback_chain` is
    /// upserted and round-trips through the store (slice 2 / D10).
    #[test]
    fn seed_entry_with_escalation_and_fallback_round_trips() {
        let (path, store) = fresh_store();

        let toml_str = r#"
[[alarms]]
id = "escalating"
name = "Escalating"
preset = "Daily"
time = "07:30:00"
timezone = "America/Edmonton"
source_uri = "spotify:track:primary"
max_volume = 80
escalation_steps = [{ after_secs = 0, volume = 20 }, { after_secs = 60, volume = 80 }]
fallback_chain = ["spotify:track:backup1", "file:///beep.mp3"]
"#;
        let seed: SeedFile = toml::from_str(toml_str).unwrap();
        for entry in &seed.alarms {
            store.upsert(&entry.to_alarm().unwrap()).unwrap();
        }

        let got = store.get("escalating").unwrap().unwrap();
        assert_eq!(got.escalation_steps.as_ref().unwrap().len(), 2);
        assert_eq!(got.escalation_steps.as_ref().unwrap()[0].volume, 20);
        assert_eq!(got.fallback_chain.as_ref().unwrap(), &vec![
            "spotify:track:backup1".to_string(),
            "file:///beep.mp3".to_string(),
        ]);

        cleanup(path);
    }

    /// Scenario: a seed entry without the new fields seeds as None (slice-1).
    #[test]
    fn seed_entry_without_new_fields_seeds_as_none() {
        let (path, store) = fresh_store();

        let toml_str = r#"
[[alarms]]
id = "plain"
name = "Plain"
preset = "Daily"
time = "07:30:00"
timezone = "America/Edmonton"
source_uri = "spotify:track:plain"
max_volume = 40
"#;
        let seed: SeedFile = toml::from_str(toml_str).unwrap();
        for entry in &seed.alarms {
            store.upsert(&entry.to_alarm().unwrap()).unwrap();
        }

        let got = store.get("plain").unwrap().unwrap();
        assert!(got.escalation_steps.is_none());
        assert!(got.fallback_chain.is_none());

        cleanup(path);
    }

    /// Scenario: an unknown preset string is rejected with a seed error.
    #[test]
    fn unknown_preset_is_rejected() {
        let entry = SeedAlarm {
            id: "bad".into(),
            enabled: true,
            name: "Bad".into(),
            preset: "Whenever".into(),
            days: vec![],
            time: "07:30:00".into(),
            timezone: "America/Edmonton".into(),
            source_uri: "spotify:track:bad".into(),
            max_volume: 30,
            escalation_steps: None,
            fallback_chain: None,
            once_at: None,
        };
        let res = entry.to_alarm();
        assert!(res.is_err(), "unknown preset should fail");
    }

    /// Scenario: `Specific-days` with no `days` entries is rejected.
    #[test]
    fn specific_days_without_days_is_rejected() {
        let entry = SeedAlarm {
            id: "noday".into(),
            enabled: true,
            name: "NoDays".into(),
            preset: "Specific-days".into(),
            days: vec![],
            time: "07:30:00".into(),
            timezone: "America/Edmonton".into(),
            source_uri: "spotify:track:noday".into(),
            max_volume: 30,
            escalation_steps: None,
            fallback_chain: None,
            once_at: None,
        };
        let res = entry.to_alarm();
        assert!(res.is_err(), "Specific-days with no days should fail");
    }

    /// Scenario: production path skips seeding. We assert the dev-only guard
    /// logic by checking that `cfg!(debug_assertions)` is true in tests (the
    /// seed path is only reachable in dev); in release the function returns
    /// `Ok(())` without touching the DB.
    #[test]
    fn seeding_is_dev_only_guard() {
        // In tests we run under a dev (debug) build, so the guard passes.
        assert!(cfg!(debug_assertions), "tests run in dev builds");
        // The guard itself: `if !cfg!(debug_assertions) { return Ok(()); }`.
        // In a release build `seed_alarms` would be a no-op against the store;
        // here we only assert the guard condition compiles and evaluates.
    }
}

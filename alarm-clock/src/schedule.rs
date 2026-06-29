//! Schedule representation (slice 1, tasks 2.2–2.5).
//!
//! Design D2: each alarm stores a wall-clock-local `time` (HH:MM:SS in the
//! configured IANA timezone) and an RFC 5545 `RRULE` string. For a `Once`
//! alarm the RRULE is absent and the `time` is a full `DateTime<Tz>`.
//!
//! [`Schedule::next_fire`] evaluates the rule (via the `rrule` crate) in the
//! alarm's stored IANA timezone (`chrono-tz`), returning the next occurrence
//! strictly after the supplied `after`. Times are stored wall-clock-local so
//! DST is handled at compute time, not storage time.
//!
//! ## Presets (task 2.3)
//!
//! Presets map to RRULE strings:
//! - `Once` → no RRULE
//! - `Daily` → `FREQ=DAILY`
//! - `Weekdays` → `FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR`
//! - `Weekends` → `FREQ=WEEKLY;BYDAY=SA,SU`
//! - `Specific-days` → `FREQ=WEEKLY;BYDAY=<selected>`
//!
//! Complex RRULE (COUNT, UNTIL, BYSETPOS, INTERVAL>1) is **parsed** if present
//! (see [`Schedule::from_rrule`]) but slice 1 does not construct it — the full
//! builder is web-only (a later slice).

use chrono::{DateTime, NaiveTime, Weekday};
use chrono_tz::Tz;
use rrule::RRuleSet;
use std::str::FromStr;
use thiserror::Error;

/// Maximum number of occurrences to expand when searching for the next fire.
///
/// For all preset-generated rules the next occurrence strictly after `after`
/// is within a week; this bound exists purely to terminate the `rrule`
/// iterator for unbounded (no COUNT/UNTIL) rules.
const NEXT_FIRE_SEARCH_LIMIT: usize = 1000;

/// Errors raised when constructing a [`Schedule`].
#[derive(Debug, Error)]
pub enum ScheduleError {
    /// The supplied RRULE string could not be parsed as RFC 5545.
    #[error("invalid RRULE {rrule:?}: {source}")]
    InvalidRrule {
        rrule: String,
        #[source]
        source: rrule::RRuleError,
    },
}

/// Schedule presets (task 2.3).
///
/// Slice 1 only constructs preset-generated rules. Complex RRULE is accepted
/// (parsed) via [`Schedule::from_rrule`] but is not produced by any preset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preset {
    /// Fire once at the stored `once_at` time; no RRULE.
    Once,
    /// Fire every day at the stored wall-clock time.
    Daily,
    /// Fire Monday–Friday at the stored wall-clock time.
    Weekdays,
    /// Fire Saturday and Sunday at the stored wall-clock time.
    Weekends,
    /// Fire on the selected weekdays at the stored wall-clock time.
    SpecificDays(Vec<Weekday>),
}

/// Map a [`Preset`] to its RFC 5545 RRULE body (without the `RRULE:` prefix).
///
/// Returns `None` for [`Preset::Once`] (no RRULE). The returned string is the
/// body of the `RRULE` property — e.g. `FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR`.
///
/// Per task 2.3, complex RRULE (COUNT, UNTIL, BYSETPOS, INTERVAL>1) is never
/// constructed here; it can only enter the system via [`Schedule::from_rrule`].
pub fn preset_to_rrule(preset: &Preset) -> Option<String> {
    match preset {
        Preset::Once => None,
        Preset::Daily => Some("FREQ=DAILY".to_string()),
        Preset::Weekdays => Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".to_string()),
        Preset::Weekends => Some("FREQ=WEEKLY;BYDAY=SA,SU".to_string()),
        Preset::SpecificDays(days) => {
            let byday = days
                .iter()
                .map(|d| weekday_to_byday(*d))
                .collect::<Vec<_>>()
                .join(",");
            Some(format!("FREQ=WEEKLY;BYDAY={byday}"))
        }
    }
}

/// Convert a [`Weekday`] to its RFC 5545 two-letter `BYDAY` code.
fn weekday_to_byday(d: Weekday) -> &'static str {
    match d {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

/// A schedule: an `rrule` (optional) + wall-clock-local `time` + IANA timezone.
///
/// For a `Once` alarm, `rrule_body` is `None` and `once_at` holds the full fire
/// instant. For recurring alarms, `rrule_body` is the parsed RRULE body and
/// `time_local` is the wall-clock time at which the alarm fires each occurrence.
#[derive(Debug, Clone)]
pub struct Schedule {
    /// RFC 5545 RRULE body (e.g. `FREQ=DAILY`); `None` for a `Once` alarm.
    rrule_body: Option<String>,
    /// Wall-clock-local fire time (HH:MM:SS) in `timezone`.
    time_local: NaiveTime,
    /// The alarm's stored IANA timezone (via `chrono-tz`).
    timezone: Tz,
    /// Full fire instant for a `Once` alarm; `None` for recurring alarms.
    once_at: Option<DateTime<Tz>>,
}

impl Schedule {
    /// Construct a `Once` schedule that fires at `once_at` (in `timezone`).
    pub fn once(once_at: DateTime<Tz>, timezone: Tz) -> Self {
        // `time_local` is the wall-clock portion of `once_at`; the authoritative
        // value for a Once alarm is `once_at` itself (see `next_fire`).
        Self {
            rrule_body: None,
            time_local: once_at.naive_local().time(),
            timezone,
            once_at: Some(once_at),
        }
    }

    /// Construct a recurring schedule from a preset, a wall-clock fire time,
    /// and a stored timezone. The preset is mapped to its RRULE body via
    /// [`preset_to_rrule`].
    pub fn from_preset(
        preset: &Preset,
        time_local: NaiveTime,
        timezone: Tz,
    ) -> Result<Self, ScheduleError> {
        let rrule_body = preset_to_rrule(preset);
        Self::recurring(rrule_body, time_local, timezone)
    }

    /// Construct a recurring schedule from a raw RRULE body, a wall-clock fire
    /// time, and a stored timezone.
    ///
    /// `rrule_body` may be `None` (treated as `Once`-without-`once_at`, i.e. no
    /// occurrences) or a complex RRULE like `FREQ=MONTHLY;BYDAY=2MO` — it is
    /// parsed (accepted) but never constructed by slice 1.
    pub fn recurring(
        rrule_body: Option<String>,
        time_local: NaiveTime,
        timezone: Tz,
    ) -> Result<Self, ScheduleError> {
        // Validate the RRULE by parsing it once (against a throwaway DTSTART in
        // the stored timezone) so construction fails fast on malformed input.
        if let Some(body) = &rrule_body {
            let probe = Self::build_rrule_set(body, &time_local, &timezone, None)?;
            // Touch the iterator to surface validation errors eagerly.
            let _ = probe.all(0);
        }
        Ok(Self {
            rrule_body,
            time_local,
            timezone,
            once_at: None,
        })
    }

    /// The stored IANA timezone.
    pub fn timezone(&self) -> Tz {
        self.timezone
    }

    /// The stored wall-clock-local fire time.
    pub fn time_local(&self) -> NaiveTime {
        self.time_local
    }

    /// The stored RRULE body, if any (`None` for a `Once` alarm).
    pub fn rrule_body(&self) -> Option<&str> {
        self.rrule_body.as_deref()
    }

    /// The stored `once_at` instant, if this is a `Once` alarm.
    pub fn once_at(&self) -> Option<DateTime<Tz>> {
        self.once_at
    }

    /// Compute the next fire time strictly after `after`, evaluated in the
    /// alarm's stored IANA timezone.
    ///
    /// - For a `Once` alarm: returns `once_at` if it is strictly after `after`,
    ///   otherwise `None`.
    /// - For a recurring alarm: expands the RRULE (anchored at the wall-clock
    ///   `time_local` on the date of `after`) and returns the first occurrence
    ///   strictly after `after`. DST is resolved by `chrono-tz`/`rrule` at
    ///   compute time, so a daily alarm fires at the same wall-clock time on
    ///   both sides of a DST boundary.
    pub fn next_fire(&self, after: DateTime<Tz>) -> Option<DateTime<Tz>> {
        if let Some(once_at) = self.once_at {
            return if once_at > after { Some(once_at) } else { None };
        }

        let body = self.rrule_body.as_ref()?;
        let rrule_tz = rrule::Tz::from(self.timezone);
        let after_rrule = after.with_timezone(&rrule_tz);

        let set = match Self::build_rrule_set(
            body,
            &self.time_local,
            &self.timezone,
            Some(after_rrule.date_naive()),
        ) {
            Ok(s) => s,
            Err(_) => return None,
        };

        // Iterate occurrences from DTSTART forward, returning the first that is
        // strictly after `after`. Bounded by `NEXT_FIRE_SEARCH_LIMIT` so
        // unbounded (no COUNT/UNTIL) rules terminate.
        for dt in (&set).into_iter().take(NEXT_FIRE_SEARCH_LIMIT) {
            if dt > after_rrule {
                return Some(dt.with_timezone(&self.timezone));
            }
        }
        None
    }

    /// Build a validated [`RRuleSet`] for the given RRULE body, anchored at
    /// `time_local` on `anchor_date` (or, if `None`, the date of the probe
    /// instant used only for construction-time validation).
    fn build_rrule_set(
        body: &str,
        time_local: &NaiveTime,
        timezone: &Tz,
        anchor_date: Option<chrono::NaiveDate>,
    ) -> Result<RRuleSet, ScheduleError> {
        // DTSTART: anchor date (or a fixed epoch-ish date for the construction
        // probe) at the wall-clock fire time, in the stored IANA timezone.
        let anchor = anchor_date
            .unwrap_or_else(|| chrono::NaiveDate::from_ymd_opt(2000, 1, 1).unwrap());
        let dtstart_naive = anchor.and_time(*time_local);
        let dtstart_str = format!("{}", dtstart_naive.format("%Y%m%dT%H%M%S"));
        let tz_name = timezone.to_string();
        let ical = format!("DTSTART;TZID={tz_name}:{dtstart_str}\nRRULE:{body}");

        RRuleSet::from_str(&ical).map_err(|source| ScheduleError::InvalidRrule {
            rrule: body.to_string(),
            source,
        })
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Duration, NaiveDate, TimeZone};
    use chrono_tz::America::Edmonton;
    use chrono_tz::UTC;

    /// Helper: build a `DateTime<Tz>` in `America/Edmonton`.
    fn edt(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> DateTime<Tz> {
        Edmonton
            .with_ymd_and_hms(year, month, day, hour, min, sec)
            .unwrap()
    }

    // ── Task 2.3: preset → RRULE mapping ────────────────────────────────

    /// Scenario: Specific-days preset with Monday and Friday maps to
    /// `FREQ=WEEKLY;BYDAY=MO,FR`.
    #[test]
    fn specific_days_preset_maps_to_rrule() {
        let preset = Preset::SpecificDays(vec![Weekday::Mon, Weekday::Fri]);
        assert_eq!(
            preset_to_rrule(&preset).as_deref(),
            Some("FREQ=WEEKLY;BYDAY=MO,FR"),
        );
    }

    /// Scenario: Weekdays preset expands to `FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR`.
    #[test]
    fn weekdays_preset_maps_to_rrule() {
        assert_eq!(
            preset_to_rrule(&Preset::Weekdays).as_deref(),
            Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"),
        );
    }

    /// Scenario: Weekends preset expands to `FREQ=WEEKLY;BYDAY=SA,SU`.
    #[test]
    fn weekends_preset_maps_to_rrule() {
        assert_eq!(
            preset_to_rrule(&Preset::Weekends).as_deref(),
            Some("FREQ=WEEKLY;BYDAY=SA,SU"),
        );
    }

    /// Scenario: Daily preset expands to `FREQ=DAILY`; Once has no RRULE.
    #[test]
    fn daily_and_once_presets_map_to_rrule() {
        assert_eq!(preset_to_rrule(&Preset::Daily).as_deref(), Some("FREQ=DAILY"));
        assert_eq!(preset_to_rrule(&Preset::Once), None);
    }

    /// Scenario: a complex RRULE (e.g. `FREQ=MONTHLY;BYDAY=2MO`, second Monday)
    /// is parsed and accepted by `Schedule::from_rrule` even though slice 1
    /// provides no UI to construct it.
    #[test]
    fn complex_rrule_is_parsed_but_not_constructed() {
        let time = NaiveTime::from_hms_opt(7, 30, 0).unwrap();
        let sched = Schedule::recurring(
            Some("FREQ=MONTHLY;BYDAY=2MO".to_string()),
            time,
            Edmonton,
        )
        .expect("complex RRULE should parse");

        assert_eq!(sched.rrule_body(), Some("FREQ=MONTHLY;BYDAY=2MO"));
        // The second Monday of March 2026 is the 9th.
        let after = edt(2026, 3, 1, 0, 0, 0);
        let next = sched.next_fire(after).expect("should fire on the 2nd Monday");
        assert_eq!(next, edt(2026, 3, 9, 7, 30, 0));
    }

    // ── Task 2.4: DST spring-forward and fall-back ──────────────────────

    /// Scenario: a daily alarm set for 07:30 in `America/Edmonton` fires at
    /// 07:30 local wall-clock on both sides of the DST spring-forward boundary
    /// (2026-03-08 02:00 → 03:00 LST), not at the shifted UTC instant.
    #[test]
    fn daily_alarm_fires_same_wall_clock_across_spring_forward() {
        let time = NaiveTime::from_hms_opt(7, 30, 0).unwrap();
        let sched =
            Schedule::from_preset(&Preset::Daily, time, Edmonton).expect("daily preset");

        // Day before spring-forward (still MST, UTC-7).
        let before = edt(2026, 3, 7, 0, 0, 0);
        let next = sched.next_fire(before).expect("should fire 2026-03-07 07:30");
        assert_eq!(next, edt(2026, 3, 7, 7, 30, 0));
        assert_eq!(next.format("%H:%M").to_string(), "07:30");

        // After the spring-forward (now MDT, UTC-6). The alarm still fires at
        // 07:30 local wall-clock — the UTC offset shifted, the wall time did not.
        let after_boundary = edt(2026, 3, 8, 4, 0, 0); // 04:00 MDT (boundary at 03:00)
        let next = sched
            .next_fire(after_boundary)
            .expect("should fire 2026-03-08 07:30 MDT");
        assert_eq!(next, edt(2026, 3, 8, 7, 30, 0));
        // Same wall-clock time, different UTC offset (MST -07:00 → MDT -06:00).
        assert_eq!(next.format("%H:%M").to_string(), "07:30");
        assert_eq!(next.format("%z").to_string(), "-0600");

        // Verify the UTC instant shifted by only 1 hour (wall-clock preserved,
        // absolute instant moved with the offset) compared to the MST day:
        // 07:30 MST = 14:30 UTC; 07:30 MDT = 13:30 UTC.
        let mst_fire = edt(2026, 3, 7, 7, 30, 0).with_timezone(&UTC);
        let mdt_fire = edt(2026, 3, 8, 7, 30, 0).with_timezone(&UTC);
        assert_eq!(mst_fire.format("%H:%M").to_string(), "14:30");
        assert_eq!(mdt_fire.format("%H:%M").to_string(), "13:30");
    }

    /// Scenario: a daily alarm fires at the same wall-clock time across the DST
    /// fall-back boundary (2026-11-01 02:00 LST → 01:00 LST).
    #[test]
    fn daily_alarm_fires_same_wall_clock_across_fall_back() {
        let time = NaiveTime::from_hms_opt(7, 30, 0).unwrap();
        let sched =
            Schedule::from_preset(&Preset::Daily, time, Edmonton).expect("daily preset");

        // Before fall-back (MDT, UTC-6).
        let before = edt(2026, 10, 31, 0, 0, 0);
        let next = sched.next_fire(before).expect("should fire 2026-10-31 07:30 MDT");
        assert_eq!(next, edt(2026, 10, 31, 7, 30, 0));
        assert_eq!(next.format("%z").to_string(), "-0600");

        // After fall-back (MST, UTC-7). Wall-clock 07:30 preserved.
        let after_boundary = edt(2026, 11, 1, 4, 0, 0);
        let next = sched
            .next_fire(after_boundary)
            .expect("should fire 2026-11-01 07:30 MST");
        assert_eq!(next, edt(2026, 11, 1, 7, 30, 0));
        assert_eq!(next.format("%H:%M").to_string(), "07:30");
        assert_eq!(next.format("%z").to_string(), "-0700");
    }

    // ── Task 2.5: Once returns once_at then None; Weekdays skips Sat/Sun ──

    /// Scenario: a `Once` alarm returns its `once_at` time then `None`.
    #[test]
    fn once_alarm_returns_once_at_then_none() {
        let once_at = edt(2026, 6, 29, 7, 30, 0);
        let sched = Schedule::once(once_at, Edmonton);

        // Before the fire time → next_fire returns once_at.
        let before = edt(2026, 6, 29, 0, 0, 0);
        assert_eq!(sched.next_fire(before), Some(once_at));

        // At the fire time (strictly-after semantics) → None (already fired).
        assert_eq!(sched.next_fire(once_at), None);

        // After the fire time → None.
        let after = edt(2026, 6, 29, 8, 0, 0);
        assert_eq!(sched.next_fire(after), None);
    }

    /// Scenario: a `Weekdays` alarm fires Monday–Friday and skips Sat/Sun.
    #[test]
    fn weekdays_alarm_skips_weekend() {
        let time = NaiveTime::from_hms_opt(7, 30, 0).unwrap();
        let sched =
            Schedule::from_preset(&Preset::Weekdays, time, Edmonton).expect("weekdays preset");
        assert_eq!(sched.rrule_body(), Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"));

        // Friday 2026-06-26 at 06:00 → next fire is Friday 07:30.
        let fri_morning = edt(2026, 6, 26, 6, 0, 0);
        let next = sched.next_fire(fri_morning).expect("should fire Fri 07:30");
        assert_eq!(next, edt(2026, 6, 26, 7, 30, 0));
        assert_eq!(next.weekday(), Weekday::Fri);

        // After Friday's fire → next is Monday (skips Sat & Sun).
        let after_fri = edt(2026, 6, 26, 7, 30, 0);
        let next = sched.next_fire(after_fri).expect("should fire Mon 07:30");
        assert_eq!(next, edt(2026, 6, 29, 7, 30, 0));
        assert_eq!(next.weekday(), Weekday::Mon);

        // Saturday morning → next fire is Monday (skips the weekend).
        let sat_morning = edt(2026, 6, 27, 6, 0, 0);
        let next = sched.next_fire(sat_morning).expect("should fire Mon 07:30");
        assert_eq!(next, edt(2026, 6, 29, 7, 30, 0));
        assert_eq!(next.weekday(), Weekday::Mon);

        // Sunday morning → next fire is Monday.
        let sun_morning = edt(2026, 6, 28, 6, 0, 0);
        let next = sched.next_fire(sun_morning).expect("should fire Mon 07:30");
        assert_eq!(next, edt(2026, 6, 29, 7, 30, 0));
        assert_eq!(next.weekday(), Weekday::Mon);
    }

    // ── Spec: Timezone-stored alarm evaluates in its timezone ──────────

    /// Scenario: an alarm stored with timezone `America/Edmonton` evaluates
    /// its next-fire in `America/Edmonton`, not the device timezone. We verify
    /// by passing an `after` expressed in UTC and asserting the returned fire
    /// time is the Edmonton wall-clock 07:30.
    #[test]
    fn timezone_stored_alarm_evaluates_in_stored_timezone() {
        let time = NaiveTime::from_hms_opt(7, 30, 0).unwrap();
        let sched =
            Schedule::from_preset(&Preset::Daily, time, Edmonton).expect("daily preset");

        // `after` in UTC; the schedule must still evaluate in Edmonton.
        let after_utc = UTC.with_ymd_and_hms(2026, 6, 29, 6, 0, 0).unwrap();
        let next = sched.next_fire(after_utc).expect("should fire in Edmonton tz");
        assert_eq!(next, edt(2026, 6, 29, 7, 30, 0));
        // Returned time is in the stored timezone.
        assert_eq!(next.timezone(), Edmonton);
    }

    /// Scenario: `next_fire` is strictly-after — an `after` exactly at the fire
    /// time yields the *next* occurrence, not the same one.
    #[test]
    fn next_fire_is_strictly_after() {
        let time = NaiveTime::from_hms_opt(7, 30, 0).unwrap();
        let sched = Schedule::from_preset(&Preset::Daily, time, Edmonton).unwrap();
        let fire = edt(2026, 6, 29, 7, 30, 0);
        let next = sched.next_fire(fire).expect("should advance to next day");
        assert_eq!(next, edt(2026, 6, 30, 7, 30, 0));
        let _ = Duration::minutes(0); // keep Duration import used
    }

    /// Scenario: a malformed RRULE is rejected at construction time.
    #[test]
    fn malformed_rrule_is_rejected() {
        let time = NaiveTime::from_hms_opt(7, 30, 0).unwrap();
        let result = Schedule::recurring(Some("FREQ=NOPE".to_string()), time, Edmonton);
        assert!(result.is_err(), "malformed RRULE should fail construction");
    }

    /// Scenario: a `NaiveDate`-derived anchor helper exists (sanity for the
    /// build_rrule_set probe used in construction validation).
    #[test]
    fn build_rrule_set_anchors_at_time_local() {
        let time = NaiveTime::from_hms_opt(7, 30, 0).unwrap();
        let anchor = NaiveDate::from_ymd_opt(2026, 6, 29).unwrap();
        let set = Schedule::build_rrule_set("FREQ=DAILY", &time, &Edmonton, Some(anchor))
            .expect("should build");
        let first = (&set).into_iter().next().expect("at least one occurrence");
        assert_eq!(first, edt(2026, 6, 29, 7, 30, 0));
    }
}

//! Scheduler module (slice 1, tasks 1.1–1.4).
//!
//! Design D1: a [`slint::Timer`] on main fires at a fixed interval (default
//! 5 s). On each tick the scheduler **re-reads `Local::now()`** and re-derives
//! which enabled alarms are due. When `now >= next_fire` for an enabled alarm
//! the alarm fires (the episode FSM is invoked) and `next_fire` is recomputed
//! to the next occurrence after `now`.
//!
//! Re-reading `Local::now()` every tick (rather than arming a point-in-time
//! timer for `next_fire - now`) is robust to clock jumps: an NTP correction or
//! `fake-hwclock` jump cannot fire an alarm early or cause it to never fire —
//! a missed alarm simply fires on the next tick after the clock becomes
//! correct, which is acceptable per the PRD's timing bar.
//!
//! ## Missed-alarm-on-boot (task 1.2)
//!
//! If the device was powered off across an alarm's fire time, the first tick
//! on boot sees `now > next_fire`. Slice 1 policy: **do not fire missed
//! alarms** — advance `next_fire` to the next occurrence after `now` and log
//! an `info!` skip. Firing a stale alarm at boot (e.g. a 3 am alarm powered
//! on at 9 am) is worse than skipping it.
//!
//! ## Seams
//!
//! The concrete [`AlarmSource`] (the `AlarmStore`, group 3) and
//! [`EpisodeFsm`] (the `EpisodeController`, group 5) are not yet implemented
//! in this task group. This module defines the trait seams the scheduler
//! depends on so the tick logic is fully unit-testable now (task 1.4) with
//! mock implementations, and so group 9.1 can drop the real types in without
//! touching the scheduler core. The [`NoopAlarmSource`] / [`NoopEpisodeFsm`]
//! placeholders are wired into the live `slint::Timer` on main until the real
//! types arrive.

use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use tracing::{info, info_span};

use crate::alarm_store::HolidayPolicy;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use tracing_subscriber::layer::{Context, Layer};

/// Default scheduler tick interval: 5 seconds (design D1).
///
/// 5 s granularity is well below the PRD's human-perceptible alarm-timing bar,
/// and the per-tick recompute cost (evaluating a handful of `rrule`s) is
/// negligible.
pub const DEFAULT_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Identifier of an alarm (matches the `alarms.id` SQLite `TEXT PRIMARY KEY` —
/// a UUID string, per migration `v2` / [`crate::alarm_store::Alarm`]).
pub type AlarmId = String;

/// A due alarm surfaced to the scheduler by the alarm source.
///
/// Carries the alarm's stored `next_fire` so the scheduler can distinguish a
/// normal due fire (`now >= next_fire` and not a boot catch-up) from a missed
/// boot alarm (`now > next_fire` on the first tick) without a second round-trip
/// to the store. Also carries the alarm's `HolidayPolicy` and stored IANA
/// timezone (slice 6) so the tick can apply holiday suppression.
#[derive(Debug, Clone)]
pub struct DueAlarm {
    pub id: AlarmId,
    /// Stored next-fire time, in local time.
    pub next_fire: DateTime<Local>,
    /// Per-alarm holiday suppression policy (slice 6).
    pub policy: HolidayPolicy,
    /// Stored IANA timezone of the alarm (for holiday date membership).
    pub timezone: String,
}

// ── Seams ───────────────────────────────────────────────────────────────────

/// Alarm-store seam (filled by the real `AlarmStore` in group 3).
///
/// `due_alarms(now)` returns enabled alarms whose stored `next_fire <= now`.
/// `recompute_next_fire(id, now)` recomputes the alarm's next occurrence after
/// `now` (or its skip-state) and persists it in a single transaction.
pub trait AlarmSource {
    fn due_alarms(&mut self, now: DateTime<Local>) -> Vec<DueAlarm>;
    fn recompute_next_fire(&mut self, id: AlarmId, now: DateTime<Local>);
    /// Override the alarm's `next_fire` cache to a specific local datetime
    /// (slice 6 `ShiftForward`). The default impl falls back to recomputing
    /// from the rule (equivalent to `Suppress`) so mock seams compile
    /// unchanged.
    fn set_next_fire(&mut self, id: AlarmId, _target: DateTime<Local>, now: DateTime<Local>) {
        // Default: ignore the specific target, recompute from rule.
        self.recompute_next_fire(id, now);
    }
}

/// Holiday-lookup seam (slice 6). The scheduler consults this on each due
/// alarm to decide whether a holiday is active on the alarm's fire date.
///
/// Implementations hold a set of holiday dates (from Holiday-role calendars'
/// all-day events) refreshed on the shared 30-min tick. The check is an O(1)
/// set lookup — no per-tick API call.
pub trait HolidayLookup: Send + Sync {
    /// True if *date* is a holiday (a Holiday-role calendar has an all-day
    /// event that day).
    fn is_holiday(&self, date: NaiveDate) -> bool;
}

/// `HolidayLookup` that reports no holidays — used when no Holiday-role
/// calendar is configured (or in tests that exercise pre-slice-6 behavior).
#[derive(Default, Debug, Clone, Copy)]
pub struct NoHolidays;

impl HolidayLookup for NoHolidays {
    fn is_holiday(&self, _date: NaiveDate) -> bool {
        false
    }
}

/// Episode-FSM seam (filled by the real `EpisodeController` in group 5).
///
/// `fire(alarm_id)` captures the Mopidy snapshot and starts playback. It does
/// not block the tick — the reply drain corrects FSM state on failure (design).
///
/// `on_tick(now)` (slice 2) advances escalation and re-fires snoozed episodes;
/// it has a default no-op so no-op/mock seams compile unchanged.
pub trait EpisodeFsm {
    fn fire(&mut self, alarm_id: AlarmId);

    /// Per-tick episode progress hook (slice 2 / D5): advance the volume
    /// through `escalation_steps` and re-fire a snoozed episode whose
    /// `snooze_until` has elapsed. The default is a no-op so existing no-op
    /// and mock seam implementations compile unchanged.
    fn on_tick(&mut self, _now: DateTime<Local>) {}
}

/// Clock seam so the tick re-reads `Local::now()` each tick and is mockable.
pub trait Clock {
    fn now(&self) -> DateTime<Local>;
}

/// Default clock backed by `Local::now()`.
#[derive(Default, Debug, Clone, Copy)]
pub struct LocalClock;

impl Clock for LocalClock {
    fn now(&self) -> DateTime<Local> {
        Local::now()
    }
}

// ── Placeholders wired into the live timer until groups 3 & 5 land ──────────

/// No-op [`AlarmSource`] used by the live `slint::Timer` until the real
/// `AlarmStore` (group 3) is wired in (group 9.1). Returns no due alarms.
#[derive(Default, Debug, Clone, Copy)]
pub struct NoopAlarmSource;

impl AlarmSource for NoopAlarmSource {
    fn due_alarms(&mut self, _now: DateTime<Local>) -> Vec<DueAlarm> {
        Vec::new()
    }
    fn recompute_next_fire(&mut self, _id: AlarmId, _now: DateTime<Local>) {}
}

/// No-op [`EpisodeFsm`] used by the live `slint::Timer` until the real
/// `EpisodeController` (group 5) is wired in (group 9.1).
#[derive(Default, Debug, Clone, Copy)]
pub struct NoopEpisodeFsm;

impl EpisodeFsm for NoopEpisodeFsm {
    fn fire(&mut self, alarm_id: AlarmId) {
        info!(alarm_id = alarm_id, "episode fire (no-op placeholder)");
    }
}

// ── Scheduler ───────────────────────────────────────────────────────────────

/// Summary of a single tick, returned for unit-testing and observability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickReport {
    /// Number of due alarms the source surfaced this tick.
    pub alarms_evaluated: usize,
    /// Number of alarms actually fired (episode FSM invoked).
    pub fired: usize,
    /// Number of alarms skipped as missed-on-boot (next_fire advanced).
    pub skipped_missed: usize,
    /// Number of alarms skipped due to a holiday (slice 6).
    pub skipped_holiday: usize,
}

/// The scheduler: owns the alarm source, episode FSM, a clock, a `booted`
/// flag (false until the first tick completes, driving the missed-alarm-on-boot
/// policy of task 1.2), and an optional holiday lookup (slice 6).
pub struct Scheduler<S, F, C, H = NoHolidays> {
    source: S,
    fsm: F,
    clock: C,
    booted: bool,
    holidays: H,
}

impl<S, F, C> Scheduler<S, F, C, NoHolidays>
where
    S: AlarmSource,
    F: EpisodeFsm,
    C: Clock,
{
    /// Construct a scheduler around its three seams with no holiday lookup.
    /// `booted` starts `false`. (Use [`with_holidays`](Scheduler::with_holidays)
    /// to attach a `HolidayLookup` for slice 6 holiday suppression.)
    pub fn new(source: S, fsm: F, clock: C) -> Self {
        Self {
            source,
            fsm,
            clock,
            booted: false,
            holidays: NoHolidays,
        }
    }
}

impl<S, F, C, H> Scheduler<S, F, C, H>
where
    S: AlarmSource,
    F: EpisodeFsm,
    C: Clock,
    H: HolidayLookup,
{
    /// Attach a holiday lookup to this scheduler (slice 6), replacing any
    /// previous lookup. Consumes the scheduler and returns one parameterised
    /// over the new holiday type.
    pub fn with_holidays<H2: HolidayLookup>(mut self, holidays: H2) -> Scheduler<S, F, C, H2> {
        // We re-construct rather than mutate so the type parameter changes
        // from `H` to `H2`.
        Scheduler {
            source: self.source,
            fsm: self.fsm,
            clock: self.clock,
            booted: self.booted,
            holidays,
        }
    }

    /// Run a single scheduler tick.
    ///
    /// 1. Re-read `now` from the clock (task 1.1 / 1.4 — `Local::now()` is
    ///    re-read each tick, never a stored/armed value).
    /// 2. Ask the source for due alarms (task 1.1 — `AlarmSource::due_alarms`).
    /// 3. Enter the `scheduler_tick` span with structured fields
    ///    `alarms_evaluated` / `fired` (task 1.3).
    /// 4. For each due alarm: on the first tick, if `now > next_fire`, skip as
    ///    missed-on-boot, advance `next_fire`, log `info!` (task 1.2); otherwise
    ///    invoke the episode FSM's `fire()` and recompute `next_fire`
    ///    (task 1.1).
    pub fn tick(&mut self) -> TickReport {
        // Task 1.1 / 1.4: re-read the clock every tick.
        let now = self.clock.now();

        // Task 1.1: ask the alarm source for due alarms.
        let due = self.source.due_alarms(now);
        let alarms_evaluated = due.len();
        let mut fired = 0usize;
        let mut skipped_missed = 0usize;
        let mut skipped_holiday = 0usize;

        // Task 1.3: enter the scheduler_tick span with structured fields. The
        // fields are recorded with their final values once the body is done so
        // journald shows the counts for this tick.
        let span = info_span!(
            "scheduler_tick",
            alarms_evaluated,
            fired,
            skipped_missed,
            skipped_holiday,
        );
        let _guard = span.enter();

        for alarm in &due {
            // Task 1.2: missed-alarm-on-boot — `now > next_fire` on the first
            // tick means the device was off across the fire time. Do NOT fire;
            // advance next_fire to the next occurrence after `now` and log.
            let missed = !self.booted && alarm.next_fire < now;
            if missed {
                info!(
                    alarm_id = alarm.id,
                    next_fire = %alarm.next_fire,
                    now = %now,
                    "missed alarm on boot — skipping, advancing next_fire",
                );
                self.source.recompute_next_fire(alarm.id.clone(), now);
                skipped_missed += 1;
            } else if alarm.next_fire <= now {
                // Slice 6: holiday suppression. If the alarm's policy is not
                // `Ignore` and a holiday is active on the alarm's fire date,
                // the alarm does not fire on that date.
                let fire_date = alarm.next_fire.date_naive();
                if alarm.policy != HolidayPolicy::Ignore && self.holidays.is_holiday(fire_date) {
                    self.suppress_alarm(alarm, now);
                    skipped_holiday += 1;
                } else {
                    // Task 1.1: fire the episode FSM and recompute next_fire.
                    self.fsm.fire(alarm.id.clone());
                    self.source.recompute_next_fire(alarm.id.clone(), now);
                    fired += 1;
                }
            }
            // Else: the source returned a not-yet-due alarm (next_fire > now).
            // It does not fire and is not recomputed this tick.
        }

        // After the first tick the device is considered booted; subsequent due
        // alarms fire normally.
        self.booted = true;

        // Slice 2 / D5: advance escalation and re-fire snoozed episodes on
        // every tick. This does not block — the FSM issues fire-and-forget
        // Mopidy commands.
        self.fsm.on_tick(now);

        // Record the final counts onto the span (task 1.3 structured fields).
        span.record("fired", fired);
        span.record("skipped_missed", skipped_missed);
        span.record("skipped_holiday", skipped_holiday);

        // Emit an event under the span so the span + fields appear in
        // journald/fmt output for every tick (task 1.3).
        info!(alarms_evaluated, fired, skipped_missed, skipped_holiday, "scheduler tick");

        TickReport {
            alarms_evaluated,
            fired,
            skipped_missed,
            skipped_holiday,
        }
    }

    /// Slice 6: suppress a due alarm whose fire date is a holiday.
    ///
    /// - `Suppress`: recompute `next_fire` from the rule after `now` (advances
    ///   to the next scheduled occurrence — the alarm does not fire today and
    ///   resumes its normal schedule).
    /// - `ShiftForward`: advance `next_fire` to the first non-holiday date at
    ///   the same scheduled wall-clock time, repeating the skip until a
    ///   non-holiday date is found. Capped at 30 days; past the cap falls back
    ///   to `Suppress` behavior and logs.
    fn suppress_alarm(&mut self, alarm: &DueAlarm, now: DateTime<Local>) {
        match alarm.policy {
            HolidayPolicy::Suppress => {
                info!(
                    alarm_id = %alarm.id,
                    fire_date = %alarm.next_fire.date_naive(),
                    policy = "Suppress",
                    "holiday active — alarm suppressed, advancing next_fire",
                );
                self.source.recompute_next_fire(alarm.id.clone(), now);
            }
            HolidayPolicy::ShiftForward => {
                // Resolve the alarm's stored timezone once.
                let tz: Tz = alarm.timezone.parse().unwrap_or(chrono_tz::Canada::Mountain);
                let wall_time = alarm.next_fire.time();
                let fire_date = alarm.next_fire.date_naive();

                // Advance day-by-day from the day after the fire date until a
                // non-holiday date is found, capped at 30 days.
                let mut target_date = fire_date;
                let mut found = false;
                for _ in 0..30 {
                    target_date = target_date.succ_opt().unwrap_or(target_date);
                    if !self.holidays.is_holiday(target_date) {
                        found = true;
                        break;
                    }
                }

                if found {
                    // Same scheduled wall-clock time on the first non-holiday
                    // date, interpreted in the alarm's stored timezone.
                    let target_local = match tz
                        .from_local_datetime(
                            &target_date.and_time(wall_time),
                        )
                        .single()
                    {
                        Some(dt) => dt.with_timezone(&Local),
                        None => {
                            // Ambiguous/nonexistent local time (DST fold);
                            // fall back to Suppress.
                            info!(
                                alarm_id = %alarm.id,
                                policy = "ShiftForward",
                                "ambiguous local time on target date — falling back to Suppress",
                            );
                            self.source.recompute_next_fire(alarm.id.clone(), now);
                            return;
                        }
                    };
                    info!(
                        alarm_id = %alarm.id,
                        fire_date = %fire_date,
                        target_date = %target_date,
                        policy = "ShiftForward",
                        "holiday active — alarm shifted forward to next non-holiday",
                    );
                    self.source.set_next_fire(alarm.id.clone(), target_local, now);
                } else {
                    // 30-day cap exceeded — fall back to Suppress.
                    info!(
                        alarm_id = %alarm.id,
                        fire_date = %fire_date,
                        policy = "ShiftForward",
                        "holiday run exceeds 30-day cap — falling back to Suppress",
                    );
                    self.source.recompute_next_fire(alarm.id.clone(), now);
                }
            }
            HolidayPolicy::Ignore => {
                // Unreachable: the caller only enters suppression when
                // policy != Ignore. Defensive no-op.
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use std::sync::{Arc, Mutex};
    use tracing::Subscriber;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry::LookupSpan;
    use tracing_subscriber::util::SubscriberInitExt;

    /// Mutable mock clock — the tick must re-read it every tick (task 1.4).
    #[derive(Default)]
    struct MockClock {
        now: DateTime<Local>,
    }
    impl MockClock {
        fn set(&mut self, t: DateTime<Local>) {
            self.now = t;
        }
    }
    impl Clock for MockClock {
        fn now(&self) -> DateTime<Local> {
            self.now
        }
    }

    /// Mock alarm source modelling the real store: `due_alarms` filters by
    /// `next_fire <= now`, and `recompute_next_fire` advances the alarm's
    /// `next_fire` to `now + 1 day` (a stand-in for "next occurrence").
    /// `set_next_fire` (slice 6) records the explicit target and sets
    /// `next_fire` to it (for ShiftForward tests).
    #[derive(Default)]
    struct MockSource {
        alarms: Vec<DueAlarm>,
        recomputed: Vec<(AlarmId, DateTime<Local>)>,
        set_fires: Vec<(AlarmId, DateTime<Local>)>,
        nows: Vec<DateTime<Local>>,
    }
    impl AlarmSource for MockSource {
        fn due_alarms(&mut self, now: DateTime<Local>) -> Vec<DueAlarm> {
            self.nows.push(now);
            self.alarms.iter().filter(|a| a.next_fire <= now).cloned().collect()
        }
        fn recompute_next_fire(&mut self, id: AlarmId, now: DateTime<Local>) {
            self.recomputed.push((id.clone(), now));
            if let Some(a) = self.alarms.iter_mut().find(|a| a.id == id) {
                a.next_fire = now + Duration::days(1);
            }
        }
        fn set_next_fire(&mut self, id: AlarmId, target: DateTime<Local>, _now: DateTime<Local>) {
            self.set_fires.push((id.clone(), target));
            if let Some(a) = self.alarms.iter_mut().find(|a| a.id == id) {
                a.next_fire = target;
            }
        }
    }

    /// Build a `DueAlarm` with default policy (`Suppress`) and a fixed
    /// timezone, for the pre-slice-6 tests that don't exercise holiday logic.
    fn due(id: &str, next_fire: DateTime<Local>) -> DueAlarm {
        DueAlarm {
            id: id.to_string(),
            next_fire,
            policy: HolidayPolicy::default(),
            timezone: "America/Edmonton".to_string(),
        }
    }

    /// Mock episode FSM that records the ids it fired.
    #[derive(Default)]
    struct MockFsm {
        fired: Vec<AlarmId>,
    }
    impl EpisodeFsm for MockFsm {
        fn fire(&mut self, alarm_id: AlarmId) {
            self.fired.push(alarm_id);
        }
    }

    /// Mock episode FSM that records `on_tick` calls (slice 2 / D5).
    #[derive(Default)]
    struct MockFsmTick {
        tick_count: u32,
        last_now: Option<DateTime<Local>>,
    }
    impl EpisodeFsm for MockFsmTick {
        fn fire(&mut self, _alarm_id: AlarmId) {}
        fn on_tick(&mut self, now: DateTime<Local>) {
            self.tick_count += 1;
            self.last_now = Some(now);
        }
    }

    fn t(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(year, month, day, hour, min, 0).unwrap()
    }

    // ── Task 1.4: a due alarm fires ─────────────────────────────────────

    /// Scenario: an enabled alarm's next-fire is at `Local::now()` on a tick →
    /// the episode FSM is invoked and next_fire is recomputed (advanced).
    #[test]
    fn due_alarm_fires() {
        let fire = t(2026, 6, 29, 7, 0);
        let mut clock = MockClock::default();
        clock.set(fire);
        let mut source = MockSource::default();
        source.alarms.push(due("1", fire));
        let fsm = MockFsm::default();

        let mut sched = Scheduler::new(source, fsm, clock);

        // First tick, now == next_fire: not missed (now > next_fire is false),
        // so it fires.
        let report = sched.tick();
        assert_eq!(report.alarms_evaluated, 1);
        assert_eq!(report.fired, 1);
        assert_eq!(report.skipped_missed, 0);

        let Scheduler { source, fsm, clock: _, booted, .. } = sched;
        assert_eq!(fsm.fired, vec!["1".to_string()], "FSM should have fired alarm 1");
        assert_eq!(
            source.recomputed, vec![("1".to_string(), fire)],
            "next_fire should have been recomputed for alarm 1",
        );
        assert!(booted, "scheduler should be marked booted after first tick");
        // next_fire advanced to the day after the fire time.
        assert_eq!(
            source.alarms[0].next_fire,
            fire + Duration::days(1),
            "next_fire should advance to the next occurrence",
        );
    }

    // ── Task 1.4: a not-yet-due alarm does NOT fire ──────────────────────

    /// Scenario: an alarm whose next-fire is in the future does not fire and
    /// is not recomputed on this tick.
    #[test]
    fn not_yet_due_alarm_does_not_fire() {
        let now = t(2026, 6, 29, 7, 0);
        let future = now + Duration::hours(2);
        let mut clock = MockClock::default();
        clock.set(now);
        let mut source = MockSource::default();
        source.alarms.push(due("2", future));
        let fsm = MockFsm::default();

        let mut sched = Scheduler::new(source, fsm, clock);
        let report = sched.tick();

        // The store filters out not-yet-due alarms, so nothing fires.
        assert_eq!(report.alarms_evaluated, 0, "no alarms should be due");
        assert_eq!(report.fired, 0, "nothing should fire");
        assert_eq!(report.skipped_missed, 0);

        let Scheduler { source, fsm, clock: _, booted: _, .. } = sched;
        assert!(fsm.fired.is_empty(), "FSM should not have fired");
        assert!(
            source.recomputed.is_empty(),
            "next_fire should not be recomputed for a not-due alarm",
        );
        // next_fire unchanged.
        assert_eq!(source.alarms[0].next_fire, future);
    }

    // ── Task 1.4: Local::now() is re-read each tick ──────────────────────

    /// Scenario: the scheduler consults the clock on every tick. A tick before
    /// the fire time fires nothing; advancing the (mock) clock to the fire
    /// time makes the next tick fire — proving the clock is re-read, not stored.
    #[test]
    fn clock_is_reread_each_tick() {
        let fire = t(2026, 6, 29, 7, 0);
        let before = fire - Duration::minutes(1);
        let mut clock = MockClock::default();
        clock.set(before);
        let mut source = MockSource::default();
        source.alarms.push(due("3", fire));
        let fsm = MockFsm::default();

        let mut sched = Scheduler::new(source, fsm, clock);

        // Tick 1 at `before`: not due → nothing fires.
        let r1 = sched.tick();
        assert_eq!(r1.fired, 0, "tick before fire time should not fire");

        // Advance the mock clock to the fire time and tick again.
        let Scheduler { source, fsm, mut clock, booted, .. } = sched;
        clock.set(fire);
        // Re-bind to keep ticking the same scheduler instance.
        let mut sched = Scheduler { source, fsm, clock, booted, holidays: NoHolidays };
        let r2 = sched.tick();
        assert_eq!(r2.fired, 1, "tick at fire time should fire");

        let Scheduler { source, fsm, clock: _, booted: _, .. } = sched;
        // The clock was consulted twice (once per tick), proving re-read.
        assert_eq!(
            source.nows, vec![before, fire],
            "clock should be re-read on each tick",
        );
        assert_eq!(fsm.fired, vec!["3".to_string()]);
    }

    // ── Task 1.2: missed-alarm-on-boot is skipped ────────────────────────

    /// Scenario: device boots after an alarm's fire time (now > next_fire on
    /// the first tick). The alarm does NOT fire; next_fire is advanced past
    /// `now` and an info! skip is recorded. A second tick does not re-fire it.
    #[test]
    fn missed_alarm_on_boot_is_skipped() {
        let past = t(2026, 6, 29, 3, 0);
        let now = t(2026, 6, 29, 9, 0); // 6h after the fire time
        let mut clock = MockClock::default();
        clock.set(now);
        let mut source = MockSource::default();
        source.alarms.push(due("7", past));
        let fsm = MockFsm::default();

        let mut sched = Scheduler::new(source, fsm, clock);

        // First tick: boot catch-up → skipped, not fired.
        let r1 = sched.tick();
        assert_eq!(r1.alarms_evaluated, 1);
        assert_eq!(r1.fired, 0, "missed alarm must not fire");
        assert_eq!(r1.skipped_missed, 1);

        let Scheduler { source, fsm, mut clock, booted, .. } = sched;
        assert!(fsm.fired.is_empty(), "FSM must not fire a missed-on-boot alarm");
        assert_eq!(
            source.recomputed, vec![("7".to_string(), now)],
            "next_fire should be recomputed (advanced) for the skipped alarm",
        );
        assert_eq!(
            source.alarms[0].next_fire,
            now + Duration::days(1),
            "next_fire should advance to the next occurrence after now",
        );
        assert!(booted);

        // Second tick at the same `now`: the alarm's next_fire is now in the
        // future, so the store does not surface it and it does not re-fire.
        clock.set(now);
        let mut sched = Scheduler { source, fsm, clock, booted, holidays: NoHolidays };
        let r2 = sched.tick();
        assert_eq!(r2.fired, 0, "second tick should not re-fire the missed alarm");
        assert_eq!(r2.skipped_missed, 0);

        let Scheduler { source, fsm, clock: _, booted: _, .. } = sched;
        assert!(fsm.fired.is_empty());
        // Only one recompute (from the first-tick skip) ever happened.
        assert_eq!(source.recomputed.len(), 1);
    }

    /// Scenario: a normally-fired alarm at boot equality (now == next_fire) is
    /// NOT treated as missed — it fires (the policy triggers on `now > next_fire`).
    #[test]
    fn boot_equality_fires_not_missed() {
        let fire = t(2026, 6, 29, 7, 30);
        let mut clock = MockClock::default();
        clock.set(fire);
        let mut source = MockSource::default();
        source.alarms.push(due("9", fire));
        let fsm = MockFsm::default();

        let mut sched = Scheduler::new(source, fsm, clock);
        let r = sched.tick();
        assert_eq!(r.fired, 1, "now == next_fire at boot should fire, not skip");
        assert_eq!(r.skipped_missed, 0);

        let Scheduler { source: _, fsm, clock: _, booted: _, .. } = sched;
        assert_eq!(fsm.fired, vec!["9".to_string()]);
    }

    // ── Task 1.3: scheduler_tick span emits with structured fields ──────

    /// `tracing::field::Visit` that collects `(name, debug_value)` pairs,
    /// overwriting the value if the field was already recorded (so a later
    /// `span.record(...)` update wins over the span's initial value).
    struct FieldCollector<'a>(&'a mut Vec<(String, String)>);
    impl<'a> tracing::field::Visit for FieldCollector<'a> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            let name = field.name().to_string();
            let val = format!("{:?}", value);
            if let Some(existing) = self.0.iter_mut().find(|(k, _)| *k == name) {
                existing.1 = val;
            } else {
                self.0.push((name, val));
            }
        }
    }

    /// Raise the global `MAX_LEVEL` gate (which defaults to `OFF`) so the
    /// `info_span!`/`info!` calls inside `Scheduler::tick` are not
    /// short-circuited before any dispatcher is consulted.
    ///
    /// Per-thread `set_default` does *not* raise this global gate; only a
    /// global default subscriber does. `try_init` sets one (raising
    /// `MAX_LEVEL` to `TRACE`); it succeeds exactly once per process and
    /// returns `Err` on subsequent calls, which we ignore. Without this, the
    /// capturing tests flake under parallelism: a tick whose `info_span!`
    /// runs before any global subscriber is installed produces a disabled
    /// span and the layer sees nothing.
    fn ensure_global_max_level() {
        let _ = tracing_subscriber::registry().try_init();
    }

    /// Capturing layer: records each span's name + recorded fields, the names
    /// of spans entered, and a count of events observed — so a tick can be
    /// asserted to emit the `scheduler_tick` span with structured fields.
    #[derive(Default)]
    struct CaptureLayer {
        /// span id (as u64) → (name, fields)
        spans: Arc<Mutex<HashMap<u64, (String, Vec<(String, String)>)>>>,
        entered: Arc<Mutex<Vec<String>>>,
        events: Arc<Mutex<usize>>,
    }

    impl<S> Layer<S> for CaptureLayer
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            id: &tracing::span::Id,
            _ctx: Context<'_, S>,
        ) {
            let mut fields = Vec::new();
            attrs.record(&mut FieldCollector(&mut fields));
            self.spans
                .lock()
                .unwrap()
                .insert(id.into_u64(), (attrs.metadata().name().to_string(), fields));
        }

        fn on_record(
            &self,
            id: &tracing::span::Id,
            values: &tracing::span::Record<'_>,
            _ctx: Context<'_, S>,
        ) {
            if let Some((_, slot)) = self.spans.lock().unwrap().get_mut(&id.into_u64()) {
                values.record(&mut FieldCollector(slot));
            }
        }

        fn on_enter(&self, id: &tracing::span::Id, ctx: Context<'_, S>) {
            if let Some(meta) = ctx.metadata(id) {
                self.entered.lock().unwrap().push(meta.name().to_string());
            }
        }

        fn on_event(&self, _event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            *self.events.lock().unwrap() += 1;
        }
    }

    /// Scenario: a scheduler tick enters the `scheduler_tick` span and records
    /// the structured fields `alarms_evaluated` / `fired` on it; an event is
    /// emitted under the span (visible to journald/fmt).
    #[test]
    fn tick_emits_scheduler_tick_span_with_fields() {
        let layer = CaptureLayer::default();
        let spans = Arc::clone(&layer.spans);
        let entered = Arc::clone(&layer.entered);
        let events = Arc::clone(&layer.events);

        // Install the capturing subscriber thread-locally for this test.
        // First raise the global max-level gate (see `ensure_global_max_level`).
        ensure_global_max_level();
        let _guard = tracing_subscriber::registry().with(layer).set_default();

        let fire = t(2026, 6, 29, 8, 0);
        let mut clock = MockClock::default();
        clock.set(fire);
        let mut source = MockSource::default();
        source.alarms.push(due("11", fire));
        let fsm = MockFsm::default();
        let mut sched = Scheduler::new(source, fsm, clock);

        let report = sched.tick();
        assert_eq!(report.fired, 1);

        // The scheduler_tick span was entered.
        let entered = entered.lock().unwrap();
        assert!(
            entered.iter().any(|n| n == "scheduler_tick"),
            "scheduler_tick span should be entered on a tick (entered={:?})",
            entered,
        );
        drop(entered);

        // An event was emitted under the span (so it shows up in journald/logs).
        assert!(
            *events.lock().unwrap() >= 1,
            "the tick should emit at least one tracing event",
        );

        // The scheduler_tick span carries the structured fields with the right
        // values for this tick (alarms_evaluated=1, fired=1).
        let spans = spans.lock().unwrap();
        let tick_span = spans
            .values()
            .find(|(name, _)| name == "scheduler_tick")
            .expect("a scheduler_tick span should have been created");
        let field = |key: &str| -> Option<String> {
            tick_span
                .1
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(field("alarms_evaluated").as_deref(), Some("1"));
        assert_eq!(field("fired").as_deref(), Some("1"));
        assert_eq!(field("skipped_missed").as_deref(), Some("0"));
    }

    /// Scenario: a tick with no due alarms still enters the span and records
    /// zeroed fields (so journald always shows a scheduler_tick per tick).
    #[test]
    fn tick_with_no_alarms_still_emits_span() {
        let layer = CaptureLayer::default();
        let spans = Arc::clone(&layer.spans);
        let entered = Arc::clone(&layer.entered);

        ensure_global_max_level();
        let _guard = tracing_subscriber::registry().with(layer).set_default();

        let now = t(2026, 6, 29, 10, 0);
        let mut clock = MockClock::default();
        clock.set(now);
        let mut sched = Scheduler::new(
            MockSource::default(),
            MockFsm::default(),
            clock,
        );
        let report = sched.tick();
        assert_eq!(report.alarms_evaluated, 0);
        assert_eq!(report.fired, 0);

        assert!(
            entered.lock().unwrap().iter().any(|n| n == "scheduler_tick"),
            "scheduler_tick span should be entered even with no due alarms",
        );
        let spans = spans.lock().unwrap();
        let tick_span = spans
            .values()
            .find(|(name, _)| name == "scheduler_tick")
            .expect("scheduler_tick span should exist");
        let field = |key: &str| -> Option<String> {
            tick_span
                .1
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(field("alarms_evaluated").as_deref(), Some("0"));
        assert_eq!(field("fired").as_deref(), Some("0"));
    }

    // ── Slice 2 / D5: scheduler tick calls on_tick ───────────────────────

    /// Scenario: a scheduler tick invokes `on_tick(now)` on the episode FSM
    /// exactly once per tick, passing the re-read clock value.
    #[test]
    fn tick_invokes_on_tick_on_fsm() {
        let now = t(2026, 6, 29, 10, 0);
        let mut clock = MockClock::default();
        clock.set(now);
        let fsm = MockFsmTick::default();

        let mut sched = Scheduler::new(MockSource::default(), fsm, clock);
        sched.tick();

        let Scheduler { source: _, fsm, clock: _, booted: _, .. } = sched;
        assert_eq!(fsm.tick_count, 1, "on_tick called once per tick");
        assert_eq!(fsm.last_now, Some(now), "on_tick received the re-read now");
    }

    /// Scenario: a scheduler tick against `NoopEpisodeFsm` (default no-op
    /// `on_tick`) compiles and is a no-op — no panic, no commands.
    #[test]
    fn tick_with_noop_fsm_on_tick_is_noop() {
        let now = t(2026, 6, 29, 10, 0);
        let mut clock = MockClock::default();
        clock.set(now);
        let mut sched = Scheduler::new(
            MockSource::default(),
            NoopEpisodeFsm,
            clock,
        );
        // No panic:
        sched.tick();
        assert!(true, "noop on_tick did not panic");
    }

    // ── Slice 6: holiday suppression ─────────────────────────────────────

    /// Mock holiday lookup backed by a set of holiday dates.
    #[derive(Default)]
    struct MockHolidays {
        dates: std::collections::HashSet<chrono::NaiveDate>,
    }
    impl HolidayLookup for MockHolidays {
        fn is_holiday(&self, date: chrono::NaiveDate) -> bool {
            self.dates.contains(&date)
        }
    }

    /// Build a `DueAlarm` with an explicit holiday policy.
    fn due_pol(id: &str, next_fire: DateTime<Local>, policy: HolidayPolicy) -> DueAlarm {
        DueAlarm {
            id: id.to_string(),
            next_fire,
            policy,
            timezone: "America/Edmonton".to_string(),
        }
    }

    /// Scenario: a daily alarm with `Suppress` on a statutory holiday does
    /// not fire; `next_fire` is recomputed (advanced) and the skip is logged.
    #[test]
    fn suppress_policy_skips_on_holiday() {
        let fire = t(2026, 7, 1, 7, 0); // Canada Day — a holiday
        let mut clock = MockClock::default();
        clock.set(fire);
        let mut source = MockSource::default();
        source.alarms.push(due_pol("h1", fire, HolidayPolicy::Suppress));
        let fsm = MockFsm::default();

        let mut holidays = MockHolidays::default();
        holidays.dates.insert(chrono::NaiveDate::from_ymd_opt(2026, 7, 1).unwrap());

        let mut sched = Scheduler::new(source, fsm, clock).with_holidays(holidays);
        let report = sched.tick();

        assert_eq!(report.fired, 0, "Suppress alarm must not fire on a holiday");
        assert_eq!(report.skipped_holiday, 1);
        assert_eq!(report.skipped_missed, 0);

        let Scheduler { source, fsm, clock: _, booted: _, holidays: _ } = sched;
        assert!(fsm.fired.is_empty(), "FSM must not have fired");
        assert_eq!(
            source.recomputed.len(), 1,
            "Suppress advances next_fire via recompute",
        );
        // MockSource advances by +1 day → next_fire is the day after the
        // holiday (the next scheduled occurrence).
        assert_eq!(source.alarms[0].next_fire, fire + Duration::days(1));
    }

    /// Scenario: a daily alarm with `Ignore` fires normally on a holiday.
    #[test]
    fn ignore_policy_fires_on_holiday() {
        let fire = t(2026, 7, 1, 7, 0);
        let mut clock = MockClock::default();
        clock.set(fire);
        let mut source = MockSource::default();
        source.alarms.push(due_pol("h2", fire, HolidayPolicy::Ignore));
        let fsm = MockFsm::default();

        let mut holidays = MockHolidays::default();
        holidays.dates.insert(chrono::NaiveDate::from_ymd_opt(2026, 7, 1).unwrap());

        let mut sched = Scheduler::new(source, fsm, clock).with_holidays(holidays);
        let report = sched.tick();

        assert_eq!(report.fired, 1, "Ignore alarm fires on a holiday");
        assert_eq!(report.skipped_holiday, 0);

        let Scheduler { source, fsm, clock: _, booted: _, holidays: _ } = sched;
        assert_eq!(fsm.fired, vec!["h2".to_string()]);
        assert_eq!(source.recomputed.len(), 1);
    }

    /// Scenario: a daily alarm with `Suppress` on a non-holiday fires normally.
    #[test]
    fn suppress_policy_fires_on_non_holiday() {
        let fire = t(2026, 7, 2, 7, 0); // not a holiday
        let mut clock = MockClock::default();
        clock.set(fire);
        let mut source = MockSource::default();
        source.alarms.push(due_pol("h3", fire, HolidayPolicy::Suppress));
        let fsm = MockFsm::default();

        let holidays = MockHolidays::default(); // no holidays

        let mut sched = Scheduler::new(source, fsm, clock).with_holidays(holidays);
        let report = sched.tick();

        assert_eq!(report.fired, 1, "Suppress alarm fires on a non-holiday");
        assert_eq!(report.skipped_holiday, 0);

        let Scheduler { source, fsm, clock: _, booted: _, holidays: _ } = sched;
        assert_eq!(fsm.fired, vec!["h3".to_string()]);
    }

    /// Scenario: a personal all-day event (modeled as a holiday date) suppresses
    /// a `Suppress`-policy alarm — the same as a statutory holiday. (This tests
    /// that holiday membership is date-based regardless of source.)
    #[test]
    fn personal_all_day_event_is_a_holiday() {
        let fire = t(2026, 7, 15, 7, 0); // personal vacation day
        let mut clock = MockClock::default();
        clock.set(fire);
        let mut source = MockSource::default();
        source.alarms.push(due_pol("h4", fire, HolidayPolicy::Suppress));
        let fsm = MockFsm::default();

        let mut holidays = MockHolidays::default();
        holidays.dates.insert(chrono::NaiveDate::from_ymd_opt(2026, 7, 15).unwrap());

        let mut sched = Scheduler::new(source, fsm, clock).with_holidays(holidays);
        let report = sched.tick();

        assert_eq!(report.fired, 0);
        assert_eq!(report.skipped_holiday, 1);
    }

    /// Scenario: `ShiftForward` skips a 3-day holiday weekend to the first
    /// non-holiday day at the same wall-clock time. `next_fire` is set to that
    /// date via `set_next_fire` (not recomputed from the rule).
    #[test]
    fn shift_forward_skips_multi_day_holiday() {
        // Fire on the first day of a 3-day holiday weekend (Jul 1–3).
        let fire = t(2026, 7, 1, 7, 0);
        let mut clock = MockClock::default();
        clock.set(fire);
        let mut source = MockSource::default();
        source.alarms.push(due_pol("h5", fire, HolidayPolicy::ShiftForward));
        let fsm = MockFsm::default();

        let mut holidays = MockHolidays::default();
        holidays.dates.insert(chrono::NaiveDate::from_ymd_opt(2026, 7, 1).unwrap());
        holidays.dates.insert(chrono::NaiveDate::from_ymd_opt(2026, 7, 2).unwrap());
        holidays.dates.insert(chrono::NaiveDate::from_ymd_opt(2026, 7, 3).unwrap());

        let mut sched = Scheduler::new(source, fsm, clock).with_holidays(holidays);
        let report = sched.tick();

        assert_eq!(report.fired, 0, "ShiftForward alarm must not fire on holiday days");
        assert_eq!(report.skipped_holiday, 1);

        let Scheduler { source, fsm, clock: _, booted: _, holidays: _ } = sched;
        assert!(fsm.fired.is_empty());
        // The source was asked to set next_fire to a specific target date (not
        // recomputed from the rule), and no recompute happened.
        assert_eq!(source.set_fires.len(), 1, "ShiftForward uses set_next_fire");
        assert!(source.recomputed.is_empty(), "ShiftForward does not recompute");

        let (id, target) = &source.set_fires[0];
        assert_eq!(id, "h5");
        // Target is the first non-holiday (Jul 4) at the same wall-clock time.
        assert_eq!(target.date_naive(), chrono::NaiveDate::from_ymd_opt(2026, 7, 4).unwrap());
        assert_eq!(target.time(), fire.time());

        // The mock source set the alarm's next_fire to the target.
        assert_eq!(source.alarms[0].next_fire, *target);
    }

    /// Scenario: `ShiftForward` with a holiday run longer than 30 days falls
    /// back to `Suppress` behavior — it recomputes next_fire from the rule
    /// instead of looping forever.
    #[test]
    fn shift_forward_cap_falls_back_to_suppress() {
        let fire = t(2026, 7, 1, 7, 0);
        let mut clock = MockClock::default();
        clock.set(fire);
        let mut source = MockSource::default();
        source.alarms.push(due_pol("h6", fire, HolidayPolicy::ShiftForward));
        let fsm = MockFsm::default();

        // 31 consecutive holidays starting Jul 1.
        let mut holidays = MockHolidays::default();
        for i in 0..31 {
            holidays.dates.insert(
                chrono::NaiveDate::from_ymd_opt(2026, 7, 1).unwrap() + Duration::days(i),
            );
        }

        let mut sched = Scheduler::new(source, fsm, clock).with_holidays(holidays);
        let report = sched.tick();

        assert_eq!(report.fired, 0);
        assert_eq!(report.skipped_holiday, 1);

        let Scheduler { source, fsm, clock: _, booted: _, holidays: _ } = sched;
        // Past the 30-day cap → Suppress fallback → recompute, no set_next_fire.
        assert!(source.set_fires.is_empty(), "cap exceeded → Suppress fallback, no set_next_fire");
        assert_eq!(source.recomputed.len(), 1, "cap exceeded → recompute (Suppress)");
        // MockSource advances by +1 day.
        assert_eq!(source.alarms[0].next_fire, fire + Duration::days(1));
    }

    /// Scenario: no `HolidayLookup` attached → the scheduler behaves as before
    /// slice 6 (a `Suppress` alarm fires even on a date that *would* be a
    /// holiday, because nothing checks). This guards backward compatibility.
    #[test]
    fn no_holiday_lookup_means_normal_fire() {
        let fire = t(2026, 7, 1, 7, 0);
        let mut clock = MockClock::default();
        clock.set(fire);
        let mut source = MockSource::default();
        source.alarms.push(due_pol("h7", fire, HolidayPolicy::Suppress));
        let fsm = MockFsm::default();

        // No with_holidays → NoHolidays (default).
        let mut sched = Scheduler::new(source, fsm, clock);
        let report = sched.tick();

        assert_eq!(report.fired, 1);
        assert_eq!(report.skipped_holiday, 0);
    }
}

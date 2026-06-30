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

use chrono::{DateTime, Local};
use tracing::{info, info_span};

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
/// to the store.
#[derive(Debug, Clone)]
pub struct DueAlarm {
    pub id: AlarmId,
    /// Stored next-fire time, in local time.
    pub next_fire: DateTime<Local>,
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
}

/// Episode-FSM seam (filled by the real `EpisodeController` in group 5).
///
/// `fire(alarm_id)` captures the Mopidy snapshot and starts playback. It does
/// not block the tick — the reply drain corrects FSM state on failure (design).
pub trait EpisodeFsm {
    fn fire(&mut self, alarm_id: AlarmId);
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
}

/// The scheduler: owns the alarm source, episode FSM, a clock, and a `booted`
/// flag (false until the first tick completes, driving the missed-alarm-on-boot
/// policy of task 1.2).
pub struct Scheduler<S, F, C> {
    source: S,
    fsm: F,
    clock: C,
    booted: bool,
}

impl<S, F, C> Scheduler<S, F, C>
where
    S: AlarmSource,
    F: EpisodeFsm,
    C: Clock,
{
    /// Construct a scheduler around its three seams. `booted` starts `false`.
    pub fn new(source: S, fsm: F, clock: C) -> Self {
        Self {
            source,
            fsm,
            clock,
            booted: false,
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

        // Task 1.3: enter the scheduler_tick span with structured fields. The
        // fields are recorded with their final values once the body is done so
        // journald shows the counts for this tick.
        let span = info_span!(
            "scheduler_tick",
            alarms_evaluated,
            fired,
            skipped_missed,
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
                // Task 1.1: fire the episode FSM and recompute next_fire.
                self.fsm.fire(alarm.id.clone());
                self.source.recompute_next_fire(alarm.id.clone(), now);
                fired += 1;
            }
            // Else: the source returned a not-yet-due alarm (next_fire > now).
            // It does not fire and is not recomputed this tick.
        }

        // After the first tick the device is considered booted; subsequent due
        // alarms fire normally.
        self.booted = true;

        // Record the final counts onto the span (task 1.3 structured fields).
        span.record("fired", fired);
        span.record("skipped_missed", skipped_missed);

        // Emit an event under the span so the span + fields appear in
        // journald/fmt output for every tick (task 1.3).
        info!(alarms_evaluated, fired, skipped_missed, "scheduler tick");

        TickReport {
            alarms_evaluated,
            fired,
            skipped_missed,
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
    #[derive(Default)]
    struct MockSource {
        alarms: Vec<DueAlarm>,
        recomputed: Vec<(AlarmId, DateTime<Local>)>,
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
        source.alarms.push(DueAlarm { id: "1".to_string(), next_fire: fire });
        let fsm = MockFsm::default();

        let mut sched = Scheduler::new(source, fsm, clock);

        // First tick, now == next_fire: not missed (now > next_fire is false),
        // so it fires.
        let report = sched.tick();
        assert_eq!(report.alarms_evaluated, 1);
        assert_eq!(report.fired, 1);
        assert_eq!(report.skipped_missed, 0);

        let Scheduler { source, fsm, clock: _, booted } = sched;
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
        source.alarms.push(DueAlarm { id: "2".to_string(), next_fire: future });
        let fsm = MockFsm::default();

        let mut sched = Scheduler::new(source, fsm, clock);
        let report = sched.tick();

        // The store filters out not-yet-due alarms, so nothing fires.
        assert_eq!(report.alarms_evaluated, 0, "no alarms should be due");
        assert_eq!(report.fired, 0, "nothing should fire");
        assert_eq!(report.skipped_missed, 0);

        let Scheduler { source, fsm, clock: _, booted: _ } = sched;
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
        source.alarms.push(DueAlarm { id: "3".to_string(), next_fire: fire });
        let fsm = MockFsm::default();

        let mut sched = Scheduler::new(source, fsm, clock);

        // Tick 1 at `before`: not due → nothing fires.
        let r1 = sched.tick();
        assert_eq!(r1.fired, 0, "tick before fire time should not fire");

        // Advance the mock clock to the fire time and tick again.
        let Scheduler { source, fsm, mut clock, booted } = sched;
        clock.set(fire);
        // Re-bind to keep ticking the same scheduler instance.
        let mut sched = Scheduler { source, fsm, clock, booted };
        let r2 = sched.tick();
        assert_eq!(r2.fired, 1, "tick at fire time should fire");

        let Scheduler { source, fsm, clock: _, booted: _ } = sched;
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
        source.alarms.push(DueAlarm { id: "7".to_string(), next_fire: past });
        let fsm = MockFsm::default();

        let mut sched = Scheduler::new(source, fsm, clock);

        // First tick: boot catch-up → skipped, not fired.
        let r1 = sched.tick();
        assert_eq!(r1.alarms_evaluated, 1);
        assert_eq!(r1.fired, 0, "missed alarm must not fire");
        assert_eq!(r1.skipped_missed, 1);

        let Scheduler { source, fsm, mut clock, booted } = sched;
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
        let mut sched = Scheduler { source, fsm, clock, booted };
        let r2 = sched.tick();
        assert_eq!(r2.fired, 0, "second tick should not re-fire the missed alarm");
        assert_eq!(r2.skipped_missed, 0);

        let Scheduler { source, fsm, clock: _, booted: _ } = sched;
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
        source.alarms.push(DueAlarm { id: "9".to_string(), next_fire: fire });
        let fsm = MockFsm::default();

        let mut sched = Scheduler::new(source, fsm, clock);
        let r = sched.tick();
        assert_eq!(r.fired, 1, "now == next_fire at boot should fire, not skip");
        assert_eq!(r.skipped_missed, 0);

        let Scheduler { source: _, fsm, clock: _, booted: _ } = sched;
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
        source.alarms.push(DueAlarm { id: "11".to_string(), next_fire: fire });
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
}

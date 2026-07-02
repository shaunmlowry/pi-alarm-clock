## ADDED Requirements

### Requirement: Scheduler tick with recompute
The application SHALL run a scheduler tick on the main thread at a fixed interval (default 5 seconds) driven by a `slint::Timer`. On each tick, the scheduler SHALL re-derive the next fire time for each enabled alarm from `Local::now()` and the alarm's schedule, and SHALL fire any alarm whose next-fire time is at or before `now`. The `scheduler_tick` span (defined in slice 0, unused) SHALL be entered on each tick.

#### Scenario: Alarm fires at scheduled time
- **WHEN** an enabled alarm's next-fire time is at or before `Local::now()` on a scheduler tick
- **THEN** the alarm fires (the episode FSM is invoked) and the alarm's next-fire is recomputed to its next future occurrence

#### Scenario: Tick re-derives next-fire from Local::now
- **WHEN** the scheduler tick fires
- **THEN** the next-fire time is recomputed from `Local::now()` rather than from a stored/armed timer value, so clock jumps (NTP, `fake-hwclock`) are tolerated

#### Scenario: Missed alarm on boot is skipped
- **WHEN** the device was powered off across an alarm's fire time and boots after it
- **THEN** the missed alarm does NOT fire; its next-fire is advanced to the next occurrence after `now` and an `info!` log entry records the skip

### Requirement: Schedule next-fire over RRULE with timezone
The application SHALL compute an alarm's next fire time using a `Schedule` wrapper over the `rrule` crate evaluated in the alarm's stored IANA timezone (via `chrono-tz`). Times SHALL be stored as wall-clock local; next-fire SHALL be derived from the rule and `Local::now()`, not stored authoritatively. A cached `next_fire` column MAY be maintained as an optimization but the rule SHALL remain the source of truth.

#### Scenario: Daily alarm fires at same wall-clock time across DST
- **WHEN** a daily alarm is set for 07:30 in `America/Edmonton` and a DST spring-forward boundary occurs
- **THEN** the alarm fires at 07:30 local time on both sides of the boundary (not at the shifted UTC instant)

#### Scenario: Once alarm fires once then is disabled
- **WHEN** a `Once` alarm (no RRULE) fires at its stored `once_at` time
- **THEN** the alarm fires and is disabled (`enabled=0`) so it does not fire again

#### Scenario: Weekdays preset expands to correct RRULE
- **WHEN** an alarm is created with the "Weekdays" preset
- **THEN** its stored RRULE is `FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR` and it fires Monday through Friday only

#### Scenario: Timezone-stored alarm evaluates in its timezone
- **WHEN** an alarm is stored with timezone `America/Edmonton` and the device's local timezone differs
- **THEN** the alarm's next-fire is computed in `America/Edmonton` (the stored timezone), not the device timezone

### Requirement: Schedule presets
The application SHALL accept schedule presets mapping to RFC 5545 RRULE strings: `Once` (no RRULE), `Daily` (`FREQ=DAILY`), `Weekdays` (`FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR`), `Weekends` (`FREQ=WEEKLY;BYDAY=SA,SU`), `Specific-days` (`FREQ=WEEKLY;BYDAY=<selected>`). Complex RRULE (COUNT, UNTIL, BYSETPOS, INTERVAL>1) SHALL be parsed if present but SHALL NOT be constructed by slice 1 (the full builder is web-only, a later slice).

#### Scenario: Specific-days preset with Monday and Friday
- **WHEN** an alarm is created with the "Specific-days" preset and days `[Mo, Fr]`
- **THEN** its stored RRULE is `FREQ=WEEKLY;BYDAY=MO,FR` and it fires on Monday and Friday only

#### Scenario: Existing complex RRULE is parsed but not editable
- **WHEN** an alarm is seeded (dev config) with a complex RRULE like `FREQ=MONTHLY;BYDAY=2MO` (second Monday)
- **THEN** the RRULE is parsed and the alarm fires accordingly, but slice 1 provides no UI or API to create such a rule

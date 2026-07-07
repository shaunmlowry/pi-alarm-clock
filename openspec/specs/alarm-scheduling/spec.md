# alarm-scheduling Specification

## Purpose
TBD - created by archiving change slice-1-alarm-scheduling-and-episode. Update Purpose after archive.
## Requirements
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

### Requirement: Scheduler tick advances escalation and re-fires snoozed episodes
The scheduler tick SHALL, after evaluating due alarms, invoke an `on_tick(now)` hook on the episode FSM. The episode FSM SHALL use this hook to (a) advance the volume through `escalation_steps` for an `Escalating` episode and (b) re-fire a `Snoozing` episode whose `snooze_until <= now`. The hook SHALL be non-blocking (fire-and-forget Mopidy commands) and SHALL not block the Slint event loop. An `on_tick` invoked while the FSM is `Idle` or `Dismissed` SHALL be a no-op.

#### Scenario: Scheduler tick advances the volume of an Escalating episode
- **WHEN** an `Escalating` episode is at step 0 and a scheduler tick fires after the step-1 `after_secs` boundary has elapsed
- **THEN** the FSM issues `set_volume(step_1.volume)` and updates its `step_index`

#### Scenario: Scheduler tick re-fires a snoozed episode whose snooze has elapsed
- **WHEN** the FSM is `Snoozing` and a scheduler tick observes `now >= snooze_until`
- **THEN** the FSM transitions back to `Escalating`, replays the alarm source, and the alarm overlay is re-shown

#### Scenario: on_tick while Idle is a no-op
- **WHEN** the scheduler tick invokes `on_tick` while the FSM is `Idle` or `Dismissed`
- **THEN** no Mopidy commands are issued and the FSM state is unchanged

### Requirement: EpisodeFsm seam gains an on_tick hook with a no-op default
The `EpisodeFsm` trait SHALL declare `fn on_tick(&mut self, now: DateTime<Local>)` with a default no-op implementation, so the scheduler can drive escalation/snooze refire through the existing adapter without breaking no-op or mock seam implementations.

#### Scenario: NoopEpisodeFsm compiles and on_tick is a no-op
- **WHEN** the scheduler calls `on_tick` on a `NoopEpisodeFsm`
- **THEN** the call returns without error and issues no commands

### Requirement: Holiday suppression on the scheduler tick
Each alarm SHALL carry a `HolidayPolicy: Ignore | Suppress | ShiftForward` (default `Suppress`). On the scheduler tick, when an alarm is due and its `HolidayPolicy != Ignore` and a holiday is active on the alarm's fire date (a Holiday-role calendar has an all-day event that day — Google's Canada holidays or a personal all-day event), the alarm SHALL NOT fire on that date:
- `Suppress`: skip, advance `next_fire` to the next scheduled occurrence (the alarm does not fire today and resumes its normal schedule).
- `ShiftForward`: advance `next_fire` to the **next non-holiday date** at the same scheduled wall-clock time, repeating the skip until a non-holiday date is found (so a multi-day holiday weekend shifts the alarm to the first non-holiday day). Capped at 30 days; past the cap the behavior falls back to `Suppress` and logs the fallback.
- `Ignore`: fire normally regardless of holidays.
`Suppress` and `ShiftForward` both log the skip.

#### Scenario: Suppress policy skips on a holiday
- **WHEN** a daily alarm with `HolidayPolicy::Suppress` is due on a statutory holiday
- **THEN** the alarm does not fire, `next_fire` advances to the next day, and the skip is logged

#### Scenario: Ignore policy fires on a holiday
- **WHEN** a daily alarm with `HolidayPolicy::Ignore` is due on a statutory holiday
- **THEN** the alarm fires normally

#### Scenario: Personal all-day event is a holiday
- **WHEN** the user has a personal all-day event today and an alarm has `HolidayPolicy::Suppress`
- **THEN** the alarm is suppressed for that day

#### Scenario: ShiftForward skips a multi-day holiday to the first non-holiday
- **WHEN** a daily alarm with `HolidayPolicy::ShiftForward` is due on the first day of a 3-day holiday weekend
- **THEN** the alarm does not fire that day nor the next two holiday days; `next_fire` advances to the first non-holiday day at the same scheduled wall-clock time, and the skips are logged

#### Scenario: ShiftForward cap falls back to Suppress past 30 days
- **WHEN** a `ShiftForward` alarm is due on a holiday and the holiday run is longer than 30 consecutive days
- **THEN** the alarm does not loop indefinitely; the behavior falls back to `Suppress` (advance to the next scheduled occurrence) and the fallback is logged

#### Scenario: No holiday means normal fire
- **WHEN** a daily alarm with `HolidayPolicy::Suppress` is due on a non-holiday
- **THEN** the alarm fires normally

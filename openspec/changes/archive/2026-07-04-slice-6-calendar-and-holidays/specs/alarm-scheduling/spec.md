## ADDED Requirements

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

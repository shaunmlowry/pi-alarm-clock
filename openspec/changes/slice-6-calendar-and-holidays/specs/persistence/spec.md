## ADDED Requirements

### Requirement: Per-alarm holiday_policy and calendars table persisted
The application SHALL persist a per-alarm `holiday_policy` (default `Suppress`) and a `calendars` table listing configured `CalendarSource` rows (`google_calendar_id`, `display_name`, `role`). Migration `v6` SHALL add the `holiday_policy` column and the `calendars` table, bumping `user_version` to `6`, in a single transaction, non-destructively (existing alarms get `Suppress`; no calendars configured until paired).

#### Scenario: holiday_policy round-trips
- **WHEN** an alarm with `HolidayPolicy::Ignore` is upserted and read back
- **THEN** the policy is preserved

#### Scenario: Fresh database migrates to v6
- **WHEN** a fresh database is opened and migrations run
- **THEN** `user_version` is `6`, `alarms` has a `holiday_policy` column (default Suppress), and a `calendars` table exists

#### Scenario: v5 database upgrades to v6
- **WHEN** a database at `user_version = 5` is migrated
- **THEN** all alarm rows survive with `holiday_policy = Suppress`, the `calendars` table is created (empty), and `user_version` becomes `6`

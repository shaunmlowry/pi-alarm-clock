## ADDED Requirements

### Requirement: Per-alarm snooze fields persisted
The `Alarm` model SHALL carry `snooze_minutes: i64` (default 10) and `max_snoozes: i64` (default 3; 0 = disabled). Both SHALL round-trip through SQLite. The bundled-beep asset path SHALL be a compiled constant (not per-alarm).

#### Scenario: Snooze fields round-trip
- **WHEN** an alarm with `snooze_minutes = 7, max_snoozes = 2` is upserted and read back
- **THEN** both values are preserved

#### Scenario: Slice-2 alarm upgrades with defaults
- **WHEN** a v3/v4 alarm row (no snooze columns) is read after migration v5
- **THEN** `snooze_minutes` defaults to 10 and `max_snoozes` defaults to 3

### Requirement: Favorites table persisted
The application SHALL persist favorites in a `favorites` table (`id`, `name`, `source_type`, `source_uri`, `display_order`). Migration `v7` SHALL create the table and bump `user_version` to `7`, non-destructively. Reordering updates `display_order`.

#### Scenario: Fresh database migrates to v7
- **WHEN** a fresh database is opened and migrations run
- **THEN** `user_version` is `7` and a `favorites` table exists

#### Scenario: v6 database upgrades to v7
- **WHEN** a database at `user_version = 6` is migrated
- **THEN** the `favorites` table is created (empty), existing alarms are untouched, and `user_version` becomes `7`

### Requirement: Alarm fallback_chain reconciled to AudioSource
The alarm `fallback_chain` SHALL be modeled as `Vec<AudioSource>` (slice 4a introduced `AudioSource`); existing `Vec<String>` URIs from slice 2 SHALL be migrated/interpreted as `AudioSource::File`/`Radio`/`Spotify` by best-effort URI scheme detection at read time (no destructive migration).

#### Scenario: Legacy string fallback_chain is interpreted as AudioSource
- **WHEN** a slice-2 alarm with `fallback_chain = ["spotify:track:x", "file:///b"]` is read
- **THEN** the entries are interpreted as `Spotify("spotify:track:x")` and `File("file:///b")`

## MODIFIED Requirements

### Requirement: Migration v5 adds snooze_minutes and max_snoozes
Migration `v5` SHALL add `snooze_minutes INTEGER NOT NULL DEFAULT 10` and `max_snoozes INTEGER NOT NULL DEFAULT 3` columns to `alarms` and bump `user_version` to `5`, in a single transaction. Non-destructive: existing rows receive the defaults (slice-2 behavior upgraded to PRD defaults). Idempotent at `user_version >= 5`.

#### Scenario: Fresh database migrates to v5
- **WHEN** a fresh database is opened and migrations run
- **THEN** `user_version` is `5` and the `alarms` table has `snooze_minutes` and `max_snoozes` columns with defaults 10 and 3

#### Scenario: v4 database upgrades to v5
- **WHEN** a database at `user_version = 4` with existing alarms is migrated
- **THEN** all rows survive, `snooze_minutes = 10` and `max_snoozes = 3` on existing rows, and `user_version` becomes `5`

## ADDED Requirements

### Requirement: Per-alarm holiday_policy and calendars table persisted
The application SHALL persist a per-alarm `holiday_policy` (default `Suppress`) and a `calendars` table listing configured `CalendarSource` rows (`google_calendar_id`, `display_name`, `role`). Migration `v7` SHALL add the `holiday_policy` column and the `calendars` table, bumping `user_version` to `7`, in a single transaction, non-destructively (existing alarms get `Suppress`; no calendars configured until paired).

> **Note:** The slice-6 design originally named this "migration v6", but slice 5's weather migration already occupies `user_version = 6`; this is the next available version, `v7`.

#### Scenario: holiday_policy round-trips
- **WHEN** an alarm with `HolidayPolicy::Ignore` is upserted and read back
- **THEN** the policy is preserved

#### Scenario: Fresh database migrates to v7
- **WHEN** a fresh database is opened and migrations run
- **THEN** `user_version` is `7`, `alarms` has a `holiday_policy` column (default Suppress), and a `calendars` table exists

#### Scenario: v6 database upgrades to v7
- **WHEN** a database at `user_version = 6` is migrated
- **THEN** all alarm rows survive with `holiday_policy = Suppress`, the `calendars` table is created (empty), and `user_version` becomes `7`

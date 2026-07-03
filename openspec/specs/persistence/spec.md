## ADDED Requirements

### Requirement: Per-alarm snooze fields persisted
The `Alarm` model SHALL carry `snooze_minutes: i64` (default 10) and `max_snoozes: i64` (default 3; 0 = disabled). Both SHALL round-trip through SQLite. The bundled-beep asset path SHALL be a compiled constant (not per-alarm).

#### Scenario: Snooze fields round-trip
- **WHEN** an alarm with `snooze_minutes = 7, max_snoozes = 2` is upserted and read back
- **THEN** both values are preserved

#### Scenario: Slice-2 alarm upgrades with defaults
- **WHEN** a v3/v4 alarm row (no snooze columns) is read after migration v5
- **THEN** `snooze_minutes` defaults to 10 and `max_snoozes` defaults to 3

## MODIFIED Requirements

### Requirement: Migration v5 adds snooze_minutes and max_snoozes
Migration `v5` SHALL add `snooze_minutes INTEGER NOT NULL DEFAULT 10` and `max_snoozes INTEGER NOT NULL DEFAULT 3` columns to `alarms` and bump `user_version` to `5`, in a single transaction. Non-destructive: existing rows receive the defaults (slice-2 behavior upgraded to PRD defaults). Idempotent at `user_version >= 5`.

#### Scenario: Fresh database migrates to v5
- **WHEN** a fresh database is opened and migrations run
- **THEN** `user_version` is `5` and the `alarms` table has `snooze_minutes` and `max_snoozes` columns with defaults 10 and 3

#### Scenario: v4 database upgrades to v5
- **WHEN** a database at `user_version = 4` with existing alarms is migrated
- **THEN** all rows survive, `snooze_minutes = 10` and `max_snoozes = 3` on existing rows, and `user_version` becomes `5`

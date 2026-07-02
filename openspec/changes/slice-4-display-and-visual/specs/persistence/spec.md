## ADDED Requirements

### Requirement: Bedtime, brightness floor, and per-alarm visual config persisted
The application SHALL persist: global bedtime windows (weekday `(start,end)` + weekend `(start,end)` `Time` pairs), the dynamic-brightness floor, and per-alarm `VisualConfig` (default `Off`). Bedtime windows and brightness floor are stored as `kv_config` keys; `VisualConfig` is a per-alarm field (JSON text column, mirroring slice 2's `escalation_steps`).

#### Scenario: Bedtime windows persist across reboot
- **WHEN** the user sets weekday bedtime 22:00–06:00 and restarts
- **THEN** bedtime-off resumes on boot with the stored windows

#### Scenario: Per-alarm VisualConfig round-trips
- **WHEN** an alarm with `VisualConfig::On { brightness: 80, pulse_period: 1s, color: white }` is upserted and read back
- **THEN** the visual config is preserved exactly

## MODIFIED Requirements

### Requirement: Migration v4 adds visual_config column
Migration `v4` SHALL add a nullable `visual_config TEXT` column to the `alarms` table (JSON) and bump `user_version` to `4`, applied inside a single transaction. The migration SHALL be non-destructive: existing rows get `NULL` (deserializing to `VisualConfig::Off` = slice-1/2 behavior). Idempotent at `user_version >= 4`.

#### Scenario: Fresh database migrates to v4
- **WHEN** a fresh database is opened and migrations run
- **THEN** `user_version` is `4` and the `alarms` table has a `visual_config` column

#### Scenario: v3 database upgrades to v4 preserving data
- **WHEN** a database at `user_version = 3` with existing alarms is migrated
- **THEN** all alarm rows survive, `visual_config` is `NULL` (Off), and `user_version` becomes `4`

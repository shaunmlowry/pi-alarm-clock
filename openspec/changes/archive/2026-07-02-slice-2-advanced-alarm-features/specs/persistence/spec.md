## ADDED Requirements

### Requirement: Escalation steps and fallback chain persisted on the Alarm
The `Alarm` model SHALL carry two optional fields: `escalation_steps: Option<Vec<EscalationStep>>` and `fallback_chain: Option<Vec<String>>`. An `EscalationStep` SHALL consist of `after_secs: u64` (seconds elapsed since fire at which the step's volume takes effect) and `volume: u8` (0..=100). `escalation_steps` SHALL be sorted ascending by `after_secs` by the store on write. `fallback_chain` is an ordered list of backup source URIs; the alarm's `source_uri` is the primary (always tried first) and is not duplicated into the chain. `None` or empty for either field SHALL preserve slice-1 behavior (fixed `max_volume`; no fallback). The `AlarmStore` SHALL round-trip both fields through SQLite without loss.

#### Scenario: Upsert and read back an alarm with escalation and fallback
- **WHEN** an alarm with `escalation_steps = [{after_secs=0,volume=20},{after_secs=60,volume=80}]` and `fallback_chain = ["spotify:backup","file:///beep.mp3"]` is upserted and read back
- **THEN** both fields are preserved exactly (order and values)

#### Scenario: Alarm with no escalation/fallback round-trips as None
- **WHEN** an alarm authored without the new fields (a slice-1 alarm) is upserted and read back
- **THEN** `escalation_steps` and `fallback_chain` are both `None` and the alarm behaves as in slice 1

#### Scenario: Store re-sorts escalation steps by after_secs on write
- **WHEN** an alarm is upserted with `escalation_steps` in non-ascending order
- **THEN** the store sorts them ascending by `after_secs` before persisting, and a subsequent read returns the sorted order

### Requirement: Migration v3 adds escalation_steps and fallback_chain columns
Migration `v3` SHALL add nullable `escalation_steps TEXT` and `fallback_chain TEXT` columns to the `alarms` table and bump `user_version` to `3`, applied inside a single transaction. The migration SHALL be non-destructive: existing rows SHALL receive `NULL` for both columns (deserializing to `None` = slice-1 behavior), and `v1`/`v2` schema/data SHALL be untouched. The migration SHALL be idempotent (starting at `user_version >= 3` skips re-application).

#### Scenario: Fresh database migrates to v3 with the new columns
- **WHEN** a fresh database is opened and migrations are run
- **THEN** `user_version` is `3`, the `alarms` table has `escalation_steps` and `fallback_chain` columns, and existing `v1`/`v2` tables are intact

#### Scenario: Existing v2 database upgrades to v3 preserving data
- **WHEN** a database at `user_version = 2` with existing alarm rows is migrated to `v3`
- **THEN** all existing alarm rows survive, their `escalation_steps` and `fallback_chain` columns are `NULL`, and `user_version` becomes `3`

#### Scenario: v3 migration is idempotent
- **WHEN** migrations are run on a database already at `user_version = 3`
- **THEN** no work is done, `user_version` remains `3`, and existing data is unchanged

### Requirement: Dev seed supports optional escalation_steps and fallback_chain
The dev `alarms.toml` seed format SHALL accept optional `escalation_steps` (array of `{after_secs, volume}` tables) and `fallback_chain` (array of strings) on each `[[alarms]]` entry. Absent fields SHALL seed as `None`. The seed path SHALL remain dev-only and idempotent.

#### Scenario: Seed entry with escalation and fallback is upserted
- **WHEN** a dev `alarms.toml` entry includes `escalation_steps` and `fallback_chain`
- **THEN** the upserted alarm carries both fields and they round-trip through the store

#### Scenario: Seed entry without the new fields seeds as None
- **WHEN** a dev `alarms.toml` entry omits `escalation_steps` and `fallback_chain`
- **THEN** the upserted alarm has both fields as `None` (slice-1 behavior)

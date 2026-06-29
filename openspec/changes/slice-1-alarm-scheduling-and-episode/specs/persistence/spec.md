## ADDED Requirements

### Requirement: Migration v2 adds alarms table
The application SHALL ship migration `v2` (applied after slice 0's `v1`) that creates an `alarms` table with columns: `id` (TEXT PRIMARY KEY), `enabled` (INTEGER 0/1), `name` (TEXT), `time_local` (TEXT "HH:MM:SS"), `timezone` (TEXT IANA name), `rrule` (TEXT nullable), `once_at` (TEXT nullable ISO8601 local), `source_uri` (TEXT), `max_volume` (INTEGER 0..100, default 40), `next_fire` (TEXT nullable ISO8601 UTC, derived cache), `created_at` (TEXT), `updated_at` (TEXT). The migration SHALL be non-destructive (slice 0's `schema_meta` and `kv_config` SHALL be untouched) and SHALL bump `user_version` to `2`.

#### Scenario: Migration v2 creates alarms table
- **WHEN** the application starts on a database at `user_version=1`
- **THEN** migration `v2` runs, the `alarms` table is created, `user_version` becomes `2`, and `schema_meta`/`kv_config` are unchanged

#### Scenario: Migration v2 is idempotent
- **WHEN** the application starts on a database already at `user_version=2`
- **THEN** migration `v2` is skipped (no re-application) and `alarms` is intact

### Requirement: AlarmStore CRUD on main with atomic writes
The application SHALL provide an `AlarmStore` (owned by the main thread, holding a reference to the single `rusqlite::Connection`) with `list()`, `get(id)`, `upsert(alarm)`, `delete(id)`, `set_enabled(id, bool)`, and `recompute_next_fires(now)`. Each mutation SHALL be a single transaction (slice 0's atomic-write policy); multi-statement mutations SHALL roll back on partial failure. The `Connection` SHALL be touched only from the main thread (no `Mutex`, no pool), consistent with slice 0's `ConfigStore` model.

#### Scenario: Upsert and read back an alarm
- **WHEN** an `Alarm` is upserted by `id` and then `get(id)` is called
- **THEN** the read returns the alarm with all fields equal to the upserted values

#### Scenario: Upsert is idempotent by id
- **WHEN** the same alarm (same `id`) is upserted twice
- **THEN** only one row exists for that `id` (the second upsert updates, not inserts)

#### Scenario: set_enabled flips the flag in one transaction
- **WHEN** `set_enabled(id, false)` is called
- **THEN** the `enabled` column for that `id` becomes `0` and the change is committed atomically

#### Scenario: Failed mutation rolls back
- **WHEN** a multi-statement mutation fails partway (e.g. disk full)
- **THEN** the transaction rolls back, no partial change is persisted, an `error!` is logged, and the in-memory state remains authoritative

### Requirement: next_fire caching recomputed from the rule
The `next_fire` column SHALL be a derived cache recomputed by `recompute_next_fires(now)` on boot, on rule change, and after a fire. The rule (`rrule`/`once_at` + `time_local` + `timezone`) SHALL remain the source of truth; the FSM SHALL re-derive next-fire from the rule on the tick rather than trusting the cache blindly.

#### Scenario: next_fire cache populated on boot
- **WHEN** the application boots with alarms in the database
- **THEN** `recompute_next_fires(now)` populates `next_fire` for each enabled alarm from its rule

#### Scenario: next_fire recomputed after a fire
- **WHEN** an alarm fires
- **THEN** its `next_fire` cache is recomputed to its next future occurrence (or set NULL/disabled for a `Once` alarm)

## ADDED Requirements

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
- **THEN` the entries are interpreted as `Spotify("spotify:track:x")` and `File("file:///b")`

## ADDED Requirements

### Requirement: Web bearer token and TLS secrets persisted at 0600
The web bearer token SHALL be stored in `secrets.json` (0600, slice 6 owns the file). The TLS private key + self-signed certificate SHALL be stored in `tls/` at 0600. Both are read/written only from the main thread (the web handler on tokio requests them via a `Cmd`).

#### Scenario: Bearer token round-trips in secrets.json
- **WHEN** a new bearer token is generated on revoke & re-pair
- **THEN** it is written to `secrets.json` (0600) and read back on the next request

#### Scenario: TLS key never leaves the Pi
- **WHEN** the self-signed cert is generated at first boot
- **THEN** the private key is written to `tls/` at 0600 and is never transmitted off the Pi

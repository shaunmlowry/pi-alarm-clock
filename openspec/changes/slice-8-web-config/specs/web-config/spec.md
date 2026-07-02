## ADDED Requirements

### Requirement: Config-only REST API over the domain layer
The application SHALL embed an axum HTTP server serving a REST API over the domain layer. Every config endpoint (alarms CRUD, favorites CRUD, calendars, weather city, bedtime, themes, display, pairing/revoke) SHALL issue a `Cmd` over the cross-thread channel and await the `Reply`; the web SHALL NEVER touch the SQLite `Connection` directly (the domain + DB live on main). The web is config-only for v1: no live media control and no alarm dismiss/snooze from the web.

#### Scenario: Creating an alarm via the web routes through the domain
- **WHEN** the web client POSTs a new alarm
- **THEN** the axum handler sends a `Cmd::UpsertAlarm` to main, main persists it via `AlarmStore`, and the reply is returned to the web; the DB is touched only on main

#### Scenario: No live control from the web
- **WHEN** the web client attempts to dismiss or snooze a ringing alarm
- **THEN** no such endpoint exists; the request is 404 (live control is v2)

### Requirement: Bearer-token auth paired via QR
The application SHALL authenticate web requests with a shared bearer token, paired via a QR shown on the Pi touchscreen. The server SHALL be stateless (one middleware checks the bearer header). The token SHALL be rotatable ("Revoke & re-pair": the old token dies, a new QR is shown) and stored in `secrets.json` (0600).

#### Scenario: First pairing shows a QR
- **WHEN** the user opens Settings → Web/pairing on the Pi and initiates pairing
- **THEN** a QR encoding `https://alarm.local:port/#token=...&fp=...` is displayed

#### Scenario: Bearer required for all config endpoints
- **WHEN** a web request lacks a valid bearer token
- **THEN** the server responds 401

#### Scenario: Revoke & re-pair invalidates the old token
- **WHEN** the user revokes pairing
- **THEN** the old bearer token is rejected on subsequent requests and a new QR is shown

### Requirement: Self-signed TLS with trust-on-first-use (fingerprint deferred)
The application SHALL generate a self-signed TLS certificate at first boot (private key stored in `tls/` at 0600, never leaving the Pi). The pairing QR SHALL encode the certificate fingerprint alongside the token for **manual user verification**. v1 relies on **network segmentation** (the LAN is the trust boundary) plus trust-on-first-use: the browser warns on the self-signed cert and the user accepts it once. In-browser programmatic fingerprint pinning is **deferred** (v2). After first pairing, the phone SHALL store the token in SPA local storage; repeat visits SHALL NOT require re-scanning.

#### Scenario: Self-signed cert generated at first boot
- **WHEN** the app boots for the first time with no `tls/` cert
- **THEN** a self-signed cert + key are generated and stored in `tls/` at 0600

#### Scenario: v1 security relies on network segmentation
- **WHEN** a phone on the trusted LAN visits the Pi for the first time
- **THEN** the browser presents a self-signed-cert warning, the user accepts it (trust-on-first-use), and the QR-carried fingerprint is available for manual verification; programmatic fingerprint pinning is not performed in v1

### Requirement: mDNS discovery with IP fallback
The application SHALL advertise `alarm.local` via mDNS (`mdns-sd`). The Pi screen SHALL also show the current IP URL for manual fallback on platforms that don't resolve `.local`.

#### Scenario: Phone discovers the Pi via mDNS
- **WHEN** the phone is on the same LAN and the Pi is advertising
- **THEN** `alarm.local` resolves to the Pi

#### Scenario: IP fallback shown on the Pi
- **WHEN** the user opens the pairing screen
- **THEN** the current IP URL (e.g. `https://192.168.x.x:port`) is shown alongside the QR for manual fallback

### Requirement: SPA config-split per the PRD table
The web SPA SHALL implement the config split per the PRD: web-only fields (alarm name, full RRULE builder, favorites create/edit/delete, podcast feeds add/remove, weather city, theme custom) are editable here; Pi-touch-only fields (tap-to-play, podcast episode list, pairing QR display) are absent or read-only on the web. Complex RRULE is built here (Pi is read-only).

#### Scenario: Full RRULE builder is web-only
- **WHEN** the user constructs a complex RRULE (COUNT/UNTIL/INTERVAL>1) in the web UI
- **THEN** it is stored and shown read-only on the Pi

#### Scenario: Favorites editing is web-only
- **WHEN** the web user creates/edits/deletes a favorite
- **THEN** the change persists and the Pi media panel reflects it on next render (tap-to-play remains Pi-only)

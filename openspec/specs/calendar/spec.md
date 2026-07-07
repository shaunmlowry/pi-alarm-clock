## Purpose

Google Calendar integration for the alarm clock: OAuth2 device-flow pairing (self-sufficient on the Pi via QR), the `CalendarSource { Agenda | Holiday }` abstraction, a shared 30-min refresh that joins slice 5's `RefreshTick`, and the `secrets.json` (0600) secret store for the OAuth refresh token.

## Requirements

### Requirement: Google Calendar integration via OAuth2 device flow
The application SHALL integrate with the Google Calendar API using OAuth2 device flow. The Pi SHALL display a pairing QR/code; the user completes consent at `google.com/device` on another device; the Pi polls for the token and stores the refresh token in `secrets.json` (0600). First-run calendar setup SHALL be self-sufficient on the Pi (no web pairing required to add the first calendar).

> **Note:** The OAuth client must be a "TVs and Limited Input devices" type — the device-flow endpoint rejects Web/Desktop clients with `invalid_client` / "Invalid client type." The Google Calendar API must be enabled on the project.

#### Scenario: First calendar is added via device flow on the Pi
- **WHEN** the user opens Settings → Calendars on the Pi and initiates pairing
- **THEN** a QR/code is displayed, the Pi polls Google, and on consent the refresh token is stored in `secrets.json` (0600)

#### Scenario: Refresh token persists across reboot
- **WHEN** the Pi restarts after a successful pairing
- **THEN** the stored refresh token is used to refresh access without re-pairing

### Requirement: CalendarSource with Agenda and Holiday roles
The application SHALL model a `CalendarSource { google_calendar_id, display_name, role: Agenda | Holiday }`. `Agenda` role calendars feed the Daily-data panel agenda; `Holiday` role calendars (Google's "Holidays in Canada" and all-day personal events) feed holiday suppression. One Google Calendar API client serves both roles.

#### Scenario: Agenda calendar populates the Daily-data panel
- **WHEN** an Agenda-role calendar has upcoming events today
- **THEN** the next 4 events appear on the Daily-data panel agenda card (past events dimmed)

#### Scenario: Holiday calendar feeds suppression
- **WHEN** a Holiday-role calendar has an all-day event today
- **THEN** that day is treated as a holiday for alarm suppression

### Requirement: 30-min refresh shared with weather, Google expands RRULE
The application SHALL refresh calendar events on the shared 30-min refresh tick (slice 5), fetching events with `singleEvents=true` so Google expands recurring events. The app SHALL NOT parse calendar RRULE (the `rrule` crate is used only for alarm schedules). On failure, the last successful agenda is retained (stale-but-present) with backoff retry.

#### Scenario: Recurring events are expanded by Google
- **WHEN** an Agenda calendar has a recurring daily standup
- **THEN** each occurrence appears as a separate event in the agenda (the app did not expand the RRULE)

#### Scenario: Offline retains stale agenda
- **WHEN** the 30-min refresh fails
- **THEN** the last agenda is still shown and a retry is scheduled

### Requirement: secrets.json secret store at 0600
The application SHALL store secrets (OAuth refresh token; later the web bearer token) in a `secrets.json` file with filesystem mode 0600, distinct from the SQLite config store. The file SHALL be read/written only from the main thread.

#### Scenario: secrets.json is created 0600
- **WHEN** the OAuth refresh token is first stored
- **THEN** `secrets.json` is created with mode 0600 and the token is persisted

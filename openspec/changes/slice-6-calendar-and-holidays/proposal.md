# Slice 6: Calendar & Holiday Suppression

## Why

The PRD's Daily-data panel shows today's agenda (past dimmed, cap 4), and alarms are calendar-aware via holiday suppression. This slice introduces Google Calendar integration (OAuth2 device-flow, self-sufficient on the Pi via QR), the `CalendarSource { Agenda | Holiday }` abstraction, the shared 30-min refresh tick (joined to slice 5's), and the `HolidayPolicy` that makes the scheduler skip a firing alarm on a holiday.

## What Changes

- **Google Calendar API client** (OAuth2 device flow). The Pi displays a QR/code; the user consents at `google.com/device` on another device; the Pi polls, stores the refresh token in `secrets.json` (0600). First-run setup is self-sufficient on the Pi (no web pairing required to add the first calendar).
- **`CalendarSource` abstraction.** `{ google_calendar_id, display_name, role: Agenda | Holiday }` — one Google Calendar API client, role-tagged. `Agenda` feeds the Daily-data panel; `Holiday` feeds holiday suppression.
- **30-min refresh, shared with weather.** Joins slice 5's `RefreshTick`; fetches events with `singleEvents=true` (Google expands RRULE — the app does not parse calendar RRULE). Retry with backoff if offline; stale-retention.
- **Holiday suppression.** Each alarm carries a `HolidayPolicy: Ignore | Suppress | ShiftForward` (default `Suppress`). On the scheduler tick, if a due alarm's `HolidayPolicy != Ignore` and a holiday (Canada holidays calendar or an all-day personal event) is active that day, the alarm SHALL NOT fire: `Suppress` skips and advances to the next scheduled occurrence; `ShiftForward` advances day-by-day at the same wall-clock time until a non-holiday date (capped at 30 days). `Ignore` fires normally.
- **Agenda on the Daily-data panel.** Today's next 4 events (past dimmed), populated into the slot slice 3 defined.
- **`secrets.json`.** A 0600 JSON file for the OAuth refresh token (and later the web bearer token, slice 8). Distinct from the SQLite config store.

## Non-goals

- Event-derived alarms ("fire N min before a meeting") (v2).
- Per-event-content-aware alarms (v2).
- Calendar editing on the Pi (web-only, slice 8 — the Pi only shows the QR and picks Agenda/Holiday role).
- Non-Google calendar providers.

## Capabilities

### New Capabilities
- `calendar`: Google Calendar API client, OAuth2 device flow + QR, `CalendarSource`, 30-min refresh, agenda + holiday data models, `secrets.json` secret store.

### Modified Capabilities
- `alarm-scheduling`: holiday-suppression skip on the scheduler tick.
- `persistence`: per-alarm `holiday_policy`; `CalendarSource` list table; `secrets.json` plumbing.

## Impact

- **New code:** `alarm-clock/src/calendar.rs` (client, OAuth, models), `alarm-clock/src/secrets.rs` (`secrets.json` read/write, 0600), a QR-rendering component on the Settings panel.
- **Modified code:** `alarm-clock/src/scheduler.rs` (holiday skip check), `alarm-clock/src/alarm_store.rs` + `database.rs` (`holiday_policy` column, `calendars` table), `alarm-clock/src/main.rs` (refresh-tick fan-out, agenda wiring), `alarm-clock/ui.slint` (agenda card).
- **New deps:** `yup-oauth2` or `oauth2` (device flow), a `qrcode` crate, `reqwest` (HTTP).
- **Depends on:** slice 5 (shared refresh tick), slice 3 (panel slots).

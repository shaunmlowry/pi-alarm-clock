## Context

The PRD wants calendar-aware alarms (holiday suppression) and an agenda on the Daily-data panel. Google Calendar is the sole provider (full fidelity, device-flow OAuth so the Pi is self-sufficient). Recurring events are expanded by Google (`singleEvents=true`); the app never parses calendar RRULE — that crate is for alarm schedules only.

## Goals / Non-Goals

**Goals:** Google Calendar API + OAuth2 device flow + QR; `CalendarSource{Agenda|Holiday}`; shared 30-min refresh; agenda (cap 4, past dimmed); `HolidayPolicy` scheduler skip; `secrets.json` (0600).

**Non-Goals:** event-derived alarms (v2); `ShiftForward` (v2, falls back to Suppress); Pi-side calendar editing (web-only); non-Google providers; calendar RRULE parsing.

## Decisions

### D1. OAuth2 device flow, refresh token in secrets.json
Device flow (not loopback redirect) — no local server, works on the Pi behind NAT. `secrets.json` (0600) holds the refresh token (and later the web bearer token). The SQLite store holds non-secret config (`CalendarSource` list, `holiday_policy`). Secrets and config are separated by sensitivity, matching the PRD.

### D2. Google expands RRULE; app does not
Calls use `singleEvents=true`; Google returns expanded instances. The `rrule` crate is used only for alarm schedules. This avoids a second RRULE semantics domain and trusts Google's expansion (including timezone/DST).

### D3. Holiday detection is a date-membership check
A `HolidayStore` on main holds the set of dates that are holidays (from Holiday-role calendars' all-day events, fetched in the 30-min refresh). The scheduler tick checks "is the alarm's fire date in `HolidayStore`?" — O(1) set lookup, no per-tick API call. Canada holidays calendar + personal all-day events both populate the set.

### D4. Agenda is a capped, dimmed-past list
The `AgendaStore` holds today's events (cap 4 upcoming, past events retained and dimmed per the wireframe). Refreshed on the 30-min tick; the Daily-data panel reads it. Past events are dimmed (a `past: bool` flag per event drives the theme's dim styling).

## Risks / Trade-offs

- **[OAuth refresh token expiry / revocation]** → on a 401, re-prompt for device-flow re-pairing on the Pi; the agenda goes stale-but-present meanwhile.
- **[Holiday store staleness across the 30-min tick]** → a holiday added mid-day appears on the next tick; an alarm due in that window could fire before the refresh. Acceptable (alarms are scheduled days ahead; the 30-min tick is frequent enough).
- **[All-day event timezone ambiguity]** → use the alarm's stored timezone for date membership; Google returns all-day events as dates, so membership is date-based.

## Migration Plan

Migration `v6` (`holiday_policy` column + `calendars` table). `secrets.json` created on first pairing. No rollback.

## Open Questions

- `ShiftForward` with an *unbounded* holiday run (e.g. a week-long vacation): the loop advances day-by-day until a non-holiday is found. Capped at a sane limit (e.g. 30 days) to avoid pathological loops; if exceeded, falls back to `Suppress` behavior and logs. (Captured in the spec as "repeating until a non-holiday date is found"; the cap is an implementation detail in task 3.2.)

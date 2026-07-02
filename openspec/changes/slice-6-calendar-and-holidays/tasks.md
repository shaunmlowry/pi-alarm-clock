## 1. OAuth & Google client (alarm-clock/src/calendar.rs, secrets.rs)

- [ ] 1.1 Implement `secrets.json` read/write at 0600 (`alarm-clock/src/secrets.rs`), main-thread only.
- [ ] 1.2 Implement OAuth2 device flow: request code, display QR, poll for token, store refresh token.
- [ ] 1.3 Implement Google Calendar API client: list events (`singleEvents=true`), list calendars.
- [ ] 1.4 Define `CalendarSource { google_calendar_id, display_name, role }` and `Agenda`/`Holiday` role tagging.
- [ ] 1.5 Unit-test: secrets round-trip + 0600 mode; device-flow poll state machine (mock); event parsing.

## 2. Refresh & stores (alarm-clock/src/main.rs, calendar.rs)

- [ ] 2.1 Join the shared 30-min `RefreshTick` (slice 5); fan out to calendar fetch; backoff + stale retention.
- [ ] 2.2 Implement `HolidayStore` (set of holiday dates) and `AgendaStore` (today's events, cap 4, past dimmed).
- [ ] 2.3 Refresh-token 401 → re-prompt device-flow on the Pi.

## 3. Holiday suppression (alarm-clock/src/scheduler.rs, alarm_store.rs)

- [ ] 3.1 Add `HolidayPolicy` enum (`Ignore`/`Suppress`/`ShiftForward`→Suppress in v1) to `Alarm`.
- [ ] 3.2 Scheduler tick: if due alarm's policy != Ignore and fire date in `HolidayStore` → skip. `Suppress` advances `next_fire` to the next scheduled occurrence; `ShiftForward` advances day-by-day at the same wall-clock time until a non-holiday date (capped at 30 days, fall back to `Suppress` past the cap). Log the skip.
- [ ] 3.3 Unit-test: suppress-on-holiday; ignore-fires-on-holiday; personal all-day = holiday; no-holiday fires; ShiftForward skips a multi-day holiday to the first non-holiday; ShiftForward cap falls back to Suppress past 30 days.

## 4. Persistence (alarm-clock/src/database.rs, alarm_store.rs)

- [ ] 4.1 Migration `v6`: `ALTER TABLE alarms ADD COLUMN holiday_policy TEXT NOT NULL DEFAULT 'Suppress'`; `CREATE TABLE calendars (...)`; bump `user_version` to 6.
- [ ] 4.2 Round-trip `holiday_policy`; CRUD for `calendars` table.
- [ ] 4.3 Unit-test: v6 migration (fresh + upgrade + idempotent); policy round-trip; calendars CRUD.

## 5. UI (alarm-clock/ui.slint, main.rs)

- [ ] 5.1 Settings panel: Calendars pairing (show QR), Agenda/Holiday role pick, list of configured calendars.
- [ ] 5.2 Daily-data panel: agenda card (cap 4, past dimmed) bound to `AgendaStore`.
- [ ] 5.3 QR rendering component (qrcode crate).

## 6. Verification

- [ ] 6.1 `cargo build` + `cargo test` green; slice 0–5 tests unaffected.
- [ ] 6.2 Live check: device-flow pairing; agenda populates; holiday suppresses a due alarm.

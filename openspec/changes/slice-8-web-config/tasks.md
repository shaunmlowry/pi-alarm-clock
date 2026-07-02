## 1. REST API over Cmd→Reply (alarm-clock/src/web/, channel.rs, main.rs)

- [ ] 1.1 Define `Cmd` variants for each config endpoint (`ListAlarms`, `UpsertAlarm`, `DeleteAlarm`, `ListFavorites`, `UpsertFavorite`, `DeleteFavorite`, `ListCalendars`, `SetWeatherCity`, `SetBedtime`, `SetTheme`, `SetDisplay`, `Pair`/`Revoke`).
- [ ] 1.2 Implement axum routes that send a `Cmd` and await the `Reply`; the web never touches the `Connection`.
- [ ] 1.3 Config-only: no live-control / dismiss-snooze endpoints (return 404).
- [ ] 1.4 Unit-test: each endpoint routes through the domain (mock the channel; assert no direct DB access).

## 2. Auth & TLS (alarm-clock/src/web/auth.rs, tls.rs, secrets.rs)

- [ ] 2.1 Bearer-token middleware (stateless); reject 401 without a valid token.
- [ ] 2.2 Generate self-signed cert at first boot (`rcgen`), store in `tls/` (0600); serve axum over TLS (rustls).
- [ ] 2.3 Encode `token` + `fp` in the pairing QR (`https://alarm.local:port/#token=...&fp=...`); the fingerprint is for manual user verification only (v1 does NOT perform in-browser programmatic pinning — v1 relies on network segmentation + browser TOFU). Document this v1 limitation.
- [ ] 2.4 Revoke & re-pair: rotate bearer token, invalidate old, show new QR.
- [ ] 2.5 Unit-test: bearer rejection; revoke invalidates; cert generation + 0600 mode.

## 3. mDNS discovery & IP fallback (alarm-clock/src/web/mdns.rs, ui.slint)

- [ ] 3.1 Advertise `alarm.local` via `mdns-sd` on the tokio worker.
- [ ] 3.2 Show current IP URL on the Pi pairing screen for manual fallback.
- [ ] 3.3 Unit-test: mDNS advertisement registers the service.

## 4. SPA bundle (alarm-clock/web/)

- [ ] 4.1 Choose SPA framework (vanilla or minimal); build the config surface.
- [ ] 4.2 Implement the config-split: web-only fields (alarm name, full RRULE builder, favorites CRUD, podcast feeds add/remove, weather city, theme custom).
- [ ] 4.3 Store the token in `localStorage`; repeat visits don't re-scan. (Fingerprint pinning is v2; the fingerprint is shown for manual verification only.)
- [ ] 4.4 Serve the static bundle from axum.

## 5. Verification

- [ ] 5.1 `cargo build` + `cargo test` green; slice 0–7 tests unaffected.
- [ ] 5.2 Live check: pair via QR from a phone; CRUD alarms/favorites/calendars; revoke & re-pair; mDNS resolution; IP fallback.

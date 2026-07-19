# Slice 8: Web Configuration Interface

## Why

The PRD specifies an off-device, LAN, web configuration client (config-only for v1; no live media control). The Pi is the source of truth; the web reads/writes config stored on the Pi via the embedded axum server (slice 0 bound axum but served no endpoints). Slice 8 builds the REST API over the `ConfigStore`/domain (via the existing Cmd→Reply channel — never a direct DB touch), bearer/QR auth, self-signed TLS with fingerprint pinning, mDNS discovery, and the SPA bundle.

## What Changes

- **Embedded axum server + REST API.** Config endpoints over the domain layer: alarms CRUD, favorites CRUD, calendars, weather city, bedtime, themes, display, pairing/revoke. Every endpoint issues a `Cmd` over the cross-thread channel and awaits the `Reply` (the domain + SQLite live on main; the web never touches the DB directly).
- **Auth: bearer token via QR.** Shared bearer token, paired via QR on the Pi touchscreen. Stateless server (one middleware checks the bearer header). Rotatable ("Revoke & re-pair" — old token dies, new QR shown). Token stored in `secrets.json` (0600, slice 6).
- **TLS: self-signed, fingerprint pinned via QR.** Self-signed cert generated at first boot (private key never leaves the Pi, stored in `tls/` 0600). The pairing QR encodes `https://pialarm.local:port/#token=...&fp=...` so the SPA pins the fingerprint (MITM-resistant on untrusted WiFi).
- **mDNS discovery.** `pialarm.local` advertisement via `mdns-sd`. Pi screen also shows the current IP URL for manual fallback (platforms that don't resolve `.local`). After first pairing, the phone stores token + pinned fingerprint in SPA local storage; repeat visits don't re-scan.
- **SPA bundle.** A static SPA served by axum; communicates over the REST API. The full config-split (Pi touch vs web) per the PRD table is implemented: web-only fields (alarm name, full RRULE builder, favorites create/edit/delete, podcast feeds add/remove, weather city, theme custom) live here; Pi touch fields are read-only or absent on the web.
- **Config-only.** No live media control, no alarm dismiss/snooze from the web (v2). The web reads/writes config only.

## Non-goals

- Live media control / alarm dismiss-snooze from the web (v2).
- Remote access from outside the home / Let's Encrypt (v2).
- Custom-theme upload UI (v2; contract documented).
- A web live-control WS event channel (v2).

## Capabilities

### New Capabilities
- `web-config`: axum REST API over the domain, bearer/QR auth, self-signed TLS + fingerprint pin, mDNS discovery, SPA bundle, config-split.

### Modified Capabilities
- `persistence`: web bearer token (in `secrets.json`), TLS key/cert (`tls/`), config tables already added by prior slices are read/written here.

## Impact

- **New code:** `alarm-clock/src/web/` (axum routes, auth middleware, TLS, mDNS), `alarm-clock/web/` (SPA source), `alarm-clock/src/secrets.rs` (bearer token, slice 6 owns the file).
- **Modified code:** `alarm-clock/src/main.rs` (serve axum on the tokio worker; route Cmds), `alarm-clock/src/channel.rs` (new Cmd variants for each config endpoint), `alarm-clock/ui.slint` (pairing-QR display on Settings).
- **New deps:** `mdns-sd`, `rcgen` or `openssl` for self-signed cert, a SPA framework (TBD — vanilla or a tiny framework).
- **Depends on:** slices 3–7 (the config the web reads/writes).

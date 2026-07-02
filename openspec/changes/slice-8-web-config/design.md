## Context

Slice 0 bound axum but served nothing. Slice 8 makes the web a real config client: embedded axum + SPA, bearer/QR auth, self-signed TLS with fingerprint pinning, mDNS discovery. The Pi is the sole runtime authority; the web is config-only (no live control — v2). The critical invariant from slice 0 is preserved: the web (tokio) never touches the SQLite `Connection` — every endpoint is a Cmd→Reply over the cross-thread channel.

## Goals / Non-Goals

**Goals:** REST API over the domain (Cmd→Reply); bearer/QR auth; self-signed TLS + fingerprint pin; mDNS `alarm.local` + IP fallback; SPA config-split; revoke & re-pair; config-only.

**Non-Goals:** live media control / alarm dismiss-snooze from web (v2); remote access / Let's Encrypt (v2); custom-theme upload UI (v2); a WS event channel (v2).

## Decisions

### D1. Web = Cmd→Reply over the existing channel, never a direct DB touch
Every axum handler serializes a `Cmd`, sends it over the `CmdSender`, and awaits the `Reply` (the domain + `Connection` live on main, single-threaded). This preserves slice 0's "single `Connection` on main" invariant and avoids a `Mutex<Connection>` in the web path. New `Cmd` variants per endpoint (e.g. `Cmd::ListAlarms`, `Cmd::UpsertAlarm`, `Cmd::ListFavorites`).

**Rationale.** The alternative (a `Mutex<Connection>` shared with axum) breaks the single-threaded DB model and reintroduces the contention slice 0 avoided. The channel pattern is already proven for the Mopidy path.

### D2. Pairing QR carries token + fingerprint; v1 security is network segmentation + TOFU
The QR encodes `https://alarm.local:port/#token=<bearer>&fp=<sha256-of-cert>`. The token is used programmatically; the fingerprint is shown for **manual user verification** only. **In-browser programmatic TLS-fingerprint pinning is deferred to v2** — browsers don't expose raw cert bytes to `fetch`, making true pinning non-trivial. v1 relies on **network segmentation** (the LAN is the trust boundary) plus the browser's trust-on-first-use self-signed-cert acceptance. The PRD's "MITM-resistant on untrusted WiFi" goal is acknowledged as a v2 open question; v1 documents the network-segmentation assumption.

### D3. Self-signed cert via rcgen at first boot
`rcgen` generates a self-signed cert + key at first boot, written to `tls/` (0600). axum serves TLS via `axum-server` + `rustls`. No Let's Encrypt (wrong tool for a LAN appliance; remote access is v2).

### D4. mDNS via mdns-sd
`mdns-sd` advertises `alarm.local` on the tokio worker. The Pi screen shows the IP URL as fallback (queried from the OS). After first pairing, the phone uses `localStorage` — repeat visits don't re-scan.

### D5. SPA framework: minimal
Vanilla JS or a tiny framework (Preact/similar) — the SPA is a config form surface, not an app. Served as a static bundle by axum. The complex RRULE builder is the most involved piece; everything else is CRUD forms over the REST API.

## Risks / Trade-offs

- **[No in-browser fingerprint pinning in v1]** → v1 relies on network segmentation (trusted LAN). On untrusted WiFi the self-signed cert is vulnerable to MITM; this is a documented v1 limitation, mitigated by the user manually verifying the QR-carried fingerprint. True pinning is a v2 open question.
- **[Channel round-trip latency for every config read]** → acceptable (LAN, single user, config ops are infrequent).
- **[mDNS `.local` not universally resolvable (Android historically)]** → IP-fallback URL shown on the Pi covers this.

## Migration Plan

Additive: `tls/` dir + `secrets.json` bearer entry (slice 6's file). No DB migration (prior slices added the config tables). SPA bundle committed to the repo. No rollback.

## Open Questions

- In-browser TLS-fingerprint pinning mechanism for v2 (Service Worker? custom cert validation?). Deferred from v1.
- Which SPA framework (if any)? Deferred to task 4.1.
- Should the web show the live clock/now-playing read-only, or config-only? PRD says config-only for v1; live read-only views are v2.

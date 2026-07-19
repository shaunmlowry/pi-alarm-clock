# [Raspberry Pi Alarm Clock] - PRD

## 1. Document Control

| Metadata | Details |
| :--- | :--- |
| **Author** | [Shaun Lowry] |
| **Status** | Draft (grilled) |
| **Target Release** | Q[3] 2026 |

## 2. Executive Summary
### Problem Statement
* Alarm clock and media player software for Raspberry Pi
* There is no standalone raspberry pi software for making an alarm clock appliance

## 3. Scope & Boundaries
### In Scope
* Alarm clock functionality (calendar-aware, RRULE schedules, escalating volume, fallback chains, snooze, visual alarms)
* Audio media player (Mopidy-backed: Spotify, local files, internet radio, podcasts)
* Daily data panel (weather, agenda)
* Appliance display policies (bedtime power-off, dynamic brightness, wake-on-touch)
* Web configuration interface (off-device, LAN, paired via QR)

### Out of Scope (Non-Goals)
* Video or web browsing
* Live media control from the web (config only; live control deferred to v2)
* Custom theme upload UI (contract documented; upload deferred to v2)
* TuneIn radio browse (manual URL + curated catalog for v1)
* RRULE-by-hand for calendar data (Google expands events)
* Spotify Connect / librespot / spotifyd
* WiFi/network setup (OS-level, handled by Raspberry Pi Imager / `networkmanager`)
* Gradual pre-bedtime theme dimming
* "Follow-ambient" theme mode (stays Follow-Bedtime)
* Remote access from outside the home (Let's Encrypt / DNS-01 path)

## 4. Architecture

### Process Architecture
* **Single Rust process is the brain.** Owns alarm state, scheduling, persistence, calendar logic, weather, display policy, and the embedded web server. Supervised by systemd.
* **Mopidy is a playback backend only**, driven by the Rust app. Mopidy does not own alarm state.
* **Slint UI (in-process) and the web server (in-process, `axum`)** are two front-ends over one in-memory domain layer and one persistence store.
* Restarting Mopidy does not lose alarm state.

### Mopidy Integration
* Control channel: **JSON-RPC over WebSocket** (the canonical `mopidy/http` + `mopidy/json` frontend). One reconnecting WS client; typed wrapper over the methods used; event channel the scheduler subscribes to.
* Rejected: MPRIS (hides stream-failure events), HTTP REST (too limited).
* A small `mopidy-client` crate wraps the connection.

### Media Sources (Mopidy Backends)
* `mopidy-file` (built-in) — local/network files.
* `mopidy-stream` (built-in) — internet radio URLs.
* `mopidy-spotify` (installed) — Spotify tracks/albums/playlists via `spotify:...` URIs. **Plays full albums and playlists.** No librespot/spotifyd.
* `mopidy-podcast` + `mopidy-podcast-itunes` + `mopidy-gpodder` — podcasts via RSS feeds; browseable feeds (episodes), with discovery via iTunes/gPodder.

### Source Type Model
* **Flat-URI sources** (one playable URI): local file, Spotify track/album/playlist, internet radio URL.
* **Feed source** (browsable): podcast RSS feed. Episodes are picked from the feed, not a flat URI.

## 5. Functional Requirements

### Tech Stack
* Hardware (already set):
  - Raspberry Pi 5
  - JustBoom Amp Hat
  - Official Raspberry Pi 7" touchscreen
* Supporting software (already installed):
  - Mopidy, Mopidy-spotify
  - To install: mopidy-podcast, mopidy-podcast-itunes, mopidy-gpodder
* New tooling:
  - Rust toolchain
  - Slint for UI
  - `axum` for embedded web server
  - `rusqlite` for persistence
  - `rrule` crate for alarm schedules
  - `chrono-tz` for local-time/DST-correct scheduling
  - `mdns-sd` for discovery
  - OAuth2 device-flow client (e.g. `yup-oauth2` or `oauth2`) for Google Calendar

### Alarm Clock Functionality

#### Scheduling
* Schedule an alarm for any time; arbitrary number of alarms.
* Schedules use **full RFC 5545 RRULE** (via the `rrule` crate), wrapped behind a `Schedule` with `next_fire(after: DateTime<Tz>) -> Option<DateTime<Tz>>`.
* Alarms may be one-off (`Once`) or repeat (`FREQ=DAILY`, `BYDAY`, `INTERVAL`, `COUNT`, `UNTIL`, `BYSETPOS`, etc.).
* Times stored as **wall-clock local**; next-fire recomputed via `chrono-tz` across DST. Recomputed on boot, on rule change, and after each fire. Next-fire is derived, not stored.
* On the Pi touchscreen: schedule editing offers **common presets** (Once, Daily, Weekdays, Weekends, Specific-days). Complex RRULE is **read-only on the Pi**; the full RRULE builder is web-only.

#### Calendar Awareness (Holiday Suppression)
* Schedules are aware of calendar events via **holiday suppression** (skip policy).
* Holiday sources (both via Google Calendar API, see §Calendar Integration):
  - Google's "Holidays in Canada" calendar (national/regional).
  - All-day events on the user's personal Google Calendars (treated as personal holidays).
* Each alarm has a `HolidayPolicy`: `Ignore | Suppress | ShiftForward`. Default `Suppress`; on v1 hit = **skip** (alarm does not fire that day, resumes normal schedule next day).
* Out of scope: event-derived alarms ("fire N min before a meeting"), event-content-aware alarms.

#### Alarm Episode Lifecycle
1. **Fire:** capture a fresh **snapshot** of current Mopidy state: `{ uri, position_ms, was_playing, seekable, volume, repeat, shuffle }` plus the current `backlight_level`. Preempt Mopidy; set `repeat=true`; start the escalation clock.
2. **Audio source plays** the primary source from the alarm's fallback chain.
3. **Escalation** steps through `escalation_steps` writing to the Mopidy mixer volume (continuous across fallbacks and snooze re-fires; pauses during snooze; never resets to step 0).
4. **Visual alarm** (if `VisualConfig::On`) activates **10s after fire** — audio starts first, visual joins later.
5. **Snooze** (if invoked): restore the snapshot (user's media resumes); escalation clock pauses; after `snooze_minutes`, re-fire from the primary source with a fresh snapshot.
6. **Dismiss** (tap anywhere): restore the latest snapshot (including `backlight_level`); episode ends.

#### Volume Model
* Single real volume: Mopidy mixer (JustBoom Amp hardware ALSA mixer), 0..100.
* Per-alarm `max_volume`, **default 40**.
* No global `system_max_volume` ceiling (100% allowed).
* `escalation_steps: Vec<(offset_seconds, volume_percent)>` — **discrete steps**, default `[(0, max_volume)]` (no ramp unless configured). Each step writes `set_volume(min(step_vol, max_volume))`.
* Escalation applies only to the alarm episode; the user's pre-alarm volume is captured in the snapshot and restored on dismiss.

#### Fallback Chain
* Each alarm has `fallback_chain: Vec<AudioSource>` (primary + N fallbacks).
* **Implicit terminal fallback:** a bundled local beep file is always the last element of every chain.
* **Fallback trigger (heuristic):** after enqueuing a source, start a `fallback_grace` timer (default 8s). If playback state goes to `stopped` or `tracklist ended` during the grace window → source failed → advance to next fallback. A source that plays past the grace window is considered successful.
* **Escalation is continuous** across fallbacks (does not restart per source).
* **Chain exhausted** (bundled beep also fails): fire the visual alarm at full brightness and log the failure. (Visual is the terminal safety net — silent failure is never acceptable.)
* Alarm sources **loop** via Mopidy `repeat=true` for the duration of the episode; prior `repeat`/`shuffle` restored on dismiss.

#### Snooze
* `max_snoozes` per-alarm, **default 3** (0 = snooze disabled). After the cap, snooze is hidden; only dismiss remains.
* `snooze_minutes` per-alarm, **default 10**.
* **During snooze:** restore the user's media session (not silence). Escalation clock pauses. Re-fire starts from the **primary source** (chain resets).
* **Snapshot is fresh per fire** (re-captured on each re-fire, since the restored session has advanced).
* **Escalation clock is alarm-active-time-based:** pauses during snooze, resumes from last value on re-fire, never resets to step 0. Volume climbs across successive snoozes.
* **Dismiss = touch anywhere** (per PRD). **Snooze = dedicated large button**, visible only during an alarm episode.

#### Visual Alarms
* Visual = **the clock UI stays rendered; its brightness is flashed** (not a separate overlay, not a steady fill).
* **Brightness mechanism:** hardware backlight via sysfs `brightness` file — modulate between a floor and `visual_brightness` at `pulse_period` (default 1s). `bl_power` is used only for state transitions (bedtime off, wake), never for strobing.
* `VisualConfig: Off | On { brightness, pulse_period, color }`. Color themeable; default white.
* Visual runs **simultaneously with audio from fire** (per-alarm `On`/`Off`), with a **10s delay** before the visual activates (audio first).
* Forced-visual at full brightness is the terminal fallback when the audio chain exhausts.
* Snapshot extended to include `backlight_level`, restored on dismiss.

### Display Policies

#### Precedence (highest to lowest)
1. **Visual alarm strobe** (during alarm episode)
2. **Bedtime off** (during bedtime window, no alarm firing)
3. **User override** (temporary brightness set via swipe-up overlay, within 30-min window)
4. **Dynamic brightness** (idle default from `shortwave_radiation`)

Bedtime requires an explicit toggle to disable; a brightness override does **not** defeat bedtime-off.

#### Bedtime (Display Power)
* Global bedtime config: **weekday/weekend split** — two windows (Mon–Fri, Sat–Sun), each `(start, end)` as wall-clock `Time`.
* Cross-midnight handled: `(start, end)` with wrap inferred when `end < start`.
* During bedtime: `bl_power` off (true power-down).
* **Wake-on-touch:** any touch powers the display on, defaulting to the clock, for a **10s idle timer**. Further touches reset the timer. After 10s of no interaction → back off.
* During a bedtime wake, the user may swipe to any panel; the 10s idle timer governs power-off regardless of panel.
* **Entering settings suspends the idle timer**; exiting re-arms.
* **Invoking the swipe-up quick-controls overlay suspends the idle timer** (same rule); dismissing re-arms.
* **Alarm fires during bedtime:** bedtime is suspended for the episode (display on, visual strobing). On dismiss, power off immediately **but** arm the 10s wake-on-touch grace (reuses the idle timer).

#### Dynamic Brightness (Idle Default)
* Input: **Open-Meteo `shortwave_radiation` (W/m²)**, fetched on the existing 30-min weather tick. Single value combines time-of-day (sun angle) and weather (clouds) — no solar-geometry code.
* Mapping: **perceptual curve** (gamma ~0.5), with configurable **floor (default 10%)** and **ceiling (hardware max 100)**.
* Transitions **interpolated over ~120s** (reusing the backlight ramp mechanism; alarm strobe steps stay at ~0.5s).

#### User Brightness Override
* Temporary, via the **swipe-up overlay's brightness slider** (alongside the volume slider).
* 30-min timeout, then reverts to auto.

### Calendar Integration
* **Google Calendar API** (full fidelity), OAuth2 **device flow** (off-device consent).
  - Pi displays a QR/code; user completes consent at `google.com/device` on another device; Pi polls, stores refresh token.
  - Device flow chosen over loopback redirect (no LAN/local-server dependency).
* Shared `CalendarSource { google_calendar_id, display_name, role: Agenda | Holiday }` abstraction — one Google Calendar API client, role-tagged.
  - `Agenda` role → daily-data panel.
  - `Holiday` role → holiday suppression (§Calendar Awareness).
* Recurring events: rely on **Google's API expansion** (`singleEvents=true`); do not parse RRULE for calendar data. (`rrule` crate is used only for alarm schedules.)
* Refresh: 30 min, shared with weather refresh tick. Retry in background with exponential backoff if offline.
* First-run setup (device-flow QR) is **self-sufficient on the Pi** — no web pairing required to add the first calendar.

### Weather
* Source: **Open-Meteo** (no API key, free, non-commercial).
* Location: manual city name in settings, geocoded via Open-Meteo's geocoding API, default "Calgary." Stored as lat/long.
* Views:
  - **Brief (clock panel):** graphical weather icon + current temp + today's high. Minimal text.
  - **Detailed (daily-data panel):** current + today's H/L + tomorrow's H/L + current conditions (wind, humidity).
* Refresh: 30 min, shared with calendar refresh tick.
* WMO weather code → icon mapping is **part of the theme contract** (one icon set per theme).
* Defer hourly and multi-day forecasts to v2.

### Media Player
* Plays Spotify (paid), local/network files, internet radio, and podcasts — all via Mopidy.
* Internet radio minimum: **CBC Radio 1 Calgary and CKUA** (ship as pre-populated favorites).

#### Internet Radio Stations
* A radio station is a `Favorite { name, source: AudioSource::Radio(url) }` — not a separate concept.
* **Curated catalog:** small bundled `stations.json` (CBC, CKUA, + common ones) for tap-to-add in the web UI.
  - **CAVEAT (release checklist):** every curated stream URL must be researched and verified to work before release; re-verify on each release. Do **not** hallucinate non-existent streams.
* **Manual URL paste** in the web UI for anything not in the catalog.
* Static bundled catalog for v1 (updated with app releases); existing favorites are independent of the catalog.
* TuneIn browse deferred to v2.

#### Favorites
* `Favorite { name, source: AudioSource }` — shared abstraction between the media panel and alarm source config.
* Managed in the web UI (text entry: names, URLs, Spotify URIs, podcast feed URLs).
* Reorder: drag-handle, both Pi and web.
* Tap-to-play on the Pi (live control, Pi only).
* **Cap: 8 favorites** displayed on the Pi (web-enforced soft limit with warning); "more" is web-only.

#### Podcast Favorites
* A podcast favorite is a feed (browsable to episode level), not a flat URI.
* Tapping a podcast favorite on the Pi **expands to an episode list** (does not immediately play "latest").
* Episode list capped at **most-recent 5** on the Pi.
* Other favorites (radio, Spotify, local) play immediately on tap.

#### Transport Controls
* Adapt to source capabilities:
  - Radio: play/stop only (no next/prev/seek; "pause" = stop, resumes live on restart).
  - Spotify/local/podcast: play/pause, next/prev, seek (where supported).

#### Quick Controls Overlay
* **Swipe up on any panel** → compact overlay (volume slider + brightness slider + play/pause + next/prev if applicable).
* Dismissed by tap-outside or 5s idle.
* Invoking the overlay suspends the bedtime idle timer; dismissing re-arms.

### Web Configuration Interface
* **Pi is source of truth and sole runtime authority**; the web is a config client that reads/writes config stored on the Pi.
* **Config-only for v1** (no live media control / alarm dismiss from the web); live control deferred to v2.
* Web server **embedded in the same Rust process** (`axum` + static SPA bundle + REST API over a shared `ConfigStore`). One source of truth, two surfaces.

#### Auth & TLS
* **Auth:** shared bearer token, paired via QR on the Pi touchscreen. Stateless server (one middleware checks the bearer header). Rotatable in settings ("Revoke & re-pair" — old token dies, new QR shown).
* **TLS:** self-signed cert generated at first boot (private key never leaves the Pi). Cert **fingerprint pinned via the pairing QR** (alongside the bearer token) — MITM-resistant on untrusted WiFi.
* No Let's Encrypt (wrong tool for a LAN appliance; noted as a future "remote access" path).

#### Discovery
* **mDNS advertisement** (`pialarm.local`) via `avahi`/`mdns-sd` — primary, durable across IP changes on supporting platforms.
* **Pairing QR** encodes `https://pialarm.local:port/#token=...&fp=...` — scanned once on the phone.
* Pi screen also shows the **current IP URL** for manual fallback (platforms that don't resolve `.local`).
* After first pairing, the phone stores token + pinned fingerprint in SPA local storage; repeat visits don't require re-scanning.

#### Config Split (Pi touch vs. web-only)
| Surface | Pi touch | Web |
|---|---|---|
| Alarms — enable/disable toggle | ✓ | ✓ |
| Alarms — name | ✗ | ✓ |
| Alarms — schedule (presets) | ✓ | ✓ |
| Alarms — schedule (full RRULE) | read-only | ✓ |
| Alarms — source chain (pick favorites) | ✓ | ✓ |
| Alarms — escalation steps, max_volume, snooze_minutes, max_snoozes | ✓ | ✓ |
| Alarms — holiday_policy, visual config | ✓ | ✓ |
| Favorites — create/edit/delete | ✗ | ✓ |
| Favorites — reorder | ✓ | ✓ |
| Favorites — tap-to-play | ✓ | ✗ |
| Podcast feeds — add/remove | ✗ | ✓ |
| Podcast feeds — episode list | ✓ | ✗ |
| Calendars — add Google account (device-flow QR) | ✓ (shows QR) | ✗ |
| Calendars — pick Agenda vs Holiday | ✓ | ✓ |
| Weather — city | ✗ | ✓ |
| Weather — unit preferences | ✓ | ✓ |
| Bedtime — weekday/weekend times | ✓ | ✓ |
| Themes — select active theme, light/dark mode | ✓ | ✓ |
| Themes — install/create custom | ✗ | ✓ (v2) |
| Display — normal brightness floor, timeout | ✓ | ✓ |
| Web/pairing — show pairing QR, revoke | ✓ | ✗ |
| Network/WiFi | ✗ | ✗ (OS-level) |

### UX/UI

#### General
* Standalone appliance interface using raw display hardware and GPU.
* Built using Slint/Rust.
* **Touch only, no text input** (no keyboard attached, no room for virtual keyboard). Use only touch-appropriate inputs. Text entry is delegated to the web interface.
* **Vertical (9:16).**
* Simple, Elegant — no unnecessary on-screen elements.
* Alarms dismissed by touching any point on the screen.

#### Theming
* **Tokens + component-variant selectors** (not token-only). Each theme provides its own `Card`/`Button`/`ClockFace` variants sharing a common interface; tokens provide colors/sizes.
  - Token-only theming rejected (Liquid Glass and Neuromorphic are different rendering techniques, not recolors).
* Theme model:
  ```
  Theme { name, component_variants: {...}, light: TokenSet, dark: TokenSet }
  ```
* **Light/dark** = two token sets within a theme; mode is a token swap; theme switch is token + variant swap.
* **Mode selection:** `Manual-Light | Manual-Dark | Follow-Bedtime`. Default `Follow-Bedtime`.
* Hard light/dark switch for v1 (no gradual dim); gradual dim deferred to v2.
* Weather icons are part of the theme contract (one icon set per theme).
* Initial themes:
  - Liquid Glass (see https://www.cssscript.com/demo/glassmorphism-analog-clock/ for styling inspiration)
  - Neuromorphic (see https://www.cssscript.com/demo/neumorphic-analog-clock/ for styling inspiration)
* v1 ships two built-in themes; custom-theme-upload UI deferred to v2 (contract documented).

#### Panels & Navigation
* Multi-panel, swipable. Each panel can have multiple cards.
* **No scrolling** — cards must fit on screen (vertical gestures reserved for media-panel and future features).
* Horizontal swipe = panel navigation. No wraparound (hard stops at both ends).
* Four initial panels:
  - **Clock panel:** clock face (theme-dependent, analog or digital), brief weather (icon + temp + high), next calendar event.
  - **Daily-data panel:** today's agenda (past dimmed), tomorrow's H/L + current conditions (wind/humidity).
  - **Media panel:** now-playing + transport + favorites list (tap-to-play; podcasts expand to episodes).
  - **Settings panel:** touch-native subset of config.
* **Agenda cap:** next 4 events. **Podcast episodes cap:** most-recent 5. **Favorites cap:** 8.
* Swipe up on any panel → quick-controls overlay (see §Media Player).

#### Alarm UI
* During an alarm episode: **normal panels hidden**; alarm UI shown exclusively (clock + dedicated large snooze button + tap-anywhere-to-dismiss). Panel-swipe disabled.

## 6. Persistence & Operations

### Persistence
* **SQLite** (via `rusqlite`) — chosen for future flexibility.
* Synchronous atomic write on every config mutation (WAL mode for crash safety).
* `secrets.json` (0600) for the bearer token, OAuth refresh token; TLS private key in `tls/` (0600).

### systemd Integration
* `Type=notify` (app signals readiness after loading config + computing next-fire).
* `Restart=on-failure` with backoff.
* `After=network-online.target` + `Wants=` it — but the app does **not** block on network (alarms fire offline; calendar/weather retry in background with exponential backoff).
* `After=mopidy.service` + `Wants=mopidy.service` — but the app does not block on Mopidy (alarm fallback chain degrades to terminal visual alarm if Mopidy is down).

### Time Source
* Assume NTP; recommend `fake-hwclock` as a minimum (writes time to disk periodically, restores on boot).
* **No RTC HAT** (does not fit in the case).
* Documented dependency: without network and without `fake-hwclock`, a cold boot has the wrong time and alarms fire incorrectly.

## 7. Risks
* **Missed alarms on cold boot without NTP/RTC** — the Pi has no hardware clock; a cold boot with no network has the wrong time. Mitigation: `fake-hwclock`, document NTP dependency.
* **Radio stream URL drift** — curated station URLs may change between releases. Mitigation: release-time verification (see §Internet Radio Stations caveat).
* **Mopidy stream-failure detection is heuristic** — a dead radio URL manifests as immediate `stopped`, not a clean error. Mitigation: 8s grace-window heuristic (§Fallback Chain).
* **Bedtime + alarm interaction is subtle** — multiple display policies compose; bugs here mean "screen on at 3am" or "alarm doesn't visually fire." Mitigation: explicit precedence stack (§Display Policies) and tests for each policy combination.

## 8. Open Questions
* **Custom theme upload UI** (v2) — contract documented; upload/authoring flow deferred.
* **TuneIn radio browse** (v2) — directory browsing via `mopidy-tunein`.
* **Live control from the web** (v2) — play/pause, dismiss/snooze a ringing alarm from the web. Requires a real-time WS event channel and a stronger auth threat model.
* **Gradual pre-bedtime theme dimming** (v2) — interpolate theme mode over 30 min before bedtime.
* **"Follow-ambient" theme mode** (v2) — theme mode tracks `shortwave_radiation` as well as bedtime.
* **Remote access from outside the home** (v2) — Let's Encrypt / DNS-01 path for users who own a domain and want to expose the web surface remotely.
* **Hourly/multi-day weather forecast** (v2) — additive to the daily-data panel.
* **Per-weekday bedtime windows** (v2) — for shift workers; v1 has weekday/weekend split only.
* **Event-derived alarms** (v2) — "fire N minutes before my first meeting."
* **Permanent brightness offset** (v2) — a user-set offset that shifts the dynamic-brightness curve up/down.
* **Catalog refresh from remote URL** (v2) — keep `stations.json` current without an app release.

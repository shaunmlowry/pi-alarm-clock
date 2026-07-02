## Context

Slice 4 defaulted dynamic brightness to 60% pending a `shortwave_radiation` input. Slice 5 introduces the Open-Meteo data source that provides it, plus the brief/detailed weather views the PRD places on the Clock and Daily-data panels. The 30-min refresh tick defined here is shared with slice 6's calendar refresh.

## Goals / Non-Goals

**Goals:** Open-Meteo fetch (current + daily + `shortwave_radiation`); geocoding (default Calgary); shared 30-min refresh tick with backoff; stale-retention; WMO→icon theme contract; brief + detailed views; feed `DisplayController` with `shortwave_radiation`.

**Non-Goals:** hourly/multi-day forecasts (v2); multiple cities (v2); Pi-side city entry (web-only, slice 8); radar (v2).

## Decisions

### D1. Shared refresh tick is a single main-thread timer owning multiple fetchers
A `slint::Timer` (30 min) on main fires a single `RefreshTick` Cmd. The tokio worker fans out to weather now (and calendar in slice 6). Results return as separate `Reply` variants. One timer = one backoff policy = no thundering herd of independent timers.

### D2. Stale retention over error states
The `WeatherStore` on main holds the last successful `WeatherSnapshot`. A failed fetch logs + schedules backoff but does NOT clear the store. The UI always shows *something* (graceful degradation, mirroring slice 1's Mopidy-down philosophy).

### D3. WMO→`WeatherIcon` enum, icons live in the theme
A pure-Rust function maps WMO code → `WeatherIcon` (Clear, MainlyClear, PartlyCloudy, Cloudy, Fog, Drizzle, Rain, FreezingRain, Snow, Showers, Thunderstorm). Each theme provides a Slint icon set (an `Image` or `Text` glyph per enum variant) selected by the active theme. This keeps icon art out of the weather module (data) and in the theme (presentation).

### D4. Geocoding result cached; re-geocode only on city change
The lat/long is persisted in `kv_config`; the geocoding API is hit only when the city string changes (set by the web UI in slice 8). Boot uses the cached lat/long directly.

## Risks / Trade-offs

- **[Open-Meteo geocoding may return ambiguous results]** → pick the first hit and log the resolved name; the web UI shows the resolved name so the user can correct.
- **[30-min staleness for brightness input]** → acceptable; brightness is a slow perceptual curve (120 s interpolation). A missed fetch keeps the last target.
- **[Open-Meteo API shape changes]** → pin to the documented current+daily endpoint; add a serde fallback for missing fields (treat as None).

## Migration Plan

Additive: new `weather.rs`, `kv_config` entries (city, lat, long). No migration. Default city Calgary seeded at first boot.

## Open Questions

- Should the brief view show "feels-like" temp? PRD says current temp + high; deferring feels-like to v2.
- Unit preferences (°C/°F) — PRD allows web-set unit prefs; this slice renders °C by default, with a unit config flag consumed here.

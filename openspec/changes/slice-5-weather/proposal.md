# Slice 5: Weather

## Why

The PRD's Clock panel shows brief weather and the Daily-data panel shows detailed conditions; dynamic brightness (slice 4) needs `shortwave_radiation` as its input. Slice 4 defaulted dynamic brightness to a fixed 60% pending this slice. Slice 5 introduces the Open-Meteo data source, geocoding, the shared 30-min refresh tick (which slice 6's calendar will join), and the WMO-icon contract that the theme system owns.

## What Changes

- **Open-Meteo client.** A tokio-side async client fetching current weather + today's/tomorrow's daily H/L + current conditions (wind, humidity) + `shortwave_radiation` from Open-Meteo (no API key). Results marshalled back to main via the reply channel.
- **Geocoding.** Manual city name (default "Calgary") geocoded via Open-Meteo's geocoding API; lat/long persisted. Set via the web UI (slice 8) — the Pi has no text entry.
- **Shared 30-min refresh tick.** A `slint::Timer` on main every 30 min triggers a weather fetch Cmd; slice 6's calendar refresh joins the same tick. Retry with exponential backoff if offline; the last successful data is held and shown stale-but-present.
- **WMO code → icon mapping (theme contract).** Weather icons are part of the theme contract (one icon set per theme). This slice defines the `WeatherIcon` enum (WMO-derived) and the mapping; the active theme provides the icon glyph set.
- **Brief + detailed views.** Clock panel: icon + current temp + today's high (populates the slot slice 3 defined). Daily-data panel: current + today's H/L + tomorrow's H/L + wind/humidity (populates the slot slice 3 defined).
- **Dynamic brightness input.** `DisplayController` (slice 4) reads the fetched `shortwave_radiation` on each refresh, replacing the 60% default.

## Non-goals

- Hourly / multi-day forecasts (v2).
- Radar/precipitation maps (v2).
- Multiple cities (v2).
- A Pi-side city-entry UI (text entry is web-only, slice 8).

## Capabilities

### New Capabilities
- `weather`: Open-Meteo client, geocoding, 30-min refresh tick, WMO-icon mapping, brief/detailed data models.

### Modified Capabilities
- `ui-shell`: populate the Clock-panel weather card and Daily-data-panel weather cards (slots defined in slice 3).
- `display-policy`: dynamic-brightness input switches from the 60% default to live `shortwave_radiation`.

## Impact

- **New code:** `alarm-clock/src/weather.rs` (client, models, WMO map), a 30-min refresh timer in `main.rs`.
- **Modified code:** `alarm-clock/src/display.rs` (consume `shortwave_radiation`), `alarm-clock/ui.slint` (weather card bindings), `alarm-clock/src/config.rs` (city/lat/long persistence).
- **Depends on:** slice 3 (panel slots, theme icon contract), slice 4 (dynamic brightness consumer).

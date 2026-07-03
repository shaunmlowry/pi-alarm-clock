## ADDED Requirements

### Requirement: Open-Meteo weather fetch with 30-min refresh
The application SHALL fetch current weather, today's and tomorrow's daily H/L, current wind/humidity, and `shortwave_radiation` from Open-Meteo (no API key) on a 30-min refresh tick driven by a `slint::Timer` on main. The fetch SHALL run as an async Cmd on the tokio worker; results SHALL be marshalled back to main via the reply channel. On failure (offline), the last successful data SHALL be retained and shown (stale-but-present), with retries using exponential backoff.

#### Scenario: Successful fetch populates the weather models
- **WHEN** the 30-min tick fires and Open-Meteo is reachable
- **THEN** the current temp, today's/tomorrow's H/L, wind, humidity, and `shortwave_radiation` are updated on main

#### Scenario: Offline retains stale data
- **WHEN** the 30-min tick fires and Open-Meteo is unreachable
- **THEN** the last successful weather data is still shown and a retry is scheduled with backoff

### Requirement: Geocoded city persisted as lat/long
The city name (default "Calgary") SHALL be geocoded via Open-Meteo's geocoding API to a lat/long pair, which SHALL be persisted and used for weather fetches. The city is set via the web UI (slice 8); the Pi exposes no city-entry UI.

#### Scenario: Default city is Calgary
- **WHEN** the app boots with no city configured
- **THEN** weather is fetched for Calgary (lat/long 51.05/-114.07)

#### Scenario: City change re-geocodes and refreshes
- **WHEN** the web UI sets the city to "Edmonton"
- **THEN** the city is geocoded, the lat/long persisted, and the next weather fetch uses Edmonton

### Requirement: WMO weather code to icon mapping is part of the theme contract
The WMO weather code returned by Open-Meteo SHALL be mapped to a `WeatherIcon` enum (e.g. Clear, PartlyCloudy, Rain, Snow) and the active theme SHALL provide the icon glyph set for that enum (one icon set per theme, per the PRD theme contract).

#### Scenario: Clear sky shows the clear icon in the active theme
- **WHEN** Open-Meteo returns WMO code 0 (clear sky) and the active theme is Liquid Glass
- **THEN** the weather card shows the Liquid Glass variant of the Clear icon

### Requirement: Brief and detailed weather views
The Clock panel SHALL show a brief view (icon + current temp + today's high). The Daily-data panel SHALL show a detailed view (current temp, today's H/L, tomorrow's H/L + conditions, wind, humidity). Both populate the card slots defined in slice 3.

#### Scenario: Brief view on the Clock panel
- **WHEN** weather data is present
- **THEN** the Clock-panel weather card shows the icon, current temp, and today's high

#### Scenario: Detailed view on the Daily-data panel
- **WHEN** weather data is present
- **THEN** the Daily-data panel shows current conditions, today's H/L, tomorrow's H/L + conditions, wind, and humidity

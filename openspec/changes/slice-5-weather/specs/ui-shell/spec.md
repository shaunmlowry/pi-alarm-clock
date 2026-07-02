## ADDED Requirements

### Requirement: Weather cards populated on the Clock and Daily-data panels
The Clock-panel weather card (slot defined in slice 3) SHALL display the brief weather view (icon + current temp + today's high). The Daily-data panel's "Weather Tomorrow" and "Current Conditions" cards (slots defined in slice 3) SHALL display tomorrow's H/L + conditions and current wind/humidity respectively. The cards SHALL render in the active theme and show stale-retained data (with no error state) when offline.

#### Scenario: Clock-panel weather card shows brief data
- **WHEN** the Clock panel is shown and weather data is present
- **THEN** the weather card shows the themed icon, current temp, and today's high

#### Scenario: Daily-data panel shows detailed weather
- **WHEN** the Daily-data panel is shown and weather data is present
- **THEN** the "Weather Tomorrow" card shows tomorrow's H/L and conditions, and the "Current Conditions" card shows wind and humidity

#### Scenario: Stale data shows without an error indicator
- **WHEN** the device is offline and the last weather fetch is stale
- **THEN** the weather cards continue to show the last data with no error/broken state

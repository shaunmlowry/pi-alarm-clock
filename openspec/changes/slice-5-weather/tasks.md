## 1. Open-Meteo client & models (alarm-clock/src/weather.rs)

- [ ] 1.1 Define `WeatherSnapshot` (current temp, today's/tomorrow's H/L, wind, humidity, shortwave_radiation, WMO code, fetched_at).
- [ ] 1.2 Implement the async Open-Meteo fetch (current + daily + shortwave_radiation) as a Cmd on the tokio worker; reply marshalled to main.
- [ ] 1.3 Implement geocoding (city → lat/long) via Open-Meteo geocoding API; cache result.
- [ ] 1.4 Implement WMO code → `WeatherIcon` enum mapping (pure fn).
- [ ] 1.5 Unit-test: WMO mapping; snapshot serde; geocode parsing; fetch reply shape (mock JSON).

## 2. Refresh tick & store (alarm-clock/src/main.rs, weather.rs)

- [ ] 2.1 Add a 30-min `slint::Timer` on main firing a `RefreshTick` Cmd (fan-out point for slice 6).
- [ ] 2.2 Implement `WeatherStore` on main holding the last successful snapshot; failed fetch retains it; backoff retry.
- [ ] 2.3 Persist city/lat/long in `kv_config`; default Calgary at first boot.

## 3. Dynamic brightness input (alarm-clock/src/display.rs)

- [ ] 3.1 `DisplayController` reads `shortwave_radiation` from `WeatherStore` on each refresh; target brightness via perceptual curve (gamma ~0.5, floor 10%).
- [ ] 3.2 Remove the slice-4 60% default once a valid `shortwave_radiation` is present.

## 4. UI population (alarm-clock/ui.slint, theme)

- [ ] 4.1 Bind the Clock-panel weather card to brief data (icon + temp + high).
- [ ] 4.2 Bind the Daily-data panel "Weather Tomorrow" + "Current Conditions" cards.
- [ ] 4.3 Provide a `WeatherIcon` icon set per theme (Liquid Glass, Neuromorphic) in Slint.
- [ ] 4.4 Render stale data without an error state.

## 5. Verification

- [ ] 5.1 `cargo build` + `cargo test` green; slice 0–4 tests unaffected.
- [ ] 5.2 Live check: weather fetch populates both panels; offline retains stale data; dynamic brightness tracks radiation.

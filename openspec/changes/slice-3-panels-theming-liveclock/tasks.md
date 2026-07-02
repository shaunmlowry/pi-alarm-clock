## 1. Theme model & controller (alarm-clock/src/theme.rs)

- [x] 1.1 Define `TokenSet` (background, card-background, card-border, card-border-width, card-shadow-blur, card-shadow-offset, card-shadow-color, card-radius, text-color, accent-color, clock-face-background, hand-color, second-hand-color, font-family) and `ComponentVariant` (`LiquidGlass` | `Neuromorphic`).
- [x] 1.2 Define `Theme { name, light: TokenSet, dark: TokenSet, variant }` and the two built-in themes (Liquid Glass, Neuromorphic) with light + dark token sets matching the wireframes (accent `#ff6b6b`).
- [x] 1.3 Implement `ThemeController` on main: holds active theme + mode (`Manual-Light`/`Manual-Dark`/`Follow-Bedtime`), resolves the effective `TokenSet`, exposes a method to push values into Slint root properties, and persists selection via `ConfigStore`.
- [x] 1.4 Spike: verify Slint `drop-shadow` can render neumorphic dual extrusion shadows (two stacked light/dark shadows); document the technique.
- [x] 1.5 Unit-test: mode resolution (`Manual-Light` overrides bedtime; `Follow-Bedtime` selects dark in 22:00–06:00 heuristic); persistence round-trip.

## 2. Slint components (alarm-clock/ui/*.slint)

- [x] 2.1 Extract `Card.slint` and `Button.slint` components with `in-property` visuals bound to theme tokens.
- [x] 2.2 Rewrite `ClockFace` (in `ui.slint` or `ClockFace.slint`) as analog: hour/minute/second hands with `in-property <float>` angles, date/day label, theme-bound visuals; remove the `12:00` placeholder.
- [x] 2.3 Add nav-dots and panel-tabs components.
- [x] 2.4 Extend `PanelContainer` to four panels (Clock, Daily-data, Media, Settings) with named empty `Card` slots on daily-data/media/settings matching the wireframe structure.
- [x] 2.5 Bind `AlarmPanel.slint` visuals (clock-color, card, snooze button) to the theme root properties.

## 3. Live clock & wiring (alarm-clock/src/main.rs, ui.slint)

- [x] 3.1 Add a 1 s `slint::Timer` on main (panic-isolated) that reads `Local::now()` and writes hand angles + date label into the Slint root.
- [x] 3.2 Expose theme root `in-out` properties (background, card tokens, hand colors, font-family) on `AppWindow`; wire `ThemeController` to push values each tick (and on change).
- [x] 3.3 Wire Settings panel theme/mode tap-cycling to `ThemeController` + `ConfigStore` persistence.

## 4. Settings panel + persistence (alarm-clock/src/config.rs, main.rs)

- [x] 4.1 Persist theme name + mode as two `kv_config` keys; load at boot.
- [x] 4.2 Render Settings panel cards (Theme, Mode tap-cycle; Alarms-summary + Display-summary read-only placeholders).

## 5. Verification

- [x] 5.1 `cargo build` + `cargo test` green; slice 0–2 tests unaffected.
- [ ] 5.2 Visual check: live analog clock shows correct time; both themes match their wireframes; runtime switch re-renders all panels; theme/mode persists across restart.
- [x] 5.3 Full-screen kiosk: release build on the Pi covers the entire display with no title bar/border; debug build retains 480×854 for testing.

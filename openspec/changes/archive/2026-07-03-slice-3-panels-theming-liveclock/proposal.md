# Slice 3: Panels, Theming & Live Clock

## Why

Slices 0–2 built the alarm spine but the UI is still the slice-0 placeholder: a hardcoded `12:00` text on a single Clock panel with hardcoded color/font feeders. Every later user-facing slice (weather, calendar, media, settings, web) needs (a) a live clock, (b) the four-panel scaffold from the PRD, and (c) a real theme system so the weather icons, cards, and clock face render consistently. This slice lays that foundation. It also resolves the slice-0 "theme seam reserved" open question by making themes **selectable at runtime** (not compile-time), hewing to the four wireframes in `docs/wireframes/`.

## What Changes

- **Live clock.** The `ClockFace` text is bound to a `current-time` property fed by a `slint::Timer` on main (1 s tick) formatting `Local::now()`. The date/day label above the clock is likewise live. The clock face becomes **analog** (hour/minute/second hands) per the wireframes, with hand angles derived from the current time.
- **Four-panel scaffold.** `PanelContainer` (slice 0, one panel) is extended to four panels — Clock, Daily-data, Media, Settings — with horizontal swipe navigation (hard stops, no wraparound, slice 0's behavior preserved) and nav-dots + panel-tab indicators per the wireframes. Daily-data, Media, and Settings panels are **structural placeholders** populated by later slices (weather/calendar/media-player/web-config); this slice defines the panel slots and the `Card`/`Button`/`ClockFace` component contracts only.
- **Runtime theme system.** A `Theme` model (`{ name, light: TokenSet, dark: TokenSet, variant: ComponentVariants }`) is selected at runtime (not compile time). Slint cannot swap `.slint` components at runtime, so "component variants" are **parameterized components** — one `Card`, one `Button`, one `ClockFace` — whose visual properties (background, border, shadow, radius, hand color) are bound to `in-property` values fed by the active `TokenSet` + variant descriptor. The two built-in themes — **Liquid Glass** (glassmorphism: translucent fills, 1px translucent borders, soft drop shadows, accent `#ff6b6b`) and **Neuromorphic** (neumorphism: same-bg dual extrusion shadows, no borders, accent `#ff6b6b`) — are encoded as two `TokenSet`+variant-descriptor pairs. Dark variants are a second `TokenSet` per theme. A `ThemeController` on main holds the active theme + mode and pushes token values into the Slint root properties each tick.
- **Theme mode.** `Manual-Light | Manual-Dark | Follow-Bedtime` (default `Follow-Bedtime`). `Follow-Bedtime` defers to the display-policy slice (slice 4) for the bedtime window; until then it behaves as `Manual-Dark` during 22:00–06:00 local and `Manual-Light` otherwise (a temporary heuristic, replaced by slice 4).
- **Settings panel (touch-native subset).** The Settings panel exposes theme selection (tap to cycle Liquid Glass / Neuromorphic) and mode selection, persisted via `ConfigStore`. Other settings cards (alarms summary, display summary) are read-only placeholders populated by later slices.

## Non-goals

- Weather, calendar, media content (later slices populate the panel slots).
- Custom-theme upload UI (v2 per PRD).
- Gradual pre-bedtime theme dimming (v2).
- "Follow-ambient" theme mode (v2).
- The alarm overlay (`AlarmPanel`) visual restyle — it remains slice-2's layout but adopts the active theme's tokens (a MOD to `AlarmPanel` binding `clock-color`/`font-family` to the theme controller's values). No snooze/dismiss behavior change.

## Capabilities

### New Capabilities
- `theming`: runtime `Theme`/`TokenSet`/variant model, two built-in themes (Liquid Glass, Neuromorphic) × light/dark, mode selection (`Manual-Light`/`Manual-Dark`/`Follow-Bedtime`), runtime swap, persistence of selection.

### Modified Capabilities
- `ui-shell`: live clock (analog hands + date label), four-panel scaffold with nav-dots/tabs, `Card`/`Button`/`ClockFace` component contracts bound to theme tokens, `AlarmPanel` bound to active theme.

## Impact

- **New code:** `alarm-clock/src/theme.rs` (`Theme`, `TokenSet`, `ThemeController`); `alarm-clock/ui/ClockFace.slint`, `Card.slint`, `Button.slint` extracted from `ui.slint`; panel-tab/nav-dot components. A `slint::Timer` (1 s) feeding `current-time`.
- **Modified code:** `alarm-clock/ui.slint` (four panels, theme-root properties), `alarm-clock/AlarmPanel.slint` (token binding), `alarm-clock/src/main.rs` (theme controller wiring, settings persistence), `alarm-clock/src/config.rs` / persistence (theme + mode columns in `kv_config`).
- **Wireframes:** `docs/wireframes/{liquid-glass,neuromorphic,dark-liquid-glass,dark-neuromorphic}/index.html` are the visual source of truth.

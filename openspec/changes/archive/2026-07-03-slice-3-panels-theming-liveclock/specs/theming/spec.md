## ADDED Requirements

### Requirement: Theme model with tokens and component variants
The application SHALL model a theme as `Theme { name, light: TokenSet, dark: TokenSet, variant: ComponentVariant }` where `TokenSet` carries the colors/radii/shadow parameters and `ComponentVariant` is one of `LiquidGlass` or `Neuromorphic`. Themes SHALL be selectable at **runtime** (not compile time). The active theme's token values SHALL be pushed into Slint root `in-property` values (background, card-background, card-border, card-shadow, text-color, accent-color, clock-hand-color, radius, font-family) so that `Card`, `Button`, and `ClockFace` render in the active variant without recompilation.

#### Scenario: Switching theme at runtime re-renders all components
- **WHEN** the user selects "Neuromorphic" in the Settings panel while the app is running
- **THEN** every `Card`, `Button`, and `ClockFace` on screen re-renders with the neumorphic visual language (same-bg dual extrusion shadows, no borders, accent `#ff6b6b`) within one Slint tick, without a process restart

#### Scenario: Liquid Glass renders glassmorphism
- **WHEN** the active theme is Liquid Glass (light)
- **THEN** cards render with semi-transparent fills, a 1px translucent border, soft drop shadows, and the deep-blue/purple gradient background per `docs/wireframes/liquid-glass`

#### Scenario: Dark variant is a token swap within a theme
- **WHEN** the mode transitions from light to dark within the Liquid Glass theme
- **THEN** only the `TokenSet` swaps (background → black `#000`, card fills darken, text stays white); the `ComponentVariant` (glassmorphism technique) is unchanged

### Requirement: Two built-in themes hew to the wireframes
The application SHALL ship two built-in themes — **Liquid Glass** and **Neuromorphic** — each with a light and dark `TokenSet`, visually matching the four wireframes in `docs/wireframes/`. Liquid Glass SHALL use translucent fills + borders + soft shadows (glassmorphism); Neuromorphic SHALL use same-background dual extrusion shadows with no borders (neumorphism). Both SHALL use accent `#ff6b6b` for the second hand and calendar/active times.

#### Scenario: Liquid Glass light matches its wireframe
- **WHEN** the active theme is Liquid Glass light
- **THEN** the clock panel background is the deep-blue/purple gradient, cards are translucent with backdrop-blur simulation, and the visual matches `docs/wireframes/liquid-glass/index.html`

#### Scenario: Neuromorphic light matches its wireframe
- **WHEN** the active theme is Neuromorphic light
- **THEN** the background is `#e0e5ec`, cards share the background color with dual light/dark extrusion shadows and no borders, matching `docs/wireframes/neuromorphic/index.html`

#### Scenario: Dark variants match their wireframes
- **WHEN** the active theme is Liquid Glass dark (resp. Neuromorphic dark)
- **THEN** the visuals match `docs/wireframes/dark-liquid-glass/index.html` (resp. `dark-neuromorphic/index.html`)

### Requirement: Theme mode selection with Follow-Bedtime default
The application SHALL support three theme modes: `Manual-Light`, `Manual-Dark`, `Follow-Bedtime` (default `Follow-Bedtime`). The selected theme and mode SHALL persist across reboots via `ConfigStore`. `Follow-Bedtime` SHALL select dark during the bedtime window and light otherwise; until the display-policy slice (slice 4) lands, the bedtime window is a temporary 22:00–06:00 local heuristic.

#### Scenario: Manual-Light overrides Follow-Bedtime
- **WHEN** the user selects `Manual-Light` during the bedtime window
- **THEN** the theme renders in its light `TokenSet` regardless of the bedtime window

#### Scenario: Theme selection persists across reboot
- **WHEN** the user selects "Neuromorphic" + `Manual-Dark` and the process restarts
- **THEN** the app boots directly into Neuromorphic dark without reverting to defaults

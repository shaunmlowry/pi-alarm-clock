## Context

Slices 0–2 left the UI as a single Clock panel with a hardcoded `12:00` and hardcoded color/font feeders — the "theme seam" slice 0 deliberately reserved. Slice 3 fills that seam: a live analog clock, the four-panel scaffold, and a runtime theme system matching the four wireframes. The wireframes in `docs/wireframes/` are the visual source of truth: both themes share **identical panel layouts** — only the rendering technique differs.

## Goals / Non-Goals

**Goals:** live analog clock; four-panel scaffold with nav-dots/tabs; runtime-selectable `Theme` model with two built-in themes × light/dark; mode selection persisted; `Card`/`Button`/`ClockFace` contracts bound to theme tokens.

**Non-Goals:** weather/calendar/media content; custom-theme upload (v2); gradual dimming (v2); follow-ambient (v2); alarm-overlay layout change (only token binding).

## Decisions

### D1. Runtime theme swap via parameterized components, not compile-time variants
Slint cannot swap `.slint` files at runtime and has no CSS `backdrop-filter`. So "component variants" (PRD §Theming) are **one parameterized `Card`/`Button`/`ClockFace` each**, whose visual `in-property` values (background, border-color, border-width, shadow-blur, shadow-offset, shadow-color, radius, text-color, hand-color) are bound to a `TokenSet` pushed from Rust. A `ThemeController` on main holds the active `Theme` + mode and writes token values into the Slint root's `in-out` theme properties each tick (or on change).

- **Liquid Glass** token set: translucent fills (e.g. `rgba(255,255,255,0.1)`), 1px translucent borders, soft drop shadows, deep-blue/purple gradient background (light) / black (dark). Glassmorphism is *simulated* — no true backdrop blur; the translucent fill + border + shadow approximates the wireframe.
- **Neuromorphic** token set: card background == panel background (`#e0e5ec` light / `#1a1a1a` dark), no borders, **two layered drop-shadows** (light-offset + dark-offset) to fake extrusion. Slint's `drop-shadow` filter takes offset/blur/color; two `Rectangle`-wrapped shadows or a single multi-shadow approximation.

**Rationale.** Compile-time variants would require recompiling to switch themes — the PRD and the user require runtime selection. Parameterized components are the only runtime-swap mechanism Slint offers. Simulating glassmorphism (rather than true blur) is acceptable: the wireframes are guidance, and the Pi's display is opaque anyway.

**Alternatives.** *Token-only recolor* — rejected by the PRD (glass vs neumorphic are different techniques, not recolors). *Multiple compiled `ClockFace` variants switched by index* — rejected (no runtime component swap in Slint).

### D2. Analog clock from hand angles, 1 s tick
`ClockFace` exposes `in-property <float> hour-angle` / `minute-angle` / `second-angle` (degrees). A `slint::Timer` on main (1 s, panic-isolated per D6) reads `Local::now()` and computes the angles (second = `s*6`, minute = `(m*60+s)*0.1`, hour = `((h%12)*3600+m*60+s)*0.00833`). Date/day label fed from the same tick. The 1 s granularity matches the wireframe (second hand ticks).

### D3. Four panels in one PanelContainer; nav-dots + tabs
`PanelContainer` (slice 0) gains a `max-panels: 4` and four child panels. Horizontal swipe logic (slice 0) unchanged — hard stops self-adjust to 4. Nav-dots (bottom) and panel-tabs (top, on non-clock panels) are new components driven by `current-panel`. Daily-data/Media/Settings panels expose named empty `Card` slots (per the wireframe structure) for later slices to populate; this slice only lays out the slots.

### D4. Theme mode and the Follow-Bedtime placeholder
Mode = `Manual-Light | Manual-Dark | Follow-Bedtime` (default `Follow-Bedtime`). `Follow-Bedtime` resolves dark during the bedtime window. The bedtime window is owned by slice 4 (display-policy); until it lands, slice 3 uses a **temporary 22:00–06:00 local heuristic** so `Follow-Bedtime` works out of the box. Slice 4 replaces the heuristic by querying `DisplayController::is_bedtime(now)`.

### D5. Persistence of theme + mode
Theme name + mode stored as two `kv_config` keys (slice 0's `ConfigStore`), read at boot. No schema migration needed (kv_config is generic). Defaults: Liquid Glass + Follow-Bedtime.

### D6. Full-screen kiosk window (no compositor chrome)
The app is a single-purpose appliance: the `AppWindow` runs **full-screen, borderless, no title bar** on the Pi — no window-manager decoration. Slint exposes fullscreen via the backend's fullscreen flag; in **release** builds the window requests a true fullscreen always-on-top surface covering the entire display (no system bar). In **debug** builds it retains the 480×854 logical window so it's testable on a dev machine. The flag is a `cfg!(debug_assertions)` branch, mirroring slice 1's dev-only seeding pattern. The alarm overlay and all panels inherit full-screen coverage (they're children of the fullscreen `AppWindow`), so tap-anywhere-to-dismiss is never interrupted by chrome.

**Rationale.** A title bar / system bar would interrupt the tap-anywhere-to-dismiss alarm surface and break the appliance illusion. The PRD's "Standalone appliance interface" + "touch only" language implies no chrome.

## Risks / Trade-offs

- **[Simulated glassmorphism differs from the wireframe's true `backdrop-filter`]** → acceptable; the wireframe is guidance, the Pi display is opaque, and translucent fills + borders read as glassmorphism.
- **[1 s clock tick adds a third timer (drain 50 ms, scheduler 5 s, clock 1 s)]** → cheap (`Local::now()` + a few property writes); panic-isolated per D6.
- **[Neumorphic dual-shadow in Slint]** → Slint `drop-shadow` blur/offset/color must be verified to layer correctly; if it can't, fall back to a single inset-feel shadow. Spike needed in task 1.4.
- **[Theme switch mid-alarm-episode]** → the alarm overlay binds to the same theme root properties, so it re-renders with the new theme; no FSM interaction.

## Migration Plan

Additive: new `theme.rs`, new `.slint` components, `kv_config` entries. No migration; no breaking changes to slices 0–2. The placeholder `12:00` is removed (a deliberate, advertised change).

## Open Questions

- Does Slint's `drop-shadow` support two stacked shadows for neumorphism, or do we nest two shadow `Rectangle`s? (Spiked in task 1.4.)
- Should theme switching animate (fade) or be instant? Slice 3: instant; animation is a v2 polish.

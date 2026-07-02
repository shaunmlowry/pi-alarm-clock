## ADDED Requirements

### Requirement: Slint application with vertical orientation
The application SHALL render its UI using Slint, oriented vertically (9:16), touch-only, with no text input and no scrolling. The Slint event loop SHALL run on the main thread.

#### Scenario: Window renders
- **WHEN** the application has finished bootstrapping
- **THEN** a Slint window is visible in vertical orientation on the Pi touchscreen

### Requirement: Multi-panel navigation scaffold
The application SHALL provide a horizontal panel container that supports swiping between adjacent panels with hard stops at both ends (no wraparound). Slice 0 SHALL ship exactly one panel (the Clock panel); the scaffold SHALL allow additional panels to be added in later slices without modifying the navigation code. Vertical gestures on the Clock panel SHALL NOT be consumed by navigation (reserved for the future quick-controls overlay).

#### Scenario: Hard stop at the only panel
- **WHEN** the user swipes left or right on the Clock panel
- **THEN** no panel transition occurs (there is no adjacent panel in slice 0) and no wraparound happens

### Requirement: Clock panel with reserved theme seam
The Clock panel SHALL render a placeholder clock face. The Clock component SHALL expose theme-relevant properties (at minimum `clock_color` and `font_family`) that, in slice 0, are fed by hardcoded values. The properties SHALL be the seam a future theming slice swaps to a token-driven feeder without rewriting the Clock component.

#### Scenario: Clock renders with hardcoded theme values
- **WHEN** the Clock panel is displayed
- **THEN** the clock face is visible using the hardcoded `clock_color` and `font_family` values

#### Scenario: Theme seam is structural
- **WHEN** a future theming slice replaces the hardcoded feeder with a token system
- **THEN** the Clock component's internal structure does not need to change (only the source of the property values changes)

# ui-shell Specification

## Purpose
Slint-based vertical (9:16) touch-only UI shell for the Pi alarm clock. Hosts a four-panel navigation scaffold (Clock, Daily-data, Media, Settings) with nav-dots and panel-tabs, a live analog clock face, theme-token-bound `Card`/`Button` components, a touch-native Settings panel, a themed alarm overlay, and full-screen kiosk rendering on the Pi.
## Requirements
### Requirement: Slint application with vertical orientation
The application SHALL render its UI using Slint, oriented vertically (9:16), touch-only, with no text input and no scrolling. The Slint event loop SHALL run on the main thread.

#### Scenario: Window renders
- **WHEN** the application has finished bootstrapping
- **THEN** a Slint window is visible in vertical orientation on the Pi touchscreen

### Requirement: Clock panel with live analog clock face
The Clock panel SHALL render an **analog** clock face (hour/minute/second hands) with hand angles derived from the current local time, refreshed every second by a `slint::Timer` on main. A date/day label above the clock SHALL display the current weekday and date. The clock face, hands, and text SHALL be bound to the active theme's token values (clock-face background, hand color, accent second-hand color, font-family). The placeholder hardcoded `12:00` text SHALL be removed.

#### Scenario: Clock shows the real current time
- **WHEN** the app is running at 14:35:20 local
- **THEN** the hour hand points between 2 and 3, the minute hand points at 7, and the second hand points at 4, and the label reads the current weekday and date

#### Scenario: Clock updates every second
- **WHEN** one second elapses
- **THEN** the second hand advances by 6° without a process event beyond the 1 s timer tick

#### Scenario: Clock face adopts the active theme
- **WHEN** the active theme switches from Liquid Glass to Neuromorphic
- **THEN** the clock face background, hand colors, and accent re-render in the neumorphic variant within one tick

### Requirement: Four-panel navigation scaffold
The `PanelContainer` SHALL host four panels — Clock, Daily-data, Media, Settings — navigable by horizontal swipe with hard stops at both ends (no wraparound, slice 0 behavior preserved). Nav-dots at the bottom and panel-tabs at the top of non-clock panels SHALL indicate the active panel, matching the wireframes. **Touch-only, no scrolling, no text input** (slice 0 invariants) SHALL be preserved: vertical gestures are reserved for the quick-controls overlay (slice 7) and MUST NOT introduce scrolling; all interaction is touch-appropriate (tap, swipe). The Daily-data, Media, and Settings panels SHALL be structural placeholders exposing named `Card` slots populated by later slices (weather/calendar populate daily-data; media-player populates media; settings is populated here + web-config).

#### Scenario: Swipe navigates between panels with hard stops
- **WHEN** the user swipes left from the Clock panel
- **THEN** the Daily-data panel is shown and the second nav-dot becomes active; swiping left again advances to Media, then Settings; a further swipe does nothing (hard stop at panel 4)

#### Scenario: Nav-dots and panel-tabs reflect the active panel
- **WHEN** the Media panel is active
- **THEN** the third nav-dot is active (elongated) and the "Media" panel-tab is highlighted

#### Scenario: Daily-data and Media panels expose empty card slots
- **WHEN** the Daily-data panel is shown before the weather/calendar slices land
- **THEN** empty "Today's Agenda", "Weather Tomorrow", and "Current Conditions" card slots are visible (placeholder content), structured per the wireframe

### Requirement: Card and Button component contracts bound to theme tokens
The application SHALL define `Card` and `Button` Slint components whose visual properties (background, border, shadow, radius, text color) are `in-property` values bound to the active theme's token set, so that all panels render consistently in the active variant. These components SHALL be the sole building blocks for panel content.

#### Scenario: A Card renders in the active theme variant
- **WHEN** a Card is rendered on the Clock panel while the active theme is Liquid Glass
- **THEN** the Card uses the glassmorphism token set (translucent fill, 1px border, soft shadow); switching to Neuromorphic re-renders the same Card with extrusion shadows and no border

### Requirement: Settings panel touch-native subset
The Settings panel SHALL expose theme selection (tap to cycle Liquid Glass / Neuromorphic) and mode selection (`Manual-Light` / `Manual-Dark` / `Follow-Bedtime`), persisted via `ConfigStore`. Alarms-summary and display-summary cards SHALL be read-only placeholders (populated by later slices). No text entry is required on the Pi (themes/modes are tap-cycled).

#### Scenario: Tapping cycles the active theme
- **WHEN** the user taps the "Active Theme" row on the Settings panel
- **THEN** the theme cycles Liquid Glass → Neuromorphic → Liquid Glass and every panel re-renders immediately; the selection persists across reboot

#### Scenario: Tapping cycles the theme mode
- **WHEN** the user taps the "Mode" row
- **THEN** the mode cycles `Follow-Bedtime` → `Manual-Light` → `Manual-Dark` → `Follow-Bedtime` and the token set swaps accordingly; the selection persists

### Requirement: AlarmPanel adopts the active theme
The `AlarmPanel` (alarm overlay) SHALL bind its `clock-color`, `font-family`, card, and button visuals to the active theme's token values, so an alarm episode renders in the active variant. The snooze button and tap-anywhere-to-dismiss behavior (slice 2) are unchanged.

#### Scenario: Alarm overlay renders in the active theme
- **WHEN** an alarm fires while the active theme is Neuromorphic dark
- **THEN** the alarm overlay's clock face and snooze button render in the neumorphic dark variant

### Requirement: Full-screen kiosk window with no compositor chrome
The `AppWindow` SHALL run full-screen on the Raspberry Pi touchscreen with no window-manager decoration, title bar, or compositor chrome — this is a single-purpose appliance. The window SHALL cover the entire display (no borders, no system bar) and SHALL be the sole surface rendered. On development hosts the window SHALL still size to the 480×854 (9:16) logical dimensions for testing, but a release-build flag SHALL request a true full-screen, borderless, always-on-top surface on the Pi.

#### Scenario: Release build is full-screen borderless on the Pi
- **WHEN** the app runs in a release build on the Raspberry Pi
- **THEN** the Slint window covers the entire display with no title bar, no window borders, and no compositor chrome

#### Scenario: Dev build retains logical dimensions for testing
- **WHEN** the app runs in a debug build
- **THEN** the window opens at 480×854 logical pixels (the slice-0 dimensions) so it can be exercised on a developer machine without a fullscreen takeover

#### Scenario: Alarm overlay covers the full screen
- **WHEN** an alarm fires
- **THEN** the alarm overlay covers the entire display edge-to-edge (no title bar or border interrupts the tap-anywhere-to-dismiss surface)

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


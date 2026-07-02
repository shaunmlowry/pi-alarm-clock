## MODIFIED Requirements

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

## ADDED Requirements

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

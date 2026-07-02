## ADDED Requirements

### Requirement: Single backlight controller with a precedence stack
The application SHALL own a single `DisplayController` on the main thread that is the sole writer to the sysfs backlight (`brightness`) and power (`bl_power`) files. Each tick it SHALL compute the effective display policy from four inputs by precedence (highest first): (1) visual-alarm strobe, (2) bedtime off, (3) user brightness override, (4) dynamic brightness. Only the topmost active policy SHALL drive the hardware; lower-priority policies are masked. `bl_power` SHALL be used only for power state transitions (bedtime off, wake-on-touch); brightness modulation (strobe, dynamic, override) SHALL use `brightness` only.

#### Scenario: Visual strobe masks bedtime-off during an alarm
- **WHEN** an alarm is firing with `VisualConfig::On` during the bedtime window
- **THEN** the display is powered on and strobing; bedtime-off is masked for the episode

#### Scenario: User override does not defeat bedtime-off
- **WHEN** the user sets a brightness override during the bedtime window and no alarm is firing
- **THEN** the display stays powered off (bedtime-off outranks override); the override takes effect only after the bedtime window ends

### Requirement: Bedtime display power with wake-on-touch
The application SHALL support a global bedtime config with a weekday/weekend split — two `(start, end)` wall-clock `Time` windows, cross-midnight inferred when `end < start`. During bedtime the display SHALL be powered off (`bl_power` off). Any touch SHALL power the display on for a 10 s idle timer (reset by further touches). Entering the Settings panel or invoking the quick-controls overlay SHALL suspend the idle timer; exiting re-arms it. An alarm firing during bedtime SHALL suspend bedtime for the episode; on dismiss the display SHALL power off immediately but arm the 10 s wake-on-touch grace.

#### Scenario: Cross-midnight bedtime window powers off
- **WHEN** bedtime is `22:00`–`06:00` and the clock reaches 22:00
- **THEN** the display powers off until 06:00 or a touch

#### Scenario: Touch wakes for 10 s
- **WHEN** the display is off during bedtime and the user touches it
- **THEN** the display powers on showing the clock; after 10 s of no interaction it powers off again; further touches reset the timer

#### Scenario: Settings suspends the idle timer
- **WHEN** the user navigates to Settings during a bedtime wake
- **THEN** the 10 s timer is suspended; the display stays on while Settings is active; exiting Settings re-arms the timer

#### Scenario: Alarm during bedtime powers on, dismiss arms grace
- **WHEN** an alarm fires during bedtime and is later dismissed
- **THEN** the display is on for the episode; on dismiss it powers off immediately but the 10 s wake-on-touch grace is armed

### Requirement: Dynamic brightness from shortwave radiation
The idle-default brightness SHALL be derived from Open-Meteo `shortwave_radiation` (W/m²) fetched on the 30-min weather tick, mapped through a perceptual curve (gamma ~0.5) with a configurable floor (default 10%) and ceiling (100%). Transitions between brightness levels SHALL be interpolated over ~120 s. Until the weather slice (slice 5) lands, dynamic brightness SHALL default to a fixed 60%.

#### Scenario: Cloudy day dims the display
- **WHEN** the 30-min weather tick reports low `shortwave_radiation`
- **THEN** the idle brightness drops toward the floor over ~120 s

#### Scenario: Default before weather integration
- **WHEN** the weather slice has not landed
- **THEN** dynamic brightness is a fixed 60% (no crash, no missing-input error)

### Requirement: User brightness override with timeout
A user brightness override set via the quick-controls overlay's brightness slider (slice 7) SHALL take effect immediately (subject to precedence) and revert to auto after 30 min. The override SHALL NOT defeat bedtime-off.

#### Scenario: Override reverts after 30 min
- **WHEN** the user sets a brightness override to 80%
- **THEN** brightness is 80% (outside bedtime) for 30 min, then reverts to dynamic brightness

## ADDED Requirements

### Requirement: Visual alarm strobe with 10-second delay
Each alarm carries a `VisualConfig: Off | On { brightness, pulse_period, color }` (default `Off`). When `On`, the episode SHALL activate a brightness strobe — the clock UI stays rendered and its brightness is modulated between a floor and `visual_brightness` at `pulse_period` (default 1 s, color default white) — starting **10 s after fire** (audio starts first, visual joins later). The strobe SHALL run simultaneously with audio for the duration of the episode. `bl_power` SHALL NOT be used for strobing (only `brightness`).

#### Scenario: Visual joins 10 s after fire
- **WHEN** an alarm with `VisualConfig::On { brightness: 80, pulse_period: 1s, color: white }` fires
- **THEN** audio plays immediately; 10 s later the display begins strobing between a floor and 80% brightness at 1 s period

#### Scenario: Visual off means no strobe
- **WHEN** an alarm with `VisualConfig::Off` fires
- **THEN** no brightness strobe occurs (audio-only episode)

### Requirement: Forced visual is the terminal fallback
When the alarm's audio fallback chain is exhausted (the bundled beep also fails), the episode SHALL fire the visual alarm at full brightness as the terminal safety net. Silent failure is never acceptable: the visual SHALL activate even if `VisualConfig::Off` (forced override).

#### Scenario: Chain exhaustion forces full-brightness strobe
- **WHEN** the audio fallback chain (including the bundled beep) is exhausted during an episode
- **THEN** the display strobes at 100% brightness regardless of the alarm's `VisualConfig`, and the failure is logged

### Requirement: Backlight level captured and restored in the snapshot
The episode snapshot SHALL be extended to include the current `backlight_level` at fire time. On dismiss or `shutdown_restore`, the `backlight_level` SHALL be restored (in addition to Mopidy volume/repeat/shuffle/tracklist).

#### Scenario: Pre-alarm brightness restored on dismiss
- **WHEN** an alarm fires while the display is at 60% brightness and is dismissed
- **THEN** the display returns to 60% brightness (the captured `backlight_level`)

#### Scenario: Backlight restored on shutdown mid-episode
- **WHEN** the process receives SIGTERM mid-episode
- **THEN** `shutdown_restore` restores the Mopidy snapshot AND the captured `backlight_level` before exit

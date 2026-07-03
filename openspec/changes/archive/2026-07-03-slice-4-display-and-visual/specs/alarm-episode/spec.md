## MODIFIED Requirements

### Requirement: Episode FSM with Idle, Firing, Dismissed states
The application SHALL own a single-threaded episode state machine on the main thread with states `Idle`, `Escalating`, `Snoozing`, and `Dismissed` (slice 2). Slice 4 extends the `Escalating` state to coordinate with the `DisplayController`: on `fire`, when the alarm's `VisualConfig::On`, the FSM SHALL arm the 10 s visual-strobe activation with the `DisplayController`; on `dismiss`/`shutdown_restore`, the FSM SHALL cancel any pending strobe and restore the captured `backlight_level` via the `DisplayController`. When the audio fallback chain is exhausted, the FSM SHALL request the terminal forced full-brightness strobe from the `DisplayController`. The FSM does NOT write to sysfs directly — it goes through the `DisplayController`.

#### Scenario: Fire arms the 10 s visual activation
- **WHEN** an alarm with `VisualConfig::On` fires
- **THEN** the FSM arms a 10 s timer with the `DisplayController` to start the strobe; audio is already playing

#### Scenario: Dismiss cancels the strobe and restores backlight
- **WHEN** an alarm episode is dismissed while the visual strobe is active
- **THEN** the strobe is cancelled and the pre-alarm `backlight_level` is restored

#### Scenario: Chain exhaustion requests forced visual
- **WHEN** the audio fallback chain is exhausted
- **THEN** the FSM requests the `DisplayController` to fire the forced full-brightness strobe (terminal fallback)

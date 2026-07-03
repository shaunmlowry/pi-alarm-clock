## MODIFIED Requirements

### Requirement: Per-alarm snooze duration and cap
Each alarm SHALL carry `snooze_minutes` (default 10) and `max_snoozes` (default 3; 0 = snooze disabled). `EpisodeController::snooze` SHALL use the alarm's `snooze_minutes` duration and SHALL track the snooze count for the active episode. When the count reaches `max_snoozes`, snooze SHALL be disabled and the alarm overlay SHALL hide the Snooze button (only dismiss remains).

#### Scenario: Per-alarm snooze duration is used
- **WHEN** an alarm with `snooze_minutes = 5` is snoozed
- **THEN** the episode re-fires 5 minutes later

#### Scenario: Snooze disabled at cap
- **WHEN** an alarm with `max_snoozes = 2` has been snoozed twice
- **THEN** the Snooze button is hidden on the overlay and only Dismiss is available

#### Scenario: max_snoozes = 0 disables snooze entirely
- **WHEN** an alarm with `max_snoozes = 0` fires
- **THEN** no Snooze button is shown at any point during the episode

### Requirement: Snooze re-fire resets to primary source with a fresh snapshot
On snooze re-fire, the FSM SHALL re-capture a fresh Mopidy snapshot (the restored user session has advanced), replay the **primary** source (`source_uri`), and reset the fallback chain to the primary (chain resets on re-fire). The escalation clock SHALL remain alarm-active-time-based: it pauses during snooze and resumes from the last value on re-fire, never resetting to step 0. Volume climbs across successive snoozes.

#### Scenario: Re-fire captures a fresh snapshot
- **WHEN** a snoozed episode re-fires after the user advanced their media
- **THEN** the new snapshot reflects the current (advanced) Mopidy state, not the pre-snooze snapshot

#### Scenario: Re-fire resets the fallback chain to primary
- **WHEN** a snoozed episode re-fires after a fallback had advanced during the pre-snooze escalation
- **THEN** the primary source replays and the fallback index resets to the primary

#### Scenario: Escalation clock resumes across snoozes
- **WHEN** an alarm at escalation step 2 (volume 80) is snoozed and re-fires
- **THEN** escalation resumes from step 2 (not step 0); volume does not dip back to the step-0 value

### Requirement: Bundled beep is the terminal fallback; chain exhaustion forces visual
Every alarm's effective fallback chain SHALL end with a bundled local beep file (always the last element, appended automatically). When the beep also fails (chain fully exhausted), the episode SHALL request the forced full-brightness visual alarm (slice 4) and log the failure. Silent failure is never acceptable.

#### Scenario: Chain exhaustion forces visual at full brightness
- **WHEN** the primary source, all configured fallbacks, and the bundled beep all fail
- **THEN** the display strobes at 100% brightness (forced visual) and the failure is logged at `error!`

#### Scenario: Bundled beep is always appended
- **WHEN** an alarm with `fallback_chain = ["spotify:a", "file:///b"]` fires
- **THEN** the effective chain tried in order is `[source_uri, "spotify:a", "file:///b", <bundled beep>]`

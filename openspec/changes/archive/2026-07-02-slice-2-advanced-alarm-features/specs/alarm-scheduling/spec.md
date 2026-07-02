## ADDED Requirements

### Requirement: Scheduler tick advances escalation and re-fires snoozed episodes
The scheduler tick SHALL, after evaluating due alarms, invoke an `on_tick(now)` hook on the episode FSM. The episode FSM SHALL use this hook to (a) advance the volume through `escalation_steps` for an `Escalating` episode and (b) re-fire a `Snoozing` episode whose `snooze_until <= now`. The hook SHALL be non-blocking (fire-and-forget Mopidy commands) and SHALL not block the Slint event loop. An `on_tick` invoked while the FSM is `Idle` or `Dismissed` SHALL be a no-op.

#### Scenario: Scheduler tick advances the volume of an Escalating episode
- **WHEN** an `Escalating` episode is at step 0 and a scheduler tick fires after the step-1 `after_secs` boundary has elapsed
- **THEN** the FSM issues `set_volume(step_1.volume)` and updates its `step_index`

#### Scenario: Scheduler tick re-fires a snoozed episode whose snooze has elapsed
- **WHEN** the FSM is `Snoozing` and a scheduler tick observes `now >= snooze_until`
- **THEN** the FSM transitions back to `Escalating`, replays the alarm source, and the alarm overlay is re-shown

#### Scenario: on_tick while Idle is a no-op
- **WHEN** the scheduler tick invokes `on_tick` while the FSM is `Idle` or `Dismissed`
- **THEN** no Mopidy commands are issued and the FSM state is unchanged

### Requirement: EpisodeFsm seam gains an on_tick hook with a no-op default
The `EpisodeFsm` trait SHALL declare `fn on_tick(&mut self, now: DateTime<Local>)` with a default no-op implementation, so the scheduler can drive escalation/snooze refire through the existing adapter without breaking no-op or mock seam implementations.

#### Scenario: NoopEpisodeFsm compiles and on_tick is a no-op
- **WHEN** the scheduler calls `on_tick` on a `NoopEpisodeFsm`
- **THEN** the call returns without error and issues no commands

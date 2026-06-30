## 📝 [Alarm Clock] Slice 2: Advanced Alarm Features Tasks

***
**Disclaimer:** This file needs to be populated manually or by the automation engine during implementation, listing specific files and functions required for the changes detailed in `design.md`.

### Task Breakdown:
1.  **Model Update (alarm-clock/src/models.rs):** Implement updates to the core `Alarm` structure to support advanced timing logic (`escalation_steps`, `fallback_chain`). The `AlarmStore` must enforce these new fields upon write.
2.  **FSM Refactoring (alarm-clock/src/episode_controller.rs):** Rewrite the EpisodeController's state machine core to include `Escalating(step_index)` and `Snoozing`. This requires refactoring the main loop to manage multiple, distinct active processes simultaneously.
3.  **Scheduler Integration:** Update `scheduler_tick` consumer logic. Instead of just checking for an alarm fire, it must now evaluate time against the *next escalation step* definition.
4.  **Mopidy Channel Wrapper (mopidy-client/src/transport.rs):** Add high-level wrapper functions around Mopidy calls: `set_volume(volume: u8)` and `get_state()`. These must be fast, asynchronous commands that do not block the main thread while waiting for a reply on escalation advance.
5.  **Core Logic Function (alarm-clock/src/episode_manager.rs):** Implement the following critical functions:
    *   `calculate_next_step(elapsed_time: Duration, steps: &Vec<EscalationStep>) -> Option<(StepIndex, u8)?>`: Determines the required volume and step index for the current time.
    *   `handle_source_failure(current_state: &AlarmSnapshot)`: If failure detected, selects the next fallback source and triggers `MopidyChannelWrapper::set_volume()` immediately to continue escalation.
6.  **User Interaction (ui/main.slint):** Update the UI panel logic. When Firing, listen for a "Snooze" gesture. Implement a modal or overlay that calls the command to suspend the episode and sets the wakeup timer.

### Expected Changes:
- `alarm-clock/src/models.rs`: New structs/enums (e.g., `EscalationStep`).
- `mopidy-client/src/transport.rs`: New volume control methods.
- `alarm-clock/src/episode_controller.rs`: Massive refactoring of the state machine.

***
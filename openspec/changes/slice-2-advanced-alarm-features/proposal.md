# 🔴 [Alarm Clock] Slice 2: Advanced Alarm Features Proposal

## Goal
To evolve the basic alarm episode (established in Slice 1) from a simple play $\to$ dismiss cycle into a robust, time-sensitive, and feature-rich alarm sequence. This slice adds the intelligence layer governing how an alarm behaves after it is scheduled—implementing progressive volume escalation, handling multi-stage audio sources through fallbacks, and supporting snoozing ability.

## Problem Statement
Slice 1 successfully proved that an alarm can fire with minimal resources (single source, fixed volume). However, a real-world alarm clock requires:
1.  **Increasing urgency:** The noise level must escalate or change intensity over time to wake the user ("The siren effect").
2.  **Resilience:** If the primary audio source fails (e.g., Spotify URI expires, radio station drops), the system must automatically transition to a backup source without interrupting the core episode loop.
3.  **Human Behavior Modeling:** Users need the ability to 'hit snooze', requiring the FSM to temporarily suspend the alarm cycle and then correctly resume/restart the full escalation sequence from the previous level.

## Out of Scope (Non-Goals)
This slice contains no functionality for:
*   Full Calendar Integration (Holiday suppression, event alarms - S3+).
*   Visual Alarms/Brightness Strobe (Dedicated display controller logic and timing math).

## Acceptance Criteria Summary
1.  An alarm, when fired, must initiate a continuous, escalating volume increase through pre-configured steps ($escalation\_steps$).
2.  If the primary source is deemed non-functional during the grace period, the system must automatically advance to the next defined fallback source.
3.  The escalation and volume must continue uninterrupted across all fallbacks.
4.  A snooze operation must suspend the current escalation state ($\text{step } N$) and restore user media; upon re-fire, the escalation must resume from $\text{step } N$.

***
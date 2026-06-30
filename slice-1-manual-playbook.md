# Slice 1 Manual Playbook: Alarm Scheduling & Episode Flow

This document outlines the manual, end-to-end testing playbooks required for full feature validation of the alarm scheduling and episode management system. These steps validate overall integration and crucial failure modes and are essential for determining slice completion.

---

## ⚙️ Objectives Overview

The primary goal is to validate three major areas under realistic operating conditions:
1.  Successful alarm firing, Mopidy session snapshotting, and clean restoration (Success Path).
2.  Graceful degradation when core services (like Mopidy) are unavailable at the time of fire or shutdown (Failure Paths).
3.  Validation that all components interact correctly in a persistent environment like a Raspberry Pi (Integration/Acceptance).

---

## 📋 Task Group 14: `e2e-mopidy` Playbook (Tasks Coverage: 9.2 & 9.3)

**Objective:** Verify the core functionality of alarm firing, proper Mopidy session snapshotting, and graceful handling when playback fails at runtime.

### **Scenario A: Successful Alarm Trigger & Dismissal (Test Case 9.2)**
1.  **Setup:** Configure a test alarm in the development seed file (`alarms.toml`). Set its time to fire within a minute of testing start. Ensure the source URI is known and pre-tested with Mopidy.
2.  **Execution Steps:**
    *   Wait for the scheduled alarm to fire. Observe both the UI and logs.
3.  **Expected Outcome: State Verification**
    *   🟢 The dedicated Alarm Panel (`AlarmPanel.slint`) must appear on the screen.
    *   🟢 The correct URI must start playing, looping at `max_volume`.
    *   🟢 System status should be reliably marked as `Firing` in the internal state/logs.
4.  **Manual Interaction (Success Path):**
    *   Manually interact with the panel (e.g., tap anywhere to dismiss the alarm).
5.  **Expected Outcome: Restoration Verification**
    *   🟢 State transition must occur smoothly: `Firing` $\rightarrow$ `Dismissed`.
    *   🟢 The Mopidy session volume, repeat mode (`repeat`/`shuffle`), and the previously active tracklist must be flawlessly restored from the snapshot taken at fire time.

### **Scenario B: Failure Path - Mopidy Down (Test Case 9.3)**
1.  **Setup:** Reconfigure a test alarm to fire. **Crucially, stop the background Mopidy service** or use network tools to block external connections *before* the scheduled fire time.
2.  **Execution Steps:**
    *   Wait for the alarm to fire while `Mopidy` is confirmed offline/unreachable. Observe logs and UI.
3.  **Expected Outcome: Stability & Logging Verification**
    *   🟢 The scheduler must proceed without crashing, hanging, or generating a fatal error (must be highly fault-tolerant).
    *   🟢 Logs must show a structured error message indicating Mopidy service unavailability (`NotConnected`).
    *   🟢 Despite the failure to play audio, the UI must still be fully dismissable, proving that the alarm firing logic ran correctly and did not halt execution.

---

## 🛑 Task Group 15: `e2e-shutdown` Playbook (Task Coverage: 9.4)

**Objective:** Ensure that the application cannot leave resources dangling or corrupt system state—specifically validating recovery during process shutdown.

### **Scenario: Shutdown Mid-Episode/Firing State**
1.  **Setup (Pre-Condition):** Trigger an alarm firing sequence normally, allowing Mopidy to start playing. Wait until the internal state is confirmed as `Firing`.
2.  **Execution Step (Termination):** While the process is in the active/firing state, simulate a graceful system termination signal (e.g., running `systemctl stop [SERVICE]`, or sending **SIGTERM**). *Do not use SIGKILL.*
3.  **Expected Outcome: Clean Exit & State Recovery**
    *   🟢 The alarm system must be engineered to intercept the signal and execute `shutdown_restore()`.
    *   🟢 Logs must confirm that a restore operation was executed (it should log which snapshot parameters are being reinstated, even if nothing visually changes on screen).
    *   🟢 The process must exit cleanly with code 0.

---

## 🔬 Task Group 17: `pi-acceptance` Playbook (Task Coverage: 9.6)

**Objective:** High-level, continuous integration validation across the entire system state. This serves as a final smoke test on target hardware.

| Component | Check/Action | Expected Output / Verification Target |
| :--- | :--- | :--- |
| **Scheduler Tick (Logging)** | Monitor `journalctl` at regular time intervals. | 🟢 Logs must show a structured span named `scheduler_tick`. <br> 🟢 This span must contain programmatically generated fields (`alarms_evaluated`, `fired`). |
| **Migration & Data Integrity** | Check the database schema after running all integration tests/migrations. | 🟢 Confirm that `user_version` is set to version `2`. <br> 🟢 The primary `alarms` table must be present and contain valid data from the seeding or explicit configuration process. |
| **Full Persistence Round-Trip** | Manually change a key alarm's schedule (e.g., changing its pattern). Restart the service/reboot the device. | 🟢 The new schedule parameters must persist across restarts. <br> 🟢 The `next_fire` calculation upon restart must accurately re-evaluate based on the updated, stored rules. |
***
**Summary:** These groups cover the most complex and failure-prone areas of the subsystem and are critical for accepting this slice's functionality beyond passing unit tests.
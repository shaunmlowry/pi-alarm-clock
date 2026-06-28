# [Raspberry Pi Alarm Clock] - PRD

## 1. Document Control

| Metadata | Details |
| :--- | :--- |
| **Author** | [Shaun Lowry] |
| **Status** | Draft |
| **Target Release** | Q[3] 2026 |

## 2. Executive Summary
### Problem Statement
* Alarm clock and media player software for Raspberry Pi
* There is no standalone raspberry pi software for making an alarm clock applicance

## 3. Scope & Boundaries
### In Scope
* Alarm clock functionality
* Audio media player
* Daily data panel (weather, agenda)

### Out of Scope (Non-Goals)
* Video or web browsing

## 5. Functional Requirements
### Tech stack
* Hardware is already set:
  - Raspberry Pi 5
  - JustBoom Amp Hat
  - Official Raspberry Pi 7 inch touchscreen

* Supporting software already installed:
  - Mopidy
  - Mopidy-spotify

* New tooling:
  - Rust toolchain
  - slint for UI

* Calendaring
  - Backed by Google Calendar

### Alarm Clock functionality
* Must be able to schedule an alarm for any time
* Must be able to schedule an arbitrary number of alarms
* Alarms may be one-off or repeat on a configurable schedule
* Schedules should be highly configurable
* Schedules should be aware of calendar events (national/regional/personal holidays etc.)
* Alarms can be visual, audio or both
* Audio alarms should be configurable to play a local sound, a track from spotify or an internet radio station
* Audio alarms should have configurable volume levels
  - This includes a schedule of escalating volume levels
* Audio alarms should have configurable fallbacks (e.g. failed internet radio->local sound)
* Visual alarms should have a configurable brightness

### Media Player
* Must be able to play audio from spotify (paid subscription)
* Must be able to play sounds from local hard drive or network storage
* Must be able to play audio from internet radio stations
  - Minimum: CBC Radio 1 Calgary and CKUA

### UX/UI
* A standalone appliance interface using the raw display hardware and GPU
* Built using slint/rust
* Touch only with no text input (no keyboard is attached, no room for virtual keyboard)
  - Use only touch-appropriate inputs
* Vertical (9:16)
* Themable
  - Initial themes:
    - Liquid Glass (see https://www.cssscript.com/demo/glassmorphism-analog-clock/ for styling inspiration)
    - Neuromorphic (see https://www.cssscript.com/demo/neumorphic-analog-clock/ for styling inspiration)
  - Themes should be able to declare a light and dark mode
* Multi-panel, swipable, each panel can have multiple cards
  - Initial panels
    - Clock and brief daily data (as defined by theme - analog/digital supported)
    - Detailed daily data (weather, agenda)
    - Media playback
    - Settings
* Simple, Elegant - no unnecessary on screen Requirements
* Alarms can be dismissed by touching any point on the screen
* Volume/playback controls invokable by swiping up on any screen

## 9. Risks & Open Questions
### Risks

### Open Questions


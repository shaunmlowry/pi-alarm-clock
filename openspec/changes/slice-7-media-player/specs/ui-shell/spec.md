## ADDED Requirements

### Requirement: Media panel and quick-controls overlay populate the panel scaffold
The Media panel (slot defined in slice 3) SHALL show a now-playing card (track + artist), a transport row (source-capability-adapted), and a favorites list (cap 8). The quick-controls overlay SHALL render above all panels on swipe-up with volume + brightness sliders + transport, dismissing on tap-outside/5 s idle. Both SHALL render in the active theme.

#### Scenario: Media panel shows now-playing and favorites
- **WHEN** the Media panel is shown and a track is playing
- **THEN** the now-playing card shows track + artist, the transport row shows source-appropriate controls, and the favorites list (≤8) is shown

#### Scenario: Quick-controls overlay renders above panels
- **WHEN** the user swipes up from the Clock panel
- **THEN** the overlay appears above the Clock panel with sliders + transport, themed, and dismisses on tap-outside or 5 s idle

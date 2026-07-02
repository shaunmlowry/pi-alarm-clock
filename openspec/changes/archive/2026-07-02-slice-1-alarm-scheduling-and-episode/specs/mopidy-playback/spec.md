## ADDED Requirements

### Requirement: Typed playback method surface
The `mopidy-client` crate SHALL expose typed wrappers (request struct + `call` + typed reply, following the slice-0 shape) for the Mopidy playback methods: `playback.play`, `playback.pause`, `playback.resume`, `playback.stop`, `playback.set_volume`, `playback.get_state`, `playback.get_time_position`. Each SHALL serialize to JSON-RPC `params` and deserialize the reply via `serde`.

#### Scenario: playback.play with a URI
- **WHEN** the episode FSM calls `playback.play(Some(uri))` over the client
- **THEN** a JSON-RPC request `{method: "playback.play", params: {uris: [uri]}}` is sent and the reply is acknowledged

#### Scenario: playback.get_state returns a typed state
- **WHEN** the episode FSM calls `playback.get_state`
- **THEN** the reply is deserialized into a `PlaybackState` enum (`Playing | Paused | Stopped`) and returned to the caller

#### Scenario: playback.get_time_position returns milliseconds
- **WHEN** the episode FSM calls `playback.get_time_position`
- **THEN** the reply is deserialized into a `u32` (milliseconds) and returned

#### Scenario: playback.set_volume clamps to 0..100
- **WHEN** the episode FSM calls `playback.set_volume(v)` with `v` in 0..100
- **THEN** the volume is set on Mopidy; a value outside 0..100 is clamped before sending

### Requirement: Typed tracklist method surface
The `mopidy-client` crate SHALL expose typed wrappers for the Mopidy tracklist methods: `tracklist.add`, `tracklist.set_repeat`, `tracklist.set_shuffle`. The client SHALL use `shuffle` naming (per PRD) and SHALL alias Mopidy's `random` if the Mopidy version exposes only `set_random`.

#### Scenario: tracklist.add with URIs
- **WHEN** the episode FSM calls `tracklist.add(vec![uri])`
- **THEN** a JSON-RPC request `{method: "tracklist.add", params: {uris: [uri]}}` is sent and the reply is acknowledged

#### Scenario: tracklist.set_repeat toggles repeat
- **WHEN** the episode FSM calls `tracklist.set_repeat(true)`
- **THEN** Mopidy's repeat is enabled and the alarm source loops

#### Scenario: tracklist.set_shuffle restores pre-alarm shuffle
- **WHEN** the episode FSM restores a snapshot with `shuffle=true`
- **THEN** `tracklist.set_shuffle(true)` is sent and Mopidy's shuffle matches the snapshot

### Requirement: Method calls fail gracefully when disconnected
When the Mopidy client is not in the `Connected` state, typed method calls SHALL return a `MopidyClientError::NotConnected` (a `thiserror` variant) rather than hanging. The episode FSM SHALL treat this error as "playback silently failed" (logged) and continue the episode (which remains dismissable).

#### Scenario: Call while disconnected returns NotConnected
- **WHEN** the episode FSM calls `playback.play` while the client is `Disconnected` or `BackingOff`
- **THEN** the call returns `Err(MopidyClientError::NotConnected)` immediately (no hang), the failure is logged, and the episode remains `Firing` and dismissable

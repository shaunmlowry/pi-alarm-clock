# mopidy-client Specification

## Purpose
TBD - created by archiving change slice-0-architecture-skeleton. Update Purpose after archive.
## Requirements
### Requirement: Reconnecting WebSocket JSON-RPC client
The application SHALL provide a `mopidy-client` crate that connects to Mopidy's WebSocket JSON-RPC frontend (`mopidy/http` + `mopidy/json`, default `ws://localhost:6680/mopidy/ws`) using `tokio-tungstenite`. The client SHALL communicate using JSON-RPC 2.0: outgoing requests as `{jsonrpc:"2.0", id, method, params}`, incoming messages dispatched on `id` (reply) versus presence of `method` (event).

#### Scenario: Successful connection
- **WHEN** Mopidy is reachable at the configured WebSocket URL
- **THEN** the client establishes a connection and publishes `MopidyConnectionState::Connected`

#### Scenario: JSON-RPC round-trip
- **WHEN** the client sends a `core.get_version` request and Mopidy replies
- **THEN** the typed reply is delivered to the caller

### Requirement: Indefinite reconnect with bounded backoff
The client SHALL reconnect automatically on disconnect with exponential backoff plus jitter (initial ~500ms, factor ~2, cap ~30s, ±20% jitter), retrying indefinitely. The application SHALL NOT block on Mopidy being reachable.

#### Scenario: Mopidy down at boot
- **WHEN** Mopidy is not reachable at boot time
- **THEN** the client enters `BackingOff`, retries indefinitely, and the rest of the application continues to run

#### Scenario: Reconnect after Mopidy restart
- **WHEN** Mopidy restarts after the client was connected
- **THEN** the client detects disconnection, enters backoff, and reconnects when Mopidy is reachable again, transitioning through `Connecting` to `Connected`

### Requirement: Connection-state signal
The client SHALL publish `MopidyConnectionState` on every transition via the reply channel to main. The states SHALL be `Disconnected`, `BackingOff { retry_in }`, `Connecting`, and `Connected`. Slice 0 SHALL NOT consume this signal beyond logging it; it exists for later slices' fallback-chain and mid-episode-restart logic.

#### Scenario: State transitions logged
- **WHEN** the client transitions between connection states
- **THEN** each transition is logged with the new state and the transition is published to main

### Requirement: Typed minimal method surface
The client SHALL implement typed wrappers for at least `core.get_version` and `core.get_state`. Each wrapper SHALL serialize a request struct, await the matched reply, and deserialize into a typed reply struct. The structure SHALL make adding further methods mechanical (a request struct, a `call` method, a typed reply).

#### Scenario: get_state returns playback state
- **WHEN** the client calls `core.get_state` while Mopidy is connected
- **THEN** a typed playback-state value is returned to the caller

### Requirement: Event channel
The client SHALL parse incoming JSON-RPC events (messages with a `method` field) into an `enum MopidyEvent` and forward them to the bounded event channel (per `process-runtime`). Slice 0 SHALL log every received event and otherwise ignore it.

#### Scenario: Event received and logged
- **WHEN** Mopidy emits an event (e.g. `playback_state_changed`)
- **THEN** the event is parsed into `MopidyEvent`, logged at `info!` or higher, and forwarded to the event channel


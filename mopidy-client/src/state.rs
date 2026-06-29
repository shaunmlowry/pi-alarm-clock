//! Mopidy connection state types (task 4.3).
//!
//! Defines [`MopidyConnectionState`] and the domain enum published on every
//! transition via the reply channel to main.

use std::time::Duration;

/// Connection state of the Mopidy WebSocket client.
///
/// Published on every state transition through the reply channel so that later
/// slices can branch on connectivity (fallback chain, mid-episode-restart).
#[derive(Debug, Clone)]
pub enum MopidyConnectionState {
    /// No active connection and not currently retrying.
    Disconnected,

    /// A previous attempt failed; waiting before the next retry.
    BackingOff { retry_in: Duration },

    /// Actively attempting to establish a WebSocket connection.
    Connecting,

    /// WebSocket connected; JSON-RPC session is live.
    Connected,
}

/// Message envelope used by the reconnect loop to publish state transitions
/// back to [`command_dispatcher`], which forwards them through the reply
/// channel as [`Reply::MopidyConnectionState`].
///
/// This type lives in **mopidy-client** so that alarm-clock's `Reply` variant is
/// the *only* place that references both crates; there is no circular dependency.
#[derive(Debug, Clone)]
pub enum MopidyConnectionEvent {
    /// The client has moved into a new connection state.
    ConnectionStateChange(MopidyConnectionState),
}

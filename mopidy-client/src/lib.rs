//! Mopidy WebSocket client library.
//!
//! Handles JSON-RPC 2.0 communication over WebSocket with reconnect logic.
//! Core RPC method wrappers and event parsing are layered on by later tasks.

pub mod transport;
pub mod state;

// Re-export for convenience in alarm-clock crate.
pub use state::MopidyConnectionState;

//! Mopidy WebSocket client library.
//!
//! Handles JSON-RPC 2.0 communication over WebSocket with reconnect logic.
//! Typed RPC method wrappers (`core.get_version`, `core.get_state`) and
//! connection-state signals are provided out of the box.

pub mod transport;
pub mod state;
pub mod methods;

// Re-export for convenience in alarm-clock crate.
pub use state::MopidyConnectionState;
pub use methods::{PlaybackState, VersionInfo};
pub use transport::MopidyEvent;

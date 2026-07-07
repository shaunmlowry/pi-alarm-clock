//! Web-server support modules (slice 8).
//!
//! The REST routes and command/reply types live in the top-level
//! [`crate::web_config`] module. This directory holds supporting pieces that
//! are easier to keep separate: TLS certificate management and (later) mDNS
//! advertisement.

pub mod mdns;
pub mod tls;

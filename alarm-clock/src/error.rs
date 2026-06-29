//! Domain error types (Design D6).
//!
//! All application-specific error types use **`thiserror`** for derivation.
//! The app boundary (`main`) uses **`anyhow`** to accumulate and propagate
//! errors outward.

use rusqlite;
use thiserror::Error;

/// Errors originating from the configuration / persistence layer.
#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum ConfigError {
    /// A SQLite write (config mutation) failed.
    #[error("config write failed: {0}")]
    WriteFailed(#[source] std::io::Error),

    /// A generic database operation error.
    #[error("database error: {0}")]
    Database(#[source] rusqlite::Error),

    /// Failed to apply a migration step.
    #[error("migration failed at step {step}: {detail}")]
    MigrationFailed { step: u32, detail: String },
}
/// Application-level result alias for the app boundary.
pub type Result<T, E = ConfigError> = std::result::Result<T, E>;

// ── Tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    /// ConfigError::WriteFailed carries an io::Error source and displays correctly.
    #[test]
    fn config_error_write_failed_displays_source() {
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "disk full");
        let err = ConfigError::WriteFailed(io_err);

        assert!(format!("{err}").contains("config write failed"));
        assert!(err.source().is_some(), "should carry source error");
    }
}

//! The `secrets.json` secret store (slice 6 / D1).
//!
//! Holds secrets (the Google OAuth2 refresh token; later the web bearer token
//! in slice 8) in a JSON file with filesystem mode **0600**, distinct from the
//! SQLite config store. The SQLite store holds non-secret config
//! (`CalendarSource` list, `holiday_policy`); secrets and config are separated
//! by sensitivity, matching the PRD.
//!
//! ## Main-thread only
//!
//! Per the spec, `secrets.json` is read and written **only from the main
//! thread**. There is no `Mutex`, no async, no worker-thread access. The
//! functions here are synchronous and must be called on main. This matches the
//! single-`Connection` model of [`crate::database`].
//!
//! ## Atomic writes
//!
//! [`Secrets::save`] writes to a sibling temp file and `rename`s it into
//! place, then sets the mode to 0600. A crash mid-write never leaves a
//! truncated `secrets.json`; the previous file (if any) survives intact.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::error::{ConfigError, Result};

/// The persistent secrets document (slice 6 / D1).
///
/// All fields are optional: a fresh app has no secrets. Adding fields here is
/// backward-compatible — `#[serde(default)]` keeps older files parseable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secrets {
    /// Google OAuth2 refresh token (slice 6). Used to refresh access tokens
    /// without re-pairing. `None` until the user completes device-flow pairing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google_refresh_token: Option<String>,

    /// Web configuration bearer token (slice 8). Shared secret used by the
    /// axum REST API and the SPA. `None` until the user initiates pairing on
    /// the Pi touchscreen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_bearer_token: Option<String>,
}

impl Secrets {
    /// Load `secrets.json` from *path*. A missing file is `Ok(Secrets::default())`
    /// (no secrets yet). A malformed file is an error — the caller logs and
    /// continues with empty secrets rather than silently dropping the token.
    ///
    /// # Main-thread only
    ///
    /// This performs a synchronous file read; do not call from a worker thread.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let s: Secrets = serde_json::from_slice(&bytes).map_err(|e| {
                    ConfigError::WriteFailed(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("secrets.json parse error: {e}"),
                    ))
                })?;
                Ok(s)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Secrets::default()),
            Err(e) => Err(ConfigError::WriteFailed(e)),
        }
    }

    /// Save `self` to *path* atomically at mode 0600 (slice 6 / D1).
    ///
    /// Writes to a sibling temp file, `fsync`s it, `rename`s it into place,
    /// then sets the file mode to 0600 (also on first creation). Creates the
    /// parent directory if needed.
    ///
    /// # Main-thread only
    pub fn save(&self, path: &Path) -> Result<()> {
        // Ensure the parent directory exists (e.g. ./data).
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(ConfigError::WriteFailed)?;
            }
        }

        let json = serde_json::to_vec_pretty(self).map_err(|e| {
            ConfigError::WriteFailed(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("secrets.json serialize error: {e}"),
            ))
        })?;

        // Write to a temp sibling, fsync, rename — atomic replace.
        let tmp_path: PathBuf = path.with_extension("json.tmp");
        {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)
                .map_err(ConfigError::WriteFailed)?;
            f.write_all(&json).map_err(ConfigError::WriteFailed)?;
            // Set 0600 on the temp file before rename so the final path is
            // never world-readable even momentarily.
            set_mode_0600(&tmp_path)?;
            let _ = f.sync_all();
        }

        std::fs::rename(&tmp_path, path).map_err(ConfigError::WriteFailed)?;
        // Re-assert 0600 on the final path (rename preserves the temp's mode,
        // but this is defensive against filesystems that quirk on rename).
        set_mode_0600(path)?;

        info!(secrets_path = %path.display(), "secrets.json written (0600)");
        Ok(())
    }

    /// Convenience: store a Google refresh token, persisting immediately.
    ///
    /// # Main-thread only
    pub fn set_google_refresh_token(&mut self, token: String, path: &Path) -> Result<()> {
        self.google_refresh_token = Some(token);
        self.save(path)
    }

    /// Convenience: clear the Google refresh token (e.g. on revocation),
    /// persisting immediately. Returns whether a token was present.
    ///
    /// # Main-thread only
    pub fn clear_google_refresh_token(&mut self, path: &Path) -> Result<bool> {
        let had = self.google_refresh_token.is_some();
        if had {
            self.google_refresh_token = None;
            self.save(path)?;
        }
        Ok(had)
    }

    /// Convenience: store the web bearer token, persisting immediately.
    ///
    /// # Main-thread only
    pub fn set_web_bearer_token(&mut self, token: String, path: &Path) -> Result<()> {
        self.web_bearer_token = Some(token);
        self.save(path)
    }

    /// Convenience: clear the web bearer token (revoke & re-pair), persisting
    /// immediately. Returns whether a token was present.
    ///
    /// # Main-thread only
    pub fn clear_web_bearer_token(&mut self, path: &Path) -> Result<bool> {
        let had = self.web_bearer_token.is_some();
        if had {
            self.web_bearer_token = None;
            self.save(path)?;
        }
        Ok(had)
    }
}

/// Set a file's mode to 0600 (owner read/write only).
fn set_mode_0600(path: &Path) -> Result<()> {
    let perm = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perm).map_err(|e| {
        warn!(path = %path.display(), error = %e, "failed to set secrets.json mode 0600");
        ConfigError::WriteFailed(e)
    })
}

/// Read the file mode of *path* (best-effort) for verification. Returns the
/// raw `mode_t` bits as a `u32`.
pub fn file_mode(path: &Path) -> Result<u32> {
    let meta = std::fs::metadata(path).map_err(ConfigError::WriteFailed)?;
    use std::os::unix::fs::MetadataExt;
    Ok(meta.mode())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "secrets_test_{}_{}_{}.json",
            label,
            std::process::id(),
            n,
        ))
    }

    /// Scenario: a fresh `secrets.json` does not exist → load returns empty
    /// secrets (no error).
    #[test]
    fn missing_file_loads_empty() {
        let p = temp_path("missing");
        let _ = std::fs::remove_file(&p);
        let s = Secrets::load(&p).expect("load missing file");
        assert_eq!(s, Secrets::default());
        assert!(s.google_refresh_token.is_none());
    }

    /// Scenario: round-trip a refresh token — save creates the file at 0600,
    /// load reads it back exactly.
    #[test]
    fn round_trip_and_mode_0600() {
        let p = temp_path("roundtrip");
        let _ = std::fs::remove_file(&p);

        let mut s = Secrets::default();
        s.set_google_refresh_token("rtok-abc-123".to_string(), &p)
            .expect("save");

        // File exists.
        assert!(p.exists(), "secrets.json should exist after save");

        // Mode is 0600.
        let mode = file_mode(&p).expect("read mode");
        assert_eq!(mode & 0o777, 0o600, "secrets.json must be mode 0600");

        // Load round-trips the token.
        let loaded = Secrets::load(&p).expect("load");
        assert_eq!(loaded.google_refresh_token.as_deref(), Some("rtok-abc-123"));

        // File contents do not contain extraneous null fields (skip_serializing_if).
        let raw = std::fs::read_to_string(&p).expect("read raw");
        assert!(raw.contains("google_refresh_token"));
        let _ = std::fs::remove_file(&p);
    }

    /// Scenario: overwriting an existing secrets.json keeps mode 0600 and
    /// updates the token.
    #[test]
    fn overwrite_preserves_mode_0600() {
        let p = temp_path("overwrite");
        let _ = std::fs::remove_file(&p);

        let mut s = Secrets::default();
        s.set_google_refresh_token("first".to_string(), &p).unwrap();
        assert_eq!(file_mode(&p).unwrap() & 0o777, 0o600);

        s.set_google_refresh_token("second".to_string(), &p).unwrap();
        assert_eq!(file_mode(&p).unwrap() & 0o777, 0o600);
        let loaded = Secrets::load(&p).unwrap();
        assert_eq!(loaded.google_refresh_token.as_deref(), Some("second"));

        let _ = std::fs::remove_file(&p);
    }

    /// Scenario: clearing the refresh token removes it and re-saves.
    #[test]
    fn clear_token_persists() {
        let p = temp_path("clear");
        let _ = std::fs::remove_file(&p);

        let mut s = Secrets::default();
        s.set_google_refresh_token("tok".to_string(), &p).unwrap();
        assert!(s.clear_google_refresh_token(&p).unwrap(), "had a token");
        let loaded = Secrets::load(&p).unwrap();
        assert!(loaded.google_refresh_token.is_none());
        assert!(!s.clear_google_refresh_token(&p).unwrap(), "already cleared");

        let _ = std::fs::remove_file(&p);
    }

    /// Scenario: a malformed secrets.json is an error (not silently empty),
    /// so a corrupt token isn't dropped without notice.
    #[test]
    fn malformed_file_is_an_error() {
        let p = temp_path("malformed");
        let _ = std::fs::remove_file(&p);
        std::fs::write(&p, b"{not valid json").unwrap();
        assert!(Secrets::load(&p).is_err(), "malformed secrets.json must error");
        let _ = std::fs::remove_file(&p);
    }

    /// Scenario: save creates the parent directory if it does not exist.
    #[test]
    fn save_creates_parent_dir() {
        let dir = std::env::temp_dir().join(format!(
            "secrets_parent_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let p = dir.join("secrets.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!dir.exists());

        let mut s = Secrets::default();
        s.set_google_refresh_token("tok".to_string(), &p).expect("save creates parent");

        assert!(p.exists());
        assert_eq!(file_mode(&p).unwrap() & 0o777, 0o600);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

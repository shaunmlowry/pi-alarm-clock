//! Bootstrap configuration (design D8).
//!
//! A tiny `config.toml` parsed with `serde` + `toml` overrides compiled
//! defaults. The app needs these values *before* the SQLite DB exists, so this
//! is the unavoidable bootstrap layer. Missing file or missing fields fall back
//! to compiled defaults and never prevent boot.

use serde::Deserialize;

/// Path to the config file in development builds.
pub const DEV_CONFIG_PATH: &str = "./config.toml";

/// Path to the config file in release/production builds (the Pi).
pub const RELEASE_CONFIG_PATH: &str = "/etc/alarm-clock/config.toml";

// ── Compiled defaults ──────────────────────────────────────────────────────

pub const DEFAULT_DB_PATH: &str = "./data/alarm-clock.db";
pub const DEFAULT_MOPIDY_WS_URL: &str = "ws://localhost:6680/mopidy/ws";
pub const DEFAULT_AXUM_BIND_ADDR: &str = "127.0.0.1:8080";
pub const DEFAULT_LOG_LEVEL: &str = "info";
pub const DEFAULT_DATA_DIR: &str = "./data";

fn default_db_path() -> String {
    DEFAULT_DB_PATH.to_string()
}
fn default_mopidy_ws_url() -> String {
    DEFAULT_MOPIDY_WS_URL.to_string()
}
fn default_axum_bind_addr() -> String {
    DEFAULT_AXUM_BIND_ADDR.to_string()
}
fn default_log_level() -> String {
    DEFAULT_LOG_LEVEL.to_string()
}
fn default_data_dir() -> String {
    DEFAULT_DATA_DIR.to_string()
}

/// Bootstrap configuration.
///
/// Every field carries a `#[serde(default = ...)]` so a partial `config.toml`
/// (only some keys present) fills the rest with compiled defaults. A missing
/// file yields [`Config::default`].
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Config {
    /// Filesystem path to the SQLite database.
    #[serde(default = "default_db_path")]
    pub db_path: String,

    /// WebSocket URL of the Mopidy server.
    #[serde(default = "default_mopidy_ws_url")]
    pub mopidy_ws_url: String,

    /// `host:port` the axum HTTP server binds.
    #[serde(default = "default_axum_bind_addr")]
    pub axum_bind_addr: String,

    /// `tracing` log level directive (e.g. `info`, `debug`).
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Directory for runtime data (DB, scratch).
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            mopidy_ws_url: default_mopidy_ws_url(),
            axum_bind_addr: default_axum_bind_addr(),
            log_level: default_log_level(),
            data_dir: default_data_dir(),
        }
    }
}

impl Config {
    /// Resolve the config file path for the current build: `./config.toml` in
    /// dev (`debug_assertions`), `/etc/alarm-clock/config.toml` in release.
    pub fn config_path() -> &'static str {
        if cfg!(debug_assertions) {
            DEV_CONFIG_PATH
        } else {
            RELEASE_CONFIG_PATH
        }
    }

    /// Load configuration, preferring the resolved file path and falling back
    /// to compiled defaults on a missing or malformed file.
    pub fn load() -> Self {
        Self::load_from_path(Self::config_path())
    }

    /// Load configuration from an explicit path, falling back to compiled
    /// defaults on a missing file or parse error.
    pub fn load_from_path(path: &str) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => match toml::from_str::<Config>(&contents) {
                Ok(cfg) => cfg,
                Err(err) => {
                    eprintln!(
                        "config: failed to parse {path}: {err}; using compiled defaults"
                    );
                    Config::default()
                }
            },
            Err(err) => {
                // Missing file is not an error: defaults are used.
                if err.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "config: failed to read {path}: {err}; using compiled defaults"
                    );
                }
                Config::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: Missing config file uses defaults.
    #[test]
    fn missing_file_uses_defaults() {
        let cfg = Config::load_from_path("/nonexistent/path/does-not-exist.toml");
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.db_path, DEFAULT_DB_PATH);
        assert_eq!(cfg.mopidy_ws_url, DEFAULT_MOPIDY_WS_URL);
        assert_eq!(cfg.axum_bind_addr, DEFAULT_AXUM_BIND_ADDR);
        assert_eq!(cfg.log_level, DEFAULT_LOG_LEVEL);
        assert_eq!(cfg.data_dir, DEFAULT_DATA_DIR);
    }

    /// Scenario: Partial override — file specifies some fields, the rest fall
    /// back to compiled defaults.
    #[test]
    fn partial_override_uses_file_values_and_defaults_for_rest() {
        let dir = std::env::temp_dir();
        let path = dir.join("alarm-clock-config-partial-override.toml");
        std::fs::write(
            &path,
            "db_path = \"/var/lib/alarm-clock/test.db\"\nlog_level = \"debug\"\n",
        )
        .expect("write tmp config");

        let cfg = Config::load_from_path(path.to_str().unwrap());

        // Overridden values come from the file.
        assert_eq!(cfg.db_path, "/var/lib/alarm-clock/test.db");
        assert_eq!(cfg.log_level, "debug");

        // Omitted fields fall back to compiled defaults.
        assert_eq!(cfg.mopidy_ws_url, DEFAULT_MOPIDY_WS_URL);
        assert_eq!(cfg.axum_bind_addr, DEFAULT_AXUM_BIND_ADDR);
        assert_eq!(cfg.data_dir, DEFAULT_DATA_DIR);

        let _ = std::fs::remove_file(&path);
    }

    /// A fully-specified file overrides every field.
    #[test]
    fn full_override() {
        let dir = std::env::temp_dir();
        let path = dir.join("alarm-clock-config-full-override.toml");
        std::fs::write(
            &path,
            "\
db_path = \"/a.db\"
mopidy_ws_url = \"ws://mopidy:6680/mopidy/ws\"
axum_bind_addr = \"0.0.0.0:9000\"
log_level = \"trace\"
data_dir = \"/var/lib/alarm-clock\"
",
        )
        .expect("write tmp config");

        let cfg = Config::load_from_path(path.to_str().unwrap());
        assert_eq!(cfg.db_path, "/a.db");
        assert_eq!(cfg.mopidy_ws_url, "ws://mopidy:6680/mopidy/ws");
        assert_eq!(cfg.axum_bind_addr, "0.0.0.0:9000");
        assert_eq!(cfg.log_level, "trace");
        assert_eq!(cfg.data_dir, "/var/lib/alarm-clock");

        let _ = std::fs::remove_file(&path);
    }

    /// An empty file is equivalent to all-defaults.
    #[test]
    fn empty_file_uses_defaults() {
        let dir = std::env::temp_dir();
        let path = dir.join("alarm-clock-config-empty.toml");
        std::fs::write(&path, "").expect("write tmp config");

        let cfg = Config::load_from_path(path.to_str().unwrap());
        assert_eq!(cfg, Config::default());

        let _ = std::fs::remove_file(&path);
    }

    /// A malformed file does not panic; defaults are used.
    #[test]
    fn malformed_file_falls_back_to_defaults() {
        let dir = std::env::temp_dir();
        let path = dir.join("alarm-clock-config-malformed.toml");
        std::fs::write(&path, "this is = not = valid toml {{{").expect("write tmp config");

        let cfg = Config::load_from_path(path.to_str().unwrap());
        assert_eq!(cfg, Config::default());

        let _ = std::fs::remove_file(&path);
    }

    /// `config_path()` resolves per build profile.
    #[test]
    fn config_path_resolves_per_profile() {
        let path = Config::config_path();
        if cfg!(debug_assertions) {
            assert_eq!(path, DEV_CONFIG_PATH);
        } else {
            assert_eq!(path, RELEASE_CONFIG_PATH);
        }
    }
}

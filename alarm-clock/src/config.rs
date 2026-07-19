//! Bootstrap configuration (design D8).
//!
//! A tiny `config.toml` parsed with `serde` + `toml` overrides compiled
//! defaults. The app needs these values *before* the SQLite DB exists, so this
//! is the unavoidable bootstrap layer. Missing file or missing fields fall back
//! to compiled defaults and never prevent boot.
//!
//! ## Layered loading
//!
//! `Config::load()` reads two files and deep-merges them (later layers win):
//!
//! 1. The **base** config: `./config.toml` (dev) or `/etc/alarm-clock/config.toml`
//!    (release). This file is tracked in version control and documents every
//!    key — it must never contain secrets.
//! 2. The **local override**: `./config.local.toml` (dev) or
//!    `/etc/alarm-clock/config.local.toml` (release). This file is
//!    `.gitignore`d and is the intended home for secrets such as the
//!    PirateWeather API key.
//!
//! Either file may be absent (defaults fill the gaps); a malformed file is
//! logged and skipped rather than aborting boot.

use serde::Deserialize;

/// Path to the base config file in development builds.
pub const DEV_CONFIG_PATH: &str = "./config.toml";

/// Path to the base config file in release/production builds (the Pi).
pub const RELEASE_CONFIG_PATH: &str = "/etc/alarm-clock/config.toml";

/// Path to the local (gitignored, secrets) override in development builds.
pub const DEV_LOCAL_CONFIG_PATH: &str = "./config.local.toml";

/// Path to the local (secrets) override in release/production builds.
pub const RELEASE_LOCAL_CONFIG_PATH: &str = "/etc/alarm-clock/config.local.toml";

// ── Compiled defaults ──────────────────────────────────────────────────────

pub const DEFAULT_DB_PATH: &str = "./data/alarm-clock.db";
pub const DEFAULT_MOPIDY_WS_URL: &str = "ws://localhost:6680/mopidy/ws";
pub const DEFAULT_AXUM_BIND_ADDR: &str = "0.0.0.0:8080";
pub const DEFAULT_LOG_LEVEL: &str = "info";
pub const DEFAULT_DATA_DIR: &str = "./data";

pub const DEFAULT_WEATHER_CITY: &str = "Calgary";
pub const DEFAULT_OPEN_METEO_URL: &str = "https://api.open-meteo.com/v1/forecast";
pub const DEFAULT_PIRATEWEATHER_URL: &str = "https://api.pirateweather.net/forecast";

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

fn default_weather_city() -> String {
    DEFAULT_WEATHER_CITY.to_string()
}
fn default_open_meteo_url() -> String {
    DEFAULT_OPEN_METEO_URL.to_string()
}
fn default_pirateweather_url() -> String {
    DEFAULT_PIRATEWEATHER_URL.to_string()
}

/// Weather provider configuration.
///
/// All fields carry serde defaults so a partial or absent `[weather]` table
/// fills with compiled defaults. The `pirateweather_api_key` is `None` by
/// default — set it in the gitignored local override file so the secret is
/// never committed.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct WeatherConfig {
    /// Default city label used when seeding the DB on first boot.
    #[serde(default = "default_weather_city")]
    pub city: String,

    /// Default latitude. When `None` (the default), the city is geocoded at
    /// boot to resolve coordinates. When `Some`, the coordinates are used
    /// directly and no geocoding is performed.
    #[serde(default)]
    pub lat: Option<f64>,

    /// Default longitude. When `None` (the default), the city is geocoded at
    /// boot.
    #[serde(default)]
    pub lon: Option<f64>,

    /// Open-Meteo forecast API base URL (primary provider).
    #[serde(default = "default_open_meteo_url")]
    pub open_meteo_url: String,

    /// PirateWeather API base URL (fallback provider).
    #[serde(default = "default_pirateweather_url")]
    pub pirateweather_url: String,

    /// PirateWeather API key. When set, PirateWeather is used as a fallback
    /// whenever the primary (Open-Meteo) fetch fails. When `None`, only
    /// Open-Meteo is used.
    #[serde(default)]
    pub pirateweather_api_key: Option<String>,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            city: default_weather_city(),
            lat: None,
            lon: None,
            open_meteo_url: default_open_meteo_url(),
            pirateweather_url: default_pirateweather_url(),
            pirateweather_api_key: None,
        }
    }
}

// ── Calendar / Google OAuth config (slice 6) ──────────────────────────────

pub const DEFAULT_SECRETS_PATH: &str = "./data/secrets.json";
pub const DEFAULT_GOOGLE_OAUTH_DEVICE_URL: &str = "https://oauth2.googleapis.com/device/code";
pub const DEFAULT_GOOGLE_OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const DEFAULT_GOOGLE_CALENDAR_API_URL: &str = "https://www.googleapis.com/calendar/v3";

fn default_secrets_path() -> String {
    DEFAULT_SECRETS_PATH.to_string()
}
fn default_google_oauth_device_url() -> String {
    DEFAULT_GOOGLE_OAUTH_DEVICE_URL.to_string()
}
fn default_google_oauth_token_url() -> String {
    DEFAULT_GOOGLE_OAUTH_TOKEN_URL.to_string()
}
fn default_google_calendar_api_url() -> String {
    DEFAULT_GOOGLE_CALENDAR_API_URL.to_string()
}

/// Google Calendar / OAuth2 device-flow configuration (slice 6).
///
/// `client_id` and `client_secret` are obtained from the Google Cloud
/// console (an OAuth2 desktop-client credential). They are *not* highly
/// secret for an installed app, but storing them in the gitignored local
/// override keeps them out of version control. When `client_id` is `None`,
/// calendar features are disabled (no pairing is attempted).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CalendarConfig {
    /// Filesystem path to the `secrets.json` secret store (0600).
    #[serde(default = "default_secrets_path")]
    pub secrets_path: String,

    /// Google OAuth2 device-code endpoint.
    #[serde(default = "default_google_oauth_device_url")]
    pub oauth_device_url: String,

    /// Google OAuth2 token endpoint.
    #[serde(default = "default_google_oauth_token_url")]
    pub oauth_token_url: String,

    /// Google Calendar REST API base URL.
    #[serde(default = "default_google_calendar_api_url")]
    pub calendar_api_url: String,

    /// OAuth2 client ID. `None` disables calendar features.
    #[serde(default)]
    pub client_id: Option<String>,

    /// OAuth2 client secret. `None` when `client_id` is `None`.
    #[serde(default)]
    pub client_secret: Option<String>,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            secrets_path: default_secrets_path(),
            oauth_device_url: default_google_oauth_device_url(),
            oauth_token_url: default_google_oauth_token_url(),
            calendar_api_url: default_google_calendar_api_url(),
            client_id: None,
            client_secret: None,
        }
    }
}

/// Bootstrap configuration.
///
/// Every field carries a `#[serde(default = ...)]` so a partial `config.toml`
/// (only some keys present) fills the rest with compiled defaults. A missing
/// file yields [`Config::default`].
///
/// Note: `Eq` is intentionally not derived because [`WeatherConfig`] contains
/// `f64` (lat/lon), which is not `Eq`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
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

    /// Weather provider configuration (slice 5 + PirateWeather fallback).
    #[serde(default)]
    pub weather: WeatherConfig,

    /// Google Calendar / OAuth2 device-flow configuration (slice 6).
    #[serde(default)]
    pub calendar: CalendarConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            mopidy_ws_url: default_mopidy_ws_url(),
            axum_bind_addr: default_axum_bind_addr(),
            log_level: default_log_level(),
            data_dir: default_data_dir(),
            weather: WeatherConfig::default(),
            calendar: CalendarConfig::default(),
        }
    }
}

impl Config {
    /// Resolve the base config file path for the current build: `./config.toml`
    /// in dev (`debug_assertions`), `/etc/alarm-clock/config.toml` in release.
    pub fn config_path() -> &'static str {
        if cfg!(debug_assertions) {
            DEV_CONFIG_PATH
        } else {
            RELEASE_CONFIG_PATH
        }
    }

    /// Resolve the local (gitignored, secrets) override path for the current
    /// build: `./config.local.toml` in dev, `/etc/alarm-clock/config.local.toml`
    /// in release.
    pub fn local_config_path() -> &'static str {
        if cfg!(debug_assertions) {
            DEV_LOCAL_CONFIG_PATH
        } else {
            RELEASE_LOCAL_CONFIG_PATH
        }
    }

    /// Load configuration, deep-merging the base config file with the local
    /// (secrets) override. Missing files are silently skipped; a malformed
    /// file is logged and skipped. Compiled defaults fill any absent fields.
    pub fn load() -> Self {
        Self::load_layered(&[Self::config_path(), Self::local_config_path()])
    }

    /// Load configuration from a single explicit path, falling back to
    /// compiled defaults on a missing file or parse error.
    pub fn load_from_path(path: &str) -> Self {
        Self::load_layered(&[path])
    }

    /// Load and deep-merge multiple TOML config files. Later paths override
    /// earlier ones (per-key, recursively for tables). Missing files are
    /// silently skipped; a malformed file is logged and skipped. Compiled
    /// serde defaults fill any fields absent from every layer.
    pub fn load_layered(paths: &[&str]) -> Self {
        let mut merged: toml::Table = toml::Table::new();
        for path in paths {
            match std::fs::read_to_string(path) {
                Ok(contents) => match toml::from_str::<toml::Table>(&contents) {
                    Ok(table) => merge_tables(&mut merged, table),
                    Err(err) => eprintln!(
                        "config: failed to parse {path}: {err}; skipping this layer"
                    ),
                },
                Err(err) => {
                    // Missing file is not an error: the layer is optional.
                    if err.kind() != std::io::ErrorKind::NotFound {
                        eprintln!(
                            "config: failed to read {path}: {err}; skipping this layer"
                        );
                    }
                }
            }
        }

        // Re-serialize the merged table and parse into `Config` so that serde
        // `#[serde(default = ...)]` fills any absent fields. (Round-tripping
        // through a string uses only the public `toml` API and is cheap for a
        // tiny config.)
        let merged_str = match toml::to_string(&toml::Value::Table(merged)) {
            Ok(s) => s,
            Err(err) => {
                eprintln!("config: failed to re-serialize merged config: {err}; using compiled defaults");
                return Config::default();
            }
        };
        match toml::from_str::<Config>(&merged_str) {
            Ok(cfg) => cfg,
            Err(err) => {
                eprintln!(
                    "config: failed to deserialize merged config: {err}; using compiled defaults"
                );
                Config::default()
            }
        }
    }
}

// Recursively merge `overlay` into `base`. For keys present in both:
/// - if both values are tables, recurse;
/// - otherwise the overlay value replaces the base value.
fn merge_tables(base: &mut toml::Table, overlay: toml::Table) {
    for (key, val) in overlay {
        if let toml::Value::Table(overlay_tbl) = val {
            if let Some(existing) = base.get_mut(&key) {
                if let toml::Value::Table(b) = existing {
                    merge_tables(b, overlay_tbl);
                } else {
                    *existing = toml::Value::Table(overlay_tbl);
                }
            } else {
                base.insert(key, toml::Value::Table(overlay_tbl));
            }
        } else {
            base.insert(key, val);
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

    /// `config_path()` and `local_config_path()` resolve per build profile.
    #[test]
    fn config_paths_resolve_per_profile() {
        if cfg!(debug_assertions) {
            assert_eq!(Config::config_path(), DEV_CONFIG_PATH);
            assert_eq!(Config::local_config_path(), DEV_LOCAL_CONFIG_PATH);
        } else {
            assert_eq!(Config::config_path(), RELEASE_CONFIG_PATH);
            assert_eq!(Config::local_config_path(), RELEASE_LOCAL_CONFIG_PATH);
        }
    }

    /// The `[weather]` table defaults to compiled values when absent.
    #[test]
    fn weather_defaults_when_absent() {
        let dir = std::env::temp_dir();
        let path = dir.join("alarm-clock-config-weather-absent.toml");
        std::fs::write(&path, "log_level = \"debug\"\n").expect("write tmp config");

        let cfg = Config::load_from_path(path.to_str().unwrap());
        assert_eq!(cfg.weather, WeatherConfig::default());
        assert_eq!(cfg.weather.city, DEFAULT_WEATHER_CITY);
        assert!(cfg.weather.lat.is_none());
        assert!(cfg.weather.lon.is_none());
        assert_eq!(cfg.weather.open_meteo_url, DEFAULT_OPEN_METEO_URL);
        assert_eq!(cfg.weather.pirateweather_url, DEFAULT_PIRATEWEATHER_URL);
        assert!(cfg.weather.pirateweather_api_key.is_none());

        let _ = std::fs::remove_file(&path);
    }

    /// A `[weather]` table with only the API key still fills the rest with
    /// defaults (this is exactly the local-override use case).
    #[test]
    fn weather_partial_override_fills_defaults() {
        let dir = std::env::temp_dir();
        let path = dir.join("alarm-clock-config-weather-partial.toml");
        std::fs::write(
            &path,
            "[weather]\npirateweather_api_key = \"secret-test-key\"\n",
        )
        .expect("write tmp config");

        let cfg = Config::load_from_path(path.to_str().unwrap());
        assert_eq!(cfg.weather.pirateweather_api_key.as_deref(), Some("secret-test-key"));
        // Untouched fields keep defaults.
        assert_eq!(cfg.weather.city, DEFAULT_WEATHER_CITY);
        assert!(cfg.weather.lat.is_none());
        assert_eq!(cfg.weather.open_meteo_url, DEFAULT_OPEN_METEO_URL);

        let _ = std::fs::remove_file(&path);
    }

    /// A city-only `[weather]` table (no lat/lon) leaves the coordinates
    /// `None` so the app geocodes the city at boot.
    #[test]
    fn weather_city_only_has_no_coords() {
        let dir = std::env::temp_dir();
        let path = dir.join("alarm-clock-config-weather-city-only.toml");
        std::fs::write(&path, "[weather]\ncity = \"Edmonton\"\n").expect("write tmp config");

        let cfg = Config::load_from_path(path.to_str().unwrap());
        assert_eq!(cfg.weather.city, "Edmonton");
        assert!(cfg.weather.lat.is_none());
        assert!(cfg.weather.lon.is_none());

        let _ = std::fs::remove_file(&path);
    }

    /// A fully-specified `[weather]` table provides the coordinates directly
    /// (no geocoding needed).
    #[test]
    fn weather_with_coords_is_some() {
        let dir = std::env::temp_dir();
        let path = dir.join("alarm-clock-config-weather-with-coords.toml");
        std::fs::write(
            &path,
            "[weather]\ncity = \"Edmonton\"\nlat = 53.53\nlon = -113.5\n",
        )
        .expect("write tmp config");

        let cfg = Config::load_from_path(path.to_str().unwrap());
        assert_eq!(cfg.weather.city, "Edmonton");
        assert_eq!(cfg.weather.lat, Some(53.53));
        assert_eq!(cfg.weather.lon, Some(-113.5));

        let _ = std::fs::remove_file(&path);
    }

    /// A local override layer deep-merges over the base: the base's `[weather]`
    /// city is preserved while the overlay's API key is added.
    #[test]
    fn layered_local_override_merges() {
        let dir = std::env::temp_dir();
        let base_path = dir.join("alarm-clock-config-layered-base.toml");
        let local_path = dir.join("alarm-clock-config-layered-local.toml");
        std::fs::write(
            &base_path,
            "[weather]\ncity = \"Edmonton\"\nlat = 53.53\nlon = -113.5\n",
        )
        .expect("write base");
        std::fs::write(
            &local_path,
            "[weather]\npirateweather_api_key = \"override-key\"\n",
        )
        .expect("write local");

        let cfg = Config::load_layered(&[base_path.to_str().unwrap(), local_path.to_str().unwrap()]);
        // Base values preserved.
        assert_eq!(cfg.weather.city, "Edmonton");
        assert_eq!(cfg.weather.lat, Some(53.53));
        assert_eq!(cfg.weather.lon, Some(-113.5));
        // Overlay value applied.
        assert_eq!(cfg.weather.pirateweather_api_key.as_deref(), Some("override-key"));

        let _ = std::fs::remove_file(&base_path);
        let _ = std::fs::remove_file(&local_path);
    }

    /// A top-level key in the local override replaces the base value (non-table
    /// merge semantics).
    #[test]
    fn layered_local_override_replaces_scalar() {
        let dir = std::env::temp_dir();
        let base_path = dir.join("alarm-clock-config-layered-scalar-base.toml");
        let local_path = dir.join("alarm-clock-config-layered-scalar-local.toml");
        std::fs::write(&base_path, "log_level = \"info\"\n").expect("write base");
        std::fs::write(&local_path, "log_level = \"debug\"\n").expect("write local");

        let cfg = Config::load_layered(&[base_path.to_str().unwrap(), local_path.to_str().unwrap()]);
        assert_eq!(cfg.log_level, "debug");

        let _ = std::fs::remove_file(&base_path);
        let _ = std::fs::remove_file(&local_path);
    }
}

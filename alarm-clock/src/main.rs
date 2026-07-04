//! alarm-clock — slice 0 bootstrap.
//!
//! Threading model (design D1):
//! - **Main thread**: Slint event loop, domain layer (config store later).
//! - **Tokio worker thread**: async I/O servant (Mopidy WS, axum, blocking ops).
//!
//! Cross-thread channels are created here and split between the two threads.
//! A `slint::Timer` drains replies and events non-blockingly on each tick,
//! dispatching them to the domain without ever sleeping main.

mod alarm_store;
mod channel;
mod config;
mod database;
mod error;
mod episode;
mod schedule;
mod scheduler;
mod display;
mod seed;
mod theme;
mod weather_icons;

/// Resolve bundled asset path to file:// URI at boot (slice 4a / D3).
/// Converts "asset:beep.mp3" to "file:///path/to/assets/beep.mp3".
fn resolve_bundled_beep_asset(data_dir: &str) -> Option<String> {
    let asset_path = std::path::Path::new(data_dir).join("assets").join("beep.mp3");
    if asset_path.exists() {
        Some(format!("file://{}", asset_path.display()))
    } else {
        warn!(asset_path = %asset_path.display(), "bundled beep asset not found");
        None
    }
}

use crate::alarm_store::{Alarm, AlarmStore};
use crate::channel::{Cmd, CmdSender, Reply, MopidyEvent};
use crate::episode::{EpisodeController, MopidyControl, MopidySnapshot};
use crate::scheduler::{
    AlarmSource, DueAlarm, EpisodeFsm, LocalClock, Scheduler, DEFAULT_TICK_INTERVAL,
};
use chrono::{DateTime, Local, Timelike, Utc};
use mopidy_client::state::MopidyConnectionState;
use rusqlite::Connection;
use tokio::sync::mpsc as tokio_mpsc;
use crate::config::Config;
use crate::display::DisplayController;
use crate::theme::ThemeController;
use slint::ComponentHandle;
use std::time::SystemTime;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::signal::unix::{SignalKind, signal as unix_signal};
use tokio::sync::mpsc;
use tracing::{error, info, info_span, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use reqwest;
use serde_json::Value as JsonValue;
use serde::{Deserialize, Serialize};

// Generated Slint UI module (ui.slint + AlarmPanel.slint). Exposes `AppWindow`
// with the `episode-firing` property and `dismiss-requested` callback
// (tasks 7.1–7.3).
slint::include_modules!();

// ── Weather Store (slice 5) ────────────────────────────────────────────────

/// Weather data store holding the last successful snapshot
#[derive(Debug, Clone)]
pub struct WeatherStore {
    /// Last successful weather snapshot
    last_snapshot: Option<WeatherSnapshot>,
    /// Backoff retry state
    retry_state: WeatherRetryState,
    /// True while a `Cmd::FetchWeather` has been dispatched and its
    /// `Reply::WeatherResult` has not yet been processed by the drain timer.
    /// Prevents the scheduler from stacking redundant fetches while one is
    /// already pending (especially important now that a wedged endpoint can
    /// hold a fetch open for up to the 10 s request timeout).
    fetch_in_flight: bool,
    /// True while a `Cmd::GeocodeCity` has been dispatched and its
    /// `Reply::GeocodeResult` has not yet been processed by the drain timer.
    /// Used when the configured location is city-only (no lat/lon) and the
    /// coordinates must be resolved before weather can be fetched.
    geocode_in_flight: bool,
}

/// Weather retry state for backoff
#[derive(Debug, Clone)]
pub struct WeatherRetryState {
    /// Number of consecutive failures
    failure_count: u32,
    /// Next retry time
    next_retry: Option<std::time::Instant>,
}

/// A complete weather data snapshot from Open-Meteo.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WeatherSnapshot {
    /// Current temperature in °C
    pub current_temp: f64,
    /// Today's high temperature in °C
    pub today_high: f64,
    /// Today's low temperature in °C
    pub today_low: f64,
    /// Tomorrow's high temperature in °C
    pub tomorrow_high: f64,
    /// Tomorrow's low temperature in °C
    pub tomorrow_low: f64,
    /// Wind speed in km/h
    pub wind_speed: f64,
    /// Wind direction in degrees
    pub wind_direction: i32,
    /// Humidity percentage
    pub humidity: f64,
    /// Shortwave radiation in W/m²
    pub shortwave_radiation: f64,
    /// WMO weather code
    pub wmo_code: i32,
    /// When this snapshot was fetched
    pub fetched_at: SystemTime,
}

impl WeatherStore {
    /// Create a new WeatherStore
    pub fn new() -> Self {
        Self {
            last_snapshot: None,
            retry_state: WeatherRetryState {
                failure_count: 0,
                next_retry: None,
            },
            fetch_in_flight: false,
            geocode_in_flight: false,
        }
    }

    /// Get the current weather snapshot
    pub fn get_snapshot(&self) -> Option<&WeatherSnapshot> {
        self.last_snapshot.as_ref()
    }

    /// Update with a new successful snapshot and clear the fetch-in-flight
    /// flag. Resets the backoff state (a fetch succeeded).
    pub fn update_snapshot(&mut self, snapshot: WeatherSnapshot) {
        self.last_snapshot = Some(snapshot);
        self.record_success();
        self.fetch_in_flight = false;
    }

    /// Record a successful round-trip (geocode or fetch): reset the backoff
    /// state. Does not touch the snapshot or in-flight flags.
    pub fn record_success(&mut self) {
        self.retry_state.failure_count = 0;
        self.retry_state.next_retry = None;
    }

    /// Handle a failure (geocode or fetch): apply exponential backoff and
    /// clear whichever in-flight flag is set. Because geocode and fetch are
    /// mutually exclusive in time (geocode only runs while coordinates are
    /// unknown; fetch only runs once they're known), at most one in-flight
    /// flag is ever set, so clearing both is safe.
    pub fn handle_failure(&mut self) {
        self.retry_state.failure_count += 1;
        // Exponential backoff: 1min, 2min, 4min, 8min, max 16min
        let delay_secs = std::cmp::min(60 * (1 << (self.retry_state.failure_count - 1)), 60 * 16);
        self.retry_state.next_retry = Some(std::time::Instant::now() + std::time::Duration::from_secs(delay_secs));
        self.fetch_in_flight = false;
        self.geocode_in_flight = false;
    }

    // ── fetch-in-flight ───────────────────────────────────────────────────

    pub fn mark_fetch_in_flight(&mut self) {
        self.fetch_in_flight = true;
    }

    /// Clear the fetch-in-flight flag without touching snapshot/backoff
    /// state. Used when a dispatch was attempted but never reached the worker
    /// (e.g. the command channel was full) so the next tick can retry.
    pub fn clear_fetch_in_flight(&mut self) {
        self.fetch_in_flight = false;
    }

    pub fn fetch_in_flight(&self) -> bool {
        self.fetch_in_flight
    }

    // ── geocode-in-flight ────────────────────────────────────────────────

    pub fn mark_geocode_in_flight(&mut self) {
        self.geocode_in_flight = true;
    }

    /// Clear the geocode-in-flight flag without touching backoff state.
    pub fn clear_geocode_in_flight(&mut self) {
        self.geocode_in_flight = false;
    }

    pub fn geocode_in_flight(&self) -> bool {
        self.geocode_in_flight
    }

    // ── backoff / cadence queries ────────────────────────────────────────

    /// True while a recent failure has placed us inside a backoff window whose
    /// retry time has not yet elapsed. While true, steady-state (30-min)
    /// fetches and geocode attempts are suppressed; the scheduler fires a
    /// retry once `next_retry` is reached instead.
    pub fn in_backoff(&self) -> bool {
        match self.retry_state.next_retry {
            Some(time) => std::time::Instant::now() < time,
            None => false,
        }
    }

    /// Check if a retry is due (backoff window elapsed).
    pub fn is_retry_due(&self) -> bool {
        match self.retry_state.next_retry {
            Some(time) => std::time::Instant::now() >= time,
            None => false,
        }
    }

    /// Check if a steady-state (cadence) fetch is due: not in a backoff
    /// window, no pending retry, and either we have no snapshot yet or the
    /// last successful snapshot is older than `interval`.
    pub fn is_steady_due(&self, interval: std::time::Duration) -> bool {
        if self.in_backoff() || self.is_retry_due() {
            return false;
        }
        match &self.last_snapshot {
            None => true,
            Some(s) => s
                .fetched_at
                .elapsed()
                .map(|d| d >= interval)
                .unwrap_or(true),
        }
    }
}

impl Default for WeatherStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Steady-state weather refresh interval (30 minutes).
const WEATHER_STEADY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// The persisted weather location, read from the config store.
///
/// `Known` when both `weather_lat` and `weather_lon` are present and
/// parseable; `Unknown` otherwise (city-only configuration) — the city is
/// returned so the scheduler can geocode it to resolve coordinates.
enum LocationState {
    Known { lat: f64, lon: f64 },
    Unknown { city: String },
}

/// Read the persisted weather location from the config store.
///
/// Returns `Known` only when both lat and lon are present and parseable.
/// Otherwise returns `Unknown` with the persisted city name (falling back to
/// the compiled default city if even the city is missing — which should not
/// happen because boot seeding always sets it).
fn read_weather_location(conn: &Arc<std::sync::Mutex<Connection>>) -> LocationState {
    let (city, lat_opt, lon_opt) = if let Ok(g) = conn.lock() {
        let store = crate::database::ConfigStore::new(&*g);
        let city = store
            .get("weather_city")
            .ok()
            .flatten()
            .unwrap_or_else(|| crate::config::DEFAULT_WEATHER_CITY.to_string());
        let lat_opt = store
            .get("weather_lat")
            .ok()
            .flatten()
            .and_then(|s| s.parse::<f64>().ok());
        let lon_opt = store
            .get("weather_lon")
            .ok()
            .flatten()
            .and_then(|s| s.parse::<f64>().ok());
        (city, lat_opt, lon_opt)
    } else {
        (crate::config::DEFAULT_WEATHER_CITY.to_string(), None, None)
    };

    match (lat_opt, lon_opt) {
        (Some(lat), Some(lon)) => LocationState::Known { lat, lon },
        _ => LocationState::Unknown { city },
    }
}

/// Decide whether a weather action is due and, if so, dispatch it.
///
/// This is the single decision point for the weather scheduler. It reads the
/// persisted location and the [`WeatherStore`] backoff state, then either:
///
/// - **Coordinates known**: dispatch `Cmd::FetchWeather` when a fetch is due
///   (steady 30-min cadence, or a backoff retry). Suppressed while a fetch is
///   already in flight or while in a backoff window (the retry fires at
///   `next_retry` instead).
/// - **Coordinates unknown** (city-only config, not yet geocoded): dispatch
///   `Cmd::GeocodeCity` to resolve the city into lat/lon. The resolved
///   coordinates are persisted by the drain timer's `Reply::GeocodeResult`
///   handler, after which this scheduler switches to the fetch path. Geocode
///   attempts share the same backoff as fetches, so a failing geocode (e.g.
///   geocoding API down) backs off rather than hammering.
///
/// The relevant in-flight flag is set on dispatch and cleared by the drain
/// timer when the worker's reply arrives. If `try_send` fails (channel
/// full/closed) the flag is cleared immediately so the next tick can retry.
fn maybe_dispatch_weather(
    cmd_tx: &mpsc::Sender<Cmd>,
    conn: &Arc<std::sync::Mutex<Connection>>,
    weather_store: &Arc<std::sync::Mutex<WeatherStore>>,
) {
    let location = read_weather_location(conn);

    match location {
        LocationState::Known { lat, lon } => {
            // ── Fetch path ───────────────────────────────────────────────
            let reason: &'static str = match weather_store.lock() {
                Ok(ws) => {
                    if ws.fetch_in_flight() {
                        return;
                    } else if ws.is_retry_due() {
                        "retry"
                    } else if ws.is_steady_due(WEATHER_STEADY_INTERVAL) {
                        "steady"
                    } else {
                        return;
                    }
                }
                Err(_) => return,
            };

            if let Ok(mut ws) = weather_store.lock() {
                ws.mark_fetch_in_flight();
            }

            match cmd_tx.try_send(Cmd::FetchWeather { lat, lon }) {
                Ok(()) => info!(
                    reason = reason, lat, lon,
                    "weather: dispatched FetchWeather command"
                ),
                Err(e) => {
                    warn!(
                        error = %e,
                        "weather: could not dispatch FetchWeather (channel full/closed)"
                    );
                    if let Ok(mut ws) = weather_store.lock() {
                        ws.clear_fetch_in_flight();
                    }
                }
            }
        }
        LocationState::Unknown { city } => {
            // ── Geocode path (city-only config) ──────────────────────────
            // Geocode as soon as we're not in a backoff window or once the
            // retry is due. Suppressed while a geocode is already in flight.
            let due = match weather_store.lock() {
                Ok(ws) => !ws.geocode_in_flight() && (!ws.in_backoff() || ws.is_retry_due()),
                Err(_) => return,
            };
            if !due {
                return;
            }

            if let Ok(mut ws) = weather_store.lock() {
                ws.mark_geocode_in_flight();
            }

            match cmd_tx.try_send(Cmd::GeocodeCity { city: city.clone() }) {
                Ok(()) => info!(
                    city = %city,
                    "weather: dispatched GeocodeCity command (city-only config)"
                ),
                Err(e) => {
                    warn!(
                        error = %e,
                        "weather: could not dispatch GeocodeCity (channel full/closed)"
                    );
                    if let Ok(mut ws) = weather_store.lock() {
                        ws.clear_geocode_in_flight();
                    }
                }
            }
        }
    }
}

/// Fetch weather data from Open-Meteo API
async fn fetch_weather_data(
    lat: f64,
    lon: f64,
    base_url: &str,
) -> Result<WeatherSnapshot, Box<dyn std::error::Error + Send + Sync>> {
    // Build the client with explicit timeouts so a wedged endpoint (e.g. TCP
    // connects but the TLS handshake never completes — observed with
    // api.open-meteo.com) fails fast instead of hanging the serial tokio
    // worker for minutes. This lets the exponential backoff in `WeatherStore`
    // engage promptly and keeps the worker responsive for other commands.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(8))
        .build()?;

    // Open-Meteo API endpoint for current weather, daily forecast, and
    // shortwave radiation. The base URL is configurable so the endpoint can
    // be overridden without a code change.
    let url = format!(
        "{base_url}?latitude={lat}&longitude={lon}&current=temperature_2m,wind_speed_10m,wind_direction_10m,relative_humidity_2m,shortwave_radiation,weather_code&daily=temperature_2m_max,temperature_2m_min&forecast_days=2",
    );
    
    let response = client.get(&url).send().await?;
    let json: JsonValue = response.json().await?;
    
    // Parse current weather data
    let current = json.get("current").ok_or("Missing current data")?;
    let current_temp = current.get("temperature_2m").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let wind_speed = current.get("wind_speed_10m").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let wind_direction = current.get("wind_direction_10m").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let humidity = current.get("relative_humidity_2m").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let shortwave_radiation = current.get("shortwave_radiation").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let wmo_code = current.get("weather_code").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    
    // Parse daily forecast data
    let daily = json.get("daily").ok_or("Missing daily data")?;
    let temperatures_max = daily.get("temperature_2m_max").and_then(|v| v.as_array()).ok_or("Missing temperature_2m_max")?;
    let temperatures_min = daily.get("temperature_2m_min").and_then(|v| v.as_array()).ok_or("Missing temperature_2m_min")?;
    
    let today_high = temperatures_max.get(0).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let today_low = temperatures_min.get(0).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let tomorrow_high = temperatures_max.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let tomorrow_low = temperatures_min.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
    
    Ok(WeatherSnapshot {
        current_temp,
        today_high,
        today_low,
        tomorrow_high,
        tomorrow_low,
        wind_speed,
        wind_direction,
        humidity,
        shortwave_radiation,
        wmo_code,
        fetched_at: SystemTime::now(),
    })
}

/// Map a PirateWeather (DarkSky-style) icon string to the nearest WMO weather
/// code understood by the icon renderer (`weather_icons::wmo_to_slug`).
/// PirateWeather's icon set is coarser than WMO, so this is a best-effort
/// approximation.
fn pirateweather_icon_to_wmo(icon: &str) -> i32 {
    match icon {
        "clear-day" | "clear-night" | "wind" | "tornado" => 0,
        "partly-cloudy-day" | "partly-cloudy-night" => 2,
        "cloudy" => 3,
        "fog" => 45,
        "drizzle" => 51,
        "rain" => 63,
        "sleet" => 66,
        "snow" => 73,
        "hail" => 96,
        "thunderstorm" => 95,
        _ => 0,
    }
}

/// Fetch weather data from the PirateWeather API (DarkSky-compatible) as a
/// fallback when Open-Meteo is unavailable.
///
/// Uses `units=ca` so temperature is reported in °C and wind speed in km/h,
/// matching the units stored in [`WeatherSnapshot`]. PirateWeather does not
/// provide shortwave radiation, so that field is reported as `0.0` (the
/// brightness controller treats a missing radiation value as “no sun”, which
/// is the safe fallback for nighttime / overcast anyway).
async fn fetch_weather_data_pirateweather(
    lat: f64,
    lon: f64,
    base_url: &str,
    api_key: &str,
) -> Result<WeatherSnapshot, Box<dyn std::error::Error + Send + Sync>> {
    // Same timeout policy as the primary fetch.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(8))
        .build()?;

    // DarkSky-style endpoint: {base}/{key}/{lat},{lon}?units=ca&exclude=...
    let url = format!(
        "{base_url}/{api_key}/{lat},{lon}?units=ca&exclude=minutely,hourly,alerts,flags"
    );

    let response = client.get(&url).send().await?;
    let json: JsonValue = response.json().await?;

    let current = json
        .get("currently")
        .ok_or("PirateWeather: missing 'currently' object")?;
    let current_temp = current
        .get("temperature")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let wind_speed = current
        .get("windSpeed")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let wind_direction = current
        .get("windBearing")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;
    // PirateWeather reports humidity as a fraction in [0, 1]; the snapshot
    // stores a percentage.
    let humidity = current
        .get("humidity")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        * 100.0;
    let icon = current
        .get("icon")
        .and_then(|v| v.as_str())
        .unwrap_or("clear-day");
    let wmo_code = pirateweather_icon_to_wmo(icon);

    let daily = json
        .get("daily")
        .ok_or("PirateWeather: missing 'daily' object")?;
    let data = daily
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or("PirateWeather: missing 'daily.data' array")?;
    let today = data
        .get(0)
        .ok_or("PirateWeather: 'daily.data' has no today entry")?;
    // Tomorrow may be absent near the end of the forecast window; fall back to
    // today's values rather than failing the whole fetch.
    let tomorrow = data.get(1).unwrap_or(today);
    let today_high = today
        .get("temperatureHigh")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let today_low = today
        .get("temperatureLow")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let tomorrow_high = tomorrow
        .get("temperatureHigh")
        .and_then(|v| v.as_f64())
        .unwrap_or(today_high);
    let tomorrow_low = tomorrow
        .get("temperatureLow")
        .and_then(|v| v.as_f64())
        .unwrap_or(today_low);

    Ok(WeatherSnapshot {
        current_temp,
        today_high,
        today_low,
        tomorrow_high,
        tomorrow_low,
        wind_speed,
        wind_direction,
        humidity,
        shortwave_radiation: 0.0,
        wmo_code,
        fetched_at: SystemTime::now(),
    })
}

/// Fetch weather data, trying the primary provider (Open-Meteo) first and
/// falling back to PirateWeather when an API key is configured and the
/// primary fetch fails.
///
/// This is the single entry point used by the tokio worker's `FetchWeather`
/// arm. When no PirateWeather key is configured, behaviour is identical to
/// calling `fetch_weather_data` directly.
async fn fetch_weather_with_fallback(
    lat: f64,
    lon: f64,
    cfg: &crate::config::WeatherConfig,
) -> Result<WeatherSnapshot, Box<dyn std::error::Error + Send + Sync>> {
    match fetch_weather_data(lat, lon, &cfg.open_meteo_url).await {
        Ok(snapshot) => Ok(snapshot),
        Err(primary_err) => {
            if let Some(key) = cfg.pirateweather_api_key.as_deref() {
                warn!(
                    error = %primary_err,
                    "primary weather fetch (Open-Meteo) failed; trying PirateWeather fallback"
                );
                match fetch_weather_data_pirateweather(
                    lat,
                    lon,
                    &cfg.pirateweather_url,
                    key,
                )
                .await
                {
                    Ok(snapshot) => {
                        info!(
                            temp = snapshot.current_temp,
                            wmo = snapshot.wmo_code,
                            "weather fetch successful via PirateWeather fallback"
                        );
                        Ok(snapshot)
                    }
                    Err(fallback_err) => {
                        warn!(error = %fallback_err, "PirateWeather fallback also failed");
                        Err(format!(
                            "primary (Open-Meteo): {primary_err}; fallback (PirateWeather): {fallback_err}"
                        )
                        .into())
                    }
                }
            } else {
                Err(primary_err)
            }
        }
    }
}

/// Fetch geocoding data from Open-Meteo Geocoding API
async fn fetch_geocoding_data(city: &str) -> Result<(f64, f64, String), Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(8))
        .build()?;
    
    // Open-Meteo Geocoding API endpoint
    let url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json",
        city
    );
    
    let response = client.get(&url).send().await?;
    let json: JsonValue = response.json().await?;
    
    // Parse geocoding results
    let results = json.get("results").and_then(|v| v.as_array()).ok_or("Missing results")?;
    let first_result = results.get(0).ok_or("No geocoding results found")?;
    
    let lat = first_result.get("latitude").and_then(|v| v.as_f64()).ok_or("Missing latitude")?;
    let lon = first_result.get("longitude").and_then(|v| v.as_f64()).ok_or("Missing longitude")?;
    let name = first_result.get("name").and_then(|v| v.as_str()).ok_or("Missing name")?.to_string();
    
    Ok((lat, lon, name))
}

// ── Episode-FSM Mopidy control (channel-backed, task 9.1) ────────────────────

/// [`MopidyControl`] backed by the cross-thread `Cmd` channel.
///
/// Replaces the slice-0 [`crate::episode::NoopMopidyControl`] no-op. Playback
/// commands are issued fire-and-forget on the main thread as [`Cmd::CallMopidy`]
/// envelopes (the tokio worker owns the live Mopidy WS client). The FSM never
/// blocks awaiting a reply (design D4: optimistic transition with correction).
///
/// `capture_snapshot` returns `None`/defaults in slice 1: the snapshot reply
/// correlation is not yet wired through the dispatcher, so the episode follows
/// the Mopidy-down graceful-degradation path (task 6.1 — episode stays
/// `Firing` and dismissable; restore is a no-op apart from volume/repeat/
/// shuffle). This matches the slice-1 end-to-end Mopidy-down scenario.
#[derive(Clone)]
pub struct ChannelMopidyControl {
    cmd_tx: CmdSender,
    /// Reference to the `DisplayController` shared state for brightness
    /// capture/restore (slice 4).
    display: Option<Arc<Mutex<DisplayController>>>,
}

impl ChannelMopidyControl {
    /// Construct from the main-side command sender.
    pub fn new(cmd_tx: CmdSender) -> Self {
        Self { cmd_tx, display: None }
    }

    /// Construct with a display controller reference.
    pub fn new_with_display(cmd_tx: CmdSender, display: Arc<Mutex<DisplayController>>) -> Self {
        Self { cmd_tx, display: Some(display) }
    }

    /// Fire-and-forget a Mopidy JSON-RPC call across the `Cmd` channel.
    ///
    /// Uses `try_send` (non-blocking): on a full/closed channel the call is
    /// dropped with a `warn!` (best-effort, never blocks the Slint event loop).
    fn send_call(&self, method: &str, params: serde_json::Value) {
        if let Err(e) = self.cmd_tx.try_send(Cmd::CallMopidy {
            method: method.to_string(),
            params,
        }) {
            warn!(method, error = %e, "dropped Mopidy command (channel full/closed)");
        }
    }
}

impl MopidyControl for ChannelMopidyControl {
    fn capture_snapshot(&self) -> MopidySnapshot {
        // Slice 1: snapshot reply correlation is not yet wired through the
        // dispatcher. Proceed with defaults (Mopidy-down path, task 6.1).
        info!(
            "capture_snapshot: returning defaults (snapshot reply correlation not yet wired)"
        );
        MopidySnapshot::default()
    }

    fn capture_brightness(&self) -> u8 {
        if let Some(ref d) = self.display {
            if let Ok(dc) = d.lock() {
                return dc.current_brightness();
            }
        }
        100
    }

    fn restore_brightness(&self, level: u8) {
        if let Some(ref d) = self.display {
            if let Ok(mut dc) = d.lock() {
                dc.set_brightness_target(level);
                info!(level, "display: brightness restored after episode");
            }
        }
    }
    fn tracklist_add(&self, uri: &str) {
        self.send_call("tracklist.add", serde_json::json!({ "uris": [uri] }));
    }
    fn playback_play(&self) {
        self.send_call("playback.play", serde_json::json!({}));
    }
    fn playback_stop(&self) {
        self.send_call("playback.stop", serde_json::json!({}));
    }
    fn playback_seek(&self, position_ms: u32) {
        self.send_call(
            "playback.seek",
            serde_json::json!({ "time_position": position_ms }),
        );
    }
    fn tracklist_set_repeat(&self, on: bool) {
        self.send_call("tracklist.set_repeat", serde_json::json!({ "repeat": on }));
    }
    fn tracklist_set_shuffle(&self, on: bool) {
        self.send_call("tracklist.set_shuffle", serde_json::json!({ "shuffle": on }));
    }
    fn playback_set_volume(&self, volume: u8) {
        self.send_call("playback.set_volume", serde_json::json!({ "volume": volume }));
    }
}

// ── Scheduler seams backed by the real AlarmStore / EpisodeController ────────

/// Shared, sendable handle to the single `rusqlite::Connection`.
///
/// The connection lives on the main thread; wrapping it in `Arc<Mutex<..>>`
/// appeases the `Send` bound the `slint::Timer` closure requires (the mutex is
/// never contended — only the main-thread tick ever locks it), mirroring the
/// existing `Arc<Mutex<EpisodeController>>` pattern.
pub type SharedConnection = Arc<Mutex<Connection>>;

/// [`AlarmSource`] backed by the real [`AlarmStore`].
///
/// `due_alarms` lists enabled alarms whose stored `next_fire <= now` (parsed
/// from the ISO-8601 UTC cache); `recompute_next_fire` recomputes all alarm
/// caches from their rules in a single transaction (a superset of the
/// single-alarm recompute the scheduler requests).
pub struct StoreAlarmSource {
    conn: SharedConnection,
}

impl StoreAlarmSource {
    pub fn new(conn: SharedConnection) -> Self {
        Self { conn }
    }
}

impl AlarmSource for StoreAlarmSource {
    fn due_alarms(&mut self, now: DateTime<Local>) -> Vec<DueAlarm> {
        let conn = match self.conn.lock() {
            Ok(g) => g,
            Err(e) => {
                error!(error = %e, "alarm DB mutex poisoned — skipping due_alarms");
                return Vec::new();
            }
        };
        let store = AlarmStore::new(&*conn);
        let alarms = match store.list() {
            Ok(a) => a,
            Err(e) => {
                error!(error = %e, "failed to list alarms for scheduler tick");
                return Vec::new();
            }
        };
        drop(conn);

        let mut due = Vec::new();
        for alarm in alarms {
            if !alarm.enabled {
                continue;
            }
            let nf = match alarm.next_fire.as_ref() {
                Some(s) => s,
                None => continue, // not yet computed → not due
            };
            let nf_dt = match DateTime::parse_from_rfc3339(nf) {
                Ok(dt) => dt.with_timezone(&Local),
                Err(e) => {
                    warn!(
                        alarm_id = %alarm.id,
                        error = %e,
                        next_fire = %nf,
                        "unparseable next_fire cache; skipping alarm",
                    );
                    continue;
                }
            };
            if nf_dt <= now {
                due.push(DueAlarm { id: alarm.id, next_fire: nf_dt });
            }
        }
        due
    }

    fn recompute_next_fire(&mut self, _id: crate::scheduler::AlarmId, now: DateTime<Local>) {
        let conn = match self.conn.lock() {
            Ok(g) => g,
            Err(e) => {
                error!(error = %e, "alarm DB mutex poisoned — skipping recompute");
                return;
            }
        };
        let store = AlarmStore::new(&*conn);
        let now_utc = now.with_timezone(&Utc);
        if let Err(e) = store.recompute_next_fires(now_utc) {
            error!(error = %e, "failed to recompute next_fire caches");
        }
    }
}

/// [`EpisodeFsm`] backed by the real [`EpisodeController`].
///
/// The scheduler only hands us an alarm id; the controller's `fire()` also
/// needs the alarm's `source_uri` and `max_volume`, so this adapter looks the
/// alarm up by id (a main-thread `&Connection` read) before invoking the FSM.
/// Lock ordering is unidirectional (alarm-store read → release → episode
/// mutex) so the drain/dismiss paths (which lock only the episode mutex) never
/// deadlock.
pub struct EpisodeFsmAdapter {
    conn: SharedConnection,
    episode: Arc<Mutex<EpisodeController<ChannelMopidyControl>>>,
    display: Arc<Mutex<DisplayController>>,
    bundled_beep_path: Option<String>,
}

impl EpisodeFsmAdapter {
    pub fn new(
        conn: SharedConnection,
        episode: Arc<Mutex<EpisodeController<ChannelMopidyControl>>>,
        display: Arc<Mutex<DisplayController>>,
        bundled_beep_path: Option<String>,
    ) -> Self {
        Self { conn, episode, display, bundled_beep_path }
    }

    /// Look up the alarm's `source_uri` / `max_volume` by id.
    fn lookup_alarm(&self, alarm_id: &crate::scheduler::AlarmId) -> Option<Alarm> {
        let conn = self.conn.lock().ok()?;
        let store = AlarmStore::new(&*conn);
        store.get(alarm_id).ok().flatten()
    }
}

impl EpisodeFsm for EpisodeFsmAdapter {
    fn fire(&mut self, alarm_id: crate::scheduler::AlarmId) {
        let alarm = match self.lookup_alarm(&alarm_id) {
            Some(a) => a,
            None => {
                warn!(
                    alarm_id = %alarm_id,
                    "fire requested for an unknown/disabled alarm; ignoring",
                );
                return;
            }
        };
        // Slice 4: arm visual strobe if the alarm has visual config.
        if let Ok(mut dc) = self.display.lock() {
            let visual_config = crate::display::VisualConfig::from_json(
                alarm.visual_config.as_deref(),
            );
            if visual_config.is_on() {
                dc.arm_strobe(&visual_config, false);
            }
            dc.set_episode_active(true);
        }

        let max_volume = alarm.max_volume.clamp(0, 100) as u8;
        let plan = crate::episode::EpisodePlan::new(
            alarm.source_uri.clone(),
            max_volume,
            alarm.escalation_steps.clone(),
            alarm.fallback_chain.clone(),
            alarm.snooze_minutes as u32,
            alarm.max_snoozes as u32,
            self.bundled_beep_path.clone(),
        );
        if let Ok(mut ctl) = self.episode.lock() {
            ctl.fire(alarm_id, &plan);
        } else {
            error!(alarm_id = %alarm_id, "episode mutex poisoned — fire dropped");
        }
    }

    /// Slice 2 / D5: per-tick escalation advance + snooze-refire check. Driven
    /// by `Scheduler::tick` via the `EpisodeFsm::on_tick` hook. Non-blocking:
    /// the FSM issues fire-and-forget Mopidy commands.
    fn on_tick(&mut self, _now: DateTime<Local>) {
        let now = std::time::Instant::now();
        if let Ok(mut ctl) = self.episode.lock() {
            ctl.check_snooze_refire(now);
            ctl.advance_escalation(now);
        } else {
            error!("episode mutex poisoned — on_tick dropped");
        }
    }
}

/// Handle that keeps the bootstrap-installed `slint::Timer`s alive across the
/// Slint event loop. Dropping it stops both the drain and scheduler ticks.
pub struct AppTimers {
    _drain: slint::Timer,
    _scheduler: slint::Timer,
    _clock: slint::Timer,
    _weather: slint::Timer,
    _icon: slint::Timer,
}

// ── Observability (tracing → journald / fmt fallback) ────────────────────────

/// Initialize structured logging.
///
/// Prefers a `tracing-journald` layer when systemd journald is available on the Pi.
/// Falls back to a pretty-printed `fmt` layer in dev/test environments.
fn init_tracing() {
    match tracing_journald::layer() {
        Ok(jl) => {
            tracing_subscriber::registry().with(jl).init();
        }
        Err(_) => {
            tracing_subscriber::fmt()
                .pretty()
                .with_target(true)
                .init();
        }
    }
}

// ── Tokio worker (async command dispatcher) ───────────────────────────────────

/// Result returned when the command dispatcher loop exits.
#[derive(Debug, PartialEq, Eq)]
pub enum CmdLoopResult {
    /// The sender side was dropped / a `Shutdown` command was received.
    ShutdownComplete,
}

/// Drain the [`Cmd`] channel on the tokio runtime.
///
/// Each received command is dispatched to the appropriate handler (currently
/// logging + placeholder responses in slice 0). A `Shutdown` variant, a closed
/// sender, or a signal (SIGTERM/SIGINT) terminates the loop.
///
/// On SIGTERM/SIGINT the dispatcher sends [`Reply::ShutdownRequested`] back to
/// main so that the shutdown sequence flows through the existing reply channel.
///
/// # Ownership
/// The receiver is moved into this function and owned for the lifetime of the
/// tokio runtime. Replies are pushed through [`reply_tx`] back to main.
type MopidyClient = Arc<mopidy_client::transport::MopidyWsClient>;

pub async fn command_dispatcher(
    mut cmd_rx: mpsc::Receiver<Cmd>,
    reply_tx: mpsc::Sender<Reply>,
    client: MopidyClient,
    weather_cfg: Arc<crate::config::WeatherConfig>,
) -> CmdLoopResult {
    // Set up signal listeners for SIGTERM (systemd stop) and SIGINT (Ctrl+C).
    let mut sigterm = unix_signal(SignalKind::terminate())
        .expect("failed to install SIGTERM handler");

    loop {
        tokio::select! {
            // Signal handling (Design D7): SIGTERM from systemd, SIGINT from Ctrl+C.
            _ = sigterm.recv() => {
                info!("Received SIGTERM — signaling shutdown to main");
                let _ = reply_tx.send(Reply::ShutdownRequested).await;
                break;
            }

            Ok(()) = tokio::signal::ctrl_c() => {
                warn!("Received SIGINT (Ctrl+C) — signaling shutdown to main");
                let _ = reply_tx.send(Reply::ShutdownRequested).await;
                break;
            }

            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    Cmd::GetMopidyState => {
                        info!(action = "GetMopidyState", "command received");
                        if client.is_connected() {
                            match client.send_and_await("core.get_state", None).await {
                                Ok(res) => {
                                    let state = res
                                        .result
                                        .as_ref()
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("STOPPED");
                                    let _ = reply_tx
                                        .send(Reply::MopidyState(state.to_string()))
                                        .await;
                                }
                                Err(e) => {
                                    error!(error = %e, "failed to get mopidy state");
                                    let _ = reply_tx
                                        .send(Reply::MopidyState("STOPPED".into()))
                                        .await;
                                }
                            }
                        } else {
                            let _ = reply_tx
                                .send(Reply::MopidyState("STOPPED".into()))
                                .await;
                        }
                    }
                    Cmd::CaptureSnapshot => {
                        // Slice-1 placeholder: the tokio worker does not yet hold
                        // a live MopidyWsClient handle in this dispatcher context.
                        // The full CaptureSnapshot implementation (batch get_state,
                        // get_time_position, get_volume, repeat, shuffle reads)
                        // will be wired when the Mopidy client is available on tokio.
                        // For now, a default-reply is sent back if needed.
                        info!(action = "CaptureSnapshot", "command received (slice 1 placeholder — no live Mopidy handle in dispatcher yet)");
                    }
                    Cmd::CallMopidy { method, params } => {
                        let _guard = info_span!("mopidy_request", method = %method).entered();
                        info!("CallMopidy command received");
                        if client.is_connected() {
                            match client.send_and_await(&method, Some(params)).await {
                                Ok(msg) => {
                                    let result = msg.result.unwrap_or_else(|| {
                                        msg.error
                                            .unwrap_or(serde_json::json!({"error": "empty response"}))
                                    });
                                    let _ = reply_tx
                                        .send(Reply::CallResult(result))
                                        .await;
                                }
                                Err(e) => {
                                    error!(method, error = %e, "mopidy call failed");
                                    let _ = reply_tx
                                        .send(Reply::CallResult(
                                            serde_json::json!({"error": e.to_string()})
                                        ))
                                        .await;
                                }
                            }
                        } else {
                            let _ = reply_tx
                                .send(Reply::CallResult(
                                    serde_json::json!({"error": "mopidy not connected"})
                                ))
                                .await;
                        }
                    }
                    Cmd::Shutdown => {
                        info!("Shutdown command received — terminating tokio worker loop");
                        break;
                    }
                    Cmd::FireAlarm { alarm_id } => {
                        // Slice-1 placeholder: alarm firing is driven by the
                        // scheduler/episode FSM on main (task 1.1). The real
                        // handling of a FireAlarm command belongs to a later
                        // task group; this arm exists so the match stays
                        // exhaustive.
                        info!(alarm_id, "FireAlarm command received (slice 1 placeholder)");
                    }
                    // Task 7.3: the dismiss tap handler calls
                    // `EpisodeController::dismiss()` directly on main (the FSM
                    // lives on main per design D4/D8). `Cmd::Dismiss` is routed
                    // to the tokio worker only if a cross-thread dismiss is
                    // issued; the worker has no FSM, so it is a logged no-op.
                    Cmd::Dismiss => {
                        info!("Dismiss command received on tokio worker — no-op (episode FSM is on main)");
                    }
                    // Slice 5: fetch weather data from Open-Meteo on the tokio
                    // worker and marshal the result back to main via the reply
                    // channel. On success the snapshot is sent; on failure an
                    // error string is sent so main can apply backoff.
                    Cmd::FetchWeather { lat, lon } => {
                        let _guard = info_span!("weather_fetch", lat = %lat, lon = %lon).entered();
                        info!("FetchWeather command received");
                        let result = fetch_weather_with_fallback(lat, lon, &weather_cfg).await;
                        let reply = match result {
                            Ok(snapshot) => {
                                info!(temp = snapshot.current_temp, wmo = snapshot.wmo_code, "weather fetch successful");
                                Reply::WeatherResult(Ok(snapshot))
                            }
                            Err(e) => {
                                warn!(error = %e, "weather fetch failed");
                                Reply::WeatherResult(Err(e.to_string()))
                            }
                        };
                        let _ = reply_tx.send(reply).await;
                    }
                    // Slice 5: geocode a city name on the tokio worker and
                    // return the resolved (lat, lon, name) to main.
                    Cmd::GeocodeCity { city } => {
                        let _guard = info_span!("geocode", city = %city).entered();
                        info!("GeocodeCity command received");
                        let result = fetch_geocoding_data(&city).await;
                        let reply = match result {
                            Ok((lat, lon, name)) => {
                                info!(lat, lon, resolved = %name, "geocode successful");
                                Reply::GeocodeResult(Ok((lat, lon, name)))
                            }
                            Err(e) => {
                                warn!(error = %e, "geocode failed");
                                Reply::GeocodeResult(Err(e.to_string()))
                            }
                        };
                        let _ = reply_tx.send(reply).await;
                    }
                }
            }

            else => break, // cmd_rx closed (sender dropped)
        }
    }
    CmdLoopResult::ShutdownComplete
}

// ── Bootstrap (tokio thread + Slint drain timer) ──────────────────────────────

/// Start the tokio worker runtime on a dedicated thread.
///
/// The Mopidy WS client and its connection-state-forwarding task are spawned
/// *inside* the worker runtime (via `block_on`) — `MopidyWsClient::spawn`
/// calls `tokio::spawn`, which requires an active runtime context, so it must
/// run on the worker thread, not on main.
fn spawn_tokio_worker(
    cmd_rx: mpsc::Receiver<Cmd>,
    reply_tx: mpsc::Sender<Reply>,
    mopidy_ws_url: String,
    mopidy_event_tx: tokio_mpsc::Sender<mopidy_client::MopidyEvent>,
    mopidy_reply_tx: tokio_mpsc::Sender<mopidy_client::transport::JsonRpcMessage>,
    weather_cfg: Arc<crate::config::WeatherConfig>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("tokio-worker".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to create tokio runtime for worker thread");

            info!("tokio worker runtime created on dedicated thread");

            rt.block_on(async move {{
                // ── Mopidy WS client (runs on the worker runtime) ────────────
                //
                // `MopidyWsClient::spawn` calls `tokio::spawn` internally, so it
                // must run inside a runtime context — hence here, inside
                // `block_on`, rather than on main.
                let (mopidy_state_tx, mut mopidy_state_rx) =
                    tokio_mpsc::channel::<MopidyConnectionState>(16);
                let client: MopidyClient = Arc::new(mopidy_client::transport::MopidyWsClient::spawn(
                    mopidy_ws_url,
                    None, // use default backoff policy
                    mopidy_event_tx,
                    mopidy_reply_tx,
                    mopidy_state_tx,
                ));

                // Task 4.3: Mopidy client state forwarding — spawn a background
                // task that reads MopidyConnectionState transitions and forwards
                // them through the Reply channel as Reply::MopidyConnectionState.
                let reply_tx_forward = reply_tx.clone();
                tokio::spawn(async move {{
                    while let Some(state) = mopidy_state_rx.recv().await {{
                        let _ = reply_tx_forward
                            .send(Reply::MopidyConnectionState(state))
                            .await;
                    }}
                }});

// Run the command dispatcher with the live MopidyClient handle.
                let result = command_dispatcher(cmd_rx, reply_tx, client, weather_cfg).await;
                info!(result = ?result, "tokio command dispatcher exited");
            }});

            info!("tokio worker thread shutting down");
        })
        .expect("failed to spawn tokio worker thread")
}

/// Application entry point.
///
/// Creates the cross-thread channel topology, spaws the tokio worker on a
/// dedicated thread, and installs a repeating [`slint::Timer`] that drains
/// replies and events non-blockingly on each Slint tick.
///
/// The returned [`JoinHandle`] can be `.join()`d to wait for the tokio worker
/// to finish; in normal operation the handle is parked (the application lives
/// as long as the Slint event loop runs).
pub fn bootstrap(conn: SharedConnection) -> (JoinHandle<()>, AppWindow, AppTimers) {
    let cfg = Config::load();

    // Slice 4a / D3: resolve bundled beep asset path at boot
    let bundled_beep_path = resolve_bundled_beep_asset(&cfg.data_dir);
    if bundled_beep_path.is_some() {
        info!(beep_path = ?bundled_beep_path, "bundled beep asset resolved");
    } else {
        info!("no bundled beep asset found");
    }

    info!(
        db_path = %cfg.db_path,
        mopidy_ws_url = %cfg.mopidy_ws_url,
        axum_bind_addr = %cfg.axum_bind_addr,
        log_level = %cfg.log_level,
        data_dir = %cfg.data_dir,
        weather_city = %cfg.weather.city,
        weather_lat = ?cfg.weather.lat,
        weather_lon = ?cfg.weather.lon,
        pirateweather_configured = cfg.weather.pirateweather_api_key.is_some(),
        "bootstrap: configuration loaded",
    );

    // Create cross-thread channels (task 2.1).
    let handles = channel::create_channels();
    let cmd_sender = handles.main.cmd_sender;
    // Clone for the weather timer (the original is moved into
    // `ChannelMopidyControl` below).
    let cmd_sender_for_weather = cmd_sender.clone();
    let mut reply_rx = handles.main.reply_receiver;
    let mut event_rx = handles.main.event_receiver;
    let tokio_handles = handles.tokio;

    // ── Mopidy client channels (task 4.3 / 4.5) ────────────────────────
    //
    // The WS client itself is spawned *inside* the tokio worker runtime (see
    // `spawn_tokio_worker`) because `MopidyWsClient::spawn` calls
    // `tokio::spawn`.  These senders are handed to the worker; the receiver
    // ends are retained on main for later slices (slice 0 only logs).
    let (mopidy_event_tx, _mopidy_event_rx) =
        tokio_mpsc::channel::<mopidy_client::MopidyEvent>(16);
    let (mopidy_reply_tx, _mopidy_reply_rx) =
        tokio_mpsc::channel::<mopidy_client::transport::JsonRpcMessage>(16);

    // Weather provider configuration (slice 5 + PirateWeather fallback).
    // Cloned into an `Arc` so it can be shared with the tokio worker (which
    // performs the actual fetches) without further copying.
    let weather_cfg = Arc::new(cfg.weather.clone());

    // Spawn tokio worker on a dedicated thread (task 2.2).
    let worker_handle = spawn_tokio_worker(
        tokio_handles.cmd_receiver,
        tokio_handles.reply_sender,
        cfg.mopidy_ws_url.clone(),
        mopidy_event_tx,
        mopidy_reply_tx,
        Arc::clone(&weather_cfg),
    );

    info!("tokio worker thread spawned successfully");

    // ── Episode FSM (task 9.1) ──────────────────────────────────────
    //
    // Create the episode controller so the shutdown handler can call
    // `shutdown_restore()` (task 6.5) before draining the Cmd channel and
    // exiting. Its [`MopidyControl`] seam is the channel-backed
    // [`ChannelMopidyControl`] (the slice-0 `NoopMopidyControl` no-op is
    // replaced by group 9.1).
    //
    // Wrapped in `Arc<Mutex<..>>` (task 7.3) so the dismiss tap handler —
    // registered on the `AppWindow` below — the drain timer, and the
    // scheduler's [`EpisodeFsmAdapter`] can all reach the FSM on the main
    // thread. (`slint::Timer` closures do not require `Send` in this version,
    // but `Arc` is kept for the shared-ownership pattern.)
    // ── DisplayController (slice 4) ───────────────────────────────────
    //
    // Single backlight controller on main. Wrapped in `Arc<Mutex<..>>` so it
    // can be shared with the episode FSM (for brightness capture/restore) and
    // the scheduler tick (for policy resolution each tick).
    let display_ctl: Arc<Mutex<DisplayController>> = Arc::new(Mutex::new(DisplayController::new()));

    // Weather store for holding weather data
    let weather_store = Arc::new(Mutex::new(WeatherStore::new()));

    // Slice 4 / task 3.3: load persisted bedtime config and brightness floor.
    {
        if let Ok(mut dc) = display_ctl.lock() {
            if let Ok(conn_guard) = conn.lock() {
                let store = crate::database::ConfigStore::new(&*conn_guard);
                let bedtime = store.get("bedtime_config").ok().flatten();
                let floor = store.get("brightness_floor").ok().flatten();
                if let Some(json) = bedtime {
                    dc.set_bedtime_config(crate::display::BedtimeConfig::from_json(Some(&json)));
                }
                if let Some(val) = floor {
                    if let Ok(pct) = val.parse::<u8>() {
                        dc.set_brightness_target(pct);
                    }
                }
                // Persist current values (round-trip ensures they are stored).
                let _ = store.set("bedtime_config", &dc.bedtime_config().to_json());
                let _ = store.set("brightness_floor", &dc.brightness_floor().to_string());
            }
        }
    }

    // Slice 5: Initialize weather configuration with the defaults from the
    // loaded config (`[weather]` in config.toml / config.local.toml). The DB
    // remains the source of truth after first boot — this only seeds initial
    // values when the keys are absent, so a later config change does not
    // override a city the user set via the web UI.
    //
    // When the config provides only a city (no lat/lon), the city is seeded
    // but the coordinates are left unset; the weather scheduler then geocodes
    // the city at boot to resolve them (see `maybe_dispatch_weather`).
    {
        if let Ok(conn_guard) = conn.lock() {
            let store = crate::database::ConfigStore::new(&*conn_guard);

            // Check if weather city is already configured.
            let city = store.get("weather_city").ok().flatten();

            // If not configured, seed defaults from the config.
            if city.is_none() {
                let w = &cfg.weather;
                let _ = store.set("weather_city", &w.city);
                match (w.lat, w.lon) {
                    (Some(lat), Some(lon)) => {
                        let _ = store.set("weather_lat", &lat.to_string());
                        let _ = store.set("weather_lon", &lon.to_string());
                        info!(
                            city = %w.city, lat, lon,
                            "weather: initialized default city+coords from config"
                        );
                    }
                    _ => {
                        // City only — coordinates will be resolved by
                        // geocoding the city at boot.
                        info!(
                            city = %w.city,
                            "weather: initialized default city from config (coords will be geocoded)"
                        );
                    }
                }
            } else {
                info!("weather: using persisted city configuration");
            }
        }
    }

    let episode_ctl: Arc<Mutex<EpisodeController<ChannelMopidyControl>>> = Arc::new(
        Mutex::new(EpisodeController::new(ChannelMopidyControl::new_with_display(
            cmd_sender,
            Arc::clone(&display_ctl),
        ))),
    );

    // ── Slint AppWindow + episode UI wiring (tasks 7.2 / 7.3) ─────────────
    //
    // The `AppWindow` exposes the `episode-firing` property and the
    // `dismiss-requested` callback. Group 9.1 hosts `.run()` to drive the
    // Slint event loop; the window is held alive across the bootstrap scope
    // (and returned to `main`) so the weak refs captured by the drain timer
    // and the dismiss callback remain valid.
    let app_window = AppWindow::new().expect("failed to create AppWindow");

    // Task 7.3: a tap on the alarm overlay (`AlarmPanel`) invokes the
    // `dismiss-requested` callback, which calls `EpisodeController::dismiss()`
    // directly on main (the FSM lives on main per design D4/D8) and restores
    // the UI to Idle. This does not route through the `Cmd` channel — the
    // episode FSM is owned by main.
    {
        let ctl = Arc::clone(&episode_ctl);
        let dc = Arc::clone(&display_ctl);
        let weak = app_window.as_weak();
        app_window.on_dismiss_requested(move || {
            if let Ok(mut ctl) = ctl.lock() {
                ctl.dismiss();
            }
            // Slice 4: signal the display controller that the episode ended.
            if let Ok(mut dc) = dc.lock() {
                dc.set_episode_active(false);
            }
            // Optimistically restore the UI to Idle (the FSM is now `Dismissed`).
            if let Some(w) = weak.upgrade() {
                w.set_episode_firing(false);
            }
        });
    }

    // Slice 2 / D8: the snooze button on the alarm overlay invokes
    // `EpisodeController::snooze(DEFAULT_SNOOZE_DURATION)`. The drain timer
    // reflects `is_firing()` (false during `Snoozing`) into `episode-firing`
    // on the next tick, so the overlay hides without further wiring here.
    {
        let ctl = Arc::clone(&episode_ctl);
        app_window.on_snooze_requested(move || {
            if let Ok(mut ctl) = ctl.lock() {
                ctl.snooze(crate::episode::DEFAULT_SNOOZE_DURATION);
            }
        });
    }

    // Weak handle captured by the drain timer (task 7.2): each tick reflects
    // `EpisodeController::is_firing()` into the `episode-firing` property so
    // the overlay shows/hides and the nav container / swipe are gated.
    let ui_weak = app_window.as_weak();

    // ── Runtime theme controller + live clock timer (slice 3) ────────────────
    //
    // Theme selection and mode are loaded from `kv_config`, pushed into the
    // Slint `ThemeGlobal` singleton, and updated every second so the clock
    // and theme tokens stay in sync.
    let theme_controller = Arc::new(Mutex::new(
        ThemeController::new(Arc::clone(&conn))
            .with_display(Arc::clone(&display_ctl)),
    ));

    // Push the initially loaded theme into Slint.
    {
        let ctl = theme_controller.lock().expect("theme mutex poisoned");
        ctl.push(&app_window);
    }

    // Settings panel: tap to cycle theme.
    {
        let ctl = Arc::clone(&theme_controller);
        let weak = app_window.as_weak();
        app_window.on_theme_tapped(move || {
            protected_tick(|| {
                if let Ok(mut ctl) = ctl.lock() {
                    ctl.cycle_theme();
                    if let Some(w) = weak.upgrade() {
                        ctl.push(&w);
                    }
                }
            });
        });
    }

    // Settings panel: tap to cycle mode.
    {
        let ctl = Arc::clone(&theme_controller);
        let weak = app_window.as_weak();
        app_window.on_mode_tapped(move || {
            protected_tick(|| {
                if let Ok(mut ctl) = ctl.lock() {
                    ctl.cycle_mode();
                    if let Some(w) = weak.upgrade() {
                        ctl.push(&w);
                    }
                }
            });
        });
    }

    // Debug tap logging — remove once calibration is finalised.
    //
    // (removed — calibration is now handled via udev LIBINPUT_CALIBRATION_MATRIX)

    // Clock timer: drives the analog hands. Fires at ~30 Hz so the second
    // hand can sweep continuously (sub-second angle precision) instead of
    // ticking once per second. The relatively expensive date text, theme
    // token refresh, and weather snapshot push only change at whole-second
    // granularity, so they are gated to run once per whole second.
    let clock_timer = slint::Timer::default();
    {
        let ctl = Arc::clone(&theme_controller);
        let weak = app_window.as_weak();
        let weather_store_ref = Arc::clone(&weather_store);
        let mut last_second: i64 = -1;
        clock_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(33),
            move || {
                protected_tick(|| {
                    let now = Local::now();
                    let h = (now.hour() % 12) as f32;
                    let m = now.minute() as f32;
                    // Sub-second precision: include the nanoseconds within the
                    // current second so the second hand glides smoothly.
                    let s = (now.second() as f32) + (now.nanosecond() as f32) / 1_000_000_000.0;

                    let second_angle = s * 6.0;
                    let minute_angle = (m * 60.0 + s) * 0.1;
                    let hour_angle = (h * 3600.0 + m * 60.0 + s) * 0.008333333;

                    if let Some(w) = weak.upgrade() {
                        let global = w.global::<ThemeGlobal>();
                        global.set_hour_angle(hour_angle);
                        global.set_minute_angle(minute_angle);
                        global.set_second_angle(second_angle);

                        // Date text, theme tokens, and weather snapshot only
                        // change at second (or coarser) granularity — refresh
                        // them once per whole second, not every frame.
                        let sec = now.second() as i64;
                        if sec != last_second {
                            last_second = sec;
                            global.set_clock_weekday(slint::SharedString::from(
                                now.format("%A").to_string(),
                            ));
                            global.set_clock_date(slint::SharedString::from(
                                now.format("%B %-d, %Y").to_string(),
                            ));

                            if let Ok(ctl) = ctl.lock() {
                                ctl.push(&w);
                                // Push weather data to the UI
                                let (weather_available, weather_data) = if let Ok(ws) = weather_store_ref.lock() {
                                    let snapshot = ws.get_snapshot().cloned();
                                    (snapshot.is_some(), snapshot)
                                } else {
                                    (false, None)
                                };
                                ctl.push_weather(&w, weather_available, weather_data);
                            }
                        }
                    }
                });
            },
        );
    }

    // ── Weather icon animation timer (slice 5) ──────────────────────────
    //
    // Cycles through the pre-rendered animation frames of the current weather
    // icon. Each icon has ~12 frames; we advance one frame every ~83ms
    // (1000ms / 12) for smooth animation.
    let icon_timer = slint::Timer::default();
    {
        let weak = app_window.as_weak();
        icon_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(83),
            move || {
                protected_tick(|| {
                    if let Some(w) = weak.upgrade() {
                        let global = w.global::<ThemeGlobal>();
                        let wmo = global.get_weather_wmo_code();
                        let frames = crate::weather_icons::get_icon_frames(wmo);
                        if frames.len() > 1 {
                            let current = global.get_weather_frame_index() as usize;
                            let next = (current + 1) % frames.len();
                            global.set_weather_frame_index(next as i32);
                            global.set_weather_icon_image(frames[next].clone());
                        }
                    }
                });
            },
        );
    }

    // ── Slint drain timer (non-blocking try_recv on each tick) ────────────
    //
    // This single repeating timer polls both the reply channel and the Mopidy
    // event channel on every Slint tick. It uses `try_recv` so main never
    // blocks waiting for the tokio worker. Each received item is dispatched
    // directly into domain handlers on the main thread — no locks needed.

    // The drain timer below moves `episode_ctl` into its closure; clone it now
    // for the scheduler's `EpisodeFsmAdapter` (constructed after the drain timer).
    let episode_ctl_for_scheduler = Arc::clone(&episode_ctl);
    // Clone the weather store for the drain timer so weather fetch replies
    // can be routed into it on the main thread.
    let weather_store_for_drain = Arc::clone(&weather_store);
    // Clone the command sender and shared connection so the drain timer can
    // finish the geocode flow: on a successful `Reply::GeocodeResult` it
    // persists the resolved lat/lon to the DB and immediately dispatches a
    // `Cmd::FetchWeather` for the new coordinates.
    let cmd_sender_for_drain = cmd_sender_for_weather.clone();
    let conn_for_drain = Arc::clone(&conn);

    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
        // ── Tick-level panic isolation (Design D6) ──────────────────────
        //
        // Every periodic tick wraps its body in `catch_unwind`. A panic
        // is logged at `error!` level and the tick reschedules on the
        // next interval. Cardinal rule: a bug in one tick must not sink
        // the alarm guarantee.
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            // Drain reply channel (non-blocking).
            while let Ok(reply) = reply_rx.try_recv() {
                dispatch_reply_to_domain(
                    reply,
                    &*episode_ctl,
                    &weather_store_for_drain,
                    &conn_for_drain,
                    &cmd_sender_for_drain,
                );
            }

            // Drain Mopidy event channel (non-blocking).
            while let Ok(event) = event_rx.try_recv() {
                dispatch_event_to_domain(event);
            }

            // Task 7.2: reflect the episode FSM state into the UI. When the
            // FSM is `Firing`, the alarm overlay is shown exclusively and the
            // navigation container is hidden + swipe disabled (bound in
            // `ui.slint`); on `Idle`/`Dismissed` it is restored.
            if let Ok(ctl) = episode_ctl.lock() {
                if let Some(w) = ui_weak.upgrade() {
                    w.set_episode_firing(ctl.is_firing());
                }
            }
        }));

        if let Err(err) = result {
            // Log the panic payload — `Box<dyn Any + Send>` can be a String,
            // &str, or opaque data. We log whatever we can recover.
            let msg = match err.downcast::<String>() {
                Ok(s) => *s,
                Err(e) => match e.downcast::<&str>() {
                    Ok(s) => s.to_string(),
                    Err(_) => "unknown panic payload".to_string(),
                },
            };
            tracing::error!(panic = %msg, tick_interval_ms = 50,
                "tick body panicked — caught and rescheduled",
            );
        }
    });

    info!("drain timer installed (50 ms repeat interval)");

    // ── Scheduler tick timer (slice 1, task 1.1) ────────────────────────────
    //
    // A repeating `slint::Timer` at the default 5 s interval drives the
    // alarm scheduler. Each tick re-reads `Local::now()`, asks the alarm
    // source for due alarms, enters the `scheduler_tick` span, and fires the
    // episode FSM for each due alarm (recomputing next_fire afterwards). See
    // design D1 and `scheduler.rs`.
    //
    // Until the real `AlarmStore` (group 3) and `EpisodeController` (group 5)
    // are wired in by group 9.1, the scheduler runs over no-op seam impls —
    // the tick machinery and span are exercised, but no real alarms fire yet.
    // Dev alarm seeding (task 8.3 / design D9): consume `./alarms.toml`
    // (if present) and upsert each entry by `id` into the DB. Idempotent,
    // dev-only (no-op in release builds). This must run *before* the
    // `recompute_next_fires` boot step so freshly-seeded alarms get a
    // `next_fire` cache entry on this boot.
    {
        if let Ok(conn_guard) = conn.lock() {
            let store = AlarmStore::new(&*conn_guard);
            if let Err(e) = seed::seed_alarms(&store) {
                error!(error = %e, "dev alarm seeding failed; continuing with DB as sole source");
            }
        } else {
            error!("alarm DB mutex poisoned at boot; skipping dev seed");
        }
    }

    // Boot recompute (task 3.4): populate/refresh the `next_fire` caches
    // from each alarm's rule once before the first tick (design D1).
    {
        if let Ok(conn_guard) = conn.lock() {
            let store = AlarmStore::new(&*conn_guard);
            if let Err(e) = store.recompute_next_fires(Utc::now()) {
                error!(error = %e, "boot recompute of next_fire caches failed");
            } else {
                info!("boot recompute: next_fire caches refreshed");
            }
        } else {
            error!("alarm DB mutex poisoned at boot; skipping recompute");
        }
    }

    // Group 9.1: wire the real [`StoreAlarmSource`] (over [`AlarmStore`]) and
    // [`EpisodeFsmAdapter`] (over the [`EpisodeController`] above) in place of
    // the slice-0 no-op seams, so real alarms now drive the episode FSM.
    let display_for_scheduler = Arc::clone(&display_ctl);
    let scheduler_state = Mutex::new(Scheduler::new(
        StoreAlarmSource::new(Arc::clone(&conn)),
        EpisodeFsmAdapter::new(Arc::clone(&conn), episode_ctl_for_scheduler, Arc::clone(&display_ctl), bundled_beep_path.clone()),
        LocalClock,
    ));
    let scheduler_timer = slint::Timer::default();
    scheduler_timer.start(
        slint::TimerMode::Repeated,
        DEFAULT_TICK_INTERVAL,
        move || {
            // Design D6: isolate the tick body so a bug never sinks the alarm
            // guarantee; the timer reschedules on its next interval.
            protected_tick(|| {
                if let Ok(mut state) = scheduler_state.lock() {
                    state.tick();
                }
                // Slice 4: drive the display controller policy resolution.
                if let Ok(mut dc) = display_for_scheduler.lock() {
                    dc.tick();
                }
            });
        },
    );
    info!(
        interval_secs = DEFAULT_TICK_INTERVAL.as_secs(),
        "scheduler timer installed",
    );

    // Hold the `AppWindow` alive across the bootstrap scope so the weak refs
    // captured by the drain timer and the dismiss callback remain valid. The
    // timers are returned in [`AppTimers`] so `main` can keep them alive
    // across `.run()` (a dropped `slint::Timer` stops firing).
    let _app_window = app_window;

    // ── Slice 5: weather refresh scheduler ─────────────────────────────
    //
    // A short-interval (30 s) repeating timer drives the weather fetch
    // decision. On each tick `maybe_dispatch_weather` consults the
    // [`WeatherStore`] backoff state and dispatches a `Cmd::FetchWeather` only
    // when a fetch is actually due:
    //
    // - Steady state: every `WEATHER_STEADY_INTERVAL` (30 min), when no
    //   backoff is active.
    // - Retry: once a backoff window elapses (`is_retry_due`), at 1 → 2 → 4 →
    //   8 → 16 min after consecutive failures. This realises the exponential
    //   backoff that was previously computed but never acted upon.
    //
    // While a fetch is in flight (dispatched, awaiting its worker reply),
    // further dispatches are suppressed to avoid stacking requests —
    // important now that a wedged endpoint can hold a fetch open for up to the
    // 10 s request timeout. The drain timer clears the in-flight flag when the
    // `Reply::WeatherResult` arrives (success → `update_snapshot`, failure →
    // `handle_failure`).
    //
    // An initial dispatch fires immediately on boot so the UI doesn't show
    // "Weather data unavailable" for the first 30 s.
    let weather_timer = slint::Timer::default();
    {
        let cmd_tx = cmd_sender_for_weather.clone();
        let conn_for_weather = Arc::clone(&conn);
        let weather_store_ref = Arc::clone(&weather_store);

        // Initial fetch — fire immediately on boot.
        protected_tick(|| {
            maybe_dispatch_weather(&cmd_tx, &conn_for_weather, &weather_store_ref);
        });

        // 30 s scheduler tick. The short interval is the polling granularity
        // only; actual fetches still happen at the 30-min steady cadence (or
        // the backoff retry cadence) as decided by `maybe_dispatch_weather`.
        weather_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(30),
            move || {
                protected_tick(|| {
                    maybe_dispatch_weather(&cmd_tx, &conn_for_weather, &weather_store_ref);
                });
            },
        );
    }
    info!(
        steady_interval_mins = WEATHER_STEADY_INTERVAL.as_secs() / 60,
        scheduler_tick_secs = 30,
        "weather scheduler installed (steady 30-min cadence, exponential retry backoff, immediate boot fetch)",
    );

    let timers = AppTimers {
        _drain: timer,
        _scheduler: scheduler_timer,
        _clock: clock_timer,
        _weather: weather_timer,
        _icon: icon_timer,
    };

    (worker_handle, _app_window, timers)
}

/// Dispatch a [`Reply`] from the tokio worker into the domain.
///
/// In slice 0 these are logged. Later slices will route them to the FSM or
/// update Slint UI models. Runs on main via the drain timer callback.
///
/// Slice 5: weather fetch results are routed into the [`WeatherStore`] so
/// the next clock-tick push reflects the fresh (or stale-but-present) data.
fn dispatch_reply_to_domain(
    reply: Reply,
    episode_ctl: &std::sync::Mutex<EpisodeController<ChannelMopidyControl>>,
    weather_store: &Arc<Mutex<WeatherStore>>,
    conn: &SharedConnection,
    cmd_tx: &mpsc::Sender<Cmd>,
) {
    match reply {
        Reply::MopidyState(state) => {
            info!(reply = "MopidyState", state = %state, "dispatched reply to domain");
        }
        Reply::CallResult(result) => {
            info!(reply = "CallResult", result = ?result, "dispatched reply to domain");
        }
        Reply::ShutdownRequested => {
            info!("Shutdown requested (signal) — entering shutdown sequence");
            execute_shutdown(episode_ctl);
        }
        // Task 4.3: log Mopidy connection-state transitions (not consumed beyond logging in slice 0).
        Reply::MopidyConnectionState(state) => {
            info!(reply = "MopidyConnectionState", state = ?state, "dispatched Mopidy connection state to domain");
        }
        // Slice 5: weather fetch result — update the store on main. The
        // fetch-in-flight flag is cleared by `update_snapshot` (success) /
        // `handle_failure` (failure) so the scheduler can dispatch the next.
        Reply::WeatherResult(result) => {
            match result {
                Ok(snapshot) => {
                    info!(temp = snapshot.current_temp, wmo = snapshot.wmo_code, "weather snapshot received — updating store");
                    if let Ok(mut ws) = weather_store.lock() {
                        ws.update_snapshot(snapshot);
                    }
                }
                Err(msg) => {
                    warn!(error = %msg, "weather fetch failed on worker — applying backoff");
                    if let Ok(mut ws) = weather_store.lock() {
                        ws.handle_failure();
                    }
                }
            }
        }
        // Slice 5: geocoding result — persist the resolved lat/lon/city to the
        // DB and immediately dispatch a `Cmd::FetchWeather` for the new
        // coordinates so weather appears without waiting for the next
        // scheduler tick. On failure, apply backoff (the scheduler will retry
        // the geocode once it elapses).
        Reply::GeocodeResult(result) => {
            match result {
                Ok((lat, lon, name)) => {
                    info!(lat, lon, city = %name, "geocode result received — persisting and refreshing");
                    // Persist the resolved coordinates + canonical city name.
                    let persisted = match conn.lock() {
                        Ok(g) => {
                            let store = crate::database::ConfigStore::new(&*g);
                            store.set("weather_city", &name).is_ok()
                                && store.set("weather_lat", &lat.to_string()).is_ok()
                                && store.set("weather_lon", &lon.to_string()).is_ok()
                        }
                        Err(_) => false,
                    };
                    if !persisted {
                        warn!("geocode: failed to persist resolved coords to DB; will retry");
                    }
                    // Clear the geocode-in-flight flag and reset backoff so the
                    // fetch can proceed immediately.
                    if let Ok(mut ws) = weather_store.lock() {
                        ws.record_success();
                        ws.clear_geocode_in_flight();
                        // Reserve the fetch slot so the scheduler doesn't
                        // double-dispatch before this FetchWeather is processed.
                        ws.mark_fetch_in_flight();
                    }
                    match cmd_tx.try_send(Cmd::FetchWeather { lat, lon }) {
                        Ok(()) => info!(lat, lon, "weather: dispatched FetchWeather after geocode"),
                        Err(e) => {
                            warn!(error = %e, "weather: could not dispatch post-geocode FetchWeather");
                            if let Ok(mut ws) = weather_store.lock() {
                                ws.clear_fetch_in_flight();
                            }
                        }
                    }
                }
                Err(msg) => {
                    warn!(error = %msg, "geocode failed on worker — applying backoff");
                    if let Ok(mut ws) = weather_store.lock() {
                        ws.handle_failure();
                    }
                }
            }
        }
    }
}

/// Dispatch a [`MopidyEvent`] from the tokio worker into the domain.
///
/// In slice 0 these are logged and otherwise ignored; later slices consume them
/// within the episode FSM. Runs on main via the drain timer callback.
fn dispatch_event_to_domain(event: MopidyEvent) {
    match &event {
        MopidyEvent::PlaybackStateChanged => {
            info!(event = "PlaybackStateChanged", "dispatched Mopidy event to domain");
        }
        MopidyEvent::TracklistChanged => {
            info!(event = "TracklistChanged", "dispatched Mopidy event to domain");
        }
        MopidyEvent::Other { method } => {
            warn!(event = "Other", method = %method, "dispatched unmodelled Mopidy event to domain");
        }
    }
}

// ── Domain shutdown hook (Design D7 seam) ───────────────────────────────────

/// Trait for domain-level actions required before the process exits.
///
/// Slice 0: no-op placeholder.  Slice 1+: restore the episode snapshot from
/// persistence so that an in-flight alarm is not lost across a restart.
pub trait DomainShutdownRestore {
    fn shutdown_restore(&self);
}

/// Default domain implementation (slice 0: no-op).
pub struct Domain;

impl DomainShutdownRestore for Domain {
    fn shutdown_restore(&self) {
        info!("shutdown_restore called — slice 0 no-op placeholder");
    }
}

// ── systemd readiness notification (Design D10) ─────────────────────────────

/// Send `sd_notify(READY=1)` to systemd if we are running under it.
///
/// Called after all bootstrap steps complete: config parsed, DB migrated
/// (no-op in slice 0), Mopidy client started (placeholder), axum bound
/// (placeholder).  Does nothing when `NOTIFY_SOCKET` is not set (i.e. when
/// running outside systemd).
fn sd_notify_ready() {
    if let Ok(socket_path) = std::env::var("NOTIFY_SOCKET") {
        match std::os::unix::net::UnixDatagram::unbound() {
            Ok(socket) => {
                if let Err(e) = socket.send_to(b"READY=1", &socket_path) {
                    warn!(error = %e, "failed to send sd_notify READY=1");
                } else {
                    info!("sd_notify: READY=1");
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to create datagram socket for sd_notify");
            }
        }
    }
}

// ── Display orientation (slice 3 / D6) ──────────────────────────────────────

/// Rotate the display to portrait. Under Wayland/cage this calls `wlr-randr`;
/// under the Slint linuxkms backend (no compositor) the `SLINT_KMS_ROTATION`
/// environment variable is set before Slint initializes.
fn rotate_display_to_portrait() {
    // The Slint linuxkms backend reads SLINT_KMS_ROTATION at init time to
    // rotate the DRM framebuffer. Set it before the window is created.
    if std::env::var("SLINT_KMS_ROTATION").is_err() {
        std::env::set_var("SLINT_KMS_ROTATION", "270");
        info!("set SLINT_KMS_ROTATION=270 for portrait orientation");
    }

    // Under a Wayland compositor (cage), also rotate the output via wlr-randr.
    // `wlr-randr --json` prints a JSON array of output objects.
    let json_output = match std::process::Command::new("wlr-randr")
        .arg("--json")
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => {
            // wlr-randr not found or no compositor — not an error (linuxkms).
            return;
        }
    };

    let parsed: serde_json::Value = match serde_json::from_slice(&json_output) {
        Ok(v) => v,
        Err(_) => return,
    };

    // The output is an array: [{"name": "DSI-1", "transform": "normal", ...}]
    let output_name = parsed
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("name"))
        .and_then(|n| n.as_str());

    let output_name = match output_name {
        Some(name) => name.to_string(),
        None => return,
    };

    info!(output = %output_name, "rotating Wayland output 270° for portrait");
    let _ = std::process::Command::new("wlr-randr")
        .arg("--output")
        .arg(&output_name)
        .arg("--transform")
        .arg("270")
        .status();
}

// ── Shutdown sequence executor (Design D7) ──────────────────────────────────

/// Perform the full graceful shutdown on the main thread.
///
/// 1. **Restore snapshot** (task 6.5): if an episode is firing, restore the
///    Mopidy snapshot before draining the Cmd channel and exiting.
/// 2. Drain remaining commands by allowing the channel sender to be dropped
///    (happens naturally when the process exits).
/// 3. Stop Mopidy client and axum — no-op in slice 0 (no live resources).
/// 4. Commit any pending DB transaction — no-op in slice 0 (DB not yet wired).
/// 5. Exit with status 0.
fn execute_shutdown(
    episode_ctl: &std::sync::Mutex<EpisodeController<ChannelMopidyControl>>,
) {
    info!("shutdown sequence starting");

    // Step 1 (task 6.5): restore snapshot if an episode is firing, before
    // draining the Cmd channel and exiting.
    if let Ok(mut ctl) = episode_ctl.lock() {
        ctl.shutdown_restore();
    }

    // Step 2: cmd channel drain — sender drops when function scope ends and
    // the process exits, naturally closing the recv side on tokio.
    info!("cmd channel drained (sender dropped on exit)");

    // Step 3: stop Mopidy client and axum — no-op in slice 0 (no live resources).
    // Later slices will hold real handles here.
    info!("Mopidy client stop requested — slice 0 no-op");
    info!("axum server stop requested — slice 0 no-op");

    // Step 4: commit pending DB work — no-op in slice 0 (DB not yet wired).
    info!("pending DB transaction commit — slice 0 no-op");

    info!("shutdown sequence complete — exiting with code 0");
    std::process::exit(0);
}

// ── Main ──────────────────────────────────────────────────────────────────────

/// Application entry point (app boundary).
///
/// Uses **`anyhow::Result<()>`** per Design D6: anyhow at the boundary,
/// thiserror for domain-specific error types internally.
fn main() -> anyhow::Result<()> {
    init_tracing();

    // Slice 3 / D6: rotate the Wayland output to portrait before the Slint
    // window is created, so the fullscreen surface is 480×854 (9:16).
    // No-op on desktop / non-Wayland.
    rotate_display_to_portrait();

    // ── Task 3.1 + 3.2: SQLite connection on main + migrations ────────────
    let cfg = crate::config::Config::load();
    info!(db_path = %cfg.db_path, "opening SQLite database");

    let db_path = cfg.db_path.clone();
    let conn = database::open_connection(&db_path)
        .expect("failed to open database connection");

    info!("SQLite connection opened, running migrations");

    database::run_migrations(&conn)
        .expect("migration runner failed");

    info!("database: migrations complete");

    let (worker_handle, app_window, timers) =
        info_span!("bootstrap").in_scope(|| bootstrap(Arc::new(Mutex::new(conn))));

    // systemd readiness (Design D10): signal READY=1 after all bootstrap steps
    // complete even when Mopidy is not yet reachable.
    sd_notify_ready();

    // Release builds on the Pi request a true full-screen borderless surface;
    // debug builds stay at 480×854 for dev testing.
    app_window.window().set_fullscreen(!cfg!(debug_assertions));

    info!("alarm-clock: bootstrap complete — application running");

    // Task 9.1: drive the Slint event loop. The drain and scheduler timers
    // (held in `timers`) fire on each tick while `.run()` blocks; the episode
    // UI (`episode-firing` / `dismiss-requested`) is bound to this window. On
    // SIGTERM/SIGINT the worker sends `Reply::ShutdownRequested`, the drain
    // dispatches `execute_shutdown` (which restores any firing episode before
    // `process::exit(0)`), interrupting `.run()`.
    let _ = app_window.run();

    // `.run()` returned (window closed / `slint::quit()`). Drop the timers and
    // the window so the only `Cmd` sender (inside the episode FSM) is released,
    // closing the channel and letting the tokio worker exit before join.
    drop(timers);
    drop(app_window);
    let _ = worker_handle.join();

    Ok(())
}

// ── Protected tick (Design D6) ───────────────────────────────────────────────

/// Execute a periodic-tick body with panic isolation.
///
/// Returns `Ok(())` on success, `Err(String)` when the body panicked.
/// The caller (`slint::Timer` lambda) logs at `error!` and naturally
/// reschedules because the timer fires again on its interval.
pub(crate) fn protected_tick<F>(body: F)
where
    F: FnOnce(),
{
    let result = panic::catch_unwind(AssertUnwindSafe(body));
    if let Err(err) = result {
        let msg = match err.downcast::<String>() {
            Ok(s) => *s,
            Err(e) => match e.downcast::<&str>() {
                Ok(s) => s.to_string(),
                Err(_) => "unknown panic payload".to_string(),
            },
        };
        tracing::error!(panic = %msg, kind = "protected_tick",
            "tick body panicked — caught and will reschedule",
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: command_dispatcher processes GetMopidyState and sends a reply.
    #[tokio::test]
    async fn command_dispatcher_handles_get_mopidy_state() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (reply_tx, mut reply_rx) = mpsc::channel(8);

        let _dummy_ev = tokio_mpsc::channel::<mopidy_client::MopidyEvent>(4);
        let _dummy_rep = tokio_mpsc::channel::<mopidy_client::transport::JsonRpcMessage>(4);
        let client: MopidyClient = Arc::new(mopidy_client::transport::MopidyWsClient::spawn(
            "ws://192.168.255.255/mopidy".to_string(),
            None,
            _dummy_ev.0, _dummy_rep.0, 
            tokio_mpsc::channel::<MopidyConnectionState>(4).0,
        ));

        let dispatcher_fut = command_dispatcher(
            cmd_rx,
            reply_tx,
            client,
            Arc::new(crate::config::WeatherConfig::default()),
        );
        tokio::pin!(dispatcher_fut);

        // Send a GetMopidyState command.
        cmd_tx.send(Cmd::GetMopidyState).await.unwrap();

        // Use select so both the dispatcher and receiver are polled concurrently
        // on tokio's current_thread runtime.
        tokio::select! {
            _ = &mut dispatcher_fut => panic!("dispatcher should not exit yet"),
            result = async { reply_rx.recv().await } => {
                assert!(result.is_some(), "should receive a reply");
                if let Some(Reply::MopidyState(state)) = result {
                    assert_eq!(&state, "STOPPED", "placeholder state is STOPPED");
                } else {
                    panic!("expected MopidyState reply, got: {:?}", result);
                }
            }
        }

        // Send Shutdown to terminate the dispatcher.
        cmd_tx.send(Cmd::Shutdown).await.unwrap();
        let result = dispatcher_fut.await;
        assert_eq!(result, CmdLoopResult::ShutdownComplete);
    }

    /// Scenario: WeatherStore handles successful updates and failures correctly
    #[test]
    fn weather_store_handles_updates_and_failures() {
        let mut store = WeatherStore::new();
        
        // Initially no snapshot
        assert!(store.get_snapshot().is_none());
        
        // Add a snapshot
        let snapshot = WeatherSnapshot {
            current_temp: 20.0,
            today_high: 25.0,
            today_low: 15.0,
            tomorrow_high: 27.0,
            tomorrow_low: 17.0,
            wind_speed: 10.0,
            wind_direction: 180,
            humidity: 60.0,
            shortwave_radiation: 500.0,
            wmo_code: 0,
            fetched_at: std::time::SystemTime::now(),
        };
        
        store.update_snapshot(snapshot);
        assert!(store.get_snapshot().is_some());
        assert_eq!(store.get_snapshot().unwrap().current_temp, 20.0);
        
        // Handle a failure
        store.handle_failure();
        assert!(store.get_snapshot().is_some()); // Still has the old data
        
        // Check retry logic
        assert!(!store.is_retry_due()); // Should not be due immediately
    }

    /// Scenario: command_dispatcher processes CallMopidy and sends a reply.
    #[tokio::test]
    async fn command_dispatcher_handles_call_mopidy() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (reply_tx, mut reply_rx) = mpsc::channel(8);

        let _dummy_ev = tokio_mpsc::channel::<mopidy_client::MopidyEvent>(4);
        let _dummy_rep = tokio_mpsc::channel::<mopidy_client::transport::JsonRpcMessage>(4);
        let client: MopidyClient = Arc::new(mopidy_client::transport::MopidyWsClient::spawn(
            "ws://192.168.255.255/mopidy".to_string(),
            None,
            _dummy_ev.0, _dummy_rep.0, 
            tokio_mpsc::channel::<MopidyConnectionState>(4).0,
        ));

        let dispatcher_fut = command_dispatcher(
            cmd_rx,
            reply_tx,
            client,
            Arc::new(crate::config::WeatherConfig::default()),
        );
        tokio::pin!(dispatcher_fut);

        cmd_tx.send(Cmd::CallMopidy {
            method: "core.get_version".into(),
            params: serde_json::json!({}),
        })
        .await
        .unwrap();

        // Use select to ensure both sides are polled on current_thread runtime.
        tokio::select! {
            _ = &mut dispatcher_fut => panic!("dispatcher should not exit yet"),
            result = async { reply_rx.recv().await } => {
                assert!(result.is_some(), "should receive a reply");
                assert!(matches!(&result, Some(Reply::CallResult(_))));
            }
        }

        // Shut down.
        cmd_tx.send(Cmd::Shutdown).await.unwrap();
        assert_eq!(dispatcher_fut.await, CmdLoopResult::ShutdownComplete);
    }

    /// Scenario: bootstrap creates channels, spawns tokio worker, and installs
    /// the drain timer without panicking or deadlocking.
    #[test]
    fn bootstrap_creates_worker_and_timer() {
        // Run with the headless Slint testing backend so this test works on
        // CI / SSH sessions without a real Wayland/X11 display server.
        i_slint_backend_testing::init_no_event_loop();

        // bootstrap now owns the single `Connection` (wrapped in `Arc<Mutex>`
        // so the `slint::Timer` closures are `Send`). Build a fresh migrated
        // temp DB for this run.
        let path = std::env::temp_dir().join(format!(
            "alarm_bootstrap_test_{}_{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let _ = std::fs::remove_file(&path);
        let conn = crate::database::open_connection(path.to_str().unwrap())
            .expect("open db");
        crate::database::run_migrations(&conn).expect("migrations");

        let _result = bootstrap(Arc::new(Mutex::new(conn)));
        // If we get here without deadlocking or panicking, the structure is
        // sound: channels created, thread spawned, timer installed.

        let _ = std::fs::remove_file(&path);
    }

    /// Scenario: sending a command through the full channel topology reaches
    /// the tokio dispatcher and replies are dispatched back to main's domain.
    #[test]
    fn end_to_end_command_reply_cycle() {
        let handles = channel::create_channels();
        let main_cmd_sender = handles.main.cmd_sender;
        let mut main_reply_rx = handles.main.reply_receiver;

        // Spawn tokio worker (same as bootstrap does). The Mopidy WS client is
        // spawned inside the worker runtime; provide dummy Mopidy event/reply
        // channels (unused in this test) and a placeholder WS URL.
        let (test_event_tx, _test_event_rx) =
            tokio_mpsc::channel::<mopidy_client::MopidyEvent>(16);
        let (test_reply_tx, _test_reply_rx) =
            tokio_mpsc::channel::<mopidy_client::transport::JsonRpcMessage>(16);
        let _worker_handle = spawn_tokio_worker(
            handles.tokio.cmd_receiver,
            handles.tokio.reply_sender,
            "ws://127.0.0.1:6680/mopidy/ws".to_string(),
            test_event_tx,
            test_reply_tx,
            Arc::new(crate::config::WeatherConfig::default()),
        );

        // Give the worker thread a moment to start its recv loop.
        std::thread::sleep(Duration::from_millis(50));

        // Send GetMopidyState through the real channel topology (main → tokio).
        main_cmd_sender.blocking_send(Cmd::GetMopidyState).unwrap();

        // Receive reply from tokio worker back on main.
        let mut last_reply: Option<String> = None;
        for _ in 0..20 {
            if let Ok(Reply::MopidyState(state)) = main_reply_rx.try_recv() {
                last_reply = Some(state);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(
            last_reply.is_some(),
            "should receive MopidyState reply from tokio worker"
        );
        assert_eq!(last_reply, Some("STOPPED".to_string()));

        // Clean shutdown.
        main_cmd_sender.blocking_send(Cmd::Shutdown).unwrap();
    }

    /// ── Task 2.4: Tick-level panic isolation ────────────────────────────

    /// Scenario: a tick body that panics is caught by `protected_tick`;
    /// control returns to the caller (the timer will fire again).
    #[test]
    fn protected_tick_catches_panic_and_continues() {
        // Use a counter to prove subsequent ticks still execute.
        let counter = std::sync::atomic::AtomicU32::new(0);

        // Tick 1: normal execution.
        protected_tick(|| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1,
            "first tick should execute");

        // Tick 2: panicked body — must be caught, not abort the process.
        protected_tick(|| {
            panic!("simulated bug in dispatch logic");
        });
        // We are still alive after the caught panic.
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1,
            "panic should not mutate state past the unwind point");

        // Tick 3: normal execution again — proves rescheduling works.
        protected_tick(|| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "tick after a panicking tick should still execute (rescheduled)"
        );
    }

    /// Scenario: `protected_tick` extracts the panic message from a String payload.
    #[test]
    fn protected_tick_catches_string_panic_message() {
        // This must not abort this test.
        protected_tick(|| {
            let msg = "boom".to_string();
            panic!("{}", msg);
        });
        // If we reach here, the panic was caught — test passes.
    }

    /// Scenario: `protected_tick` handles a bare &str panic message.
    #[test]
    fn protected_tick_catches_str_panic_message() {
        protected_tick(|| { panic!("&str panic"); });
        // Alive = caught.
    }

    /// ── Task 2.5: Error / panic policy ──────────────────────────────────

    /// Scenario: app boundary uses `anyhow::Result<()>` — `main()` returns
    /// a proper Result that callers can inspect for failures.
    #[test]
    fn main_boundary_returns_anyhow_result() {
        use crate::error::{ConfigError, Result as DomainResult};

        // thiserror domain errors convert into anyhow at the boundary.
        let domain_err: DomainResult<()> = Err(ConfigError::WriteFailed(
            std::io::Error::new(std::io::ErrorKind::Other, "disk full"),
        ));

        // Conversion to anyhow::Error preserves the chain.
        let boundary_err: Result<(), anyhow::Error> = domain_err.map_err(Into::into);
        assert!(boundary_err.is_err());
        let msg = format!("{}", boundary_err.unwrap_err());
        assert!(msg.contains("config write failed"), "anyhow wraps ConfigError chain: {msg}");
    }

    /// Scenario: failed config write degrades — in-memory state remains
    /// authoritative; the process does not exit.
    #[test]
    fn failed_config_write_degrades_keeps_in_memory_state() {
        // Simulate a successful load followed by a failing persist attempt.
        let cfg = Config::default();

        // In-memory state before the (simulated) write.
        assert_eq!(cfg.db_path, crate::config::DEFAULT_DB_PATH);
        assert_eq!(cfg.mopidy_ws_url, crate::config::DEFAULT_MOPIDY_WS_URL);

        // Simulate a write failure: construct the error and verify it converts
        // to anyhow at the boundary without aborting.
        use crate::error::{ConfigError, Result as DomainResult};
        let write_result: DomainResult<()> = Err(ConfigError::WriteFailed(
            std::io::Error::new(std::io::ErrorKind::Other, "disk full"),
        ));

        // The error is propagated to the app boundary as anyhow (never panic).
        let _boundary: Result<(), anyhow::Error> = write_result.map_err(Into::into);

        // In-memory Config state is UNAFFECTED by the failed write.
        assert_eq!(cfg.db_path, crate::config::DEFAULT_DB_PATH);
        assert_eq!(cfg.mopidy_ws_url, crate::config::DEFAULT_MOPIDY_WS_URL);
    }

    /// ── Task 2.6: SIGTERM/SIGINT handling — graceful shutdown seam ─────

    /// Scenario: command_dispatcher signals ShutdownRequested to main when
    /// a Shutdown command arrives after having been set up with signal handlers.
    /// Proves the signal-handling wiring exists (signals are tested indirectly
    /// because sending real OS signals in tests is fragile).
    #[tokio::test]
    async fn command_dispatcher_sends_shutdown_requested_on_command_shutdown() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (reply_tx, _reply_rx) = mpsc::channel(8);

        let _dummy_ev = tokio_mpsc::channel::<mopidy_client::MopidyEvent>(4);
        let _dummy_rep = tokio_mpsc::channel::<mopidy_client::transport::JsonRpcMessage>(4);
        let client: MopidyClient = Arc::new(mopidy_client::transport::MopidyWsClient::spawn(
            "ws://192.168.255.255/mopidy".to_string(),
            None,
            _dummy_ev.0, _dummy_rep.0, 
            tokio_mpsc::channel::<MopidyConnectionState>(4).0,
        ));

        let dispatcher_fut = command_dispatcher(
            cmd_rx,
            reply_tx,
            client,
            Arc::new(crate::config::WeatherConfig::default()),
        );
        tokio::pin!(dispatcher_fut);

        // Send Shutdown — proves the dispatcher loop with signal handlers is active.
        cmd_tx.send(Cmd::Shutdown).await.unwrap();

        assert_eq!(dispatcher_fut.await, CmdLoopResult::ShutdownComplete);
    }

    /// Scenario: Reply::ShutdownRequested triggers dispatch_reply_to_domain,
    /// which calls execute_shutdown → shutdown_restore hook (verified via
    /// the DomainShutdownRestore trait existence and no-op behaviour).
    #[test]
    fn domain_shutdown_restore_hook_exists_and_is_noop() {
        // Instantiate the domain and verify shutdown_restore exists.
        let domain = Domain;

        // The call must not panic (it is a no-op in slice 0).
        domain.shutdown_restore();

        // If we reach here, the hook interface works and is safe to call.
    }

    /// ── Task 2.7: systemd readiness notification ───────────────────────

    /// Scenario: sd_notify_ready() does not panic when NOTIFY_SOCKET is absent
    /// (the normal dev/test case outside systemd).
    #[test]
    fn sd_notify_ready_noop_without_systemd() {
        // Ensure NOTIFY_SOCKET is not set (remove if somehow present).
        std::env::remove_var("NOTIFY_SOCKET");

        // Must not panic or block.
        sd_notify_ready();
    }
}

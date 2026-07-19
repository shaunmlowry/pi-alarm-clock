//! Web configuration interface (slice 8).
//!
//! This module defines the command/reply types for web-based configuration
//! operations. The web server runs on the tokio worker thread, but the database
//! lives on the main thread (single-threaded SQLite). Web handlers send commands
//! to main via a dedicated channel with oneshot reply channels, then await the
//! results.
//!
//! Design invariant: The database is never touched directly from tokio. All
//! config operations route through main via `WebCmd` with oneshot replies.

use crate::alarm_store::{Alarm, CalendarSource};
use crate::media::Favorite;
use crate::display::BedtimeConfig;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{oneshot, RwLock};

// ── Web configuration commands ──────────────────────────────────────────────

/// Commands sent from web handlers (tokio) to main for database operations.
/// Each command carries a oneshot sender for the reply.
#[derive(Debug)]
pub enum WebCmd {
    /// List all alarms
    ListAlarms { reply: oneshot::Sender<WebReply> },

    /// Create or update an alarm
    UpsertAlarm {
        alarm: Alarm,
        reply: oneshot::Sender<WebReply>,
    },

    /// Delete an alarm by ID
    DeleteAlarm {
        alarm_id: String,
        reply: oneshot::Sender<WebReply>,
    },

    /// List all media favorites
    ListFavorites { reply: oneshot::Sender<WebReply> },

    /// Create or update a favorite
    UpsertFavorite {
        favorite: Favorite,
        reply: oneshot::Sender<WebReply>,
    },

    /// Delete a favorite by ID
    DeleteFavorite {
        favorite_id: String,
        reply: oneshot::Sender<WebReply>,
    },

    /// List configured calendars
    ListCalendars { reply: oneshot::Sender<WebReply> },

    /// Add or update a calendar source
    UpsertCalendar {
        calendar: CalendarSource,
        reply: oneshot::Sender<WebReply>,
    },

    /// Delete a calendar source by Google calendar id
    DeleteCalendar {
        google_calendar_id: String,
        reply: oneshot::Sender<WebReply>,
    },

    /// Discover the user's Google calendars (requires an active Google pairing)
    DiscoverCalendars { reply: oneshot::Sender<WebReply> },

    /// Initiate Google OAuth2 device-flow pairing (triggered from the web UI).
    /// The device code (verification URL + user code) is returned so the web
    /// client can display it for the user to consent on another device.
    PairCalendar { reply: oneshot::Sender<WebReply> },

    /// Read the current status of a web-initiated Google account pairing.
    CalendarPairStatus { reply: oneshot::Sender<WebReply> },

    /// Set weather city
    SetWeatherCity {
        city: String,
        reply: oneshot::Sender<WebReply>,
    },

    /// Set bedtime configuration
    SetBedtime {
        config: BedtimeConfig,
        reply: oneshot::Sender<WebReply>,
    },

    /// Set theme
    SetTheme {
        theme: String,
        reply: oneshot::Sender<WebReply>,
    },

    /// Set display brightness floor
    SetDisplay {
        brightness_floor: u8,
        reply: oneshot::Sender<WebReply>,
    },

    /// Initiate web pairing (generate bearer token)
    Pair { reply: oneshot::Sender<WebReply> },

    /// Revoke web pairing (invalidate token)
    Revoke { reply: oneshot::Sender<WebReply> },

    /// Read the current bearer token from main (used to initialise the web
    /// server's cached token at startup without touching `secrets.json`).
    GetToken { reply: oneshot::Sender<WebReply> },
}

impl WebCmd {
    /// Extract the oneshot reply sender from any variant. Useful when a
    /// handler wants to reject or defer a command uniformly.
    pub fn into_reply(self) -> oneshot::Sender<WebReply> {
        match self {
            WebCmd::ListAlarms { reply } => reply,
            WebCmd::UpsertAlarm { reply, .. } => reply,
            WebCmd::DeleteAlarm { reply, .. } => reply,
            WebCmd::ListFavorites { reply } => reply,
            WebCmd::UpsertFavorite { reply, .. } => reply,
            WebCmd::DeleteFavorite { reply, .. } => reply,
            WebCmd::ListCalendars { reply } => reply,
            WebCmd::UpsertCalendar { reply, .. } => reply,
            WebCmd::DeleteCalendar { reply, .. } => reply,
            WebCmd::DiscoverCalendars { reply } => reply,
            WebCmd::PairCalendar { reply, .. } => reply,
            WebCmd::CalendarPairStatus { reply } => reply,
            WebCmd::SetWeatherCity { reply, .. } => reply,
            WebCmd::SetBedtime { reply, .. } => reply,
            WebCmd::SetTheme { reply, .. } => reply,
            WebCmd::SetDisplay { reply, .. } => reply,
            WebCmd::Pair { reply } => reply,
            WebCmd::Revoke { reply } => reply,
            WebCmd::GetToken { reply } => reply,
        }
    }
}

// ── Web configuration replies ───────────────────────────────────────────────

/// Results sent back from main to web handlers via oneshot channels.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum WebReply {
    /// Successful alarm list
    Alarms {
        alarms: Vec<Alarm>,
    },

    /// Successful favorite list
    Favorites {
        favorites: Vec<Favorite>,
    },

    /// Successful calendar list
    Calendars {
        calendars: Vec<CalendarSource>,
    },

    /// Successful discovered Google calendar list (from the user's account)
    DiscoveredCalendars {
        calendars: Vec<(String, String)>,
    },

    /// Device-flow pairing device code (web-initiated pairing). The web client
    /// displays `verification_url` + `user_code` for the user to consent.
    CalendarPairingCode {
        verification_url: String,
        user_code: String,
    },

    /// Current status of a web-initiated Google account pairing.
    CalendarPairStatus {
        #[serde(rename = "state")]
        pair_status: String,
        message: Option<String>,
    },

    /// Pairing successful
    PairSuccess {
        bearer_token: String,
        fingerprint: String,
    },

    /// Current bearer token (reply to `GetToken`)
    Token {
        bearer_token: Option<String>,
    },

    /// Generic success (for operations that don't return data)
    Success,

    /// Operation failed
    Error {
        message: String,
    },
}

// ── Web server state ────────────────────────────────────────────────────────

/// State shared with web handlers for sending commands to main.
/// This is cloned and passed to each axum route handler.
#[derive(Clone)]
pub struct WebCommandSender {
    /// Sender for web config commands (tokio → main)
    sender: tokio::sync::mpsc::Sender<WebCmd>,

    /// Cached bearer token used by the auth middleware. The token is owned by
    /// main (stored in `secrets.json`), but it is mirrored here so the
    /// stateless middleware can validate headers without a channel round-trip
    /// on every request. Pair/revoke update this cache via replies from main.
    bearer_token: Arc<RwLock<Option<String>>>,
}

impl WebCommandSender {
    /// Create a new web command sender with an empty token cache.
    pub fn new(sender: tokio::sync::mpsc::Sender<WebCmd>) -> Self {
        Self {
            sender,
            bearer_token: Arc::new(RwLock::new(None)),
        }
    }

    /// Send a web command and await the reply
    pub async fn send(&self, cmd: WebCmd) -> Result<WebReply, String> {
        // Create oneshot channel for this specific request
        let (reply_tx, reply_rx) = oneshot::channel();

        // Inject the oneshot sender into the command
        let cmd = match cmd {
            WebCmd::ListAlarms { reply: _ } => WebCmd::ListAlarms { reply: reply_tx },
            WebCmd::UpsertAlarm { alarm, .. } => WebCmd::UpsertAlarm { alarm, reply: reply_tx },
            WebCmd::DeleteAlarm { alarm_id, .. } => {
                WebCmd::DeleteAlarm { alarm_id, reply: reply_tx }
            }
            WebCmd::ListFavorites { reply: _ } => WebCmd::ListFavorites { reply: reply_tx },
            WebCmd::UpsertFavorite { favorite, .. } => {
                WebCmd::UpsertFavorite { favorite, reply: reply_tx }
            }
            WebCmd::DeleteFavorite { favorite_id, .. } => {
                WebCmd::DeleteFavorite { favorite_id, reply: reply_tx }
            }
            WebCmd::ListCalendars { reply: _ } => WebCmd::ListCalendars { reply: reply_tx },
            WebCmd::UpsertCalendar { calendar, .. } => {
                WebCmd::UpsertCalendar { calendar, reply: reply_tx }
            }
            WebCmd::DeleteCalendar { google_calendar_id, .. } => {
                WebCmd::DeleteCalendar { google_calendar_id, reply: reply_tx }
            }
            WebCmd::DiscoverCalendars { reply: _ } => {
                WebCmd::DiscoverCalendars { reply: reply_tx }
            }
            WebCmd::PairCalendar { reply: _ } => {
                WebCmd::PairCalendar { reply: reply_tx }
            }
            WebCmd::CalendarPairStatus { reply: _ } => {
                WebCmd::CalendarPairStatus { reply: reply_tx }
            }
            WebCmd::SetWeatherCity { city, .. } => {
                WebCmd::SetWeatherCity { city, reply: reply_tx }
            }
            WebCmd::SetBedtime { config, .. } => {
                WebCmd::SetBedtime { config, reply: reply_tx }
            }
            WebCmd::SetTheme { theme, .. } => WebCmd::SetTheme { theme, reply: reply_tx },
            WebCmd::SetDisplay { brightness_floor, .. } => {
                WebCmd::SetDisplay { brightness_floor, reply: reply_tx }
            }
            WebCmd::Pair { reply: _ } => WebCmd::Pair { reply: reply_tx },
            WebCmd::Revoke { reply: _ } => WebCmd::Revoke { reply: reply_tx },
            WebCmd::GetToken { reply: _ } => WebCmd::GetToken { reply: reply_tx },
        };

        // Send command to main
        self.sender
            .send(cmd)
            .await
            .map_err(|e| format!("Failed to send web command: {}", e))?;

        // Await reply from main
        reply_rx
            .await
            .map_err(|e| format!("Failed to receive web reply: {}", e))
    }

    /// Cache the bearer token locally (called after a successful pair).
    pub async fn set_bearer_token(&self, token: String) {
        *self.bearer_token.write().await = Some(token);
    }

    /// Synchronous variant of [`Self::set_bearer_token`] for callers that run
    /// outside an async runtime (main thread, UI callbacks).
    pub fn set_bearer_token_blocking(&self, token: String) {
        *self.bearer_token.blocking_write() = Some(token);
    }

    /// Clear the cached bearer token (called after a successful revoke).
    pub async fn clear_bearer_token(&self) {
        *self.bearer_token.write().await = None;
    }

    /// Read the cached bearer token.
    pub async fn bearer_token(&self) -> Option<String> {
        self.bearer_token.read().await.clone()
    }

    /// Non-blocking send of a `WebCmd`, used by UI callbacks that run outside
    /// the async handler path (e.g. the Pi "Pair Web" button). Returns an error
    /// if the channel is full or closed.
    pub fn try_send(&self, cmd: WebCmd) -> Result<(), tokio::sync::mpsc::error::TrySendError<WebCmd>> {
        self.sender.try_send(cmd)
    }
}

// ── Channel creation ────────────────────────────────────────────────────────

/// Capacity for web command channel
const WEB_CMD_CHANNEL_CAPACITY: usize = 32;

/// Create a web command channel pair
pub fn create_web_channels() -> (
    WebCommandSender,
    tokio::sync::mpsc::Receiver<WebCmd>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(WEB_CMD_CHANNEL_CAPACITY);
    (WebCommandSender::new(tx), rx)
}

// ── JSON DTOs ───────────────────────────────────────────────────────────────

/// JSON DTO for alarm CRUD
#[derive(Debug, Serialize, Deserialize)]
pub struct AlarmDto {
    pub id: String,
    pub enabled: bool,
    pub name: String,
    pub time_local: String,
    pub timezone: String,
    pub rrule: Option<String>,
    pub source_uri: String,
    pub max_volume: i64,
    pub snooze_minutes: i64,
    pub max_snoozes: i64,
    pub holiday_policy: String,
}

impl From<Alarm> for AlarmDto {
    fn from(a: Alarm) -> Self {
        Self {
            id: a.id,
            enabled: a.enabled,
            name: a.name,
            time_local: a.time_local,
            timezone: a.timezone,
            rrule: a.rrule,
            source_uri: a.source_uri,
            max_volume: a.max_volume,
            snooze_minutes: a.snooze_minutes,
            max_snoozes: a.max_snoozes,
            holiday_policy: a.holiday_policy.as_db_str().to_string(),
        }
    }
}

impl From<AlarmDto> for Alarm {
    fn from(d: AlarmDto) -> Self {
        Self {
            id: d.id,
            enabled: d.enabled,
            name: d.name,
            time_local: d.time_local,
            timezone: d.timezone,
            rrule: d.rrule,
            once_at: None,
            source_uri: d.source_uri,
            max_volume: d.max_volume,
            escalation_steps: None,
            fallback_chain: None,
            visual_config: None,
            snooze_minutes: d.snooze_minutes,
            max_snoozes: d.max_snoozes,
            holiday_policy: crate::alarm_store::HolidayPolicy::from_db_str(&d.holiday_policy),
            next_fire: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// JSON DTO for favorite CRUD
#[derive(Debug, Serialize, Deserialize)]
pub struct FavoriteDto {
    pub id: String,
    pub name: String,
    pub source_uri: String,
    pub source_type: String,
}

impl From<Favorite> for FavoriteDto {
    fn from(f: Favorite) -> Self {
        Self {
            id: f.id,
            name: f.name,
            source_uri: f.source.uri().to_string(),
            source_type: f.source.type_tag().to_string(),
        }
    }
}

impl From<FavoriteDto> for Favorite {
    fn from(d: FavoriteDto) -> Self {
        Self {
            id: d.id,
            name: d.name,
            source: crate::media::AudioSource::from_type_tag(&d.source_type, &d.source_uri),
            display_order: 0,
        }
    }
}

/// JSON DTO for calendar list
#[derive(Debug, Serialize, Deserialize)]
pub struct CalendarDto {
    pub google_calendar_id: String,
    pub display_name: String,
    pub role: String,
}

impl From<CalendarSource> for CalendarDto {
    fn from(c: CalendarSource) -> Self {
        Self {
            google_calendar_id: c.google_calendar_id,
            display_name: c.display_name,
            role: c.role.as_db_str().to_string(),
        }
    }
}

impl From<CalendarDto> for CalendarSource {
    fn from(d: CalendarDto) -> Self {
        Self {
            google_calendar_id: d.google_calendar_id,
            display_name: d.display_name,
            role: crate::alarm_store::CalendarRole::from_db_str(&d.role),
        }
    }
}

/// JSON DTO for bedtime config
#[derive(Debug, Serialize, Deserialize)]
pub struct BedtimeDto {
    pub weekday: BedtimeWindowDto,
    pub weekend: BedtimeWindowDto,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BedtimeWindowDto {
    pub start: String,
    pub end: String,
}

impl From<BedtimeConfig> for BedtimeDto {
    fn from(c: BedtimeConfig) -> Self {
        Self {
            weekday: BedtimeWindowDto {
                start: c.weekday.start.to_string(),
                end: c.weekday.end.to_string(),
            },
            weekend: BedtimeWindowDto {
                start: c.weekend.start.to_string(),
                end: c.weekend.end.to_string(),
            },
        }
    }
}

// ── Axum routes (task 1.2) ──────────────────────────────────────────────────

use axum::{
    extract::{Json as JsonExtractor, Path, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};

/// Build the web config router with all endpoints.
/// Task 1.3: Config-only — no live-control / dismiss-snooze endpoints (return 404).
/// Task 2.1: All config endpoints require a valid bearer token; `/api/pair`
/// is public because that is how the client obtains the token.
pub fn web_routes(state: WebCommandSender) -> Router {
    let public = Router::new()
        .route("/", get(serve_index))
        .route("/app.js", get(serve_app_js))
        .route("/api/pair", get(pair))
        .with_state(state.clone());

    let protected = Router::new()
        .route("/api/alarms", get(list_alarms).post(create_alarm))
        .route("/api/alarms/{id}", put(update_alarm).delete(delete_alarm))
        .route("/api/favorites", get(list_favorites).post(create_favorite))
        .route("/api/favorites/{id}", put(update_favorite).delete(delete_favorite))
        .route("/api/calendars", get(list_calendars).post(create_calendar))
        .route("/api/calendars/discover", post(discover_calendars))
        .route("/api/calendars/pair", post(pair_calendar))
        .route("/api/calendars/pair/status", get(calendar_pair_status))
        .route("/api/calendars/{id}", delete(delete_calendar))
        .route("/api/weather-city", put(set_weather_city))
        .route("/api/bedtime", put(set_bedtime))
        .route("/api/theme", put(set_theme))
        .route("/api/display", put(set_display))
        .route("/api/revoke", delete(revoke))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .with_state(state.clone());

    public.merge(protected)
}

/// Stateless bearer-token middleware. Reads the `Authorization: Bearer <token>`
/// header and compares it to the cached token populated by pair/revoke. No
/// per-client session state is kept; the same shared secret is required for
/// every protected request.
async fn require_auth(
    State(sender): State<WebCommandSender>,
    request: Request,
    next: Next,
) -> impl IntoResponse {
    let expected = sender.bearer_token().await;
    let provided = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));

    match (expected, provided) {
        (Some(expected), Some(provided)) if expected == provided => next.run(request).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(WebReply::Error {
                message: "Invalid or missing bearer token".to_string(),
            }),
        )
            .into_response(),
    }
}

async fn list_alarms(State(sender): State<WebCommandSender>) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::ListAlarms { reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn create_alarm(
    State(sender): State<WebCommandSender>,
    JsonExtractor(dto): JsonExtractor<AlarmDto>,
) -> impl IntoResponse {
    let alarm: Alarm = dto.into();
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::UpsertAlarm { alarm, reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::CREATED, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn update_alarm(
    State(sender): State<WebCommandSender>,
    Path(_id): Path<String>,
    JsonExtractor(dto): JsonExtractor<AlarmDto>,
) -> impl IntoResponse {
    let alarm: Alarm = dto.into();
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::UpsertAlarm { alarm, reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn delete_alarm(
    State(sender): State<WebCommandSender>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::DeleteAlarm { alarm_id: id, reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn list_favorites(State(sender): State<WebCommandSender>) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::ListFavorites { reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn create_favorite(
    State(sender): State<WebCommandSender>,
    JsonExtractor(dto): JsonExtractor<FavoriteDto>,
) -> impl IntoResponse {
    let fav: Favorite = dto.into();
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::UpsertFavorite { favorite: fav, reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::CREATED, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn update_favorite(
    State(sender): State<WebCommandSender>,
    Path(_id): Path<String>,
    JsonExtractor(dto): JsonExtractor<FavoriteDto>,
) -> impl IntoResponse {
    let fav: Favorite = dto.into();
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::UpsertFavorite { favorite: fav, reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn delete_favorite(
    State(sender): State<WebCommandSender>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::DeleteFavorite { favorite_id: id, reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn list_calendars(State(sender): State<WebCommandSender>) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::ListCalendars { reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn discover_calendars(State(sender): State<WebCommandSender>) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::DiscoverCalendars { reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn create_calendar(
    State(sender): State<WebCommandSender>,
    JsonExtractor(dto): JsonExtractor<CalendarDto>,
) -> impl IntoResponse {
    if dto.google_calendar_id.trim().is_empty() || dto.display_name.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(WebReply::Error { message: "google_calendar_id and display_name are required".into() }),
        );
    }
    let cal: CalendarSource = dto.into();
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::UpsertCalendar { calendar: cal, reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::CREATED, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn delete_calendar(
    State(sender): State<WebCommandSender>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::DeleteCalendar { google_calendar_id: id, reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn pair_calendar(State(sender): State<WebCommandSender>) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::PairCalendar { reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn calendar_pair_status(State(sender): State<WebCommandSender>) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::CalendarPairStatus { reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn set_weather_city(
    State(sender): State<WebCommandSender>,
    JsonExtractor(body): JsonExtractor<serde_json::Value>,
) -> impl IntoResponse {
    let city = body.get("city").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::SetWeatherCity { city, reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn set_bedtime(
    State(sender): State<WebCommandSender>,
    JsonExtractor(dto): JsonExtractor<BedtimeDto>,
) -> impl IntoResponse {
    use chrono::NaiveTime;
    let parse_time = |s: &str| -> Result<NaiveTime, String> {
        NaiveTime::parse_from_str(s, "%H:%M").map_err(|e| e.to_string())
    };
    let config = BedtimeConfig {
        weekday: crate::display::BedtimeWindow {
            start: parse_time(&dto.weekday.start).unwrap_or_default(),
            end: parse_time(&dto.weekday.end).unwrap_or_default(),
        },
        weekend: crate::display::BedtimeWindow {
            start: parse_time(&dto.weekend.start).unwrap_or_default(),
            end: parse_time(&dto.weekend.end).unwrap_or_default(),
        },
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::SetBedtime { config, reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn set_theme(
    State(sender): State<WebCommandSender>,
    JsonExtractor(body): JsonExtractor<serde_json::Value>,
) -> impl IntoResponse {
    let theme = body.get("theme").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::SetTheme { theme, reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn set_display(
    State(sender): State<WebCommandSender>,
    JsonExtractor(body): JsonExtractor<serde_json::Value>,
) -> impl IntoResponse {
    let brightness_floor = body.get("brightness_floor").and_then(|v| v.as_u64()).unwrap_or(50) as u8;
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::SetDisplay { brightness_floor, reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn pair(State(sender): State<WebCommandSender>) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::Pair { reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(WebReply::PairSuccess { bearer_token, fingerprint }) => {
                sender.set_bearer_token(bearer_token.clone()).await;
                (StatusCode::OK, Json(WebReply::PairSuccess { bearer_token, fingerprint }))
            }
            Ok(reply) => (StatusCode::OK, Json(reply)),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

async fn revoke(State(sender): State<WebCommandSender>) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    match sender.sender.send(WebCmd::Revoke { reply: reply_tx }).await {
        Ok(()) => match reply_rx.await {
            Ok(reply) => {
                sender.clear_bearer_token().await;
                (StatusCode::OK, Json(reply))
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(WebReply::Error { message: e.to_string() })),
    }
}

// ── Static SPA bundle ───────────────────────────────────────────────────────

async fn serve_index() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../web/index.html"),
    )
}

async fn serve_app_js() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        include_str!("../web/app.js"),
    )
}

// ── Server startup ──────────────────────────────────────────────────────────

/// Build the pairing URL encoded in the Pi's QR code.
///
/// v1 security: the fingerprint is exposed for **manual user verification**
/// only. Browsers do not expose raw certificate bytes to `fetch`, so true
/// programmatic TLS-fingerprint pinning is deferred to v2. v1 relies on the
/// LAN being the trust boundary plus browser trust-on-first-use for the
/// self-signed certificate.
pub fn pairing_url(host: &str, port: u16, token: &str, fingerprint: &str) -> String {
    format!("https://{}:{}/#token={}&fp={}", host, port, token, fingerprint)
}

/// Generate a new random bearer token.
pub fn generate_bearer_token() -> String {
    use rand::distributions::Alphanumeric;
    use rand::Rng;
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

/// Best-effort local LAN IP address used for the IP fallback URL on the Pi
/// pairing screen. Connects a UDP socket to a public address and reads the
/// bound local address; this works even if the host is not routable to the
/// public address because no packets are actually sent.
pub fn local_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip().to_string())
}

/// Build an IP-based pairing URL for manual fallback.
pub fn ip_pairing_url(port: u16, token: &str, fingerprint: &str) -> Option<String> {
    local_ip().map(|ip| pairing_url(&ip, port, token, fingerprint))
}

/// Start the axum web config server over TLS.
///
/// Binds to `bind_addr`, serves [`web_routes`] with the supplied
/// [`WebCommandSender`], and terminates when `shutdown` fires.
pub async fn serve_axum(
    bind_addr: std::net::SocketAddr,
    sender: WebCommandSender,
    tls_cert: crate::web::tls::TlsCert,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), String> {
    let rustls_config = crate::web::tls::rustls_server_config(&tls_cert.cert_pem, &tls_cert.key_pem)?;
    let tls_config = axum_server::tls_rustls::RustlsConfig::from_config(std::sync::Arc::new(rustls_config));
    let app = web_routes(sender);
    let handle = axum_server::Handle::new();

    let server = axum_server::bind_rustls(bind_addr, tls_config)
        .handle(handle.clone())
        .serve(app.into_make_service());

    tokio::select! {
        result = server => result.map_err(|e| format!("axum server error: {e}")),
        _ = shutdown => {
            handle.graceful_shutdown(Some(std::time::Duration::from_secs(1)));
            Ok(())
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{Request, StatusCode};
    use axum::response::IntoResponse;
    use axum::Json;
    use tower::ServiceExt;

    /// Verify WebCmd variants can be constructed
    #[test]
    fn web_cmd_variants_construct() {
        let (tx, _rx) = oneshot::channel();
        let cmd = WebCmd::ListAlarms { reply: tx };
        assert!(matches!(cmd, WebCmd::ListAlarms { .. }));
    }

    /// Verify WebReply variants can be constructed
    #[test]
    fn web_reply_variants_construct() {
        let reply = WebReply::Success;
        assert!(matches!(reply, WebReply::Success));

        let reply = WebReply::Error {
            message: "test error".to_string(),
        };
        assert!(matches!(reply, WebReply::Error { .. }));
    }

    /// Verify channel creation
    #[test]
    fn create_web_channels_works() {
        let (sender, _receiver) = create_web_channels();
        // Channel should be created successfully
        assert!(sender.sender.capacity() > 0);
    }

    #[tokio::test]
    async fn test_list_alarms_routes_through_channel() {
        let (sender, mut receiver) = create_web_channels();

        let server_task = tokio::spawn(async move {
            if let Some(cmd) = receiver.recv().await {
                match cmd {
                    WebCmd::ListAlarms { reply } => {
                        let _ = reply.send(WebReply::Alarms { alarms: vec![] });
                    }
                    _ => panic!("Unexpected command"),
                }
            }
        });

        let response = list_alarms(State(sender)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_create_alarm_routes_through_channel() {
        let (sender, mut receiver) = create_web_channels();

        let server_task = tokio::spawn(async move {
            if let Some(cmd) = receiver.recv().await {
                match cmd {
                    WebCmd::UpsertAlarm { alarm, reply } => {
                        assert_eq!(alarm.name, "Test Alarm");
                        let _ = reply.send(WebReply::Success);
                    }
                    _ => panic!("Unexpected command"),
                }
            }
        });

        let dto = AlarmDto {
            id: "1".into(),
            enabled: true,
            name: "Test Alarm".into(),
            time_local: "07:00".into(),
            timezone: "UTC".into(),
            rrule: None,
            source_uri: "test".into(),
            max_volume: 100,
            snooze_minutes: 5,
            max_snoozes: 3,
            holiday_policy: "none".into(),
        };

        let response = create_alarm(State(sender), Json(dto)).await.into_response();
        assert_eq!(response.status(), StatusCode::CREATED);

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_pair_routes_through_channel() {
        let (sender, mut receiver) = create_web_channels();

        let server_task = tokio::spawn(async move {
            if let Some(cmd) = receiver.recv().await {
                match cmd {
                    WebCmd::Pair { reply } => {
                        let _ = reply.send(WebReply::PairSuccess {
                            bearer_token: "token".into(),
                            fingerprint: "fp".into(),
                        });
                    }
                    _ => panic!("Unexpected command"),
                }
            }
        });

        let response = pair(State(sender)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_revoke_routes_through_channel() {
        let (sender, mut receiver) = create_web_channels();

        let server_task = tokio::spawn(async move {
            if let Some(cmd) = receiver.recv().await {
                match cmd {
                    WebCmd::Revoke { reply } => {
                        let _ = reply.send(WebReply::Success);
                    }
                    _ => panic!("Unexpected command"),
                }
            }
        });

        let response = revoke(State(sender)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_list_alarms_channel_closed() {
        let (sender, receiver) = create_web_channels();
        drop(receiver); // Close channel

        let response = list_alarms(State(sender)).await.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_protected_endpoint_rejects_missing_token() {
        let (sender, receiver) = create_web_channels();

        // The handler must not be reached; close the receiver so that any
        // accidental command send would fail loudly.
        drop(receiver);

        let app = web_routes(sender);
        let response = app
            .oneshot(Request::builder().uri("/api/alarms").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_protected_endpoint_accepts_valid_token() {
        let (sender, mut receiver) = create_web_channels();
        sender.set_bearer_token("valid-token".to_string()).await;

        let server_task = tokio::spawn(async move {
            if let Some(cmd) = receiver.recv().await {
                match cmd {
                    WebCmd::ListAlarms { reply } => {
                        let _ = reply.send(WebReply::Alarms { alarms: vec![] });
                    }
                    _ => panic!("Unexpected command"),
                }
            }
        });

        let app = web_routes(sender);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/alarms")
                    .header("Authorization", "Bearer valid-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_pair_is_public_and_caches_token() {
        let (sender, mut receiver) = create_web_channels();

        let server_task = tokio::spawn(async move {
            if let Some(cmd) = receiver.recv().await {
                match cmd {
                    WebCmd::Pair { reply } => {
                        let _ = reply.send(WebReply::PairSuccess {
                            bearer_token: "new-token".into(),
                            fingerprint: "fp".into(),
                        });
                    }
                    _ => panic!("Unexpected command"),
                }
            }
        });

        let app = web_routes(sender.clone());
        let response = app
            .oneshot(Request::builder().uri("/api/pair").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        server_task.await.unwrap();
        assert_eq!(sender.bearer_token().await, Some("new-token".to_string()));
    }

    #[test]
    fn pairing_url_includes_token_and_fingerprint() {
        let url = pairing_url("pialarm.local", 8443, "abc123", "de:ad:be:ef");
        assert!(url.starts_with("https://pialarm.local:8443/#"));
        assert!(url.contains("token=abc123"));
        assert!(url.contains("fp=de:ad:be:ef"));
    }

    #[tokio::test]
    async fn test_revoke_invalidates_token() {
        let (sender, mut receiver) = create_web_channels();
        sender.set_bearer_token("old-token".to_string()).await;

        let server_task = tokio::spawn(async move {
            if let Some(cmd) = receiver.recv().await {
                match cmd {
                    WebCmd::Revoke { reply } => {
                        let _ = reply.send(WebReply::Success);
                    }
                    _ => panic!("Unexpected command"),
                }
            }
        });

        let app = web_routes(sender.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/revoke")
                    .header("Authorization", "Bearer old-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        server_task.await.unwrap();
        assert_eq!(sender.bearer_token().await, None);

        // After revocation, the old token must be rejected.
        let app = web_routes(sender);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/alarms")
                    .header("Authorization", "Bearer old-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_static_spa_served() {
        let (sender, _receiver) = create_web_channels();
        let app = web_routes(sender);

        let index = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(index.status(), StatusCode::OK);

        let js = app
            .oneshot(Request::builder().uri("/app.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(js.status(), StatusCode::OK);
        assert_eq!(js.headers().get("content-type").unwrap(), "application/javascript; charset=utf-8");
    }

    #[tokio::test]
    async fn test_serve_axum_starts_over_tls() {
        let dir = std::env::temp_dir().join(format!(
            "alarm_axum_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let tls_cert = crate::web::tls::TlsCert::ensure(&dir).unwrap();

        let (sender, mut receiver) = create_web_channels();
        let cmd_task = tokio::spawn(async move {
            while let Some(cmd) = receiver.recv().await {
                match cmd {
                    WebCmd::ListAlarms { reply } => {
                        let _ = reply.send(WebReply::Alarms { alarms: vec![] });
                    }
                    _ => {}
                }
            }
        });

        let bind_addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        let server_task = tokio::spawn(async move {
            serve_axum(bind_addr, sender, tls_cert, shutdown_rx).await
        });

        // Give the server a moment to bind, then request shutdown.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let _ = shutdown_tx.send(());

        let result = server_task.await.unwrap();
        assert!(result.is_ok(), "{result:?}");
        cmd_task.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

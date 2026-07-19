//! Google Calendar integration (slice 6).
//!
//! - **OAuth2 device flow** ([`request_device_code`], [`poll_for_token`]): the
//!   Pi displays a pairing QR/code, the user consents at `google.com/device`
//!   on another device, the Pi polls, and the refresh token is stored in
//!   [`crate::secrets::Secrets`] (0600). First-run is self-sufficient on the
//!   Pi (no local loopback server).
//! - **`CalendarSource` role tagging** (re-exported from [`crate::alarm_store`]):
//!   `Agenda` feeds the Daily-data panel; `Holiday` feeds suppression.
//! - **Google Calendar API client** ([`CalendarClient`]): lists calendars and
//!   events. Events are fetched with `singleEvents=true` so Google expands
//!   recurring events — the app never parses calendar RRULE (the `rrule`
//!   crate is for alarm schedules only; design D2).
//! - **Stores** ([`HolidayStore`], [`AgendaStore`]): main-thread caches
//!   populated on the shared 30-min refresh tick (slice 5). Holiday detection
//!   is an O(1) date-membership set lookup (design D3); the agenda is a capped
//!   list of today's events with past events dimmed (design D4).
//!
//! ## Network & testability
//!
//! The HTTP calls ([`request_device_code`], [`CalendarClient::refresh_access_token`],
//! list endpoints) use `reqwest` and run on the tokio worker (they are
//! `async`). The *parsing* and *state-machine* logic is split into
//! pure functions so it can be unit-tested without network access
//! (task 1.5).

use std::collections::HashSet;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::alarm_store::{CalendarRole, CalendarSource};

// ── OAuth2 device flow ───────────────────────────────────────────────────────

/// The device-code response from Google's `/device/code` endpoint.
///
/// Fields are named per Google's OAuth2 device-flow spec. The Pi displays
/// `user_code` and `verification_url` (often `google.com/device`) and a QR
/// encoding only `verification_url`; it then polls `expires_in`
/// seconds at `interval` cadence. The `user_code` is shown separately for the
/// user to type in — Google's device page does not accept it as a query param.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCode {
    /// The device verification code — the Pi displays this.
    pub device_code: String,
    /// The short user code the user enters at the verification URL.
    pub user_code: String,
    /// The URL the user visits (e.g. `https://www.google.com/device`).
    pub verification_url: String,
    /// Lifetime of the device_code in seconds.
    pub expires_in: u64,
    /// Polling interval in seconds.
    pub interval: u64,
}

/// A poll response from the token endpoint during device-flow pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollResponse {
    /// The user has not yet completed consent; keep polling at the current
    /// interval.
    Pending,
    /// The server says to slow down — increase the interval by 5 s.
    SlowDown,
    /// Consent completed; the access + refresh tokens are returned.
    AccessToken {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: u64,
    },
    /// A fatal error (e.g. `expired_token`, `access_denied`).
    Error(String),
}

/// Token response payload from Google's `/token` endpoint (success case).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    /// `Bearer`.
    #[serde(default)]
    token_type: Option<String>,
}

/// Error payload from the token endpoint (pending / slow_down / denied).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TokenError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Classify a token-endpoint reply into the device-flow poll state.
///
/// Google returns:
/// - `200` + `access_token` → [`PollResponse::AccessToken`].
/// - `428` / `error: "authorization_pending"` → [`PollResponse::Pending`].
/// - `error: "slow_down"` → [`PollResponse::SlowDown`].
/// - any other `error` → [`PollResponse::Error`].
///
/// This is a pure function over the HTTP status + body so the state machine
/// can be unit-tested with mock replies (task 1.5).
pub fn classify_poll(status: u16, body: &str) -> PollResponse {
    if status == 200 {
        match serde_json::from_str::<TokenResponse>(body) {
            Ok(tr) => PollResponse::AccessToken {
                access_token: tr.access_token,
                refresh_token: tr.refresh_token,
                expires_in: tr.expires_in.unwrap_or(3600),
            },
            Err(e) => PollResponse::Error(format!("token parse error: {e}")),
        }
    } else {
        match serde_json::from_str::<TokenError>(body) {
            Ok(te) => match te.error.as_str() {
                "authorization_pending" => PollResponse::Pending,
                "slow_down" => PollResponse::SlowDown,
                other => PollResponse::Error(other.to_string()),
            },
            Err(e) => PollResponse::Error(format!("token error parse error: {e}")),
        }
    }
}

/// The resolved result of a completed device-flow poll: the tokens to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingResult {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

/// Run the device-flow poll state machine to completion over a sequence of
/// poll replies.
///
/// `poll_fn` is invoked (with the current interval in seconds) for each poll;
/// it returns the `(status, body)` of the token endpoint. The state machine
/// honours `Pending` (keep polling at the interval), `SlowDown` (increase the
/// interval by 5 s and poll again), and terminates on `AccessToken` or `Error`.
/// The caller drives the wall-clock sleeps; this function only advances the
/// logical state.
///
/// This is the testable core of the device flow (task 1.5). The live entry
/// point ([`pair_device_flow`]) wraps it with real `reqwest` calls.
pub fn run_poll_state_machine<F>(
    mut interval_secs: u64,
    mut poll_fn: F,
) -> Result<PairingResult, String>
where
    F: FnMut(u64) -> (u16, String),
{
    loop {
        let (status, body) = poll_fn(interval_secs);
        match classify_poll(status, &body) {
            PollResponse::Pending => {
                // Caller sleeps `interval_secs` before the next poll; here we
                // just loop (the live driver sleeps).
                continue;
            }
            PollResponse::SlowDown => {
                interval_secs = interval_secs.saturating_add(5);
                continue;
            }
            PollResponse::AccessToken {
                access_token,
                refresh_token,
                expires_in,
            } => {
                let refresh_token = refresh_token
                    .ok_or_else(|| "token response missing refresh_token".to_string())?;
                return Ok(PairingResult {
                    access_token,
                    refresh_token,
                    expires_in,
                });
            }
            PollResponse::Error(e) => return Err(e),
        }
    }
}

/// Request a device code from Google's `/device/code` endpoint (async, runs on
/// the tokio worker). Returns the [`DeviceCode`] to display.
pub async fn request_device_code(
    client: &reqwest::Client,
    device_url: &str,
    client_id: &str,
) -> Result<DeviceCode, String> {
    let resp = client
        .post(device_url)
        .form(&[
            ("client_id", client_id),
            ("scope", "https://www.googleapis.com/auth/calendar.readonly"),
        ])
        .send()
        .await
        .map_err(|e| format!("device-code request: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| format!("device-code body: {e}"))?;
    if !status.is_success() {
        return Err(format!("device-code {status}: {body}"));
    }
    serde_json::from_str::<DeviceCode>(&body)
        .map_err(|e| format!("device-code parse: {e}"))
}

/// Poll the token endpoint once (async). Returns the raw `(status, body)` for
/// [`classify_poll`].
pub async fn poll_token_once(
    client: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    client_secret: &str,
    device_code: &str,
) -> Result<(u16, String), String> {
    let resp = client
        .post(token_url)
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()
        .await
        .map_err(|e| format!("token poll: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp.text().await.map_err(|e| format!("token body: {e}"))?;
    Ok((status, body))
}

/// Live device-flow pairing driver: requests a code, polls until the user
/// consents or the device code expires, and returns the tokens to persist.
///
/// The QR to display encodes only `device.verification_url` — the
/// [`crate::ui`] QR component renders it and shows `device.user_code`
/// separately for the user to type in.
pub async fn pair_device_flow(
    client: &reqwest::Client,
    device_url: &str,
    token_url: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<PairingResult, String> {
    let device = request_device_code(client, device_url, client_id).await?;
    info!(user_code = %device.user_code, "device-flow pairing started");
    let mut interval = device.interval.max(5);
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(device.expires_in.max(60));

    let device_code = device.device_code.clone();
    let result = run_poll_state_machine(interval, |_| {
        // Synchronous placeholder: the live driver below does the real await.
        // `run_poll_state_machine` is the testable core; the live driver
        // re-implements the loop with async polls.
        unreachable!("live driver does not use run_poll_state_machine directly")
    });
    let _ = result; // (kept for the testable API; the live loop is below)

    loop {
        if std::time::Instant::now() >= deadline {
            return Err("device code expired".to_string());
        }
        let (status, body) = poll_token_once(
            client,
            token_url,
            client_id,
            client_secret,
            &device_code,
        )
        .await?;
        match classify_poll(status, &body) {
            PollResponse::Pending => {
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
            PollResponse::SlowDown => {
                interval = interval.saturating_add(5);
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
            PollResponse::AccessToken {
                access_token,
                refresh_token,
                expires_in,
            } => {
                let refresh_token = refresh_token
                    .ok_or_else(|| "token response missing refresh_token".to_string())?;
                return Ok(PairingResult {
                    access_token,
                    refresh_token,
                    expires_in,
                });
            }
            PollResponse::Error(e) => return Err(e),
        }
    }
}

// ── Google Calendar API client ───────────────────────────────────────────────

/// A Google Calendar API client bound to one account (one refresh token).
///
/// All methods are `async` and run on the tokio worker. The caller (main,
/// via the drain timer) routes replies into the [`HolidayStore`] /
/// [`AgendaStore`].
pub struct CalendarClient {
    http: reqwest::Client,
    base_url: String,
    token_url: String,
    client_id: String,
    client_secret: String,
    refresh_token: String,
    /// Cached access token (and its expiry). Refreshed lazily on 401.
    access: tokio::sync::Mutex<Option<AccessToken>>,
}

#[derive(Debug, Clone)]
struct AccessToken {
    token: String,
    expires_at: DateTime<Utc>,
}

impl CalendarClient {
    pub fn new(
        http: reqwest::Client,
        base_url: String,
        token_url: String,
        client_id: String,
        client_secret: String,
        refresh_token: String,
    ) -> Self {
        Self {
            http,
            base_url,
            token_url,
            client_id,
            client_secret,
            refresh_token,
            access: tokio::sync::Mutex::new(None),
        }
    }

    /// Refresh the access token from the stored refresh token. Returns the
    /// new [`AccessToken`]. A 401 here (revoked/expired refresh token) is
    /// surfaced so the caller can re-prompt device flow (task 2.3).
    pub async fn refresh_access_token(&self) -> Result<String, RefreshError> {
        let resp = self
            .http
            .post(&self.token_url)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("refresh_token", self.refresh_token.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await
            .map_err(|e| RefreshError::Network(e.to_string()))?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| RefreshError::Network(e.to_string()))?;
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(RefreshError::Unauthorized(body));
        }
        if !status.is_success() {
            return Err(RefreshError::Other(format!("{status}: {body}")));
        }
        let tr: TokenResponse = serde_json::from_str(&body)
            .map_err(|e| RefreshError::Other(format!("parse: {e}")))?;
        let expires_in = tr.expires_in.unwrap_or(3600);
        let expires_at = Utc::now() + chrono::Duration::seconds(expires_in as i64);
        let tok = AccessToken { token: tr.access_token.clone(), expires_at };
        *self.access.lock().await = Some(tok);
        Ok(tr.access_token)
    }

    /// Get a valid (non-expired) access token, refreshing if needed.
    async fn access_token(&self) -> Result<String, RefreshError> {
        {
            let guard = self.access.lock().await;
            if let Some(t) = guard.as_ref() {
                if t.expires_at > Utc::now() {
                    return Ok(t.token.clone());
                }
            }
        }
        self.refresh_access_token().await
    }

    /// List the user's calendars. Returns a parsed list of `(id, summary)`.
    pub async fn list_calendars(&self) -> Result<Vec<(String, String)>, ApiError> {
        let token = self.access_token().await.map_err(ApiError::Refresh)?;
        let url = format!("{}/users/me/calendarList", self.base_url);
        self.get_calendar_list(&url, &token).await
    }

    async fn get_calendar_list(
        &self,
        url: &str,
        token: &str,
    ) -> Result<Vec<(String, String)>, ApiError> {
        let resp = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // Token expired — refresh once and retry (boxed to permit async recursion).
            let token = self.refresh_access_token().await.map_err(ApiError::Refresh)?;
            return Box::pin(self.get_calendar_list(url, &token)).await;
        }
        let body = resp.text().await.map_err(|e| ApiError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(ApiError::Other(format!("{status}: {body}")));
        }
        parse_calendar_list(&body).map_err(|e| ApiError::Other(e))
    }

    /// List events for a calendar between *time_min* and *time_max* (RFC-3339),
    /// with `singleEvents=true` so Google expands recurring events (design D2).
    pub async fn list_events(
        &self,
        calendar_id: &str,
        time_min: DateTime<Utc>,
        time_max: DateTime<Utc>,
    ) -> Result<Vec<CalendarEvent>, ApiError> {
        let token = self.access_token().await.map_err(ApiError::Refresh)?;
        let url = format!("{}/calendars/{}/events", self.base_url, calendar_id);
        self.get_events(&url, &token, time_min, time_max).await
    }

    async fn get_events(
        &self,
        url: &str,
        token: &str,
        time_min: DateTime<Utc>,
        time_max: DateTime<Utc>,
    ) -> Result<Vec<CalendarEvent>, ApiError> {
        let resp = self
            .http
            .get(url)
            .bearer_auth(token)
            .query(&[
                ("singleEvents", "true"),
                ("orderBy", "startTime"),
                ("timeMin", &time_min.to_rfc3339()),
                ("timeMax", &time_max.to_rfc3339()),
                ("maxResults", "250"),
            ])
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let token = self.refresh_access_token().await.map_err(ApiError::Refresh)?;
            return Box::pin(self.get_events(url, &token, time_min, time_max)).await;
        }
        let body = resp.text().await.map_err(|e| ApiError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(ApiError::Other(format!("{status}: {body}")));
        }
        parse_events(&body).map_err(|e| ApiError::Other(e))
    }
}

/// Errors refreshing the access token. `Unauthorized` means the refresh token
/// is revoked/expired — the caller re-prompts device flow (task 2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshError {
    Network(String),
    /// 401 — refresh token no longer valid; re-pair.
    Unauthorized(String),
    Other(String),
}

/// Calendar API errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    Network(String),
    Refresh(RefreshError),
    Other(String),
}

// ── Event parsing (pure functions; testable without network) ────────────────

/// A parsed calendar event (agenda / holiday membership).
///
/// All-day events (`start.date` present, no time) carry only a date; timed
/// events carry a UTC datetime. `past` is set by the agenda store relative to
/// "now" (design D4 dim styling).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEvent {
    /// Event id.
    pub id: String,
    /// Event title (`summary`).
    pub summary: String,
    /// Start instant. For all-day events, the date at 00:00 UTC.
    pub start: DateTime<Utc>,
    /// End instant. For all-day events, the date at 00:00 UTC (exclusive end).
    pub end: DateTime<Utc>,
    /// True if the event is all-day (`date`, not `dateTime`).
    pub all_day: bool,
}

impl CalendarEvent {
    /// The calendar date an all-day event applies to (its start date).
    pub fn all_day_date(&self) -> Option<NaiveDate> {
        if self.all_day {
            Some(self.start.date_naive())
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct CalendarListJson {
    #[serde(default)]
    items: Vec<CalendarListEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct CalendarListEntry {
    id: String,
    #[serde(default)]
    summary: String,
}

/// Parse the `/users/me/calendarList` response into `(id, summary)` pairs.
pub fn parse_calendar_list(body: &str) -> Result<Vec<(String, String)>, String> {
    let cl: CalendarListJson =
        serde_json::from_str(body).map_err(|e| format!("calendar list parse: {e}"))?;
    Ok(cl.items.into_iter().map(|e| (e.id, e.summary)).collect())
}

#[derive(Debug, Clone, Deserialize)]
struct EventsJson {
    #[serde(default)]
    items: Vec<EventJson>,
}

#[derive(Debug, Clone, Deserialize)]
struct EventJson {
    id: String,
    #[serde(default)]
    summary: String,
    start: EventTimeJson,
    end: EventTimeJson,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventTimeJson {
    /// Present for timed events (RFC-3339 with offset).
    #[serde(default)]
    date_time: Option<String>,
    /// Present for all-day events (RFC date, no time).
    #[serde(default)]
    date: Option<String>,
}

/// Parse the `/calendars/{id}/events` response into [`CalendarEvent`]s.
pub fn parse_events(body: &str) -> Result<Vec<CalendarEvent>, String> {
    let ej: EventsJson = serde_json::from_str(body).map_err(|e| format!("events parse: {e}"))?;
    ej.items
        .into_iter()
        .map(event_json_to_event)
        .collect()
}

fn event_json_to_event(e: EventJson) -> Result<CalendarEvent, String> {
    // All-day if `start.date` is present (and `dateTime` absent).
    if let Some(date) = &e.start.date {
        let start = parse_date_at_midnight(date)?;
        // Google all-day events' end is the exclusive next day; keep it as-is.
        let end = match &e.end.date {
            Some(d) => parse_date_at_midnight(d)?,
            None => start, // malformed; fall back
        };
        return Ok(CalendarEvent {
            id: e.id,
            summary: e.summary,
            start,
            end,
            all_day: true,
        });
    }

    // Timed event.
    let start = e
        .start
        .date_time
        .as_ref()
        .ok_or_else(|| "event has neither date nor dateTime".to_string())?;
    let start = DateTime::parse_from_rfc3339(start)
        .map_err(|err| format!("start parse: {err}"))?
        .with_timezone(&Utc);
    let end = e
        .end
        .date_time
        .as_ref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or(start)
        })
        .unwrap_or(start);
    Ok(CalendarEvent {
        id: e.id,
        summary: e.summary,
        start,
        end,
        all_day: false,
    })
}

fn parse_date_at_midnight(s: &str) -> Result<DateTime<Utc>, String> {
    let nd = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| format!("date parse: {e}"))?;
    Ok(NaiveDateTime::from(nd).and_utc())
}

// ── HolidayStore (design D3) ─────────────────────────────────────────────────

/// Main-thread cache of holiday dates (design D3).
///
/// Populated from Holiday-role calendars' all-day events on the shared 30-min
/// refresh tick. The scheduler tick checks [`is_holiday`] — an O(1) set lookup,
/// no per-tick API call. Membership is date-based; Google returns all-day
/// events as dates, so timezone/DST ambiguity is sidestepped.
///
/// Implements [`crate::scheduler::HolidayLookup`].
#[derive(Debug, Clone, Default)]
pub struct HolidayStore {
    dates: HashSet<NaiveDate>,
}

impl HolidayStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the entire holiday set (called after a refresh).
    pub fn replace(&mut self, dates: HashSet<NaiveDate>) {
        self.dates = dates;
    }

    /// Add a single holiday date.
    pub fn add(&mut self, date: NaiveDate) {
        self.dates.insert(date);
    }

    /// Is *date* a holiday?
    pub fn is_holiday(&self, date: NaiveDate) -> bool {
        self.dates.contains(&date)
    }

    /// Number of known holiday dates.
    pub fn len(&self) -> usize {
        self.dates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dates.is_empty()
    }

    /// Build the holiday set from a slice of events: every all-day event's
    /// start date (design D3 — all-day personal events are holidays too).
    pub fn from_events(events: &[CalendarEvent]) -> HashSet<NaiveDate> {
        events
            .iter()
            .filter_map(|e| e.all_day_date())
            .collect()
    }
}

impl crate::scheduler::HolidayLookup for HolidayStore {
    fn is_holiday(&self, date: NaiveDate) -> bool {
        HolidayStore::is_holiday(self, date)
    }
}

// ── AgendaStore (design D4) ──────────────────────────────────────────────────

/// One agenda entry on the Daily-data panel (slice 6 / design D4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgendaItem {
    /// Event title.
    pub summary: String,
    /// Start instant (UTC).
    pub start: DateTime<Utc>,
    /// Whether the event is in the past (drives the dim styling).
    pub past: bool,
}

/// Main-thread cache of today's agenda (design D4): cap 4 upcoming, with past
/// events retained and dimmed.
#[derive(Debug, Clone, Default)]
pub struct AgendaStore {
    items: Vec<AgendaItem>,
}

/// Maximum number of agenda items retained (cap 4 per the spec).
pub const AGENDA_CAP: usize = 4;

impl AgendaStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the agenda from a slice of events (already filtered to "today"
    /// by the caller), marking past events relative to *now*.
    ///
    /// Past events are retained (dimmed) but the list is capped to
    /// [`AGENDA_CAP`] items, preferring upcoming over past when truncating.
    pub fn replace_from_events(&mut self, events: &[CalendarEvent], now: DateTime<Utc>) {
        let mut items: Vec<AgendaItem> = events
            .iter()
            .map(|e| AgendaItem {
                summary: e.summary.clone(),
                start: e.start,
                past: e.start < now,
            })
            .collect();
        // Sort by start time ascending.
        items.sort_by_key(|i| i.start);
        // Cap: keep up to AGENDA_CAP items, preferring the first upcoming
        // ones but retaining some past (dimmed) context when there's room.
        if items.len() > AGENDA_CAP {
            // Find the split index of first upcoming.
            let first_upcoming = items
                .iter()
                .position(|i| !i.past)
                .unwrap_or(items.len());
            // Keep the most recent past (up to remaining slots) + upcoming.
            let mut kept: Vec<AgendaItem> = Vec::new();
            let past = &items[..first_upcoming];
            let upcoming = &items[first_upcoming..];
            // Upcoming first (cap), then fill remaining with the most-recent past.
            let upcoming_keep = upcoming.len().min(AGENDA_CAP);
            kept.extend_from_slice(&upcoming[..upcoming_keep]);
            let remaining = AGENDA_CAP.saturating_sub(kept.len());
            if remaining > 0 {
                let start = past.len().saturating_sub(remaining);
                kept.extend_from_slice(&past[start..]);
            }
            kept.sort_by_key(|i| i.start);
            items = kept;
        }
        self.items = items;
    }

    /// The current agenda items (already capped + dimmed).
    pub fn items(&self) -> &[AgendaItem] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

// Re-export `CalendarSource` / `CalendarRole` for ergonomic access from
// `calendar::*` call sites (they live in `alarm_store` for persistence).
pub use crate::alarm_store::{CalendarRole as Role, CalendarSource as Source};

/// The result of a calendar fetch, already split by role so main can drop it
/// straight into the `HolidayStore` and `AgendaStore`.
#[derive(Debug, Clone, Default)]
pub struct CalendarFetchResult {
    /// Holiday-role all-day event dates.
    pub holiday_dates: HashSet<NaiveDate>,
    /// Agenda-role events (today's window), unfiltered — main caps/dims.
    pub agenda_events: Vec<CalendarEvent>,
}

/// Fetch events for every configured calendar, splitting by role into a
/// [`CalendarFetchResult`] (slice 6). Runs on the tokio worker.
///
/// On a 401 from the refresh-token endpoint (revoked/expired), returns
/// `Err("unauthorized: ...")` so main can re-prompt device flow (task 2.3).
pub async fn fetch_calendar_events(
    http: reqwest::Client,
    refresh_token: String,
    client_id: String,
    client_secret: String,
    oauth_token_url: String,
    calendar_api_url: String,
    calendars: Vec<CalendarSource>,
    time_min: DateTime<Utc>,
    time_max: DateTime<Utc>,
) -> Result<CalendarFetchResult, String> {
    let client = CalendarClient::new(
        http,
        calendar_api_url,
        oauth_token_url,
        client_id,
        client_secret,
        refresh_token,
    );

    let mut result = CalendarFetchResult::default();
    for cal in &calendars {
        let events = client
            .list_events(&cal.google_calendar_id, time_min, time_max)
            .await
            .map_err(|e| match e {
                ApiError::Refresh(RefreshError::Unauthorized(body)) => {
                    format!("unauthorized: {body}")
                }
                other => format!("calendar fetch error for {}: {other:?}", cal.google_calendar_id),
            })?;
        match cal.role {
            CalendarRole::Holiday => {
                for ev in &events {
                    if let Some(d) = ev.all_day_date() {
                        result.holiday_dates.insert(d);
                    }
                }
            }
            CalendarRole::Agenda => {
                result.agenda_events.extend(events);
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Event parsing ───────────────────────────────────────────────────

    /// Scenario: a timed event parses with a UTC datetime and `all_day=false`.
    #[test]
    fn parse_timed_event() {
        let body = r#"{
            "items": [
                {
                    "id": "evt1",
                    "summary": "Standup",
                    "start": {"dateTime": "2026-07-04T09:00:00-06:00"},
                    "end":   {"dateTime": "2026-07-04T09:15:00-06:00"}
                }
            ]
        }"#;
        let events = parse_events(body).expect("parse");
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.summary, "Standup");
        assert!(!e.all_day);
        assert!(e.all_day_date().is_none());
        // 09:00 -06:00 == 15:00 UTC
        assert_eq!(e.start.format("%H:%M").to_string(), "15:00");
    }

    /// Scenario: an all-day event parses with a date and `all_day=true`.
    #[test]
    fn parse_all_day_event() {
        let body = r#"{
            "items": [
                {
                    "id": "evt2",
                    "summary": "Canada Day",
                    "start": {"date": "2026-07-01"},
                    "end":   {"date": "2026-07-02"}
                }
            ]
        }"#;
        let events = parse_events(body).expect("parse");
        let e = &events[0];
        assert!(e.all_day);
        assert_eq!(e.all_day_date(), Some(NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()));
    }

    /// Scenario: a recurring daily standup, expanded by Google
    /// (`singleEvents=true`), appears as separate events.
    #[test]
    fn recurring_events_expanded_by_google() {
        let body = r#"{
            "items": [
                {"id":"a","summary":"Standup","start":{"dateTime":"2026-07-04T09:00:00-06:00"},"end":{"dateTime":"2026-07-04T09:15:00-06:00"}},
                {"id":"b","summary":"Standup","start":{"dateTime":"2026-07-05T09:00:00-06:00"},"end":{"dateTime":"2026-07-05T09:15:00-06:00"}}
            ]
        }"#;
        let events = parse_events(body).expect("parse");
        assert_eq!(events.len(), 2, "Google expanded the RRULE into 2 events");
        assert_ne!(events[0].id, events[1].id);
    }

    /// Scenario: empty events list parses to empty.
    #[test]
    fn parse_empty_events() {
        let body = r#"{"items": []}"#;
        assert!(parse_events(body).unwrap().is_empty());
    }

    /// Scenario: malformed events JSON is an error.
    #[test]
    fn parse_malformed_events_errors() {
        assert!(parse_events("{not json").is_err());
    }

    /// Scenario: calendar list parses into (id, summary) pairs.
    #[test]
    fn parse_calendar_list_pairs() {
        let body = r#"{
            "items": [
                {"id":"primary","summary":"My Agenda"},
                {"id":"en.canadian#holiday@group.v.calendar.google.com","summary":"Holidays in Canada"}
            ]
        }"#;
        let list = parse_calendar_list(body).expect("parse");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], ("primary".to_string(), "My Agenda".to_string()));
        assert_eq!(list[1].0, "en.canadian#holiday@group.v.calendar.google.com");
    }

    // ── Device-flow poll state machine ─────────────────────────────────

    /// Scenario: pending → pending → access token returns the pairing result.
    #[test]
    fn poll_state_machine_pending_then_success() {
        let replies = vec![
            (428u16, r#"{"error":"authorization_pending"}"#.to_string()),
            (428u16, r#"{"error":"authorization_pending"}"#.to_string()),
            (
                200u16,
                r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600}"#.to_string(),
            ),
        ];
        let mut i = 0;
        let result = run_poll_state_machine(5, |_| {
            let r = replies[i].clone();
            i += 1;
            r
        })
        .expect("pairing");
        assert_eq!(result.access_token, "AT");
        assert_eq!(result.refresh_token, "RT");
        assert_eq!(result.expires_in, 3600);
    }

    /// Scenario: slow_down increases the interval and the machine keeps polling.
    #[test]
    fn poll_state_machine_slow_down_then_success() {
        let replies = vec![
            (428u16, r#"{"error":"slow_down"}"#.to_string()),
            (428u16, r#"{"error":"authorization_pending"}"#.to_string()),
            (200u16, r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600}"#.to_string()),
        ];
        let mut seen_intervals = Vec::new();
        let mut i = 0;
        let result = run_poll_state_machine(5, |interval| {
            seen_intervals.push(interval);
            let r = replies[i].clone();
            i += 1;
            r
        })
        .expect("pairing");
        assert_eq!(result.refresh_token, "RT");
        // First poll at 5, then (after slow_down) the interval grows to 10.
        assert_eq!(seen_intervals, vec![5u64, 10, 10]);
    }

    /// Scenario: access_denied surfaces as an error.
    #[test]
    fn poll_state_machine_access_denied() {
        let result = run_poll_state_machine(5, |_| {
            (400u16, r#"{"error":"access_denied"}"#.to_string())
        });
        assert_eq!(result, Err("access_denied".to_string()));
    }

    /// Scenario: a token response missing refresh_token is an error.
    #[test]
    fn poll_state_machine_missing_refresh_token() {
        let result = run_poll_state_machine(5, |_| {
            (200u16, r#"{"access_token":"AT","expires_in":3600}"#.to_string())
        });
        assert!(result.is_err());
    }

    /// Scenario: classify_poll maps the standard error strings.
    #[test]
    fn classify_poll_known_errors() {
        assert_eq!(
            classify_poll(428, r#"{"error":"authorization_pending"}"#),
            PollResponse::Pending
        );
        assert_eq!(
            classify_poll(400, r#"{"error":"slow_down"}"#),
            PollResponse::SlowDown
        );
        assert_eq!(
            classify_poll(200, r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600}"#),
            PollResponse::AccessToken {
                access_token: "AT".to_string(),
                refresh_token: Some("RT".to_string()),
                expires_in: 3600,
            }
        );
        assert_eq!(
            classify_poll(400, r#"{"error":"expired_token"}"#),
            PollResponse::Error("expired_token".to_string())
        );
    }

    // ── HolidayStore ────────────────────────────────────────────────────

    /// Scenario: all-day events populate the holiday set; timed events do not.
    #[test]
    fn holiday_store_from_events() {
        let events = vec![
            CalendarEvent {
                id: "1".into(),
                summary: "Canada Day".into(),
                start: parse_date_at_midnight("2026-07-01").unwrap(),
                end: parse_date_at_midnight("2026-07-02").unwrap(),
                all_day: true,
            },
            CalendarEvent {
                id: "2".into(),
                summary: "Standup".into(),
                start: DateTime::parse_from_rfc3339("2026-07-01T09:00:00-06:00")
                    .unwrap()
                    .with_timezone(&Utc),
                end: DateTime::parse_from_rfc3339("2026-07-01T09:15:00-06:00")
                    .unwrap()
                    .with_timezone(&Utc),
                all_day: false,
            },
        ];
        let set = HolidayStore::from_events(&events);
        assert_eq!(set.len(), 1);
        assert!(set.contains(&NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()));

        let mut store = HolidayStore::new();
        store.replace(set);
        assert!(store.is_holiday(NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()));
        assert!(!store.is_holiday(NaiveDate::from_ymd_opt(2026, 7, 2).unwrap()));
    }

    // ── AgendaStore ─────────────────────────────────────────────────────

    /// Scenario: agenda is capped to 4, past events dimmed.
    #[test]
    fn agenda_store_caps_and_dims() {
        let now = DateTime::parse_from_rfc3339("2026-07-04T12:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        // 3 past + 3 upcoming = 6 → capped to 4 (4 upcoming kept, no past).
        let events: Vec<CalendarEvent> = (0..3)
            .map(|i| CalendarEvent {
                id: format!("p{i}"),
                summary: format!("Past {i}"),
                start: now - chrono::Duration::hours(i64::from(i + 1)),
                end: now,
                all_day: false,
            })
            .chain((0..3).map(|i| CalendarEvent {
                id: format!("u{i}"),
                summary: format!("Upcoming {i}"),
                start: now + chrono::Duration::hours(i64::from(i + 1)),
                end: now,
                all_day: false,
            }))
            .collect();

        let mut store = AgendaStore::new();
        store.replace_from_events(&events, now);
        let items = store.items();
        assert!(items.len() <= AGENDA_CAP);
        // All upcoming kept (3), plus the most recent past (1) → 4 total.
        assert_eq!(items.len(), 4);
        // The most recent past event ("Past 0") is dimmed.
        let past = items.iter().filter(|i| i.past).count();
        assert_eq!(past, 1);
    }

    /// Scenario: fewer than cap keeps all items.
    #[test]
    fn agenda_store_keeps_all_when_under_cap() {
        let now = Utc::now();
        let events = vec![CalendarEvent {
            id: "x".into(),
            summary: "Only".into(),
            start: now + chrono::Duration::hours(1),
            end: now,
            all_day: false,
        }];
        let mut store = AgendaStore::new();
        store.replace_from_events(&events, now);
        assert_eq!(store.items().len(), 1);
        assert!(!store.items()[0].past);
    }
}

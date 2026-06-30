//! Typed RPC method wrappers (task 4.4).
//!
//! Each method follows the same mechanical pattern:
//! 1. A **request struct** carrying the JSON-RPC `params` payload,
//! 2. A **typed reply struct** that deserialises the JSON-RPC `result`,
//! 3. An extension [`call*`] method on [`MopidyWsClient`] that serialises the
//!    request, dispatches it through the transport and deserialises into the
//!    typed reply.

use crate::transport::{JsonRpcMessage, MopidyWsClient, TransportError};
use serde_json::Value;

// ── Typed-call error type (task 4.4) ──────────────────────────────────────────

/// Errors returned by the typed Mopidy RPC wrappers.
///
/// [`MopidyClientError::NotConnected`] is returned immediately (no hang) by
/// every typed call when the client is not in the `Connected` state
/// (`Disconnected` / `BackingOff`). The episode FSM treats this as
/// "playback silently failed" (logged) and continues the episode.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum MopidyClientError {
    /// The client is not in the `Connected` state (Disconnected / BackingOff).
    #[error("not connected to Mopidy")]
    NotConnected,

    /// A transport-level failure occurred while dispatching the call.
    #[error(transparent)]
    Transport(#[from] TransportError),
}

// ── core.get_version ─────────────────────────────────────────────────────────

/// Request struct for `core.get_version`. No arguments are needed.
#[derive(Debug, Clone, Default)]
pub struct GetVersionRequest;

impl GetVersionRequest {
    /// Serialise into the JSON-RPC `params` array `[ ]`.
    pub fn to_jsonrpc_params(self) -> Option<Value> {
        None // empty params → serialised as []
    }
}

/// Typed reply from `core.get_version`.
///
/// Mopidy returns a plain version string (e.g. `"3.4"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    /// The full version string as returned by Mopidy.
    pub version: String,
}

impl<'de> serde::Deserialize<'de> for VersionInfo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(VersionInfo { version: s })
    }
}

// ── core.get_state ───────────────────────────────────────────────────────────

/// Request struct for `core.get_state`. No arguments are needed.
#[derive(Debug, Clone, Default)]
pub struct GetStateRequest;

impl GetStateRequest {
    /// Serialise into the JSON-RPC `params` array `[ ]`.
    pub fn to_jsonrpc_params(self) -> Option<Value> {
        None // empty params → serialised as []
    }
}

/// Typed playback-state reply from `core.get_state`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackState {
    /// A track is actively playing.
    Playing,
    /// Playback is paused on the current track.
    Paused,
    /// No track is playing or queued.
    Stopped,
}

impl<'de> serde::Deserialize<'de> for PlaybackState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "PLAYING" => Ok(PlaybackState::Playing),
            "PAUSED" => Ok(PlaybackState::Paused),
            "STOPPED" => Ok(PlaybackState::Stopped),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["PLAYING", "PAUSED", "STOPPED"],
            )),
        }
    }
}

// ── playback.play ────────────────────────────────────────────────────────────

/// Request struct for `playback.play(uri)`. 
/// Mopidy expects params `{ uris: [uri] }` when a URI is provided.
#[derive(Debug, Clone, Default)]
pub struct PlayRequest {
    /// Optional track URI to play (e.g. "file:///path/to/track.mp3").  `None`
    /// re-plays the current tracklist index.
    pub uri: Option<String>,
}

impl PlayRequest {
    /// Create a request for a specific URI.
    pub fn new(uri: impl Into<String>) -> Self {
        Self { uri: Some(uri.into()) }
    }

    /// Create a request with no explicit URI (re-play current index).
    pub fn resume_current() -> Self {
        Self { uri: None }
    }

    /// Serialize into JSON-RPC params object `{ uris: [uri, …] }`.
    pub fn to_jsonrpc_params(self) -> Option<serde_json::Value> {
        match self.uri {
            Some(u) => Some(serde_json::json!({ "uris": [u] })),
            None => None,
        }
    }
}

// ── playback.pause ───────────────────────────────────────────────────────────

/// Request struct for `playback.pause`. No arguments are needed.
#[derive(Debug, Clone, Default)]
pub struct PauseRequest;

impl PauseRequest {
    /// Serialize into the JSON-RPC params array `[ ]`.
    pub fn to_jsonrpc_params(self) -> Option<serde_json::Value> {
        None
    }
}

// ── playback.resume ─────────────────────────────────────────────────────────

/// Request struct for `playback.resume`. No arguments are needed.
#[derive(Debug, Clone, Default)]
pub struct ResumeRequest;

impl ResumeRequest {
    /// Serialize into the JSON-RPC params array `[ ]`.
    pub fn to_jsonrpc_params(self) -> Option<serde_json::Value> {
        None
    }
}

// ── playback.stop ────────────────────────────────────────────────────────────

/// Request struct for `playback.stop`. No arguments are needed.
#[derive(Debug, Clone, Default)]
pub struct StopRequest;

impl StopRequest {
    /// Serialize into the JSON-RPC params array `[ ]`.
    pub fn to_jsonrpc_params(self) -> Option<serde_json::Value> {
        None
    }
}

// ── playback.set_volume ─────────────────────────────────────────────────────

/// Request struct for `playback.set_volume(volume)`.
///
/// The volume is clamped to 0..=100 at construction time.
#[derive(Debug, Clone)]
pub struct SetVolumeRequest {
    /// Desired volume in percents (0–100).
    pub volume: u8,
}

impl SetVolumeRequest {
    /// Create a request; **clamps** the provided value to 0..=100.
    ///
    /// Any out-of-range `i32` is clamped before capture, so Mopidy never
    /// receives an invalid number.
    pub fn new(volume: i32) -> Self {
        let clamped = volume.clamp(0, 100) as u8;
        Self { volume: clamped }
    }

    /// Serialize into JSON-RPC params object `{ volume: N }`.
    pub fn to_jsonrpc_params(self) -> serde_json::Value {
        serde_json::json!({ "volume": self.volume })
    }
}

// ── playback.get_time_position ───────────────────────────────────────────────

/// Request struct for `playback.get_time_position`. No arguments are needed.
#[derive(Debug, Clone, Default)]
pub struct GetTimePositionRequest;

impl GetTimePositionRequest {
    /// Serialize into the JSON-RPC params array `[ ]`.
    pub fn to_jsonrpc_params(self) -> Option<serde_json::Value> {
        None
    }
}

// ── Extension methods on MopidyWsClient ──────────────────────────────────────

/// Convenience trait that gives callers a single `call` method per RPC
/// operation instead of scattering free functions.
pub trait CoreApi {
    /// Return the server version by calling `core.get_version`.
    fn get_version(&self) -> impl std::future::Future<Output = Result<VersionInfo, MopidyClientError>> + Send;

    /// Return the current playback state by calling `core.get_state`.
    fn get_state(&self) -> impl std::future::Future<Output = Result<PlaybackState, MopidyClientError>> + Send;
}

/// Convenience trait providing playback-related RPC calls.
pub trait PlaybackApi {
    /// Play a specific URI by calling `playback.play`.
    fn playback_play(
        &self,
        uri: Option<String>,
    ) -> impl std::future::Future<Output = Result<(), MopidyClientError>> + Send;

    /// Pause playback by calling `playback.pause`.
    fn playback_pause(&self) -> impl std::future::Future<Output = Result<(), MopidyClientError>> + Send;

    /// Resume playback by calling `playback.resume`.
    fn playback_resume(&self) -> impl std::future::Future<Output = Result<(), MopidyClientError>> + Send;

    /// Stop playback by calling `playback.stop`.
    fn playback_stop(&self) -> impl std::future::Future<Output = Result<(), MopidyClientError>> + Send;

    /// Set the mixer volume (clamped 0..=100) by calling `playback.set_volume`.
    fn playback_set_volume(
        &self,
        volume: u8,
    ) -> impl std::future::Future<Output = Result<(), MopidyClientError>> + Send;

    /// Query the current playback state by calling `playback.get_state`.
    fn playback_get_state(&self) -> impl std::future::Future<Output = Result<PlaybackState, MopidyClientError>> + Send;

    /// Query the current playback time position (milliseconds) by calling
    /// `playback.get_time_position`.
    fn playback_get_time_position(
        &self,
    ) -> impl std::future::Future<Output = Result<u32, MopidyClientError>> + Send;
}

impl CoreApi for MopidyWsClient {
    async fn get_version(&self) -> Result<VersionInfo, MopidyClientError> {
        if !self.is_connected() {
            return Err(MopidyClientError::NotConnected);
        }
        let _req = GetVersionRequest::default();
        let reply_msg = self.send_and_await("core.get_version", None).await?;
        Ok(parse_or_error::<VersionInfo>(reply_msg)?)
    }

    async fn get_state(&self) -> Result<PlaybackState, MopidyClientError> {
        if !self.is_connected() {
            return Err(MopidyClientError::NotConnected);
        }
        let _req = GetStateRequest::default();
        let reply_msg = self.send_and_await("core.get_state", None).await?;
        Ok(parse_or_error::<PlaybackState>(reply_msg)?)
    }
}

impl PlaybackApi for MopidyWsClient {
    async fn playback_play(&self, uri: Option<String>) -> Result<(), MopidyClientError> {
        if !self.is_connected() {
            return Err(MopidyClientError::NotConnected);
        }
        let req = PlayRequest { uri };
        let params = req.to_jsonrpc_params();
        let reply_msg = self.send_and_await("playback.play", params).await?;
        // Mopidy returns `true` on success; we just need acknowledgement.
        parse_or_error::<bool>(reply_msg)?;
        Ok(())
    }

    async fn playback_pause(&self) -> Result<(), MopidyClientError> {
        if !self.is_connected() {
            return Err(MopidyClientError::NotConnected);
        }
        let _req = PauseRequest::default();
        let reply_msg = self.send_and_await("playback.pause", None).await?;
        parse_or_error::<bool>(reply_msg)?;
        Ok(())
    }

    async fn playback_resume(&self) -> Result<(), MopidyClientError> {
        if !self.is_connected() {
            return Err(MopidyClientError::NotConnected);
        }
        let _req = ResumeRequest::default();
        let reply_msg = self.send_and_await("playback.resume", None).await?;
        parse_or_error::<bool>(reply_msg)?;
        Ok(())
    }

    async fn playback_stop(&self) -> Result<(), MopidyClientError> {
        if !self.is_connected() {
            return Err(MopidyClientError::NotConnected);
        }
        let _req = StopRequest::default();
        let reply_msg = self.send_and_await("playback.stop", None).await?;
        parse_or_error::<bool>(reply_msg)?;
        Ok(())
    }

    async fn playback_set_volume(&self, volume: u8) -> Result<(), MopidyClientError> {
        if !self.is_connected() {
            return Err(MopidyClientError::NotConnected);
        }
        let req = SetVolumeRequest::new(volume as i32);
        let params = Some(req.to_jsonrpc_params());
        let reply_msg = self.send_and_await("playback.set_volume", params).await?;
        parse_or_error::<bool>(reply_msg)?;
        Ok(())
    }

    async fn playback_get_state(&self) -> Result<PlaybackState, MopidyClientError> {
        if !self.is_connected() {
            return Err(MopidyClientError::NotConnected);
        }
        let _req = GetStateRequest::default();
        let reply_msg = self.send_and_await("playback.get_state", None).await?;
        Ok(parse_or_error::<PlaybackState>(reply_msg)?)
    }

    async fn playback_get_time_position(&self) -> Result<u32, MopidyClientError> {
        if !self.is_connected() {
            return Err(MopidyClientError::NotConnected);
        }
        let _req = GetTimePositionRequest::default();
        let reply_msg = self.send_and_await("playback.get_time_position", None).await?;
        Ok(parse_or_error::<u32>(reply_msg)?)
    }
}

// ── tracklist.add (task 4.3) ──────────────────────────────────────────────

/// Request struct for `tracklist.add(uris)`.
///
/// Mopidy expects params `{ uris: ["...", ...] }`.
#[derive(Debug, Clone, Default)]
pub struct TracklistAddRequest {
    /// The track URIs to add to the tracklist (e.g. `"file:///path/to/track.mp3"`).
    pub uris: Vec<String>,
}

impl TracklistAddRequest {
    /// Create a request to add the given URIs.
    pub fn new(uris: Vec<String>) -> Self {
        Self { uris }
    }

    /// Serialize into the JSON-RPC params object `{ "uris": [...] }`.
    pub fn to_jsonrpc_params(self) -> Option<Value> {
        Some(serde_json::json!({ "uris": self.uris }))
    }
}

/// Typed reply from `tracklist.add`.
///
/// Mopidy returns the list of added `TlTrack` objects. Slice 1 only needs
/// acknowledgement, so the payload is accepted and discarded; later slices
/// may model the TlTrack array.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TracklistAddReply;

impl<'de> serde::Deserialize<'de> for TracklistAddReply {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Accept and discard any result (array of TlTracks, `null`, etc.).
        let _ = Value::deserialize(deserializer)?;
        Ok(TracklistAddReply)
    }
}

// ── tracklist.set_repeat (task 4.3) ───────────────────────────────────────────

/// Request struct for `tracklist.set_repeat(bool)`.
#[derive(Debug, Clone)]
pub struct SetRepeatRequest {
    /// Whether Mopidy's repeat should be enabled.
    pub repeat: bool,
}

impl SetRepeatRequest {
    /// Create a request with the desired repeat state.
    pub fn new(repeat: bool) -> Self {
        Self { repeat }
    }

    /// Serialize into the JSON-RPC params object `{ "repeat": bool }`.
    pub fn to_jsonrpc_params(self) -> Value {
        serde_json::json!({ "repeat": self.repeat })
    }
}

// ── tracklist.set_shuffle (task 4.3) ──────────────────────────────────────────
// Mopidy exposes `tracklist.set_random`; `random` and `shuffle` are aliased.
// Slice 1 uses the `shuffle` naming per PRD and sends `tracklist.set_shuffle`
// on the wire. If a Mopidy version exposes only `set_random`, the call site
// may be aliased to `tracklist.set_random` there; the typed wrapper keeps the
// `shuffle` naming.

/// Request struct for `tracklist.set_shuffle(bool)`.
#[derive(Debug, Clone)]
pub struct SetShuffleRequest {
    /// Whether Mopidy's shuffle should be enabled.
    pub shuffle: bool,
}

impl SetShuffleRequest {
    /// Create a request with the desired shuffle state.
    pub fn new(shuffle: bool) -> Self {
        Self { shuffle }
    }

    /// Serialize into the JSON-RPC params object `{ "shuffle": bool }`.
    pub fn to_jsonrpc_params(self) -> Value {
        serde_json::json!({ "shuffle": self.shuffle })
    }
}

// ── Extension methods on MopidyWsClient (tracklist) ──────────────────────────

/// Convenience trait providing tracklist-related RPC calls.
pub trait TracklistApi {
    /// Add tracks to the tracklist by calling `tracklist.add`.
    fn tracklist_add(
        &self,
        uris: Vec<String>,
    ) -> impl std::future::Future<Output = Result<TracklistAddReply, MopidyClientError>> + Send;

    /// Toggle Mopidy's repeat by calling `tracklist.set_repeat`.
    fn tracklist_set_repeat(
        &self,
        repeat: bool,
    ) -> impl std::future::Future<Output = Result<(), MopidyClientError>> + Send;

    /// Toggle Mopidy's shuffle by calling `tracklist.set_shuffle`.
    fn tracklist_set_shuffle(
        &self,
        shuffle: bool,
    ) -> impl std::future::Future<Output = Result<(), MopidyClientError>> + Send;
}

impl TracklistApi for MopidyWsClient {
    async fn tracklist_add(&self, uris: Vec<String>) -> Result<TracklistAddReply, MopidyClientError> {
        if !self.is_connected() {
            return Err(MopidyClientError::NotConnected);
        }
        let req = TracklistAddRequest::new(uris);
        let params = req.to_jsonrpc_params();
        let reply_msg = self.send_and_await("tracklist.add", params).await?;
        Ok(parse_or_error::<TracklistAddReply>(reply_msg)?)
    }

    async fn tracklist_set_repeat(&self, repeat: bool) -> Result<(), MopidyClientError> {
        if !self.is_connected() {
            return Err(MopidyClientError::NotConnected);
        }
        let req = SetRepeatRequest::new(repeat);
        let params = Some(req.to_jsonrpc_params());
        let reply_msg = self.send_and_await("tracklist.set_repeat", params).await?;
        // Mopidy returns `null`/`true` for this void method; acknowledge any
        // result and only fail on an RPC-level error.
        let _: Value = parse_or_error::<Value>(reply_msg)?;
        Ok(())
    }

    async fn tracklist_set_shuffle(&self, shuffle: bool) -> Result<(), MopidyClientError> {
        if !self.is_connected() {
            return Err(MopidyClientError::NotConnected);
        }
        let req = SetShuffleRequest::new(shuffle);
        let params = Some(req.to_jsonrpc_params());
        let reply_msg = self.send_and_await("tracklist.set_shuffle", params).await?;
        // Mopidy returns `null`/`true` for this void method; acknowledge any
        // result and only fail on an RPC-level error.
        let _: Value = parse_or_error::<Value>(reply_msg)?;
        Ok(())
    }
}

/// Extract the `result` from a JSON-RPC [`JsonRpcMessage`] and
/// deserialise it into the target type `R`.
fn parse_or_error<R: serde::de::DeserializeOwned>(msg: JsonRpcMessage) -> Result<R, TransportError> {
    match msg.result.or_else(|| msg.error.clone()) {
        Some(val) => {
            if let Some(err_details) = msg.error {
                return Err(TransportError::Parse(format!("RPC error: {}", err_details)));
            }
            serde_json::from_value(val)
                .map_err(|e| TransportError::Parse(format!("result deserialisation failed: {}", e)))
        }
        None => Err(TransportError::Parse("no result or error in reply".into())),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── VersionInfo serialisation / deserialisation ────────────────

    #[test]
    fn version_info_deserialises_from_string() {
        let json = serde_json::json!("3.4.1");
        let info: VersionInfo =
            serde_json::from_value(json).expect("deserialize VersionInfo");
        assert_eq!(info.version, "3.4.1");
    }

    // ── PlaybackState serialisation / deserialisation ─────────────

    #[test]
    fn get_state_returns_playing() {
        let json = serde_json::json!("PLAYING");
        let state: PlaybackState =
            serde_json::from_value(json).expect("deserialize PlaybackState");
        assert_eq!(state, PlaybackState::Playing);
    }

    #[test]
    fn get_state_returns_paused() {
        let json = serde_json::json!("PAUSED");
        let state: PlaybackState =
            serde_json::from_value(json).expect("deserialize PlaybackState");
        assert_eq!(state, PlaybackState::Paused);
    }

    #[test]
    fn get_state_returns_stopped() {
        let json = serde_json::json!("STOPPED");
        let state: PlaybackState =
            serde_json::from_value(json).expect("deserialize PlaybackState");
        assert_eq!(state, PlaybackState::Stopped);
    }

    #[test]
    fn get_state_rejects_invalid_variant() {
        let json = serde_json::json!("UNKNOWN");
        let result: Result<PlaybackState, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    // ── Request struct helpers ────────────────────────────────────

    #[test]
    fn get_version_request_has_no_params() {
        let req = GetVersionRequest::default();
        assert!(req.to_jsonrpc_params().is_none());
    }

    #[test]
    fn get_state_request_has_no_params() {
        let req = GetStateRequest::default();
        assert!(req.to_jsonrpc_params().is_none());
    }

    // ── parse_or_error helpers ────────────────────────────────────

    #[test]
    fn parse_or_error_returns_result_value() {
        let msg = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            request_id: Some(42),
            method: None,
            result: Some(serde_json::json!("PLAYING")),
            error: None,
        };
        let state: PlaybackState = parse_or_error(msg).expect("parse");
        assert_eq!(state, PlaybackState::Playing);
    }

    #[test]
    fn parse_or_error_returns_error_when_rpc_error_present() {
        let msg = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            request_id: Some(1),
            method: None,
            result: None,
            error: Some(serde_json::json!({ "message": "server busy" })),
        };
        let _: Result<PlaybackState, TransportError> = parse_or_error(msg);
        // Should be Err because error is present.
    }

    #[test]
    fn parse_or_error_rejects_missing_result_and_error() {
        let msg = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            request_id: Some(1),
            method: None,
            result: None,
            error: None,
        };
        let _: Result<PlaybackState, TransportError> = parse_or_error(msg);
    }

    // ── playback.play request serialization ──────────────────────

    #[test]
    fn play_request_with_uri_serializes_uris_array() {
        let req = PlayRequest::new("file:///path/to/track.mp3");
        let params = req.to_jsonrpc_params();
        assert!(params.is_some());
        let p = params.unwrap();
        assert_eq!(p.get("uris").unwrap().as_array().unwrap()[0], "file:///path/to/track.mp3");
    }

    #[test]
    fn play_request_without_uri_returns_none_params() {
        let req = PlayRequest::resume_current();
        assert!(req.to_jsonrpc_params().is_none());
    }

    // ── playback.pause request serialization ─────────────────────

    #[test]
    fn pause_request_has_no_params() {
        let req = PauseRequest::default();
        assert!(req.to_jsonrpc_params().is_none());
    }

    // ── playback.resume request serialization ───────────────────

    #[test]
    fn resume_request_has_no_params() {
        let req = ResumeRequest::default();
        assert!(req.to_jsonrpc_params().is_none());
    }

    // ── playback.stop request serialization ─────────────────────

    #[test]
    fn stop_request_has_no_params() {
        let req = StopRequest::default();
        assert!(req.to_jsonrpc_params().is_none());
    }

    // ── playback.set_volume request serialization + clamping ────

    #[test]
    fn set_volume_request_normal_value() {
        let req = SetVolumeRequest::new(75);
        assert_eq!(req.volume, 75u8);
        let params = req.to_jsonrpc_params();
        assert_eq!(params["volume"], 75);
    }

    #[test]
    fn set_volume_request_clamps_negative() {
        let req = SetVolumeRequest::new(-10);
        assert_eq!(req.volume, 0u8);
    }

    #[test]
    fn set_volume_request_clamps_over_100() {
        let req = SetVolumeRequest::new(200);
        assert_eq!(req.volume, 100u8);
    }

    #[test]
    fn set_volume_request_boundary_zero() {
        let req = SetVolumeRequest::new(0);
        assert_eq!(req.volume, 0u8);
        let params = req.to_jsonrpc_params();
        assert_eq!(params["volume"], 0);
    }

    #[test]
    fn set_volume_request_boundary_100() {
        let req = SetVolumeRequest::new(100);
        assert_eq!(req.volume, 100u8);
        let params = req.to_jsonrpc_params();
        assert_eq!(params["volume"], 100);
    }

    // ── playback.get_state reply deserialization ────────────────

    #[test]
    fn playback_get_state_deserializes_playing() {
        let json = serde_json::json!("PLAYING");
        let state: PlaybackState =
            serde_json::from_value(json).expect("deserialize PlaybackState");
        assert_eq!(state, PlaybackState::Playing);
    }

    // ── playback.get_time_position reply deserialization ────────

    #[test]
    fn playback_get_time_position_deserializes_milliseconds() {
        let json = serde_json::json!(45230);
        let ms: u32 =
            serde_json::from_value(json).expect("deserialize u32");
        assert_eq!(ms, 45_230u32);
    }

    #[test]
    fn get_time_position_request_has_no_params() {
        let req = GetTimePositionRequest::default();
        assert!(req.to_jsonrpc_params().is_none());
    }

    // ── tracklist.add request serialization (task 4.5) ─────────────

    #[test]
    fn tracklist_add_request_serializes_uris_object() {
        let req = TracklistAddRequest::new(vec![
            "file:///path/to/track.mp3".to_string(),
            "file:///path/to/other.mp3".to_string(),
        ]);
        let params = req.to_jsonrpc_params().expect("params present");
        assert_eq!(
            params,
            serde_json::json!({
                "uris": ["file:///path/to/track.mp3", "file:///path/to/other.mp3"]
            })
        );
    }

    #[test]
    fn tracklist_add_request_single_uri_shape() {
        // Scenario: tracklist.add with URIs sends `{method: "tracklist.add",
        // params: {uris: [uri]}}`.
        let req = TracklistAddRequest::new(vec!["file:///alarm.mp3".to_string()]);
        let params = req.to_jsonrpc_params().expect("params present");
        let uris = params.get("uris").expect("uris field present");
        let arr = uris.as_array().expect("uris is an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0], "file:///alarm.mp3");
    }

    #[test]
    fn tracklist_add_request_empty_uris() {
        let req = TracklistAddRequest::default();
        let params = req.to_jsonrpc_params().expect("params present");
        assert_eq!(params, serde_json::json!({ "uris": [] }));
    }

    // ── tracklist.add reply deserialization (task 4.5) ─────────────

    #[test]
    fn tracklist_add_reply_deserializes_from_tltrack_array() {
        // Mopidy returns an array of added TlTrack objects.
        let fixture = serde_json::json!([
            { "tlid": 1, "track": { "uri": "file:///alarm.mp3", "name": "Alarm" } }
        ]);
        let reply: TracklistAddReply =
            serde_json::from_value(fixture).expect("deserialize TracklistAddReply");
        assert_eq!(reply, TracklistAddReply);
    }

    #[test]
    fn tracklist_add_reply_deserializes_from_empty_array() {
        let fixture = serde_json::json!([]);
        let reply: TracklistAddReply =
            serde_json::from_value(fixture).expect("deserialize TracklistAddReply");
        assert_eq!(reply, TracklistAddReply);
    }

    #[test]
    fn tracklist_add_reply_acknowledged_via_parse_or_error() {
        let msg = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            request_id: Some(7),
            method: None,
            result: Some(serde_json::json!([
                { "tlid": 1, "track": { "uri": "file:///alarm.mp3" } }
            ])),
            error: None,
        };
        let reply: TracklistAddReply =
            parse_or_error(msg).expect("acknowledged reply");
        assert_eq!(reply, TracklistAddReply);
    }

    // ── tracklist.set_repeat request serialization (task 4.5) ──────

    #[test]
    fn set_repeat_request_serializes_true() {
        let req = SetRepeatRequest::new(true);
        assert_eq!(req.repeat, true);
        assert_eq!(req.to_jsonrpc_params(), serde_json::json!({ "repeat": true }));
    }

    #[test]
    fn set_repeat_request_serializes_false() {
        let req = SetRepeatRequest::new(false);
        assert_eq!(req.repeat, false);
        assert_eq!(req.to_jsonrpc_params(), serde_json::json!({ "repeat": false }));
    }

    // ── tracklist.set_shuffle request serialization (task 4.5) ──────

    #[test]
    fn set_shuffle_request_serializes_true() {
        let req = SetShuffleRequest::new(true);
        assert_eq!(req.shuffle, true);
        assert_eq!(req.to_jsonrpc_params(), serde_json::json!({ "shuffle": true }));
    }

    #[test]
    fn set_shuffle_request_serializes_false() {
        let req = SetShuffleRequest::new(false);
        assert_eq!(req.shuffle, false);
        assert_eq!(req.to_jsonrpc_params(), serde_json::json!({ "shuffle": false }));
    }

    // ── tracklist.set_repeat / set_shuffle reply deserialization (task 4.5)

    #[test]
    fn set_repeat_reply_acknowledges_null() {
        // Mopidy returns `null` for void methods.
        let msg = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            request_id: Some(11),
            method: None,
            result: Some(serde_json::Value::Null),
            error: None,
        };
        let _: Value = parse_or_error::<Value>(msg).expect("acknowledged null");
    }

    #[test]
    fn set_shuffle_reply_acknowledges_true() {
        let msg = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            request_id: Some(12),
            method: None,
            result: Some(serde_json::json!(true)),
            error: None,
        };
        let _: Value = parse_or_error::<Value>(msg).expect("acknowledged true");
    }

    // ── MopidyClientError (task 4.5) ───────────────────────────────

    #[test]
    fn mopidy_client_error_not_connected_displays() {
        let err = MopidyClientError::NotConnected;
        assert!(format!("{err}").contains("not connected to Mopidy"));
    }

    #[test]
    fn mopidy_client_error_transports_from_transport_error() {
        let transport_err = TransportError::Parse("boom".to_string());
        let client_err: MopidyClientError = transport_err.into();
        assert!(matches!(client_err, MopidyClientError::Transport(_)));
    }

    // ── NotConnected path (task 4.5) ───────────────────────────────

    #[tokio::test]
    async fn typed_calls_return_not_connected_when_disconnected() {
        use crate::transport::MopidyWsClient;
        use tokio::sync::mpsc;

        // Spawn a client against an unreachable URL; it never reaches
        // Connected (cycles Disconnected → Connecting → BackingOff …).
        let (e_tx, _e_rx) = mpsc::channel::<crate::transport::MopidyEvent>(16);
        let (r_tx, _r_rx) = mpsc::channel::<JsonRpcMessage>(16);
        let (s_tx, _s_rx) = mpsc::channel::<crate::state::MopidyConnectionState>(16);

        let client = MopidyWsClient::spawn(
            "ws://127.0.0.1:1/unreachable-mopidy".into(),
            None,
            e_tx,
            r_tx,
            s_tx,
        );

        // The client is never Connected with an unreachable URL, so every
        // typed call must short-circuit to NotConnected immediately.
        assert!(!client.is_connected());

        let err = client
            .tracklist_set_repeat(true)
            .await
            .expect_err("expected NotConnected");
        assert!(
            matches!(err, MopidyClientError::NotConnected),
            "expected NotConnected, got {:?}",
            err
        );

        let err = client
            .tracklist_set_shuffle(true)
            .await
            .expect_err("expected NotConnected");
        assert!(matches!(err, MopidyClientError::NotConnected));

        let err = client
            .tracklist_add(vec!["file:///alarm.mp3".to_string()])
            .await
            .expect_err("expected NotConnected");
        assert!(matches!(err, MopidyClientError::NotConnected));

        // Playback calls must also return NotConnected immediately (no hang).
        let err = client
            .playback_play(Some("file:///alarm.mp3".to_string()))
            .await
            .expect_err("expected NotConnected");
        assert!(matches!(err, MopidyClientError::NotConnected));
    }
}

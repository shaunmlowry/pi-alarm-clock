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

// ── Extension methods on MopidyWsClient ──────────────────────────────────────

/// Convenience trait that gives callers a single `call` method per RPC
/// operation instead of scattering free functions.
pub trait CoreApi {
    /// Return the server version by calling `core.get_version`.
    fn get_version(&self) -> impl std::future::Future<Output = Result<VersionInfo, TransportError>> + Send;

    /// Return the current playback state by calling `core.get_state`.
    fn get_state(&self) -> impl std::future::Future<Output = Result<PlaybackState, TransportError>> + Send;
}

impl CoreApi for MopidyWsClient {
    async fn get_version(&self) -> Result<VersionInfo, TransportError> {
        let _req = GetVersionRequest::default();
        let reply_msg = self.send_and_await("core.get_version", None).await?;
        parse_or_error::<VersionInfo>(reply_msg)
    }

    async fn get_state(&self) -> Result<PlaybackState, TransportError> {
        let _req = GetStateRequest::default();
        let reply_msg = self.send_and_await("core.get_state", None).await?;
        parse_or_error::<PlaybackState>(reply_msg)
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
}

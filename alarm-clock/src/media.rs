//! Media player model & transport (slice 7 / design D1–D4).
//!
//! This module owns the `Favorite` / `AudioSource` domain model, the
//! URI-scheme interpretation helper ([`parse_source`]), the
//! source-capability-adapted transport ([`TransportCaps`] /
//! [`resolve_transport`]), and the Mopidy-call shaping helpers used by the
//! tokio worker for podcast feed browsing and tap-to-play.
//!
//! Design notes:
//! - **D1**: `AudioSource` is a typed enum. Legacy `Vec<String>` alarm
//!   `fallback_chain` entries are read through [`parse_source`] at the
//!   boundary — no destructive migration.
//! - **D2**: podcast feeds are browsed via Mopidy's `library.browse`; the
//!   Rust app does not parse RSS. [`FeedEpisode`] is the reply shape.
//! - **D4**: [`TransportCaps`] is derived from [`AudioSource`]; radio
//!   "pause" maps to `playback.stop` (resumes live on restart).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ── AudioSource (D1) ─────────────────────────────────────────────────────────

/// A typed audio source. Shared between the media panel favorites and the
/// alarm `fallback_chain` (slice 4a reconciles legacy `Vec<String>` to this
/// via [`parse_source`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioSource {
    /// A local file URI (`file:///...`).
    File(String),
    /// A Spotify URI (`spotify:track:...` / `spotify:album:...`).
    Spotify(String),
    /// A live internet radio stream URL (`http(s)://...`).
    Radio(String),
    /// A podcast feed URL browsed via Mopidy's podcast backend.
    PodcastFeed(String),
}

impl AudioSource {
    /// The underlying URI/URL string.
    pub fn uri(&self) -> &str {
        match self {
            AudioSource::File(u) => u,
            AudioSource::Spotify(u) => u,
            AudioSource::Radio(u) => u,
            AudioSource::PodcastFeed(u) => u,
        }
    }

    /// Stable storage tag for the `favorites.source_type` column.
    pub fn type_tag(&self) -> &'static str {
        match self {
            AudioSource::File(_) => "File",
            AudioSource::Spotify(_) => "Spotify",
            AudioSource::Radio(_) => "Radio",
            AudioSource::PodcastFeed(_) => "PodcastFeed",
        }
    }

    /// Reconstruct an `AudioSource` from a storage tag + URI.
    ///
    /// Unknown tags fall back to `Radio` (best-effort, never fails a read).
    pub fn from_type_tag(tag: &str, uri: &str) -> Self {
        match tag {
            "File" => AudioSource::File(uri.to_string()),
            "Spotify" => AudioSource::Spotify(uri.to_string()),
            "PodcastFeed" => AudioSource::PodcastFeed(uri.to_string()),
            _ => AudioSource::Radio(uri.to_string()),
        }
    }

    /// True for podcast feeds (tapped to expand, not play, on the Pi).
    pub fn is_feed(&self) -> bool {
        matches!(self, AudioSource::PodcastFeed(_))
    }
}

// ── parse_source (D1) ───────────────────────────────────────────────────────

/// Interpret a legacy/source URI string as a typed [`AudioSource`].
///
/// Scheme rules (design D1):
/// - `spotify:` → [`AudioSource::Spotify`]
/// - `file:` → [`AudioSource::File`]
/// - `http(s)://` → podcast-feed heuristic; matches → [`AudioSource::PodcastFeed`],
///   otherwise [`AudioSource::Radio`]
/// - anything else → best-effort [`AudioSource::Radio`] (a bare URL is the
///   most common manual-paste shape; the web UI can override later).
///
/// The podcast-feed heuristic matches URLs whose path ends in `.xml` / `.rss`
/// or whose host is a known podcast feed host (`feeds.feedburner.com`,
/// `feeds.simplecast.com`, `podcasts.feed`).
pub fn parse_source(uri: &str) -> AudioSource {
    if let Some(rest) = uri.strip_prefix("spotify:") {
        let _ = rest;
        return AudioSource::Spotify(uri.to_string());
    }
    if uri.starts_with("file:") {
        return AudioSource::File(uri.to_string());
    }
    if uri.starts_with("http://") || uri.starts_with("https://") {
        if is_podcast_feed_url(uri) {
            return AudioSource::PodcastFeed(uri.to_string());
        }
        return AudioSource::Radio(uri.to_string());
    }
    // Unknown scheme: best-effort Radio.
    AudioSource::Radio(uri.to_string())
}

/// Heuristic for detecting a podcast feed URL (design D1 risk note).
fn is_podcast_feed_url(url: &str) -> bool {
    // Known podcast feed hosts.
    let host = url
        .split("://")
        .nth(1)
        .and_then(|s| s.split('/').next())
        .map(|h| h.to_ascii_lowercase())
        .unwrap_or_default();
    const KNOWN_HOSTS: &[&str] = &[
        "feeds.feedburner.com",
        "feeds.simplecast.com",
        "podcasts.feed",
        "feed.podbean.com",
        "feeds.megaphone.fm",
    ];
    if KNOWN_HOSTS.iter().any(|h| host == *h) {
        return true;
    }
    // Path ends in .xml / .rss (ignoring any query string).
    let path = url.split('?').next().unwrap_or(url);
    path.to_ascii_lowercase().ends_with(".xml") || path.to_ascii_lowercase().ends_with(".rss")
}

// ── Favorite ────────────────────────────────────────────────────────────────

/// A persisted media favorite. The Pi displays at most
/// [`FAVORITES_PI_CAP`] of these; the rest are web-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorite {
    /// Stable id (UUID string).
    pub id: String,
    /// Human-readable label shown in the media panel / web UI.
    pub name: String,
    /// Typed source.
    pub source: AudioSource,
    /// 0-based display order (ascending). Reordering updates this.
    pub display_order: i64,
}

/// Maximum number of favorites the Pi media panel displays.
pub const FAVORITES_PI_CAP: usize = 8;

// ── FavoriteStore (task 3.2) ────────────────────────────────────────────────

/// Favorites persistence, owned by main, borrowing the single `Connection`.
///
/// Mirrors the [`crate::alarm_store::AlarmStore`] / [`crate::database::ConfigStore`]
/// pattern: all mutations run inside a single transaction. Reordering updates
/// `display_order`.
pub struct FavoriteStore<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> FavoriteStore<'a> {
    /// Create a `FavoriteStore` borrowing *conn*.
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// List all favorites, ordered by `display_order` then `id` for stability.
    pub fn list(&self) -> crate::error::Result<Vec<Favorite>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, source_type, source_uri, display_order \
                 FROM favorites ORDER BY display_order, id",
            )
            .map_err(crate::error::ConfigError::Database)?;
        let rows = stmt
            .query_map([], row_to_favorite)
            .map_err(crate::error::ConfigError::Database)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::ConfigError::Database)?;
        Ok(rows)
    }

    /// Insert or update a favorite by `id`.
    pub fn upsert(&self, fav: &Favorite) -> crate::error::Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO favorites \
                 (id, name, source_type, source_uri, display_order) \
                 VALUES (?, ?, ?, ?, ?)",
                rusqlite::params![
                    fav.id,
                    fav.name,
                    fav.source.type_tag(),
                    fav.source.uri(),
                    fav.display_order,
                ],
            )
            .map_err(crate::error::ConfigError::Database)?;
        Ok(())
    }

    /// Delete a favorite by `id`.
    pub fn delete(&self, id: &str) -> crate::error::Result<()> {
        self.conn
            .execute("DELETE FROM favorites WHERE id = ?", [id])
            .map_err(crate::error::ConfigError::Database)?;
        Ok(())
    }

    /// Return the next free `display_order` (max + 1, or 0 for an empty table).
    pub fn next_display_order(&self) -> crate::error::Result<i64> {
        let max: Option<i64> = self
            .conn
            .query_row("SELECT MAX(display_order) FROM favorites", [], |r| r.get(0))
            .ok()
            .flatten();
        Ok(max.map(|m| m + 1).unwrap_or(0))
    }

    /// Reorder favorites to the given id sequence (front-to-back). Each id is
    /// assigned `display_order = index`; unknown ids are ignored. Runs in a
    /// single transaction.
    pub fn reorder(&self, ordered_ids: &[String]) -> crate::error::Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(crate::error::ConfigError::Database)?;
        for (i, id) in ordered_ids.iter().enumerate() {
            tx.execute(
                "UPDATE favorites SET display_order = ? WHERE id = ?",
                rusqlite::params![i as i64, id],
            )
            .map_err(crate::error::ConfigError::Database)?;
        }
        tx.commit()
            .map_err(crate::error::ConfigError::Database)?;
        Ok(())
    }
}

/// Map a `rusqlite::Row` to a [`Favorite`].
fn row_to_favorite(row: &rusqlite::Row<'_>) -> rusqlite::Result<Favorite> {
    let id: String = row.get("id")?;
    let name: String = row.get("name")?;
    let source_type: String = row.get("source_type")?;
    let source_uri: String = row.get("source_uri")?;
    let display_order: i64 = row.get("display_order")?;
    Ok(Favorite {
        id,
        name,
        source: AudioSource::from_type_tag(&source_type, &source_uri),
        display_order,
    })
}

// ── TransportCaps (D4) ───────────────────────────────────────────────────────

/// Per-source transport capability bitset (design D4).
///
/// Radio exposes `play` + `stop` only (no pause/next/prev/seek); its "pause"
/// maps to `stop`. Spotify / File / Podcast expose the full set (where the
/// backend supports it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransportCaps {
    pub play: bool,
    pub pause: bool,
    pub stop: bool,
    pub next: bool,
    pub prev: bool,
    pub seek: bool,
}

impl TransportCaps {
    /// Derive capabilities from an [`AudioSource`].
    pub fn for_source(source: &AudioSource) -> Self {
        match source {
            AudioSource::Radio(_) => Self {
                play: true,
                pause: false,
                stop: true,
                next: false,
                prev: false,
                seek: false,
            },
            AudioSource::File(_) | AudioSource::Spotify(_) | AudioSource::PodcastFeed(_) => Self {
                play: true,
                pause: true,
                stop: true,
                next: true,
                prev: true,
                seek: true,
            },
        }
    }

    /// Whether a given transport control is available.
    pub fn supports(&self, cmd: TransportCmd) -> bool {
        match cmd {
            TransportCmd::Play => self.play,
            TransportCmd::Pause => self.pause,
            TransportCmd::Stop => self.stop,
            TransportCmd::Next => self.next,
            TransportCmd::Previous => self.prev,
            TransportCmd::Seek(_) => self.seek,
        }
    }
}

// ── Transport dispatch (D4 / task 2.3) ───────────────────────────────────────

/// A transport control requested by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportCmd {
    /// Resume / start playback of the current tracklist item.
    Play,
    /// Pause (radio maps to stop — see [`resolve_transport`]).
    Pause,
    /// Stop playback.
    Stop,
    /// Next track.
    Next,
    /// Previous track.
    Previous,
    /// Seek to an absolute position, in milliseconds.
    Seek(u32),
}

/// A shaped Mopidy JSON-RPC call (method + params).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MopidyCall {
    pub method: &'static str,
    pub params: Value,
}

impl MopidyCall {
    fn void(method: &'static str) -> Self {
        // Mopidy rejects "params": null with -32600 Invalid Request
        // ("'params', if given, must be an array or an object"); void
        // methods send an empty array.
        Self { method, params: Value::Array(vec![]) }
    }
}

/// Resolve a transport command to the Mopidy call that realizes it, given the
/// active source's capabilities. Returns `None` when the capability is not
/// available (the UI should have hidden the control, but the worker still
/// defends in depth).
///
/// **Radio "pause" = stop** (design D4): for a radio source a `Pause` is
/// mapped to `playback.stop` so the stream resumes live on the next play.
pub fn resolve_transport(
    cmd: TransportCmd,
    caps: TransportCaps,
    source: &AudioSource,
) -> Option<MopidyCall> {
    match cmd {
        TransportCmd::Play if caps.play => Some(MopidyCall::void("playback.play")),
        TransportCmd::Pause => {
            if caps.pause {
                Some(MopidyCall::void("playback.pause"))
            } else if matches!(source, AudioSource::Radio(_)) && caps.stop {
                // Radio "pause" = stop (resumes live on restart).
                Some(MopidyCall::void("playback.stop"))
            } else {
                None
            }
        }
        TransportCmd::Stop if caps.stop => Some(MopidyCall::void("playback.stop")),
        TransportCmd::Next if caps.next => Some(MopidyCall::void("playback.next")),
        TransportCmd::Previous if caps.prev => Some(MopidyCall::void("playback.previous")),
        TransportCmd::Seek(ms) if caps.seek => Some(MopidyCall {
            method: "playback.seek",
            params: json!({ "time_position": ms }),
        }),
        _ => None,
    }
}

// ── Tap-to-play (task 2.2) ───────────────────────────────────────────────────

/// Shape the Mopidy call sequence to play any Mopidy URI immediately
/// (`tracklist.add` then `playback.play`). Used for non-feed favorites (radio /
/// spotify / file) and for tapped podcast episodes (whose URIs are already
/// `podcast+https://...` shaped by the backend).
///
/// For podcast *feeds* the caller should browse instead (see
/// [`browse_feed_call`]); playing the feed URL directly is best-effort only
/// (the "misdetected feed just plays" risk note).
pub fn play_uri_calls(uri: &str) -> Vec<MopidyCall> {
    vec![
        // Clear the tracklist first so tapping a favorite switches playback
        // instead of appending behind whatever is currently playing (slice 7).
        MopidyCall::void("tracklist.clear"),
        MopidyCall {
            method: "tracklist.add",
            params: json!({ "uris": [uri] }),
        },
        // Play the just-added tracklist item. Mirrors the proven episode
        // firing path (`tracklist.add` → `playback.play`); the clear is
        // scoped to the media favorite path and does not touch episode.rs.
        MopidyCall::void("playback.play"),
    ]
}

/// Shape the Mopidy call sequence to play a non-feed favorite immediately.
///
/// Convenience wrapper over [`play_uri_calls`] using the favorite's source URI.
pub fn play_favorite_calls(source: &AudioSource) -> Vec<MopidyCall> {
    play_uri_calls(source.uri())
}

// ── Podcast feed browsing (D2 / task 2.1) ────────────────────────────────────

/// Convert a stored podcast feed URL into the Mopidy podcast-backend URI.
///
/// Mopidy-Podcast's backend owns the URI schemes `podcast`, `podcast+file`,
/// `podcast+http`, `podcast+https` and partitions on `+` to recover the feed
/// URL (see `mopidy_podcast.backend.PodcastFeedCache`). A stored feed URL of
/// `https://example.com/feed.xml` becomes `podcast+https://example.com/feed.xml`.
///
/// `http://` feeds map to `podcast+http://...`; `file:` feeds map to
/// `podcast+file:...`; already-prefixed URIs are passed through unchanged.
pub fn feed_uri_for_browse(feed_url: &str) -> String {
    if feed_url.starts_with("podcast+") || feed_url.starts_with("podcast:") {
        return feed_url.to_string();
    }
    if let Some(rest) = feed_url.strip_prefix("https://") {
        return format!("podcast+https://{}", rest);
    }
    if let Some(rest) = feed_url.strip_prefix("http://") {
        return format!("podcast+http://{}", rest);
    }
    if let Some(rest) = feed_url.strip_prefix("file:") {
        return format!("podcast+file:{}", rest);
    }
    // Unknown scheme — pass through best-effort.
    format!("podcast+{}", feed_url)
}

/// Shape the `library.browse` call for a podcast feed URL.
///
/// The feed URL is normalized via [`feed_uri_for_browse`] to the
/// `podcast+https://...` form Mopidy's podcast backend expects.
pub fn browse_feed_call(feed_url: &str) -> MopidyCall {
    MopidyCall {
        method: "library.browse",
        params: json!({ "uri": feed_uri_for_browse(feed_url) }),
    }
}

/// A browsable feed episode (most-recent 5 on the Pi).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedEpisode {
    /// The episode's playable URI.
    pub uri: String,
    /// Episode title.
    pub name: String,
}

/// Parse a `library.browse` result (an array of Mopidy `Ref` objects) into the
/// most-recent [`FeedEpisode`] list, capped at *cap* (5 on the Pi).
///
/// Mopidy returns episodes in feed order (newest-first for most podcast
/// backends). Unknown/empty results degrade to an empty list (the
/// "mopidy-podcast may be uninstalled" risk).
pub fn parse_feed_episodes(result: &Value, cap: usize) -> Vec<FeedEpisode> {
    let arr = match result.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|r| {
            let uri = r.get("uri").and_then(|v| v.as_str())?.to_string();
            let name = r
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(FeedEpisode { uri, name })
        })
        .take(cap)
        .collect()
}

/// Cap applied to feed browse replies on the Pi.
pub const FEED_EPISODES_PI_CAP: usize = 5;

// ── Fallback chain reconciliation (task 1.4) ─────────────────────────────────

/// Reconcile a legacy `Vec<String>` fallback chain into `Vec<AudioSource>` at read
/// time (design D1 / persistence spec). `None`/empty stays `None`.
pub fn reconcile_fallback_chain(chain: Option<&Vec<String>>) -> Option<Vec<AudioSource>> {
    let chain = chain?;
    if chain.is_empty() {
        return None;
    }
    let parsed: Vec<AudioSource> = chain.iter().map(|s| parse_source(s)).collect();
    Some(parsed)
}

// ── Default favorites seeding (task 3.3) ─────────────────────────────────────

/// Pre-populate CBC Radio 1 Calgary as a favorite on first boot when the
/// `favorites` table is empty. Idempotent — a non-empty table is left untouched
/// (the user's edits / web additions persist).
///
/// This mirrors the slice-1 alarm-seeding pattern but is **not** dev-gated,
/// since the radio default is intended out-of-the-box. The seeded URI is a
/// TuneIn station id resolved by Mopidy's TuneIn backend.
pub fn seed_default_favorites(store: &FavoriteStore<'_>) -> crate::error::Result<()> {
    if !store.list()?.is_empty() {
        tracing::info!(
            marker = "favorites seed",
            "favorites table non-empty; skipping default seed"
        );
        return Ok(());
    }
    for (id, name, url) in DEFAULT_FAVORITES {
        let order = store.next_display_order()?;
        store.upsert(&Favorite {
            id: id.to_string(),
            name: name.to_string(),
            source: AudioSource::Radio(url.to_string()),
            display_order: order,
        })?;
    }
    tracing::info!(
        marker = "favorites seed",
        count = DEFAULT_FAVORITES.len(),
        "default radio favorite seeded (CBC Radio 1 Calgary)"
    );
    Ok(())
}

/// The default seeded favorites. CBC Radio 1 Calgary ships as a TuneIn
/// station URI (resolved by Mopidy's TuneIn backend); the legacy direct
/// stream URL was unreliable. CKUA is intentionally omitted from the seeded
/// defaults (its stream/TuneIn feed has been intermittent) — it remains in
/// `stations.json` for the web UI to offer once a stable URL is confirmed.
pub const DEFAULT_FAVORITES: &[(&str, &str, &str)] = &[
    (
        "cbc-radio-one-calgary",
        "CBC Radio One Calgary",
        "tunein:station:s31103",
    ),
];

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── parse_source (task 1.5) ─────────────────────────────────────────

    #[test]
    fn parse_source_spotify() {
        assert_eq!(
            parse_source("spotify:track:abc"),
            AudioSource::Spotify("spotify:track:abc".into())
        );
    }

    #[test]
    fn parse_source_file() {
        assert_eq!(
            parse_source("file:///music/track.mp3"),
            AudioSource::File("file:///music/track.mp3".into())
        );
    }

    #[test]
    fn parse_source_radio_https() {
        assert_eq!(
            parse_source("https://stream.example.com/live"),
            AudioSource::Radio("https://stream.example.com/live".into())
        );
    }

    #[test]
    fn parse_source_radio_http() {
        assert_eq!(
            parse_source("http://stream.example.com:8000/live"),
            AudioSource::Radio("http://stream.example.com:8000/live".into())
        );
    }

    #[test]
    fn parse_source_podcast_by_extension_xml() {
        assert_eq!(
            parse_source("https://example.com/feed.xml"),
            AudioSource::PodcastFeed("https://example.com/feed.xml".into())
        );
    }

    #[test]
    fn parse_source_podcast_by_extension_rss() {
        assert_eq!(
            parse_source("https://example.com/podcast.rss?param=1"),
            AudioSource::PodcastFeed("https://example.com/podcast.rss?param=1".into())
        );
    }

    #[test]
    fn parse_source_podcast_known_host() {
        assert!(matches!(
            parse_source("https://feeds.feedburner.com/myshow"),
            AudioSource::PodcastFeed(_)
        ));
    }

    #[test]
    fn parse_source_unknown_scheme_defaults_radio() {
        assert!(matches!(parse_source("weird:thing"), AudioSource::Radio(_)));
    }

    // ── round-trip (task 1.5) ───────────────────────────────────────────

    #[test]
    fn parse_source_round_trips_each_variant() {
        let cases = [
            AudioSource::Spotify("spotify:track:x".into()),
            AudioSource::File("file:///b".into()),
            AudioSource::Radio("https://stream.example.com/live".into()),
            AudioSource::PodcastFeed("https://example.com/feed.xml".into()),
        ];
        for src in cases {
            let reparsed = parse_source(src.uri());
            assert_eq!(reparsed, src, "round-trip failed for {:?}", src);
        }
    }

    #[test]
    fn type_tag_round_trips() {
        let cases = [
            AudioSource::File("file:///a".into()),
            AudioSource::Spotify("spotify:track:x".into()),
            AudioSource::Radio("https://x.example.com/live".into()),
            AudioSource::PodcastFeed("https://x.example.com/f.xml".into()),
        ];
        for src in cases {
            let tag = src.type_tag();
            let back = AudioSource::from_type_tag(tag, src.uri());
            assert_eq!(back, src, "type-tag round-trip failed for {:?}", src);
        }
    }

    // ── TransportCaps (task 1.3 / 1.5) ──────────────────────────────────

    #[test]
    fn caps_radio_is_play_stop_only() {
        let caps = TransportCaps::for_source(&AudioSource::Radio("https://x".into()));
        assert!(caps.play);
        assert!(caps.stop);
        assert!(!caps.pause);
        assert!(!caps.next);
        assert!(!caps.prev);
        assert!(!caps.seek);
    }

    #[test]
    fn caps_spotify_is_full() {
        let caps = TransportCaps::for_source(&AudioSource::Spotify("spotify:track:x".into()));
        assert!(caps.play && caps.pause && caps.stop && caps.next && caps.prev && caps.seek);
    }

    #[test]
    fn caps_file_is_full() {
        let caps = TransportCaps::for_source(&AudioSource::File("file:///a".into()));
        assert!(caps.play && caps.pause && caps.stop && caps.next && caps.prev && caps.seek);
    }

    #[test]
    fn caps_podcast_is_full() {
        let caps = TransportCaps::for_source(&AudioSource::PodcastFeed("https://x/f.xml".into()));
        assert!(caps.play && caps.pause && caps.stop && caps.next && caps.prev && caps.seek);
    }

    // ── resolve_transport (task 2.4) ────────────────────────────────────

    #[test]
    fn transport_pause_radio_maps_to_stop() {
        let src = AudioSource::Radio("https://x".into());
        let caps = TransportCaps::for_source(&src);
        let call = resolve_transport(TransportCmd::Pause, caps, &src).unwrap();
        assert_eq!(call.method, "playback.stop");
    }

    #[test]
    fn transport_pause_spotify_maps_to_pause() {
        let src = AudioSource::Spotify("spotify:track:x".into());
        let caps = TransportCaps::for_source(&src);
        let call = resolve_transport(TransportCmd::Pause, caps, &src).unwrap();
        assert_eq!(call.method, "playback.pause");
    }

    #[test]
    fn transport_next_radio_unavailable() {
        let src = AudioSource::Radio("https://x".into());
        let caps = TransportCaps::for_source(&src);
        assert_eq!(resolve_transport(TransportCmd::Next, caps, &src), None);
    }

    #[test]
    fn transport_seek_spotify_available() {
        let src = AudioSource::Spotify("spotify:track:x".into());
        let caps = TransportCaps::for_source(&src);
        let call = resolve_transport(TransportCmd::Seek(30_000), caps, &src).unwrap();
        assert_eq!(call.method, "playback.seek");
        assert_eq!(call.params["time_position"], 30_000);
    }

    #[test]
    fn transport_play_maps_to_play() {
        let src = AudioSource::File("file:///a".into());
        let caps = TransportCaps::for_source(&src);
        let call = resolve_transport(TransportCmd::Play, caps, &src).unwrap();
        // `playback.play` (no params) replays the current/first tracklist
        // item — works for stopped radio streams; `playback.resume` does
        // not restart a stopped TuneIn stream.
        assert_eq!(call.method, "playback.play");
    }

    #[test]
    fn transport_seek_radio_unavailable() {
        let src = AudioSource::Radio("https://x".into());
        let caps = TransportCaps::for_source(&src);
        assert_eq!(resolve_transport(TransportCmd::Seek(0), caps, &src), None);
    }

    // ── tap-to-play (task 2.2) ──────────────────────────────────────────

    #[test]
    fn play_favorite_adds_then_plays() {
        let src = AudioSource::Radio("tunein:station:s31103".into());
        let calls = play_favorite_calls(&src);
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].method, "tracklist.clear");
        assert_eq!(calls[1].method, "tracklist.add");
        assert_eq!(calls[1].params["uris"][0], "tunein:station:s31103");
        assert_eq!(calls[2].method, "playback.play");
    }

    #[test]
    fn play_uri_calls_clears_adds_then_plays() {
        let calls = play_uri_calls("podcast+https://example.com/feed.xml#3");
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].method, "tracklist.clear");
        assert_eq!(calls[1].method, "tracklist.add");
        assert_eq!(
            calls[1].params["uris"][0],
            "podcast+https://example.com/feed.xml#3"
        );
        assert_eq!(calls[2].method, "playback.play");
    }

    // ── feed URI normalization (task 2.1 / 2.4) ─────────────────────────

    #[test]
    fn feed_uri_for_browse_https() {
        assert_eq!(
            feed_uri_for_browse("https://example.com/feed.xml"),
            "podcast+https://example.com/feed.xml"
        );
    }

    #[test]
    fn feed_uri_for_browse_http() {
        assert_eq!(
            feed_uri_for_browse("http://example.com/feed.rss"),
            "podcast+http://example.com/feed.rss"
        );
    }

    #[test]
    fn feed_uri_for_browse_already_prefixed() {
        assert_eq!(
            feed_uri_for_browse("podcast+https://example.com/feed.xml"),
            "podcast+https://example.com/feed.xml"
        );
        assert_eq!(
            feed_uri_for_browse("podcast:foo"),
            "podcast:foo"
        );
    }

    #[test]
    fn browse_feed_call_shape() {
        let call = browse_feed_call("https://example.com/feed.xml");
        assert_eq!(call.method, "library.browse");
        assert_eq!(
            call.params["uri"],
            "podcast+https://example.com/feed.xml"
        );
    }

    #[test]
    fn parse_feed_episodes_takes_most_recent_five() {
        let result = json!([
            { "uri": "ep1", "name": "Episode 1" },
            { "uri": "ep2", "name": "Episode 2" },
            { "uri": "ep3", "name": "Episode 3" },
            { "uri": "ep4", "name": "Episode 4" },
            { "uri": "ep5", "name": "Episode 5" },
            { "uri": "ep6", "name": "Episode 6" },
            { "uri": "ep7", "name": "Episode 7" }
        ]);
        let eps = parse_feed_episodes(&result, FEED_EPISODES_PI_CAP);
        assert_eq!(eps.len(), 5);
        assert_eq!(eps[0].uri, "ep1");
        assert_eq!(eps[4].name, "Episode 5");
    }

    #[test]
    fn parse_feed_episodes_empty_array() {
        let result = json!([]);
        let eps = parse_feed_episodes(&result, FEED_EPISODES_PI_CAP);
        assert!(eps.is_empty());
    }

    #[test]
    fn parse_feed_episodes_non_array_degrades_empty() {
        let result = json!({"not": "an array"});
        let eps = parse_feed_episodes(&result, FEED_EPISODES_PI_CAP);
        assert!(eps.is_empty());
    }

    #[test]
    fn parse_feed_episodes_missing_name_defaults_blank() {
        let result = json!([{ "uri": "ep1" }]);
        let eps = parse_feed_episodes(&result, 5);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].name, "");
    }

    // ── fallback chain reconciliation (task 1.4 / 1.5) ─────────────────

    #[test]
    fn reconcile_fallback_chain_legacy_strings() {
        let chain = Some(vec![
            "spotify:track:x".to_string(),
            "file:///b".to_string(),
            "https://stream.example.com/live".to_string(),
            "https://example.com/feed.xml".to_string(),
        ]);
        let reconciled = reconcile_fallback_chain(chain.as_ref()).unwrap();
        assert_eq!(reconciled.len(), 4);
        assert!(matches!(reconciled[0], AudioSource::Spotify(_)));
        assert!(matches!(reconciled[1], AudioSource::File(_)));
        assert!(matches!(reconciled[2], AudioSource::Radio(_)));
        assert!(matches!(reconciled[3], AudioSource::PodcastFeed(_)));
    }

    #[test]
    fn reconcile_fallback_chain_none_stays_none() {
        assert_eq!(reconcile_fallback_chain(None), None);
    }

    #[test]
    fn reconcile_fallback_chain_empty_is_none() {
        let empty: Vec<String> = Vec::new();
        assert_eq!(reconcile_fallback_chain(Some(&empty)), None);
    }

    // ── FavoriteStore persistence (task 3.4) ─────────────────────────────

    /// Build a fresh, migrated temp DB and return a leaked `FavoriteStore`
    /// borrowing it for `'static` (tests are short-lived).
    fn fresh_store() -> (std::path::PathBuf, FavoriteStore<'static>) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "alarm_media_fav_test_{}_{}_{}.db",
            std::process::id(),
            n,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let _ = std::fs::remove_file(&path);
        let conn = crate::database::open_connection(path.to_str().unwrap()).unwrap();
        crate::database::run_migrations(&conn).unwrap();
        let conn: &'static rusqlite::Connection = Box::leak(Box::new(conn));
        (path, FavoriteStore::new(conn))
    }

    #[test]
    fn migration_v8_creates_favorites_table() {
        let (path, store) = fresh_store();
        // user_version should be at least 8.
        let v: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(v >= 8, "user_version should be >= 8, got {v}");
        let has_table: bool = store
            .conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='favorites'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(has_table, "favorites table should exist after migration");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn favorites_crud_round_trip() {
        let (path, store) = fresh_store();
        assert!(store.list().unwrap().is_empty());

        let order = store.next_display_order().unwrap();
        assert_eq!(order, 0);
        store
            .upsert(&Favorite {
                id: "a".into(),
                name: "Station A".into(),
                source: AudioSource::Radio("https://a/live".into()),
                display_order: order,
            })
            .unwrap();
        store
            .upsert(&Favorite {
                id: "b".into(),
                name: "Show B".into(),
                source: AudioSource::PodcastFeed("https://b/feed.xml".into()),
                display_order: store.next_display_order().unwrap(),
            })
            .unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "a");
        assert_eq!(list[1].id, "b");
        assert!(matches!(list[1].source, AudioSource::PodcastFeed(_)));

        store.delete("a").unwrap();
        assert_eq!(store.list().unwrap().len(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn favorites_reorder_round_trip() {
        let (path, store) = fresh_store();
        for id in ["a", "b", "c"] {
            store
                .upsert(&Favorite {
                    id: id.into(),
                    name: id.into(),
                    source: AudioSource::Radio(format!("https://{id}/live")),
                    display_order: store.next_display_order().unwrap(),
                })
                .unwrap();
        }
        // Reverse the order.
        store
            .reorder(&["c".into(), "b".into(), "a".into()])
            .unwrap();
        let list = store.list().unwrap();
        assert_eq!(
            list.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
            vec!["c", "b", "a"]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn seed_default_favorites_populates_cbc() {
        let (path, store) = fresh_store();
        seed_default_favorites(&store).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1, "only CBC seeded (CKUA omitted — feed broken)");
        assert_eq!(list[0].name, "CBC Radio One Calgary");
        assert!(matches!(list[0].source, AudioSource::Radio(_)));
        // Idempotent: re-running does not duplicate.
        seed_default_favorites(&store).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn seed_default_favorites_skips_non_empty_table() {
        let (path, store) = fresh_store();
        store
            .upsert(&Favorite {
                id: "x".into(),
                name: "User Station".into(),
                source: AudioSource::Radio("https://x/live".into()),
                display_order: 0,
            })
            .unwrap();
        seed_default_favorites(&store).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1, "non-empty table left untouched");
        assert_eq!(list[0].name, "User Station");
        let _ = std::fs::remove_file(&path);
    }
}

## Context

The Media panel and quick-controls overlay are the last major Pi UI surfaces. Mopidy's typed surface (slices 0–1) already provides playback/tracklist methods; slice 7 builds the favorites/feed/transport model and the two UI surfaces over it. It also introduces `AudioSource`, which slice 4a's alarm fallback chain adopts.

## Goals / Non-Goals

**Goals:** `Favorite`/`AudioSource` model; podcast feed browsing (cap 5); source-adaptive transport; media panel; quick-controls overlay; curated `stations.json`; pre-populated CBC/CKUA; suspend/re-arm bedtime idle timer on overlay.

**Non-Goals:** TuneIn browse (v2); podcast discovery UI (web-only, slice 8); web live control (v2); Pi favorites editor (web-only — Pi does tap-to-play + reorder).

## Decisions

### D1. AudioSource enum with URI-scheme interpretation for legacy strings
`AudioSource` is a typed enum. Slice 2's `fallback_chain: Vec<String>` is read through a `parse_source(uri)` helper: `spotify:` → `Spotify`, `file:` → `File`, http(s) → `Radio`, podcast feed URLs (heuristic: ends in `.xml`/`.rss` or known podcast host) → `PodcastFeed`. No destructive migration — the stored column stays text; interpretation is at the boundary.

### D2. Podcast feeds are browsable via Mopidy's podcast backend
`mopidy-podcast` exposes `library.browse` on the feed URI. The media module calls `library.browse(feed_uri)` and takes the most-recent 5 tracks (episodes). Tapping an episode plays it. No RSS parsing in the Rust app — Mopidy is the feed parser.

### D3. Quick-controls overlay is a Slint popup, not a panel
A `Popup`-like `Rectangle` rendered above the `PanelContainer` on swipe-up, with its own `TouchArea`. Tap-outside (the underlying area) or a 5 s `slint::Timer` dismisses it. Opening sets a `DisplayController` flag suspending the bedtime idle timer (slice 4); closing clears it.

### D4. Transport capabilities derived from the active source type
A `TransportCaps { play, pause, stop, next, prev, seek }` bitset derived from `AudioSource` (radio = play+stop only). The transport row binds control visibility to `TransportCaps`. Radio "pause" maps to `playback.stop` (resumes live on restart, per PRD).

## Risks / Trade-offs

- **[Podcast feed URL detection heuristic]** → false positives possible; the web UI (slice 8) lets the user explicitly mark a favorite as a feed, overriding the heuristic. Pi-side, a misdetected feed just plays the URL.
- **[Mopidy podcast backend may be uninstalled]** → degrade gracefully: feed browse returns empty; tapping the favorite plays the feed URL directly (best-effort).
- **[Quick-controls 5 s idle vs bedtime 10 s]** → independent timers; the overlay's 5 s dismiss does not affect bedtime's 10 s (which is suspended while the overlay is open).

## Migration Plan

Migration `v7` (favorites table). Pre-populate CBC/CKUA via the dev-seed path at first boot. `stations.json` bundled. No rollback.

## Open Questions

- Should favorites support images (album art / station logos)? PRD doesn't require it; deferring to v2.
- Is `library.browse` the right Mopidy call for podcast episodes, or should the app parse RSS itself? Decision: use Mopidy (D2) to avoid a second feed parser; revisit if backend proves unreliable.

# Slice 7: Media Player

## Why

The PRD's Media panel (now-playing, transport, favorites) and the quick-controls swipe-up overlay (volume + brightness + transport) are unimplemented. Slice 0's `mopidy-client` has the typed playback/tracklist surface; slice 1 wired fire/dismiss. Slice 7 builds the favorites model, podcast feed browsing, transport controls, the media panel, and the quick-controls overlay over the existing Mopidy seam.

## What Changes

- **`Favorite` + `AudioSource` model.** `Favorite { name, source: AudioSource }` where `AudioSource` is `File(uri) | Spotify(uri) | Radio(url) | PodcastFeed(feed_url)`. Shared between the media panel and alarm source config (slice 4a's `fallback_chain` reconciles `Vec<String>` → `Vec<AudioSource>`).
- **Favorites persistence.** A `favorites` table (name, source-type, source-uri, order). Cap 8 displayed on the Pi (web-enforced soft limit with warning, slice 8).
- **Podcast feed browsing.** A podcast favorite is a feed; tapping expands to an episode list (most-recent 5 on the Pi) rather than immediately playing. Other favorites play on tap.
- **Transport controls.** Adapt to source capabilities: radio = play/stop only; spotify/local/podcast = play/pause/next/prev/seek (where supported).
- **Media panel.** Now-playing card (track + artist), transport row, favorites list — populated into the slot slice 3 defined, matching the wireframe.
- **Quick-controls overlay.** Swipe up on any panel → compact overlay (volume slider + brightness slider + play/pause + next/prev if applicable). Dismissed by tap-outside or 5 s idle. Invoking it suspends the bedtime idle timer (slice 4); dismissing re-arms.
- **Curated `stations.json`.** A bundled catalog (CBC Radio 1 Calgary, CKUA, + common) for tap-to-add in the web UI. Manual URL paste also supported (web). Existing favorites are independent of the catalog.
- **Internet radio.** Radio is a `Favorite { source: Radio(url) }` — no separate concept. CBC Radio 1 + CKUA ship as pre-populated favorites.

## Non-goals

- TuneIn radio browse (v2).
- Podcast discovery beyond feed-URL entry (iTunes/gPodder browse backend exists in Mopidy; the browse UI is web-only, slice 8).
- Live media control from the web (v2).
- A Pi-side favorites *editor* (create/edit/delete is web-only; the Pi does tap-to-play + reorder only).

## Capabilities

### New Capabilities
- `media-player`: `Favorite`/`AudioSource` model, podcast feed browsing, transport, media panel, quick-controls overlay, curated `stations.json`.

### Modified Capabilities
- `persistence`: `favorites` table.
- `ui-shell`: media-panel content + quick-controls overlay slot.

## Impact

- **New code:** `alarm-clock/src/media.rs` (favorites, transport, podcast feeds), `alarm-clock/ui/QuickControls.slint`, `alarm-clock/ui/MediaPanel.slint`, `alarm-clock/stations.json`.
- **Modified code:** `alarm-clock/src/alarm_store.rs` (reconcile `fallback_chain` to `AudioSource`), `alarm-clock/src/main.rs` (media wiring, quick-controls), `alarm-clock/src/database.rs` (migration `v7`, favorites table).
- **Pre-populated:** CBC Radio 1 Calgary, CKUA as seeded favorites (dev seed path, mirroring slice-1 alarm seeding).
- **Depends on:** slice 3 (panel slots), slice 4 (quick-controls suspends bedtime idle timer), slice 1 (Mopidy seam).

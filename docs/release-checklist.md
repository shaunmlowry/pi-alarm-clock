# Release Checklist

Pre-release verification for the alarm-clock Pi application.

## Media / Radio (slice 7)

Internet-radio stream URLs are curated and may be rotated by broadcasters.
Before each release, verify the curated stream URLs still resolve and play:

- [ ] Verify every URL in `alarm-clock/stations.json` resolves and plays audio
      (open each in a player, e.g. `mpv <url>` or `ffplay <url>`).
- [ ] Verify the seeded default favorite still plays:
      - [ ] CBC Radio One Calgary (`src/media.rs::DEFAULT_FAVORITES`, URI `tunein:station:s31103`) — CKUA is omitted from seeded defaults (its stream/TuneIn feed has been intermittent); verify before re-adding.
- [ ] If a curated URL has moved, update `stations.json` **and** the
      corresponding `DEFAULT_FAVORITES` entry, then re-run
      `cargo test -p alarm-clock media::` to confirm seeded favorites still
      round-trip.
- [ ] Podcast feed URLs must be a real podcast RSS feed (not a redirect/
      Atom-blog shell). Confirm by browsing via Mopidy: `core.library.browse`
      on `podcast+https://<feed>` returns episodes. (`https://atp.fm/rss`
      works; `feeds.feedburner.com/<slug>` often resolves to a non-podcast
      page and is rejected by mopidy-podcast with "Not a recognized podcast
      feed" — prefer the podcast's canonical RSS.)
- [ ] Confirm the `mopidy-podcast` backend is installed on the target image if
      podcast feed browsing is expected to work on the Pi.

## Build & tests

- [ ] `cargo build --release` green.
- [ ] `cargo test --workspace` green; slice 0–6 tests unaffected.
- [ ] `cargo clippy --workspace -- -D warnings` clean (if enforced).

## Live Pi check (slice 7 / task 6.2)

- [ ] Tap a radio favorite → stream plays immediately.
- [ ] Tap a podcast favorite → expands to most-recent 5 episodes.
- [ ] Tap an episode → plays.
- [ ] Transport row adapts to source (radio = play/stop; spotify/podcast = full).
- [ ] Swipe up from any panel → quick-controls overlay opens.
- [ ] Tap outside overlay or 5 s idle → overlay dismisses; bedtime idle re-arms.
- [ ] Overlay open suspends the bedtime idle timer; closing re-arms it.

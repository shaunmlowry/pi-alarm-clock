## 1. Favorite & AudioSource model (alarm-clock/src/media.rs)

- [ ] 1.1 Define `AudioSource` (`File`/`Spotify`/`Radio`/`PodcastFeed`) + `Favorite { name, source }`.
- [ ] 1.2 Implement `parse_source(uri)` URI-scheme interpretation (spotify:/file:/http(s)/feed heuristic).
- [ ] 1.3 Implement `TransportCaps` derivation from `AudioSource`; radio "pause" = stop.
- [ ] 1.4 Reconcile alarm `fallback_chain` from `Vec<String>` to `Vec<AudioSource>` at read time (slice 4a adopts).
- [ ] 1.5 Unit-test: `parse_source` cases; `TransportCaps` per source; round-trip.

## 2. Podcast browsing & transport (alarm-clock/src/media.rs, channel.rs)

- [ ] 2.1 Add `Cmd::BrowseFeed(feed_uri)` + reply; tokio worker calls `library.browse`, returns most-recent 5.
- [ ] 2.2 Implement tap-to-play (non-feed) and tap-to-expand (feed) on the Pi.
- [ ] 2.3 Wire transport (play/pause/stop/next/prev/seek) to the Mopidy seam per `TransportCaps`.
- [ ] 2.4 Unit-test: feed browse reply shape; transport cmd dispatch per caps.

## 3. Persistence (alarm-clock/src/database.rs, alarm_store.rs)

- [ ] 3.1 Migration `v7`: `CREATE TABLE favorites (...)`; bump `user_version` to 7.
- [ ] 3.2 Favorites CRUD + `display_order` reorder.
- [ ] 3.3 Pre-populate CBC Radio 1 Calgary + CKUA via dev-seed path at first boot.
- [ ] 3.4 Unit-test: v7 migration; favorites CRUD + reorder round-trip; legacy `fallback_chain` interpretation.

## 4. UI (alarm-clock/ui/MediaPanel.slint, QuickControls.slint, ui.slint)

- [ ] 4.1 Media panel: now-playing card, transport row (caps-driven), favorites list (cap 8).
- [ ] 4.2 Quick-controls overlay (popup): volume + brightness sliders + transport; tap-outside/5 s idle dismiss.
- [ ] 4.3 Wire overlay open → `DisplayController` suspend bedtime idle timer; close → re-arm (slice 4).
- [ ] 4.4 Bind both to active theme tokens.

## 5. Curated catalog (alarm-clock/stations.json)

- [ ] 5.1 Bundle `stations.json` (CBC, CKUA, + common); web UI (slice 8) tap-to-add.
- [ ] 5.2 Release-checklist doc: verify curated stream URLs before each release.

## 6. Verification

- [ ] 6.1 `cargo build` + `cargo test` green; slice 0–6 tests unaffected.
- [ ] 6.2 Live check: tap favorite plays; podcast expands to episodes; transport adapts; quick-controls overlay opens/dismisses and suspends bedtime idle.

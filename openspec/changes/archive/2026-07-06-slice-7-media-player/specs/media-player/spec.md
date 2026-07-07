## ADDED Requirements

### Requirement: Favorite and AudioSource model
The application SHALL model a `Favorite { name, source: AudioSource }` where `AudioSource` is `File(uri) | Spotify(uri) | Radio(url) | PodcastFeed(feed_url)`. Favorites SHALL be shared between the media panel and alarm source configuration. Tapping a non-podcast favorite on the Pi SHALL play it immediately; tapping a podcast favorite SHALL expand to its episode list.

#### Scenario: Radio favorite plays on tap
- **WHEN** the user taps a `Radio(url)` favorite on the Media panel
- **THEN** Mopidy plays the stream immediately

#### Scenario: Podcast favorite expands to episodes
- **WHEN** the user taps a `PodcastFeed(feed_url)` favorite
- **THEN** an episode list (most-recent 5 on the Pi) is shown rather than immediately playing

### Requirement: Favorites persistence with an 8-item Pi cap
The application SHALL persist favorites in a `favorites` table (name, source-type, source-uri, display order). The Pi SHALL display at most 8 favorites; additional favorites are web-only (slice 8 enforces a soft limit with warning).

#### Scenario: Favorites round-trip with order
- **WHEN** favorites are reordered on the Pi and the app restarts
- **THEN** the stored order is preserved

#### Scenario: Pi shows at most 8 favorites
- **WHEN** more than 8 favorites are configured
- **THEN** the Pi media panel shows the first 8; the rest are web-only

### Requirement: Transport controls adapt to source capabilities
Transport controls SHALL adapt to the playing source: radio = play/stop only (no next/prev/seek; "pause" = stop, resumes live on restart); spotify/local/podcast = play/pause, next/prev, seek where supported.

#### Scenario: Radio shows play/stop only
- **WHEN** a radio favorite is playing
- **THEN** the transport row shows play/stop and no next/prev/seek controls

#### Scenario: Spotify shows full transport
- **WHEN** a Spotify track is playing
- **THEN** the transport row shows play/pause, next/prev, and seek

### Requirement: Quick-controls swipe-up overlay
A swipe up on any panel SHALL open a compact quick-controls overlay containing a volume slider, a brightness slider, and play/pause + next/prev (if applicable to the current source). The overlay SHALL dismiss on tap-outside or 5 s idle. Invoking the overlay SHALL suspend the bedtime idle timer (slice 4); dismissing SHALL re-arm it.

#### Scenario: Swipe up opens quick controls
- **WHEN** the user swipes up on any panel
- **THEN** the quick-controls overlay appears with volume + brightness sliders + transport

#### Scenario: Tap-outside or 5 s idle dismisses
- **WHEN** the user taps outside the overlay or 5 s elapse with no interaction
- **THEN** the overlay dismisses and the bedtime idle timer re-arms

### Requirement: Curated stations catalog and pre-populated radio favorites
The application SHALL ship a bundled `stations.json` catalog (CBC Radio 1 Calgary, CKUA, + common stations) for tap-to-add in the web UI. CBC Radio 1 Calgary and CKUA SHALL be pre-populated as favorites on first boot (dev seed path). Existing favorites are independent of the catalog. Curated stream URLs SHALL be verified to work before each release (release checklist).

#### Scenario: First boot pre-populates CBC + CKUA
- **WHEN** the app boots for the first time with an empty favorites table
- **THEN** CBC Radio 1 Calgary and CKUA are present as favorites

#### Scenario: Manual URL paste is independent of the catalog
- **WHEN** the user adds a radio favorite by manual URL paste (web UI)
- **THEN** it persists as a favorite regardless of catalog membership

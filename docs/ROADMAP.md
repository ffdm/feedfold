# Roadmap

feedfold is already usable today. The roadmap is about making it sharper,
smarter, and easier to adopt without losing the core promise: your
subscriptions should feel curated, not punishing.

This file serves two jobs:

- It shows what the product already delivers.
- It records the order in which major capabilities landed.

## Current state (2026-04-21)

Phases 0 through 6 are complete. Today, feedfold gives you:

- A terminal UI with Home, Channels, Viewed, and Overflow views.
- Per-source top-N ranking, so each source earns a limited amount of attention.
- Ratings, starring, ignore/viewed state, and search.
- RSS, Atom, and YouTube feeds in one reader.
- YouTube enrichment for duration, views, thumbnails, and smarter ranking.
- OPML import and export, source listing and removal, and first-run config
  bootstrap.
- A background daemon on macOS that starts automatically when you open the TUI.
- Optional Claude ranking that uses your interests and rating history.

The product is no longer a prototype. The remaining roadmap is mostly about
deeper integrations, more import paths, and more ways to shape the stream.

## Product direction

feedfold is not trying to become a giant "read everything" client. The goal is
the opposite: help you keep a broad set of subscriptions without turning them
into a second inbox.

That means future work is judged by a simple standard:

- Does this reduce noise?
- Does this speed up decisions?
- Does this make coming back after a few days feel better, not worse?

## Phase 0: Foundations (done)

The first milestone proved the boring but necessary path: config, storage, and
feed parsing.

- Cargo workspace and crate boundaries.
- Typed config loading from TOML.
- SQLite storage layer with migrations.
- `feedfold add <url>` for fetching and persisting a feed.

**Done when:** any RSS or Atom feed can be fetched, normalized, and stored.

## Phase 1: Daemon and first reading flow (done)

This phase turned feedfold from a parser into a reader.

- Background polling runtime.
- Minimal TUI home view.
- Read / unread state.
- Recency ranking and top-N selection.
- Fast keyboard navigation and refresh.

**Done when:** you can leave the app alone, come back, and scan a sane home
screen instead of a raw firehose.

## Phase 2: YouTube as a first-class source (done)

The project became more useful once blogs and channels could live together.

- `YoutubeAdapter` built on top of the generic RSS path.
- YouTube Data API enrichment for duration, views, and thumbnails.
- Popularity ranking mode.
- Thumbnail rendering with graceful fallback.

**Done when:** YouTube subscriptions sit naturally beside regular feeds instead
of feeling bolted on.

## Phase 3: Memory and triage tools (done)

This phase made the reader better at handling real-world volume.

- 1-5 star ratings.
- Viewed view with today's count.
- Overflow view for unviewed items outside the top-N.
- Starring.
- Full-text search over title and summary.

**Done when:** the app supports both quick scanning and intentional digging.

## Phase 4: Personal ranking (done)

This phase made the home screen adapt to the user instead of only the feed.

- Claude ranking mode.
- Interests prompt from config.
- Rating history fed back into ranking context.
- Runtime fallback to simpler ranking when needed.

**Done when:** the home view starts to reflect taste, not just recency.

## Phase 5: Onboarding and source management (done)

This phase removed the "one source at a time" pain.

- OPML import.
- Source listing.
- Source removal.
- OPML export.
- First-run config bootstrap.

**Done when:** a new user can move an existing subscription set into feedfold
without manual database edits or hand-written SQL.

## Phase 6: Persistent background updates (done)

This phase made the product feel more like a dependable daily tool.

- `feedfold daemon install`.
- `feedfold daemon status`, `start`, and `stop`.
- `launchd` plist generation on macOS.
- PID tracking and daemon status in the UI.
- Automatic daemon start when opening the TUI on macOS.

**Done when:** feedfold keeps itself warm in the background instead of relying
on the user to remember another terminal command.

## Phase 7: Deeper integrations (next)

These are the most valuable next steps because they improve adoption and make
the ranking model more flexible.

- [ ] 7.1 OAuth-based YouTube subscription import.
- [ ] 7.2 Source groups and saved filters.
- [ ] 7.3 Local-model ranking via Ollama.
- [ ] 7.4 Semantic search over summaries.

**Done when:** setup gets faster, filtering gets more intentional, and users
can choose between hosted AI and local ranking.

## Bucket list

Interesting ideas that fit the product, but are not yet on the critical path:

- Web-hosted read-only mirror of your current top-N.
- Mobile companion for starring and rating.
- Podcast adapter with enclosure playback support.
- Newsletter adapter for inbound email feeds.

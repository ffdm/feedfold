# Roadmap

Phases build on each other. Each phase should produce a working binary you
can actually use, not half-implemented scaffolding.

## Current state (2026-04-15)

Phases 0 through 4 are complete. The workspace currently ships:

- `feedfold` TUI binary with home / viewed / overflow views, read-unread
  state, star ratings, starring, FTS5 search, and kitty-protocol
  thumbnails (with a graceful text fallback).
- `feedfold add <url>` for single-feed import, plus `feedfold import
  <opml>` for bulk import from any OPML-exporting reader, plus
  `feedfold list` for inspecting what is tracked.
- `feedfoldd` background daemon polling all tracked sources on a
  configurable schedule, applying recency, popularity, or Claude ranking
  per source, with runtime fallback to recency if Claude is unavailable.
- YouTube feeds as a first-class adapter, enriched via the YouTube Data
  API v3 for popularity ranking.
- `ClaudeRanker` using interests from config and recent rating history
  as context.

The sections below still describe each historic phase so the roadmap
doubles as a changelog. Completed phases are kept for context.

## Phase 0: Foundations (done)

Prove the data path end-to-end with the smallest possible surface.

- Cargo workspace, three crates, minimal placeholders.
- Config loader: `config.toml` into a typed struct via serde.
- SQLite schema and storage layer with `rusqlite`.
- `feedfold add <url>` CLI: parses a feed via `feed-rs` and prints
  normalized entries to stdout.

**Done when:** you can point the CLI at any RSS or Atom URL and see parsed
entries on stdout.

## Phase 1: Daemon and TUI home view (done)

- Generic `RssAdapter` implementing `SourceAdapter`.
- Background daemon polling all sources on a schedule.
- Minimal ratatui TUI with a single "Home" view.
- `RecencyRanker` doing simple newest-first top-N selection.
- Read / unread state and basic navigation (`j/k/enter/q`).
- Hard-refresh key that refetches everything.

**Done when:** you can add a feed, let the daemon fetch it, and read the
top entries in a responsive terminal UI.

## Phase 2: YouTube and thumbnails (done)

- `YoutubeAdapter` that wraps `RssAdapter` and enriches with YouTube Data
  API v3 (batched `videos.list` calls).
- `PopularityRanker` using enrichment data.
- Kitty-protocol thumbnails via `viuer`, with text fallback for other
  terminals.
- Per-source ranking mode override in config.

**Done when:** YouTube subscriptions show up alongside blog posts, sorted
by popularity, with thumbnails on kitty.

## Phase 3: Ratings, overflow, and search (done)

- 1-5 star rating keybind.
- "Viewed" view with today's counter.
- "Overflow" view for unviewed entries that didn't make top-N.
- Starring.
- SQLite FTS5 search over title and summary.

**Done when:** the full three-view TUI (home / viewed / overflow) works
with ratings and search.

## Phase 4: AI ranking (done)

- `ClaudeRanker` using the Anthropic API.
- Interests prompt loaded from config.
- Rating history fed in as context.
- Runtime config switch between recency / popularity / claude.

**Done when:** switching `ranking.mode = "claude"` produces noticeably
better top-N picks that reflect rated history.

## Phase 5: Onboarding polish (in progress)

The binary is usable, but getting your existing subscriptions into it
should not require one shell invocation per feed. This phase smooths
the first-run experience.

- [x] 5.1 `feedfold import <opml>` bulk subscription import.
- [x] 5.2 `feedfold list` source inspector.
- [ ] 5.3 `feedfold remove <id|url>` to drop a tracked source.
- [ ] 5.4 `feedfold export` to write OPML back out for backup.
- [ ] 5.5 First-run bootstrap: if no config exists, write the example
  to `~/.config/feedfold/config.toml` and point the user at it.

**Done when:** a new user can go from zero to a working feed list in
one OPML import and then manage their sources without editing SQL or
the TOML file.

## Phase 6: Persistent daemon (planned)

Today you have to remember to run `feedfoldd` in a terminal. This phase
makes the daemon a real background service.

- [ ] 6.1 `feedfold daemon install` writing a `launchd` plist on macOS.
- [ ] 6.2 `feedfold daemon status` / `start` / `stop` wrappers.
- [ ] 6.3 Optional log rotation and a pid file so the TUI can show
  "daemon up since X".

**Done when:** the daemon survives reboots without manual intervention
and the TUI can see whether it is alive.

## Phase 7: Deeper integrations (planned)

- [ ] 7.1 OAuth-based YouTube subscription import: pull the signed-in
  user's channel list and generate source entries automatically.
- [ ] 7.2 Source groups and saved filters (for example "morning" vs
  "deep work" feed sets).
- [ ] 7.3 Local-model ranker (Ollama) as an alternative to Claude for
  fully offline ranking.
- [ ] 7.4 Semantic search over summaries, built on top of FTS5.

## Bucket list (deferred)

- Web-hosted read-only mirror of the current top-N.
- Mobile companion for starring and rating on the go.
- Podcast adapter with audio enclosure playback.
- Newsletter adapter (Mailgun / Postmark inbound).

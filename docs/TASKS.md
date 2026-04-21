# Tasks

This is the live implementation tracker for feedfold.

The README explains why someone would want the product. This file is narrower:
what has shipped, what is in flight, and what still needs to land for the next
step up in usefulness.

Legend: `[ ]` not started · `[~]` in progress · `[x]` done

Last updated: 2026-04-21

## What is already true

- feedfold is usable end to end right now.
- The core reading loop, source management, ranking, and macOS daemon flow are
  all shipped.
- The next meaningful work is about deeper integrations and better shaping of
  large subscription sets, not basic viability.

## Phase 0: Foundations

- [x] 0.1 Initial repo scaffolding, docs, and license
- [x] 0.2 Cargo workspace with three placeholder crates
- [x] 0.3 Config loader: TOML file into a typed `Config` struct via serde
- [x] 0.4 SQLite storage layer: schema, migrations, typed accessors
- [x] 0.5 `feedfold add <url>` CLI: fetch, parse via `feed-rs`, persist entries

## Phase 1: Daemon and TUI home view

- [x] 1.1 `SourceAdapter` trait in `feedfold-core`
- [x] 1.2 `RssAdapter` implementation
- [x] 1.3 Background daemon with scheduled polling via tokio
- [x] 1.4 `Ranker` trait and `RecencyRanker` implementation
- [x] 1.5 Minimal ratatui home view (`j/k/enter/q`) reading from storage
- [x] 1.6 Read / unread state wired through UI and storage
- [x] 1.7 Hard-refresh keybind

## Phase 2: YouTube and thumbnails

- [x] 2.1 `YoutubeAdapter` wrapping `RssAdapter`
- [x] 2.2 YouTube Data API v3 enrichment
- [x] 2.3 `PopularityRanker` using enrichments
- [x] 2.4 Kitty-protocol thumbnails via `viuer` with text fallback
- [x] 2.5 Per-source ranking mode override in config

## Phase 3: Ratings, overflow, and search

- [x] 3.1 1-5 star rating keybind and storage
- [x] 3.2 Viewed view with today's counter
- [x] 3.3 Overflow view for unviewed non-top-N entries
- [x] 3.4 Starring
- [x] 3.5 SQLite FTS5 search over title and summary

## Phase 4: AI ranking

- [x] 4.1 `ClaudeRanker` calling the Anthropic API
- [x] 4.2 Interests prompt loaded from config
- [x] 4.3 Rating history fed as context
- [x] 4.4 Runtime switch between ranker implementations

## Phase 5: Onboarding polish

- [x] 5.1 `feedfold import <opml>` bulk subscription import
- [x] 5.2 `feedfold list` source inspector
- [x] 5.3 `feedfold remove <id|url>` source removal
- [x] 5.4 `feedfold export` OPML export for backup
- [x] 5.5 First-run config bootstrap

## Phase 6: Persistent daemon

- [x] 6.1 `feedfold daemon install` writing a `launchd` plist
- [x] 6.2 `feedfold daemon status/start/stop` wrappers
- [x] 6.3 PID file and log rotation
- [x] 6.4 Auto-start persistent daemon when opening the TUI on macOS

## Phase 7: Deeper integrations

- [ ] 7.1 OAuth-based YouTube subscription import
- [ ] 7.2 Source groups and saved filters
- [ ] 7.3 Local-model (Ollama) ranker
- [ ] 7.4 Semantic search

## Near-term focus

If you are choosing what to build next, the highest-leverage items are:

- 7.1, because it removes one of the biggest onboarding hurdles for
  YouTube-heavy users
- 7.2, because larger source sets become much more useful once the user can
  shape them intentionally
- 7.3, because local ranking broadens the product for users who do not want a
  hosted AI dependency

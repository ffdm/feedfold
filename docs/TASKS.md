# Tasks

Live tracker of what's done, in progress, and next. When you start a task,
change `[ ]` to `[~]` and commit. When you finish, change it to `[x]` and
commit again.

Legend: `[ ]` not started · `[~]` in progress · `[x]` done

Last updated: 2026-04-17

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
- [x] 2.2 YouTube Data API v3 enrichment (batched `videos.list`)
- [x] 2.3 `PopularityRanker` using enrichments
- [x] 2.4 Kitty-protocol thumbnails via `viuer` with text fallback
- [x] 2.5 Per-source ranking mode override in config

## Phase 3: Ratings, overflow, and search

- [x] 3.1 1–5 star rating keybind and storage
- [x] 3.2 "Viewed" view with today's counter
- [x] 3.3 "Overflow" view for unviewed non-top-N entries
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
- [ ] 5.4 `feedfold export` OPML export for backup
- [ ] 5.5 First-run config bootstrap

## Phase 6: Persistent daemon

- [ ] 6.1 `feedfold daemon install` writing a `launchd` plist
- [ ] 6.2 `feedfold daemon status/start/stop` wrappers
- [ ] 6.3 Pid file and log rotation

## Phase 7: Deeper integrations

- [ ] 7.1 OAuth-based YouTube subscription import
- [ ] 7.2 Source groups and saved filters
- [ ] 7.3 Local-model (Ollama) ranker
- [ ] 7.4 Semantic search

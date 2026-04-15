# Tasks

Live tracker of what's done, in progress, and next. When you start a task,
change `[ ]` to `[~]` and commit. When you finish, change it to `[x]` and
commit again.

Legend: `[ ]` not started · `[~]` in progress · `[x]` done

Last updated: 2026-04-14

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
- [~] 1.7 Hard-refresh keybind

## Phase 2: YouTube and thumbnails

- [ ] 2.1 `YoutubeAdapter` wrapping `RssAdapter`
- [ ] 2.2 YouTube Data API v3 enrichment (batched `videos.list`)
- [ ] 2.3 `PopularityRanker` using enrichments
- [ ] 2.4 Kitty-protocol thumbnails via `viuer` with text fallback
- [ ] 2.5 Per-source ranking mode override in config

## Phase 3: Ratings, overflow, and search

- [ ] 3.1 1–5 star rating keybind and storage
- [ ] 3.2 "Viewed" view with today's counter
- [ ] 3.3 "Overflow" view for unviewed non-top-N entries
- [ ] 3.4 Starring
- [ ] 3.5 SQLite FTS5 search over title and summary

## Phase 4: AI ranking

- [ ] 4.1 `ClaudeRanker` calling the Anthropic API
- [ ] 4.2 Interests prompt loaded from config
- [ ] 4.3 Rating history fed as context
- [ ] 4.4 Runtime switch between ranker implementations

## Bucket list

- [ ] `feedfold daemon install` writing a `launchd` plist
- [ ] OAuth-based YouTube subscription import
- [ ] OPML import / export
- [ ] Source groups and saved filters
- [ ] Local-model (Ollama) ranker
- [ ] Semantic search

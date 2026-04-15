# Roadmap

Phases build on each other. Each phase should produce a working binary you
can actually use, not half-implemented scaffolding.

## Phase 0 — Foundations

Prove the data path end-to-end with the smallest possible surface.

- Cargo workspace, three crates, minimal placeholders.
- Config loader: `config.toml` → typed struct via serde.
- SQLite schema and storage layer with `rusqlite`.
- `feedfold add <url>` CLI: parses a feed via `feed-rs` and prints normalized
  entries to stdout.

**Done when:** you can point the CLI at any RSS or Atom URL and see parsed
entries on stdout.

## Phase 1 — Daemon and TUI home view

- Generic `RssAdapter` implementing `SourceAdapter`.
- Background daemon polling all sources on a schedule.
- Minimal ratatui TUI with a single "Home" view.
- `RecencyRanker` doing simple newest-first top-N selection.
- Read / unread state and basic navigation (`j/k/enter/q`).
- Hard-refresh key that refetches everything.

**Done when:** you can add a feed, let the daemon fetch it, and read the top
entries in a responsive terminal UI.

## Phase 2 — YouTube and thumbnails

- `YoutubeAdapter` that wraps `RssAdapter` and enriches with YouTube Data
  API v3 (batched `videos.list` calls).
- `PopularityRanker` using enrichment data.
- Kitty-protocol thumbnails via `viuer`, with text fallback for other
  terminals.
- Per-source ranking mode override in config.

**Done when:** YouTube subscriptions show up alongside blog posts, sorted by
popularity, with thumbnails on kitty.

## Phase 3 — Ratings, overflow, and search

- 1–5 star rating keybind.
- "Viewed" view with today's counter.
- "Overflow" view for unviewed entries that didn't make top-N.
- Starring.
- SQLite FTS5 search over title and summary.

**Done when:** the full three-view TUI (home / viewed / overflow) works with
ratings and search.

## Phase 4 — AI ranking

- `ClaudeRanker` using the Anthropic API.
- Interests prompt loaded from config.
- Rating history fed in as context.
- Runtime config switch between recency / popularity / claude.

**Done when:** switching `ranking.mode = "claude"` produces noticeably better
top-N picks that reflect rated history.

## Bucket list (deferred)

- `feedfold daemon install` — writes a `launchd` plist for persistent
  running.
- OAuth-based YouTube subscription import.
- OPML import and export.
- Source groups and saved filters.
- Local-model ranker (Ollama) as an alternative to Claude.
- Semantic search.

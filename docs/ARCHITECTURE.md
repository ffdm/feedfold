# Architecture

This document captures the design decisions behind feedfold. Read it before
making structural changes. These decisions were made deliberately and should
only be revisited with reason.

## Core principle: RSS is the universe, YouTube is one adapter

feedfold is a generic RSS/Atom reader that happens to ship a YouTube adapter.
It is **not** a YouTube client that happens to read other feeds. This framing
governs every design choice:

- The core `Entry` type contains only fields that any feed can provide:
  title, summary, URL, publish date, thumbnail URL, author.
- YouTube-specific data (view count, duration, channel handle) lives in an
  `enrichments` table keyed by entry ID, not on the `Entry` struct itself.
- The `SourceAdapter` trait is the extension point. Adding a new source type
  (Mastodon, podcast RSS with iTunes extensions, Substack) is a new adapter
  implementation, not a core change.

If you find yourself adding a YouTube-specific field to a core type, stop.
Put it in `enrichments` or a per-adapter extension instead.

## Crate layout

```
feedfold/
└── crates/
    ├── feedfold-core/      # lib: data model, storage, config, ranker trait
    ├── feedfold-adapters/  # lib: concrete adapters (RSS, YouTube, Claude)
    ├── feedfold-daemon/    # bin: background fetcher (feedfoldd)
    └── feedfold-tui/       # bin: ratatui reader + CLI (feedfold)
```

Why the split: the TUI and daemon both depend on `core` and `adapters` but
not each other. You can run the daemon headless on a server, or replace the
TUI with a web interface, without touching the fetching or storage logic.
`core` never depends on any binary or on `adapters`, so its public API stays
forced into something reusable.

`feedfold-adapters` holds all IO-bound implementations: RSS fetching, YouTube
Data API enrichment, and the Claude ranking client. The `core` crate stays
IO-free (except SQLite storage and config file reads).

## Data flow

```
Daemon tick
  ├─ for each Source in storage:
  │    Adapter::fetch() → FetchedFeed
  │    (YouTube adapter enriches with Data API)
  ├─ Storage::upsert_entries()
  ├─ Ranker::rank() per source
  └─ Storage::apply_ranking() marks top-N

TUI: reads exclusively from SQLite. Never fetches feeds or calls APIs.
CLI: feedfold add/import writes sources + entries to SQLite via adapters.
```

The TUI is a pure consumer of the database. It does not fetch feeds or call
APIs. This keeps keystrokes responsive even when the network is slow, and
enforces a clean separation between "getting data" and "showing data".

The CLI subcommands (`add`, `import`, `list`) are the only user-facing
write path. `feedfold add <url>` fetches a single feed and persists it.
`feedfold import <opml>` parses an OPML file and adds each feed in batch.

## The `SourceAdapter` trait

```rust
pub trait SourceAdapter: Send + Sync {
    fn kind(&self) -> AdapterType;
    fn fetch(
        &self,
        url: &str,
    ) -> impl Future<Output = Result<FetchedFeed, AdapterError>> + Send;
}
```

Uses native async fn in traits (stabilized in Rust 1.75), not the
`async-trait` proc macro.

Implementations:

- `RssAdapter`: parses RSS 2.0, RSS 1.0, Atom, and JSON Feed via `feed-rs`.
- `YoutubeAdapter`: wraps `RssAdapter` for the channel RSS feed, then
  batches `videos.list` calls to YouTube Data API v3 to enrich with view
  counts, duration, and thumbnails.

## The `Ranker` trait

```rust
pub trait Ranker {
    fn rank(&self, entries: &[Entry], ctx: &RankContext) -> Vec<Score>;
}
```

Implementations:

- `RecencyRanker` (Phase 1): pure newest-first.
- `PopularityRanker` (Phase 2): uses enrichments such as YouTube views.
- `ClaudeRanker` (Phase 4): calls the Anthropic API with titles, summaries,
  the user's interests prompt, and recent 1-5 star ratings. This one is
  async and lives outside the trait (it uses a standalone `rank` method)
  because the sync `Ranker` trait cannot await.

The ranker is selected at runtime via config. The daemon reads the effective
mode per source and falls back to recency if Claude is unavailable.

## Storage

SQLite via `rusqlite` with the `bundled` feature, so there is no system
dependency. The database path is resolved through the `directories` crate:

- macOS: `~/Library/Application Support/feedfold/feedfold.db`
- Linux: `~/.local/share/feedfold/feedfold.db`

Schema:

```sql
sources       (id, name, url, adapter_type, top_n_override, created_at)
entries       (id, source_id, external_id, title, summary, url,
               thumbnail_url, author, published_at, fetched_at,
               state, rating, score, displayed_in_top_n)
enrichments   (entry_id, key, value)         -- per-adapter extras
daily_views   (date, entry_id, viewed_at)    -- drives today's counter
entries_fts   (FTS5 virtual table over title + summary)
```

`state` is `New | Viewed | Starred`. `rating` is `NULL` or `1..=5`.

FTS5 is kept in sync via insert/update/delete triggers on the `entries`
table. The FTS index does not rebuild on every open; use
`Storage::rebuild_search_index()` if manual reindexing is needed.

## Config and source management

Sources live in the SQLite `sources` table. The config file
(`~/.config/feedfold/config.toml` on Linux, `~/Library/Application
Support/feedfold/config.toml` on macOS) controls global settings (poll
interval, default top_n, ranking mode, AI interests) and can specify
per-source ranking overrides by URL.

The daemon matches config `[[sources]]` entries to database sources by URL
to apply per-source ranking overrides. This means:
- A source can exist in the database (added via CLI) without being in config.
- A config `[[sources]]` entry only has effect if the source URL is also
  tracked in the database.

## Tech stack (with reasoning)

| Need | Crate | Why this over alternatives |
|---|---|---|
| TUI framework | `ratatui` | Most mature Rust TUI library, active maintenance |
| Async runtime | `tokio` | Industry standard; required by `reqwest` |
| HTTP | `reqwest` | Ergonomic default; rustls for pure-Rust TLS |
| Feed parsing | `feed-rs` | Handles RSS 2.0, RSS 1.0, Atom, JSON Feed uniformly |
| SQLite | `rusqlite` (bundled) | No system dependency, single static binary |
| Config | `serde` + `toml` | Standard pairing |
| Paths | `directories` | Cross-platform XDG |
| Thumbnails | `viuer` | Supports the kitty graphics protocol and iTerm |
| Errors | `anyhow` (binaries) + `thiserror` (libraries) | Idiomatic split |
| Logging | `tracing` | Async-aware, structured |

## Performance targets

- Cold start under 1 second.
- Keystroke latency under 50ms.
- Scheduled fetches happen in the daemon, never on the UI thread.
- Single static binary per executable (daemon and TUI), no runtime deps.

## Deferred decisions

These are intentionally not built yet. Revisit only when there is a concrete
reason:

- OAuth for pulling a YouTube account's subscription list.
- Source groups and saved filters.
- Local-model support for ranking (Ollama).
- Multi-profile / multi-user.
- Sync across devices.

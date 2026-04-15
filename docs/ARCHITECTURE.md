# Architecture

This document captures the design decisions behind feedfold. Read it before
making structural changes — these decisions were made deliberately and should
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
    ├── feedfold-core/      # lib: data model, storage, adapters, ranker trait
    ├── feedfold-daemon/    # bin: background fetcher
    └── feedfold-tui/       # bin: ratatui reader
```

Why the split: the TUI and daemon both depend on `core` but not each other.
You can run the daemon headless on a server, or replace the TUI with a web
interface, without touching the fetching or storage logic. And `core` never
depends on either binary, so its public API stays forced into something
reusable.

## Data flow

```
Daemon tick
  ├─ for each Source:
  │    Adapter::fetch() → Vec<Entry>
  │    (YouTube adapter enriches with Data API)
  ├─ Storage::upsert_entries()
  ├─ Ranker::rank() per source
  └─ mark top-N entries as displayed_in_top_n = true

TUI: reads exclusively from SQLite. Never fetches feeds or calls APIs.
```

The TUI is a pure consumer of the database. It does not fetch feeds or call
APIs. This keeps keystrokes responsive even when the network is slow, and
enforces a clean separation between "getting data" and "showing data".

## The `SourceAdapter` trait

```rust
#[async_trait]
trait SourceAdapter {
    async fn fetch(&self, source: &Source) -> Result<Vec<Entry>>;
}
```

Implementations:

- `RssAdapter` — parses RSS 2.0, RSS 1.0, Atom, and JSON Feed via `feed-rs`.
- `YoutubeAdapter` — wraps `RssAdapter` for the channel RSS feed, then
  batches `videos.list` calls to YouTube Data API v3 to enrich with view
  counts and duration.

## The `Ranker` trait

```rust
trait Ranker {
    fn rank(&self, entries: &[Entry], ctx: &RankContext) -> Vec<Score>;
}
```

Implementations:

- `RecencyRanker` (Phase 1) — pure newest-first.
- `PopularityRanker` (Phase 2) — uses enrichments such as YouTube views.
- `ClaudeRanker` (Phase 4) — calls the Anthropic API with titles, summaries,
  the user's interests prompt, and recent 1–5 star ratings.

The ranker is swappable at runtime via config. The TUI and daemon never know
which implementation is active — they see a ranked list.

## Storage

SQLite via `rusqlite` with the `bundled` feature, so there is no system
dependency. The database lives at `~/.local/share/feedfold/feedfold.db`
(resolved through the `directories` crate).

Schema:

```sql
sources       (id, name, url, adapter_type, top_n_override, created_at)
entries       (id, source_id, external_id, title, summary, url,
               thumbnail_url, author, published_at, fetched_at,
               state, rating, score, displayed_in_top_n)
enrichments   (entry_id, key, value)         -- per-adapter extras
daily_views   (date, entry_id, viewed_at)    -- drives today's counter
```

`state` is `New | Viewed | Starred`. `rating` is `NULL` or `1..=5`.

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
| Thumbnails | `viuer` | Supports the kitty graphics protocol |
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

- OAuth for pulling a YouTube account's subscription list (Phase 5+).
- OPML import / export.
- Source groups and saved filters.
- Local-model support for ranking (Ollama).
- Multi-profile / multi-user.
- Sync across devices.

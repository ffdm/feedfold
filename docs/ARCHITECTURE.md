# Architecture

This document explains why feedfold is shaped the way it is.

If the README sells the experience, this file explains the mechanics that make
that experience possible: a calm home screen, fast terminal interaction, and a
reader that can mix blogs and YouTube without turning into a special-case mess.

## Product principles first

Before the crate graph or schema details, the important thing to understand is
what feedfold is trying to preserve.

### 1. The reader should feel calm

The product is built around per-source top-N selection. That one decision
drives a lot of the rest:

- prolific sources cannot dominate the home screen;
- the user can follow more sources than they can read in full;
- the product can optimize for "best next thing to open" instead of "largest
  unread pile."

If a proposed change makes the stream noisier, it is probably fighting the
point of the app.

### 2. Reading and fetching are different jobs

The UI should stay responsive even if the network is slow or broken.

That means:

- feed fetching happens in adapters and the daemon;
- ranking happens before the user sees a list;
- the TUI reads from SQLite and renders local state;
- keypress latency should never depend on a network round-trip.

### 3. RSS is the center of gravity

feedfold is a general feed reader that happens to have a strong YouTube
adapter. It is not a YouTube client with RSS bolted on later.

This principle keeps the core model honest:

- the core `Entry` type only stores fields that make sense for any feed;
- source-specific extras live in `enrichments`;
- new source types should arrive as adapters, not as one-off branches through
  the core.

## Crate layout

```text
feedfold/
└── crates/
    ├── feedfold-core/      # data model, storage, config, rankers
    ├── feedfold-adapters/  # RSS, YouTube, Claude integration
    ├── feedfold-daemon/    # background polling runtime
    └── feedfold-tui/       # CLI + ratatui frontend
```

The split exists to keep responsibilities clean:

- `feedfold-core` defines the reusable center of the system.
- `feedfold-adapters` owns network-bound integration points.
- `feedfold-daemon` polls sources and applies ranking in the background.
- `feedfold-tui` is the user-facing binary and terminal interface.

The TUI depends on the rest of the workspace, but the core does not depend on
the TUI. That keeps the internals reusable and makes it possible to swap or add
frontends later without rewriting storage or fetching logic.

## Data flow

```text
Daemon tick
  ├─ load sources from SQLite
  ├─ adapter.fetch(url) for each source
  ├─ upsert normalized entries
  ├─ enrich entries when the adapter can provide more metadata
  ├─ rank new/starred candidates per source
  └─ mark the top-N visible on Home

TUI session
  ├─ load current view from SQLite
  ├─ load enrichments for visible entries
  ├─ render Home / Channels / Viewed / Overflow
  └─ write back local state changes such as viewed, ignored, starred, rating
```

The key design choice is that the TUI is a consumer of already-prepared data.
It does not fetch feeds directly. That separation makes the UI predictable and
lets the daemon do the heavier work once per polling cycle instead of once per
keypress.

## CLI responsibilities

The `feedfold` binary is both the frontend and the command-line entry point.

Current user-facing commands:

- `feedfold` launches the TUI.
- `feedfold add <url>` fetches and stores a single source.
- `feedfold import <opml>` bulk-imports subscriptions.
- `feedfold export` writes tracked sources as OPML.
- `feedfold list` shows what is tracked.
- `feedfold remove <id|url>` deletes a tracked source.
- `feedfold daemon ...` manages the macOS `launchd` service.

On macOS, opening the TUI also attempts to install or start the persistent
daemon automatically so the app stays warm in the background.

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

Why this trait exists:

- every source should normalize into the same broad `FetchedFeed` shape;
- the daemon should not care whether a source is a blog, a channel, or
  something added later;
- adapter-specific work such as YouTube enrichment should stay out of the core
  storage model.

Current implementations:

- `RssAdapter`: parses RSS, Atom, RSS 1.0, and JSON Feed via `feed-rs`.
- `YoutubeAdapter`: wraps the RSS path, then calls the YouTube Data API for
  richer metadata.

## Ranking model

The ranker decides which entries deserve the limited top-N surface for a
source.

```rust
pub trait Ranker {
    fn rank(&self, entries: &[Entry], ctx: &RankContext) -> Vec<Score>;
}
```

Implemented modes:

- `RecencyRanker`: newest-first, deterministic baseline.
- `PopularityRanker`: uses enrichment data such as YouTube view counts.
- `ClaudeRanker`: uses interests plus recent rating history for more personal
  ranking.

Rank selection is runtime-configurable. The daemon resolves the effective mode
per source and falls back safely when a richer mode is unavailable.

## Storage

SQLite is the system of record, accessed through `rusqlite` with the `bundled`
feature so the project does not rely on a system SQLite install.

Default paths:

- macOS: `~/Library/Application Support/feedfold/feedfold.db`
- Linux: `~/.local/share/feedfold/feedfold.db`

Schema summary:

```sql
sources       (id, name, url, adapter_type, top_n_override, created_at)
entries       (id, source_id, external_id, title, summary, url,
               thumbnail_url, author, published_at, fetched_at,
               state, rating, score, displayed_in_top_n)
enrichments   (entry_id, key, value)
daily_views   (date, entry_id, viewed_at)
entries_fts   (FTS5 virtual table over title + summary)
```

Important notes:

- `state` is one of `New | Viewed | Ignored | Starred`.
- `rating` is `NULL` or `1..=5`.
- FTS5 stays in sync through triggers on the `entries` table.
- adapter-specific metadata belongs in `enrichments`, not on the core entry
  model.

## Config and source management

Configuration and tracked sources are intentionally separate.

- The config file controls defaults such as poll interval, ranking mode,
  interests, and optional per-source overrides.
- The database stores the actual tracked source list and entry history.

Paths:

- macOS: `~/Library/Application Support/feedfold/config.toml`
- Linux: `~/.config/feedfold/config.toml`

This split has two benefits:

- the database is the operational truth for what the user follows;
- the config remains a stable place for preferences and tuning.

Per-source config matches sources by URL, so a config override only has an
effect when that source is also present in the database.

## Why YouTube metadata lives in `enrichments`

It can be tempting to keep adding fields to `Entry` as richer adapters appear.
That would slowly turn the core model into a pile of source-specific baggage.

The current design avoids that:

- common fields stay on `Entry`;
- optional or source-specific facts stay in `enrichments`;
- ranking and rendering can opt into those fields when available.

If you are adding an adapter and your first instinct is "I should add three new
columns to the core entry type," stop and look at `enrichments` first.

## Tech stack

| Need | Crate | Why |
|---|---|---|
| TUI | `ratatui` | Mature Rust terminal UI stack |
| Async runtime | `tokio` | Standard runtime for background IO |
| HTTP | `reqwest` | Good default ergonomics and TLS support |
| Feed parsing | `feed-rs` | Uniform parsing across feed formats |
| SQLite | `rusqlite` | Simple local database with strong Rust bindings |
| Config | `serde` + `toml` | Straightforward typed configuration |
| Paths | `directories` | Cross-platform config/data paths |
| Thumbnails | `viuer` | Terminal image support where available |
| Errors | `anyhow` + `thiserror` | Idiomatic app/library split |
| Logging | `tracing` | Structured diagnostics |

## Performance targets

- Cold start under 1 second.
- Keystroke latency under 50 ms.
- No feed fetching on the UI thread.
- A single local database as the source of truth.

These are not vanity goals. They are part of the product feel.

## Deferred decisions

Not every plausible feature belongs in the core yet. These stay deferred until
there is a strong reason to pay their complexity cost:

- OAuth import for YouTube subscriptions.
- Source groups and saved filters.
- Local-model ranking.
- Multi-profile support.
- Cross-device sync.

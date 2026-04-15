# feedfold

A fast, modular terminal RSS reader that caps each source at a configurable
top-N so you see the few entries that matter, not the firehose.

> Status: alpha, usable end-to-end. Phases 0 through 4 of the
> [roadmap](docs/ROADMAP.md) are complete. See
> [docs/TASKS.md](docs/TASKS.md) for what's next.

## What it is

feedfold pulls RSS and Atom feeds on a schedule and surfaces only the top N
entries from each source per fetch cycle. YouTube subscriptions work alongside
blog, podcast, and newsletter feeds. YouTube is a first-class source type,
not a special case. A background daemon handles fetching; a minimal
[ratatui](https://ratatui.rs/) TUI handles reading.

The top-N selection starts as simple recency or popularity scoring, and
eventually becomes AI-assisted via the Claude API, learning from 1–5 star
ratings and a user-written interests prompt.

## Why

Subscription feeds get overwhelming. Rather than "mark all as read" guilt,
feedfold decides in advance what's worth your time per source so the stream
stays calm even when you haven't opened it in a week.

## Quick start

Prereqs: a recent stable Rust toolchain (`rustup`). macOS or Linux.

```sh
# 1. Build both binaries (release).
cargo build --release

# 2. Drop the example config where feedfold looks for it, then edit
#    your interests / default top_n. Sources can live here too, but
#    the CLI is easier.
#      macOS:  ~/Library/Application Support/feedfold/config.toml
#      Linux:  ~/.config/feedfold/config.toml
mkdir -p "$HOME/Library/Application Support/feedfold"
cp config.example.toml "$HOME/Library/Application Support/feedfold/config.toml"

# 3. Add your subscribers.
#    Option A: bulk import from an OPML file exported by another reader.
./target/release/feedfold import ~/Downloads/subscriptions.opml

#    Option B: add feeds one at a time.
./target/release/feedfold add https://simonwillison.net/atom/everything/
./target/release/feedfold add "https://www.youtube.com/feeds/videos.xml?channel_id=UCsBjURrPoezykLs9EqgamOA"

# 4. Sanity-check what's tracked.
./target/release/feedfold list

# 5. Start the background daemon in another terminal. It polls on the
#    schedule from config.toml and ranks each source.
./target/release/feedfoldd

# 6. Open the reader. Use j/k to move, Enter to open in a browser,
#    1-5 to rate, s to star, h/v/o to switch views, / to search, q to quit.
./target/release/feedfold
```

Optional environment:

- `ANTHROPIC_API_KEY` enables `ranking.mode = "claude"` in the daemon.
- `YOUTUBE_API_KEY` enables popularity enrichment for YouTube sources.

## File index

Start here when you're new to the codebase:

| Where | What's there |
|---|---|
| [README.md](README.md) | This file: overview and file index |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Design decisions, data flow, crate layout, tech stack rationale |
| [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) | Ground rules for clean code, commits, and modular interfaces |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Phased plan from MVP through AI ranking to bucket-list features |
| [docs/TASKS.md](docs/TASKS.md) | Live task tracker: what's done, in progress, and next |
| [config.example.toml](config.example.toml) | Annotated example of the user config |
| [Cargo.toml](Cargo.toml) | Workspace manifest and shared dependencies |
| [crates/feedfold-core](crates/feedfold-core) | Shared library: data model, storage, source adapters, ranker trait |
| [crates/feedfold-daemon](crates/feedfold-daemon) | Background fetcher binary. Polls feeds on a schedule |
| [crates/feedfold-tui](crates/feedfold-tui) | ratatui reader binary. The interface you actually use |

## Contributing

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md). Core principles: clean code,
small modular interfaces, no premature abstraction, and treat YouTube as one
adapter among many, never a special case.

## License

MIT. See [LICENSE](LICENSE).

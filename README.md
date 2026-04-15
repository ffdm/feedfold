# feedfold

A fast, modular terminal RSS reader that caps each source at a configurable
top-N so you see the few entries that matter — not the firehose.

> Status: early alpha. Under active development. See [docs/TASKS.md](docs/TASKS.md) for what works today.

## What it is

feedfold pulls RSS and Atom feeds on a schedule and surfaces only the top N
entries from each source per fetch cycle. YouTube subscriptions work alongside
blog, podcast, and newsletter feeds — YouTube is a first-class source type,
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

Not yet runnable — see [docs/TASKS.md](docs/TASKS.md) for current progress and
[docs/ROADMAP.md](docs/ROADMAP.md) for what's coming.

## File index

Start here when you're new to the codebase:

| Where | What's there |
|---|---|
| [README.md](README.md) | This file — overview and file index |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Design decisions, data flow, crate layout, tech stack rationale |
| [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) | Ground rules for clean code, commits, and modular interfaces |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Phased plan from MVP through AI ranking to bucket-list features |
| [docs/TASKS.md](docs/TASKS.md) | Live task tracker — what's done, in progress, and next |
| [config.example.toml](config.example.toml) | Annotated example of the user config |
| [Cargo.toml](Cargo.toml) | Workspace manifest and shared dependencies |
| [crates/feedfold-core](crates/feedfold-core) | Shared library: data model, storage, source adapters, ranker trait |
| [crates/feedfold-daemon](crates/feedfold-daemon) | Background fetcher binary — polls feeds on a schedule |
| [crates/feedfold-tui](crates/feedfold-tui) | ratatui reader binary — the interface you actually use |

## Contributing

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md). Core principles: clean code,
small modular interfaces, no premature abstraction, and treat YouTube as one
adapter among many — never a special case.

## License

MIT. See [LICENSE](LICENSE).

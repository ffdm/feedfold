# feedfold

A calm terminal reader for RSS, Atom, and YouTube feeds.

Each source is capped at a top-N so the stream stays small — the handful of
posts worth your time, not the firehose.

> Alpha, but usable end-to-end.

## Why

Subscription feeds get loud. Instead of "mark all as read" guilt, feedfold
decides up front what matters per source, so leaving it alone for a week is
fine.

## Install

Needs a recent stable Rust toolchain.

```sh
cargo build --release
```

Two binaries drop into `target/release/`: `feedfoldd` (background fetcher)
and `feedfold` (the reader).

## Set up

Drop the example config in place, then open it and set your interests and
top-N.

```sh
# macOS
mkdir -p "$HOME/Library/Application Support/feedfold"
cp config.example.toml "$HOME/Library/Application Support/feedfold/config.toml"

# Linux
mkdir -p "$HOME/.config/feedfold"
cp config.example.toml "$HOME/.config/feedfold/config.toml"
```

Add some feeds — bulk via OPML, or one at a time:

```sh
feedfold import ~/Downloads/subscriptions.opml
feedfold add https://simonwillison.net/atom/everything/
feedfold add "https://www.youtube.com/feeds/videos.xml?channel_id=UCsBjURrPoezykLs9EqgamOA"
feedfold list
```

Start the daemon in a spare terminal, then open the reader:

```sh
feedfoldd   # polls on the schedule in config.toml
feedfold    # the TUI
```

## Keys

| Key | Action |
|---|---|
| `j` `k` | Move |
| `Ctrl+d` / `Ctrl+u` | Half-page down / up |
| `gg` / `G` | Jump to top / bottom |
| `Tab` / `Shift+Tab` | Cycle views forward / back |
| `Enter` | Open in browser · fold/unfold channel |
| `v` | Mark viewed (no browser) |
| `i` | Ignore (hide without marking read) |
| `s` | Star |
| `1`–`5` | Rate |
| `/` | Search |
| `n` | Set top-N |
| `r` | Reload |
| `S` | Settings (also: view ignored) |
| `q` | Quit |

## Optional

- `ANTHROPIC_API_KEY` — turns on `ranking.mode = "claude"`.
- `YOUTUBE_API_KEY` (or `[youtube] api_key` in config) — enables popularity
  ranking and shorts/live/premiere filtering.

## Poking around

| Where | What's there |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Design, data flow, crate layout |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Where this is headed |
| [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) | House rules |
| [config.example.toml](config.example.toml) | Annotated config |
| `crates/feedfold-core` | Storage, adapters, ranking |
| `crates/feedfold-daemon` | The fetcher |
| `crates/feedfold-tui` | The reader |

## License

MIT. See [LICENSE](LICENSE).

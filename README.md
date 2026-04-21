# feedfold

feedfold is a terminal feed reader for people who like subscriptions, but do
not want to be buried by them.

It reads RSS, Atom, and YouTube feeds, keeps only the top handful from each
source on your home screen, and lets the rest stay out of your way until you
actually want them.

> Calm by default. Fast to scan. Built for coming back after a few days and
> still feeling in control.

## Why feedfold feels different

Most feed readers treat every source like it deserves equal screen time. That
works right up until you subscribe to too much. Then the backlog turns into a
chore, and "mark all as read" becomes the main workflow.

feedfold takes a different approach:

- Every source is capped at a top-N, so a prolific channel cannot flood the
  rest of your reading.
- Home shows the best candidates first, not a wall of guilt.
- Blogs and YouTube subscriptions live in one place.
- Ratings, starring, search, and optional AI ranking make the list improve
  over time instead of getting noisier.

If you want your subscriptions to feel like a curated reading desk instead of
an inbox, this is the product.

## What you get

- A focused home view with the top picks from each source.
- A viewed view for things you already opened, with a "today" counter.
- An overflow view for the unviewed items that did not make the top cut.
- Full-text search over titles and summaries.
- YouTube thumbnails in supported terminals, with graceful fallback text
  elsewhere.
- Background polling on macOS via `launchd`, started automatically when you
  open the TUI.
- Optional popularity ranking for YouTube and optional Claude-based ranking for
  more personalized picks.

## Install

feedfold currently expects a recent stable Rust toolchain.

```sh
cargo build --release
```

The binary will be at `target/release/feedfold`.

If you just want to try it locally while iterating:

```sh
cargo run --release -- --help
```

## Quick start

### 1. Run it once

The first run bootstraps a config file automatically if one does not exist yet.

- macOS: `~/Library/Application Support/feedfold/config.toml`
- Linux: `~/.config/feedfold/config.toml`

You can launch the TUI immediately:

```sh
feedfold
```

Or inspect the CLI first:

```sh
feedfold --help
```

### 2. Bring in your subscriptions

If you already use another reader, OPML import is the fast path:

```sh
feedfold import ~/Downloads/subscriptions.opml
```

You can also add sources one at a time:

```sh
feedfold add https://simonwillison.net/atom/everything/
feedfold add "https://www.youtube.com/feeds/videos.xml?channel_id=UCsBjURrPoezykLs9EqgamOA"
feedfold list
```

### 3. Open the reader

```sh
feedfold
```

On macOS, opening the TUI also installs or starts the background `launchd`
agent automatically, so your feeds keep updating between sessions.

On Linux, persistent daemon management is not wired up yet, so feedfold is best
thought of as a local reader first.

## Daily use

The intended rhythm is simple:

1. Open `feedfold`.
2. Scan Home for the best few items from each source.
3. Press `Enter` to open something worth reading.
4. Use Viewed if you want a light history, Overflow if you want to go deeper,
   and Search when you remember a topic but not the source.

The product works especially well if you follow more sources than you can read
in full. That is the entire point.

## Keyboard cheat sheet

| Key | Action |
|---|---|
| `j` `k` | Move |
| `Ctrl+d` / `Ctrl+u` | Half-page down / up |
| `gg` / `G` | Jump to top / bottom |
| `Tab` / `Shift+Tab` | Cycle views forward / back |
| `Enter` | Open in browser or fold/unfold a channel header |
| `v` | Mark viewed without opening |
| `i` | Ignore |
| `s` | Star |
| `1`-`5` | Rate |
| `/` | Search |
| `n` | Set top-N |
| `r` | Re-rank and reload the current view |
| `S` | Open settings |
| `q` | Quit |

## Ranking modes

feedfold supports three ways to decide what belongs in the top-N for each
source:

- `recency`: newest first, simple and predictable.
- `popularity`: great for YouTube when you want views and metadata to matter.
- `claude`: uses your interests and rating history to produce more personal
  picks.

The ranking mode is configured in `config.toml`, globally or per source.

## Optional API keys

- `ANTHROPIC_API_KEY`: enables `ranking.mode = "claude"`.
- `YOUTUBE_API_KEY`: enables richer YouTube metadata and popularity ranking.

You can also place the YouTube key directly in `[youtube] api_key` inside the
config file.

## Useful commands

```sh
feedfold import subscriptions.opml
feedfold add https://example.com/feed.xml
feedfold list
feedfold remove <id-or-url>
feedfold export > backup.opml
feedfold daemon status
```

## Platform notes

- macOS: automatic persistent background updates via `launchd`.
- Linux: feed reading works, but built-in persistent daemon management is not
  finished yet.

## Read next

| Document | Why open it |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | How the data model, adapters, ranking, and TUI fit together |
| [docs/ROADMAP.md](docs/ROADMAP.md) | What is shipped now and what is coming next |
| [docs/TASKS.md](docs/TASKS.md) | Live implementation tracker |
| [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) | Project conventions |
| [config.example.toml](config.example.toml) | Annotated configuration reference |

## Workspace layout

| Path | Purpose |
|---|---|
| `crates/feedfold-core` | Storage, config, ranking, shared data model |
| `crates/feedfold-adapters` | RSS, YouTube, and Claude integration |
| `crates/feedfold-daemon` | Background polling runtime |
| `crates/feedfold-tui` | Main binary, CLI, and TUI |

## License

MIT. See [LICENSE](LICENSE).

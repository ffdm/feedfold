# Contributing to feedfold

Welcome. These are the ground rules for working on this codebase.

## Code quality

**Clean and simple over clever.** If a function is hard to read, simplify it
before optimizing it. Name things so the code reads like English. Short
functions over long ones. Flat over nested.

**No premature abstraction.** Three similar lines are better than a badly
named helper. Don't introduce a trait, generic, or config knob until there
are at least two concrete use cases driving it.

**Trust internal code.** Validate at system boundaries only: user input,
HTTP responses, config files, database reads. Internal function calls should
trust their arguments. Every `unwrap_or_default`, `if let Err(_)`, and
fallback branch should have a specific reason to exist.

**Default: no comments.** Well-named identifiers explain the *what*. Write a
comment only when the *why* is non-obvious: a hidden constraint, a subtle
invariant, a workaround for a specific bug. Never narrate what the code
does.

**No numbered step-by-step comments.** On top of the general no-narration
rule, never write comments like `// 1. parse config` / `// 2. open db` /
`// 3. fetch feeds`. Well-named functions should make the sequence obvious.
Numbered narration is the worst form of commentary and is banned even when
the surrounding code is complex.

**No em dashes.** Use commas, colons, parentheses, or new sentences instead
of the `—` character. This rule applies to code comments, commit messages,
documentation, and any prose in this repository. En dashes for numeric
ranges like `1–5 stars` are fine; the ban is specifically on em dashes.

**Clippy clean.** `cargo clippy --workspace --all-targets -- -D warnings`
should pass on every commit. Don't silence lints without a comment
explaining why.

## Modular interfaces

**Core stays generic.** `feedfold-core` must not contain code specific to
any one source type. YouTube-specific logic lives in
`feedfold-core::adapters::youtube`, behind the `SourceAdapter` trait. If
you're tempted to add a `youtube_views` field to `Entry`, stop. Put it in
`enrichments` instead.

**Crates depend one direction.** `feedfold-daemon` and `feedfold-tui` depend
on `feedfold-core`. Neither binary depends on the other. The core never
depends on either.

**Adapters and rankers are traits.** Adding a new source type or ranking
strategy should be a new file implementing a trait, not a change to
existing code.

## Commits

- **Small and focused.** One logical change per commit. A commit that
  touches fifteen unrelated files is a code smell.
- **Messages explain *why*.** The diff shows *what*. Use the imperative
  mood: "Add recency ranker" not "Added recency ranker".
- **No `Co-Authored-By` trailers from AI tools.** Commits are authored by
  the human who accepted the change.
- **Don't commit `config.toml`.** It may contain API keys. Only
  `config.example.toml` is tracked.

## Testing

- Unit tests live alongside the code they test (`#[cfg(test)] mod tests`).
- Integration tests go in `crates/*/tests/`.
- Hitting the real network in tests is forbidden except in a dedicated
  `#[ignore]` suite run manually.
- Ranker implementations must have deterministic tests. Mock the clock,
  mock the API client.

## Running the project

Prerequisites:

- Rust stable, 1.75 or later, installed via [rustup](https://rustup.rs/).
- A kitty-protocol terminal (kitty, WezTerm, iTerm2) for inline thumbnails.
  Other terminals will degrade gracefully to text-only.
- A YouTube Data API v3 key for popularity sorting on YouTube sources.
- An Anthropic API key for Claude-based ranking (Phase 4+).

Build and run:

```sh
cargo check                 # fast type check across the workspace
cargo build                 # debug build of all crates
cargo build --release       # optimized; what you use day-to-day
cargo test --workspace      # run unit and integration tests
cargo clippy --workspace --all-targets -- -D warnings
```

## Task tracking

See [TASKS.md](TASKS.md) for the live task list. When you start a task,
mark it in progress (`[~]`) and commit before writing code. When you
finish, mark it done (`[x]`) and commit again. This keeps the task file
honest and makes history scannable.

## Questions or big changes

Open an issue before a large refactor. "Big" means anything that touches
multiple crates, changes a trait signature, or introduces a new core type.

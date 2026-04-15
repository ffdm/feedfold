# Ralph loop prompt: complete one feedfold phase

You are running inside a Ralph loop. Each invocation is a fresh Claude
Code context with no memory of previous iterations. Your job is to
complete **exactly one** phase task from `docs/TASKS.md`, commit the
work, push, and exit. The outer shell script will call you again for
the next phase.

Do not ask the user anything. Do not wait for input. Make the best
judgment call you can and proceed.

## 1. Orient yourself (every run)

Read these first, in order:

1. `docs/TASKS.md` — the live task tracker. Source of truth for what's
   next.
2. `docs/ROADMAP.md` — context on why each phase exists.
3. `docs/CONTRIBUTING.md` — style rules and ground rules.
4. `docs/ARCHITECTURE.md` — high-level design so your implementation
   fits.
5. Most recent file in `walkthroughs/` — to see the tone and depth
   expected for learning notes.

Also run `git status` and `git log --oneline -10` so you know the
starting state.

## 2. Pick the task

Scan `docs/TASKS.md`. Look at the **Phase 0 through Phase 4** sections
only (never the "Bucket list" section).

- If any task is marked `[~]` (in progress), that means a previous run
  crashed mid-phase. Resume it. Inspect `git log` to see if the start
  commit exists and how much was done.
- Otherwise, pick the first task marked `[ ]` (not started) under the
  earliest unfinished phase. Prerequisites matter: don't start 1.3
  before 1.2 is `[x]`.
- If every task in Phases 0-4 is `[x]`, print
  `[ralph] all phases complete` and exit 0. Do nothing else.

Call the selected task `X.Y` for the rest of this prompt.

## 3. Workflow for one phase

### 3.1 Start commit

Edit `docs/TASKS.md` to flip the selected line from `[ ]` to `[~]`
(leave it at `[~]` if you're resuming). Commit with:

```
git commit -m "Start X.Y: <short description from TASKS.md>"
```

Only skip this step if the `[~]` marker and the "Start X.Y" commit
already exist from a previous crashed run.

### 3.2 Plan, then implement

Before writing any code: read enough of the existing codebase to
understand where the new code fits. Prefer editing existing files
over creating new ones. Do the **smallest thing** that satisfies the
TASKS.md description for this phase. Do not bundle in cleanup,
refactors, or "while I'm here" work. If you notice a real issue
outside the current scope, leave a note in chat and move on.

If the phase inherently spans multiple files or crates, that's fine.
The "smallest thing" rule is about scope, not line count.

### 3.3 Verify

Before committing completion, these must all pass:

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

If either fails, fix the root cause and re-run. Do not suppress
warnings with `#[allow(...)]` unless you can justify it in a code
comment. Do not skip hooks.

For phases with user-facing behavior (a CLI subcommand, a TUI view,
etc.), also run a smoke test manually against a real input where
feasible. Network access may be blocked in the sandbox; if `cargo`
or `reqwest` can't reach the internet, note it in the chat summary
and skip the live test. Do not fake network results.

### 3.4 Write the walkthrough

Create `walkthroughs/X.Y-<slug>.md` (the `walkthroughs/` directory is
gitignored on purpose). This file is a learning aid for the user, who
is a first-time Rust programmer. It should:

- Explain what shipped and why at a high level.
- Walk through new Rust concepts this phase introduced. If a concept
  already appeared in an earlier walkthrough, link it or reference it,
  don't re-explain from scratch.
- Include actual code excerpts where they illustrate the concept.
- End with a short "what this phase does NOT do" list and a "Rust
  concepts introduced" index.

Match the depth and tone of the existing walkthroughs (look at the
most recent one in `walkthroughs/`). These files are where the
teaching lives; keep the in-chat summary terse.

### 3.5 Complete commit

Edit `docs/TASKS.md` to flip `[~]` to `[x]` for this task. Stage only
the files you actually changed (not `.ralph/` logs, not walkthroughs).
Commit with:

```
git commit -m "$(cat <<'EOF'
Complete X.Y: <one-line summary>

<2-4 sentence body explaining *why* this change exists and what the
next phase will build on top of it. Focus on design intent, not a
file-by-file diff.>
EOF
)"
```

Then:

```
git push
```

### 3.6 Exit

Print a terse (≤6 line) summary of what shipped and what's next. Do
not offer to continue. The Ralph loop will invoke you again.

## 4. Invariants that are easy to forget

These are the things that aren't obvious from reading the codebase.
Reread this section each run.

### 4.1 Commit authorship

**Never** add `Co-Authored-By: AI` (or any other AI trailer)
to commit messages. Commits go under the user's identity only.

### 4.2 No em dashes, anywhere

Do not use `—` in prose, chat messages, docs, code comments, commit
messages, or the walkthroughs. Use commas, colons, parentheses, or
sentence breaks. This applies to the `—` character specifically; `-`
and `--` are fine.

### 4.3 No numbered step-by-step code comments

Don't write code comments shaped like:

```rust
// 1. do the thing
// 2. do the next thing
// 3. profit
```

Write a single short comment only when the *why* is non-obvious. Let
well-named identifiers carry the *what*.

### 4.4 Walkthroughs are gitignored

`walkthroughs/` is in `.gitignore`. Write to it freely; do not stage
it; do not try to "fix" the gitignore. The user wants those files
local-only.

### 4.5 Do not re-scope the phase

If TASKS.md says "0.5 `feedfold add` CLI", that means the CLI. Not
config flags, not pretty output, not a future-proofing abstraction.
Phase 1.1 exists specifically so 0.5 doesn't have to invent a trait.
Trust the roadmap.

### 4.6 `feedfold-core` stays IO-free

No `reqwest`, no `feed-rs`, no filesystem I/O outside of `storage` and
`config`, in `feedfold-core`. Adapter implementations live in
`feedfold-adapters`. If a phase tempts you to add a network dep to
`feedfold-core`, you're probably solving the wrong problem.

### 4.7 Storage mutations need `&mut self`

`Storage::upsert_entries` takes `&mut self` because it opens a
transaction. That requirement propagates: any caller that reaches
upsert must hold a `mut` binding. This is not a bug.

### 4.8 Adapter errors box the inner source

`AdapterError::Fetch` and `::Parse` wrap
`Box<dyn std::error::Error + Send + Sync>`. The `Send + Sync` bounds
are required because adapter futures run on a multi-threaded runtime.
Don't "simplify" to `Box<dyn Error>`.

### 4.9 Terse chat, rich walkthrough

The in-chat completion summary should be at most ~6 lines: what
shipped, what's next. Deep detail goes in the walkthrough file. Do
not repeat walkthrough content in chat.

### 4.10 Clean working tree at exit

When you exit, `git status` must show nothing staged and nothing
unstaged that belongs in the tracked set. The only thing that should
remain untracked is the new walkthrough file (and maybe `.ralph/`
logs). If anything else is dirty, you forgot to commit it.

## 5. Common pitfalls

- **Forgetting the Start commit.** The two-commit workflow (Start X.Y
  then Complete X.Y) exists so that progress through the roadmap is
  visible in `git log`. Don't skip it.
- **Writing `#[allow(dead_code)]` to silence clippy.** Usually means
  you added speculative API surface. Delete the speculative code
  instead.
- **Pulling in a new crate from crates.io mid-loop.** The sandbox may
  have no network access for `cargo fetch`. If a phase requires a new
  dep, check `~/.cargo/registry/cache/` first, and if it's not cached,
  prefer a standard-library or existing-dep solution. Native async fn
  in traits instead of `async-trait` is the canonical example.
- **Running `git add -A` or `git add .`.** Stage specific files. The
  `.ralph/` log directory and `walkthroughs/` are gitignored but
  accidents happen with submodules or new files.
- **Amending commits.** Always create new commits; never `--amend`.
  If a hook rejects a commit, fix the issue and make a fresh commit.
- **Destructive git commands.** Never `reset --hard`, `push --force`,
  or `branch -D` without an explicit user instruction. The Ralph loop
  never has that authorization.

## 6. If you get stuck

If a phase genuinely cannot be completed in this run (missing
external dependency, ambiguous requirement, failing test you don't
understand), do **not** fake it. Leave the task at `[~]`, commit any
partial progress under a `Start X.Y: WIP <reason>` commit so the next
run can pick it up, print a clear one-paragraph explanation of what
blocked you, and exit. A human will intervene.
ene.

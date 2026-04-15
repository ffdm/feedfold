#!/usr/bin/env bash
# Ralph loop for feedfold.
#
# Runs Claude Code repeatedly in non-interactive mode. Each iteration
# reads scripts/ralph.md as its prompt, picks the next unfinished
# phase from docs/TASKS.md, implements it, commits, and pushes.
#
# Usage:
#   scripts/ralph.sh              # up to 10 iterations
#   scripts/ralph.sh 3            # up to 3 iterations
#   MODEL=opus scripts/ralph.sh   # pin a specific model
#
# Stop the loop any time with Ctrl-C.

set -euo pipefail

cd "$(dirname "$0")/.."

PROMPT_FILE="scripts/ralph.md"
MAX_ITERS="${1:-10}"
LOG_DIR=".ralph"
MODEL="${MODEL:-}"

if [[ ! -f "$PROMPT_FILE" ]]; then
    echo "[ralph] missing $PROMPT_FILE" >&2
    exit 1
fi

if ! command -v claude >/dev/null 2>&1; then
    echo "[ralph] claude CLI not on PATH" >&2
    exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
    echo "[ralph] working tree is dirty; commit or stash before starting" >&2
    git status --short >&2
    exit 1
fi

mkdir -p "$LOG_DIR"

# Match either [ ] or [~] under Phase 0-4 sections; ignore the
# Bucket list section at the bottom of TASKS.md.
has_pending_phase_task() {
    awk '
        /^## Bucket list/ { stop = 1 }
        !stop && /^[[:space:]]*-[[:space:]]*\[[ ~]\]/ { found = 1 }
        END { exit found ? 0 : 1 }
    ' docs/TASKS.md
}

claude_args=(-p --dangerously-skip-permissions)
if [[ -n "$MODEL" ]]; then
    claude_args+=(--model "$MODEL")
fi

for i in $(seq 1 "$MAX_ITERS"); do
    if ! has_pending_phase_task; then
        echo "[ralph] no pending phase tasks remain; stopping."
        break
    fi

    ts=$(date +%Y%m%d-%H%M%S)
    log="$LOG_DIR/iter-$(printf '%02d' "$i")-$ts.log"

    echo "[ralph] iteration $i/$MAX_ITERS starting ($(date '+%H:%M:%S')) → $log"
    echo "[ralph] current pending line:"
    awk '
        /^## Bucket list/ { exit }
        /^[[:space:]]*-[[:space:]]*\[[ ~]\]/ { print "  " $0; found = 1; exit }
    ' docs/TASKS.md

    if ! claude "${claude_args[@]}" < "$PROMPT_FILE" 2>&1 | tee "$log"; then
        echo "[ralph] claude exited non-zero; see $log" >&2
        exit 1
    fi

    echo "[ralph] iteration $i finished ($(date '+%H:%M:%S'))"
    sleep 2
done

echo "[ralph] done."

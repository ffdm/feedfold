#!/usr/bin/env bash
# Ralph loop for feedfold.
#
# Runs an AI agent repeatedly in non-interactive mode. Each iteration
# reads scripts/ralph.md as its prompt, picks the next unfinished
# phase from docs/TASKS.md, implements it, commits, and pushes.
#
# Usage:
#   scripts/ralph.sh              # up to 10 iterations
#   scripts/ralph.sh 3            # up to 3 iterations
#   AGENT=gemini scripts/ralph.sh # pin a specific agent (claude, gemini, codex)
#   MODEL=opus scripts/ralph.sh   # pin a specific model
#
# Stop the loop any time with Ctrl-C.

set -euo pipefail

cd "$(dirname "$0")/.."

PROMPT_FILE="scripts/ralph.md"
MAX_ITERS="${1:-10}"
LOG_DIR=".ralph"
AGENT="${AGENT:-claude}"
MODEL="${MODEL:-}"

if [[ ! -f "$PROMPT_FILE" ]]; then
    echo "[ralph] missing $PROMPT_FILE" >&2
    exit 1
fi

agent_cmd=()
if [[ "$AGENT" == "claude" ]]; then
    agent_cmd=(claude -p --dangerously-skip-permissions)
    if [[ -n "$MODEL" ]]; then
        agent_cmd+=(--model "$MODEL")
    fi
elif [[ "$AGENT" == "gemini" ]]; then
    agent_cmd=(gemini -y)
    if [[ -n "$MODEL" ]]; then
        agent_cmd+=(--model "$MODEL")
    fi
elif [[ "$AGENT" == "codex" ]]; then
    agent_cmd=(codex --yolo)
    if [[ -n "$MODEL" ]]; then
        agent_cmd+=(--model "$MODEL")
    fi
else
    agent_cmd=("$AGENT")
fi

if ! command -v "${agent_cmd[0]}" >/dev/null 2>&1; then
    echo "[ralph] ${agent_cmd[0]} CLI not on PATH" >&2
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

    if ! "${agent_cmd[@]}" < "$PROMPT_FILE" 2>&1 | tee "$log"; then
        echo "[ralph] agent exited non-zero; see $log" >&2
        exit 1
    fi

    echo "[ralph] iteration $i finished ($(date '+%H:%M:%S'))"
    sleep 2
done

echo "[ralph] done."

#!/usr/bin/env bash
#
# Import YouTube subscriptions from a Google Takeout CSV.
#
# Usage:
#   ./scripts/import-youtube-csv.sh ~/Downloads/subscriptions.csv

set -euo pipefail

if [ $# -ne 1 ]; then
    echo "Usage: $0 <subscriptions.csv>" >&2
    exit 1
fi

csv="$1"

if [ ! -f "$csv" ]; then
    echo "File not found: $csv" >&2
    exit 1
fi

feedfold_bin="$(dirname "$0")/../target/release/feedfold"
if [ ! -x "$feedfold_bin" ]; then
    feedfold_bin="$(dirname "$0")/../target/debug/feedfold"
fi
if [ ! -x "$feedfold_bin" ]; then
    echo "feedfold binary not found. Run 'cargo build' first." >&2
    exit 1
fi

added=0
failed=0
skipped=0

while IFS=, read -r channel_id channel_url title; do
    channel_id="$(echo "$channel_id" | tr -d '[:space:]"')"
    title="$(echo "$title" | sed 's/^[[:space:]"]*//;s/[[:space:]"]*$//')"

    if [ "$channel_id" = "Channel Id" ]; then
        continue
    fi

    if [ -z "$channel_id" ]; then
        skipped=$((skipped + 1))
        continue
    fi

    feed_url="https://www.youtube.com/feeds/videos.xml?channel_id=${channel_id}"

    if "$feedfold_bin" add "$feed_url" --name "$title"; then
        added=$((added + 1))
    else
        echo "  ! Failed: $title ($channel_id)" >&2
        failed=$((failed + 1))
    fi
done < "$csv"

echo ""
echo "Done: $added added, $failed failed, $skipped skipped."

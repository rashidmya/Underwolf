#!/usr/bin/env bash
# A/B overlay benchmark against an already-running target (game or vkcube with the layer).
# Switches the layer's mode through ~/.cache/underwolf-spike/control: A B A B ... (runs × 2),
# each for SECS + 5 s (the first 5 s of every run are dropped by uw-spike-bench), then
# compares the A segments against the B segments.
#
#   usage: ab.sh <modeA> <modeB> [runs=3] [secs=60]
#   e.g.:  ab.sh passthrough quad
set -euo pipefail

a=${1:?modeA}; b=${2:?modeB}; runs=${3:-3}; secs=${4:-60}
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
dir="$HOME/.cache/underwolf-spike"
mkdir -p "$dir"

newest_pid() {
    ls -t "$dir"/frames-*.csv 2>/dev/null | head -1 | sed -E 's|.*/frames-([0-9]+)-.*|\1|'
}

pid=$(newest_pid)
[ -n "$pid" ] || { echo "no frames CSV yet: is the target running with the layer?" >&2; exit 2; }
echo "target pid: $pid"

before=$(mktemp)
ls "$dir"/frames-"$pid"-*.csv | sort > "$before"

for i in $(seq 1 "$runs"); do
    for m in "$a" "$b"; do
        echo "$m" > "$dir/control"
        echo "run $i/$runs: $m (${secs}s + 5s warm-up)"
        sleep $((secs + 5))
    done
done
sleep 2  # let the last second flush

new=$(comm -13 "$before" <(ls "$dir"/frames-"$pid"-*.csv | sort))
rm -f "$before"
echo "$a" > "$dir/control"  # leave the target in mode A

base=$(grep -E -- "-$a-[0-9]+\.csv$" <<<"$new" | tr '\n' ' ')
test=$(grep -E -- "-$b-[0-9]+\.csv$" <<<"$new" | tr '\n' ' ')
# shellcheck disable=SC2086
cargo run -q --release --manifest-path "$root/Cargo.toml" -p uw-spike-common --bin uw-spike-bench -- \
    compare $base -- $test

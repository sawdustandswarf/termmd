#!/bin/sh
# Throughput and memory of termmd beside foot, alacritty and kitty (those
# installed), each in a real window with 10k lines of scrollback: cat
# each corpus from the bench example and time it inside the terminal, and
# sample the terminal's peak resident memory (VmHWM) while it runs.
#
#   dist/bench/compare.sh [DIR] [MiB] [RUNS]
#
# Opens one window per terminal per corpus, RUNS (10) rounds of each, a
# round going through every terminal so background load hits them alike;
# leave them alone while it runs. Reports the median MiB/s with its range,
# and the highest peak memory. On Hyprland it stops, reporting what it has,
# once the focus doesn't come back to the window it started from: each new
# window takes the focus, so someone working elsewhere would lose it.
# Keypress latency isn't measured here (that takes a camera or typometer).
set -eu

dir=${1:-${XDG_RUNTIME_DIR:-/tmp}/termmd-bench}
mib=${2:-32}
runs=${3:-10}
cd "$(dirname "$0")/../.."
# Rustup's cargo, ahead of version-manager shims that may have no Rust set.
cargo=cargo
[ -x "$HOME/.cargo/bin/cargo" ] && cargo=$HOME/.cargo/bin/cargo
"$cargo" run -q --release --example bench -- --write "$dir" "$mib"

run() { # terminal corpus
    out="$dir/$1-$(basename "$2" .txt)"
    script="s=\$(date +%s%N); cat '$2'; e=\$(date +%s%N); echo \$(( (e - s) / 1000000 )) > '$out.ms'"
    case $1 in
        termmd) termmd -e sh -c "$script" & ;;
        foot) foot sh -c "$script" & ;;
        alacritty) alacritty -o scrolling.history=10000 -e sh -c "$script" & ;;
        kitty) kitty -o scrollback_lines=10000 sh -c "$script" & ;;
    esac
    pid=$!
    peak=0
    while kill -0 "$pid" 2>/dev/null; do
        hwm=$(awk '/^VmHWM:/ { print $2 }' "/proc/$pid/status" 2>/dev/null || true)
        [ -n "$hwm" ] && peak=$hwm
        sleep 0.1
    done
    wait "$pid" || true
    echo "$peak" > "$out.kib"
}

# The focused window's address; empty away from Hyprland.
focused() { hyprctl activewindow 2>/dev/null | awk 'NR == 1 { print $2; exit }' || true; }
home=$(focused)
# Whether the focus is back home, given a moment after a window closes.
watched() {
    [ -z "$home" ] && return 0
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        [ "$(focused)" = "$home" ] && return 0
        sleep 0.1
    done
    return 1
}

terminals=
for t in termmd foot alacritty kitty; do
    command -v "$t" >/dev/null && terminals="$terminals $t"
done

results="$dir/results"
: > "$results"
round=1
while [ "$round" -le "$runs" ]; do
    echo "round $round of $runs" >&2
    for f in "$dir"/*.txt; do
        name=$(basename "$f" .txt)
        for t in $terminals; do
            if ! watched; then
                echo "stopped: the focus moved away, in round $round of $runs" >&2
                break 3
            fi
            rm -f "$dir/$t-$name.ms"
            run "$t" "$f"
            ms=$(cat "$dir/$t-$name.ms" 2>/dev/null || echo 0)
            echo "$name $t $ms $(cat "$dir/$t-$name.kib")" >> "$results"
        done
    done
    round=$((round + 1))
done

# Median and range of MiB/s per corpus and terminal (failed runs, 0 ms,
# left out), highest peak memory.
sort -k1,1 -k2,2 -k3,3n "$results" | awk -v mib="$mib" '
    function flush() {
        if (key == "") return
        med = n ? (n % 2 ? v[(n + 1) / 2] : (v[n / 2] + v[n / 2 + 1]) / 2) : 0
        lo = n ? v[n] : 0; hi = n ? v[1] : 0
        printf "%-12s %-10s %8.1f %8.1f-%-8.1f %6d/%-4d %8.1f\n", c, t, med, lo, hi, n, total, peak / 1024
    }
    BEGIN { printf "%-12s %-10s %8s %17s %11s %8s\n", "corpus", "terminal", "MiB/s", "range", "runs", "peak MiB" }
    {
        if ($1 " " $2 != key) { flush(); key = $1 " " $2; c = $1; t = $2; n = 0; total = 0; peak = 0 }
        total++
        if ($4 > peak) peak = $4
        # Ascending ms is descending MiB/s.
        if ($3 > 0) v[++n] = mib * 1000 / $3
    }
    END { flush() }'

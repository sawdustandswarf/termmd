#!/usr/bin/env bash
# A tour of termmd in the termmd it is run in: five scenes of SCENE seconds
# (4) each. Every call it makes is printed, in bold, as it is made.
#
#   1. Markdown: the cheat sheet (docs/samples), a block at a time, slowed
#      on purpose and labelled so.
#   2. Following a file: a document shown with `f` while it is rewritten.
#   3. Editing in place: `f`, then the keys a person would press
#      (Ctrl+Shift+E, vi keys, ZZ), typed with wtype, and the file after.
#      Only while this window has the keyboard focus, since wtype types
#      into whichever window has it; otherwise the scene says so and waits.
#   4. Images: `f` on a PNG.
#   5. lsmd: a directory listed as a Markdown table.
#
# For the whole tour, at once:
#   - embedded-app demos one after another in the bottom right, the last
#     two faded (opacity) and under the text (z), to show stacking;
#   - the time, termmd's clock (dist/clock), in the top right;
#   - the Omarchy theme changing every 5 seconds, named over the top left
#     (demos/theme.sh, as a subtask), and back to yours at the end.
#
#   demos/tour.sh                 # the demos/*.sh of repositories cloned beside this one
#   demos/tour.sh A.sh B.sh ...   # these demos
#
# A demo is a script that prints the command it runs, starts one app in a
# pane of the termmd it runs in, in the background, and returns; PLACE gives
# it its placement keys and DURATION how long it runs.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(dirname "$here")

if [[ -z ${TERMMD_SURFACES:-} || ! -t 1 ]]; then
    echo "tour.sh: run it inside termmd" >&2
    exit 1
fi

if (($# > 0)); then
    demos=("$@")
else
    shopt -s nullglob
    demos=()
    for d in "$(dirname "$root")"/*/demos/*.sh; do
        [[ $d == "$here"/* ]] || demos+=("$d")
    done
fi

scene=${SCENE:-4}
scenes=5
total=$(awk -v s="$scene" -v n="$scenes" 'BEGIN { print s * n }')
duration=$(awk -v t="$total" -v n="${#demos[@]}" 'BEGIN { print n ? t / n : t }')
md=${MD:-$root/docs/samples/markdown-cheat-sheet.md}

# The apps' places: the last two show fading and stacking under the text.
place_for() {
    local base="fixed=bottom-right w=50% h=50%" n=${#demos[@]}
    if ((n >= 3 && $1 == n - 2)); then
        echo "$base opacity=60%"
    elif ((n >= 2 && $1 == n - 1)); then
        echo "$base z=-1"
    else
        echo "$base"
    fi
}

# A call, printed as if typed.
shown() {
    printf '\e[1m$ %s\e[0m\n' "$1"
}

# Sleep until $2 seconds after $1 (an $EPOCHREALTIME).
until_at() {
    sleep "$(awk -v s="$1" -v now="$EPOCHREALTIME" -v d="$2" 'BEGIN { w = s + d - now; print (w > 0 ? w : 0) }')"
}

# The end of scene $1 (from 1).
end_of_scene() {
    until_at "$start" "$(awk -v s="$scene" -v k="$1" 'BEGIN { print s * k }')"
}

# A Markdown file's blocks, NUL-separated (split at blank lines, not inside
# a fence).
md_pieces() {
    awk '/^```/ { f = !f }
        { b = b $0 "\n" }
        /^$/ && !f { if (b ~ /[^\n]/) printf "%s%c", b, 0; b = "" }
        END { if (b ~ /[^\n]/) printf "%s%c", b, 0 }' "$1"
}

# Lay out the NUL-separated Markdown pieces on stdin, one at a time, spread
# evenly over $1 seconds and paced by the clock, from folder $2 (so relative
# links and images resolve).
paced() {
    local pieces=() i start=$EPOCHREALTIME
    mapfile -d '' pieces
    for i in "${!pieces[@]}"; do
        (cd "$2" && termmd --md <<<"${pieces[i]}")
        until_at "$start" "$(awk -v t="$1" -v k=$((i + 1)) -v n="${#pieces[@]}" 'BEGIN { print t * k / n }')"
    done
}

# f shows a file as a linked document (termmd's dist/f).
f() {
    if type -P f >/dev/null; then command f "$@"; else termmd --show "$@"; fi
}

# A label, $2, pinned over the text for $1 seconds, in an embedded terminal
# of its own that closes when it is done.
label() {
    termmd --embed="fixed=4,8 w=$((${#2} + 5)) h=3 focus=no" \
        bash -c 'printf "\e[?25l\n \e[1;97;41m %s \e[0m" "$1"; sleep "$2"' _ "$2" "$1" &
}

# Whether this window has the keyboard focus (Hyprland): the focused window's
# process is one of this script's ancestors.
focused() {
    local active p=$$
    active=$(hyprctl activewindow -j 2>/dev/null | awk -F': *' '/"pid"/ { gsub(/[ ,]/, "", $2); print $2 }')
    [[ -n $active ]] || return 1
    while ((p > 1)); do
        [[ $p == "$active" ]] && return 0
        p=$(ps -o ppid= -p "$p" | tr -d ' ')
    done
    return 1
}

# Keys typed into this window with wtype, shown first; nothing if it has lost
# the focus.
keys() {
    local what=$1
    shift
    focused || { shown "# ($what: skipped, the window lost the focus)"; return 1; }
    shown "wtype $* # $what"
    wtype "$@"
}

clear
cd "$root"
tmp=$(mktemp -d)
start=$EPOCHREALTIME

# termmd's clock, top right, for the whole tour.
clock=$(type -P clock || echo dist/clock)
shown "termmd --embed='fixed=top-right w=29 h=7 focus=no' timeout $total ${clock##*/} &"
termmd --embed="fixed=top-right w=29 h=7 focus=no" timeout "$total" "$clock" &

# The theme, changing as a subtask (it prints each call); stopped, and so back
# to yours, when the tour ends, however it ends.
shown "demos/theme.sh --loop --all &"
"$here/theme.sh" --loop --all &
themes=$!
trap 'kill "$themes" 2>/dev/null || true; wait "$themes" 2>/dev/null || true; rm -rf "$tmp"' EXIT

# The app demos, bottom right, one after another (each prints its call); each
# closes as the next starts.
(
    for i in "${!demos[@]}"; do
        DURATION="$duration" PLACE="$(place_for "$i")" bash "${demos[i]}"
        until_at "$start" "$(awk -v d="$duration" -v k=$((i + 1)) 'BEGIN { print d * k }')"
    done
) &

# 1. Markdown, slowed on purpose.
shown "termmd --md ${md#"$root"/}   # a block at a time"
label "$scene" "DELIBERATELY SLOWED"
md_pieces "$md" | paced "$(awk -v s="$scene" 'BEGIN { print s * 0.9 }')" "$(dirname "$md")"
end_of_scene 1

# 2. Following a file: rewritten while it is shown.
live=$tmp/live.md
printf '# A live document\n\nThis file is rewritten while termmd shows it.\n\n| tick | time |\n|---|---|\n' >"$live"
shown "f live.md"
f "$live"
for i in 1 2 3 4 5 6; do
    sleep "$(awk -v s="$scene" 'BEGIN { print s / 8 }')"
    row="| $i | $(date +%T.%2N) |"
    shown "echo '$row' >> live.md"
    echo "$row" >>"$live"
done
end_of_scene 2

# 3. Editing in place, with the keys a person would press. Cleared first, so
# the file to edit is the only document in view (Ctrl+Shift+E edits that one).
clear
edit=$tmp/edit.md
printf '# Edit me\n\n- a list in a file\n' >"$edit"
shown "f edit.md"
f "$edit"
sleep 0.4
if command -v wtype >/dev/null &&
    keys "start editing" -M ctrl -M shift -k e -m shift -m ctrl &&
    { sleep 0.4; keys "last line, open one below" -d 80 Go; } &&
    keys "type" -d 40 -k minus " typed in place at $(date +%T)" &&
    keys "Normal mode" -k Escape &&
    { sleep 0.3; keys "write and stop" -d 80 ":wq" -k Return; }; then
    sleep 0.6
    shown "cat edit.md"
    cat "$edit"
else
    command -v wtype >/dev/null || shown "# (editing in place needs wtype)"
    # Out of an edit session left half done, without writing, if this window
    # still has the keys.
    focused && wtype -k Escape -d 80 ":q!" -k Return 2>/dev/null || true
fi
end_of_scene 3

# 4. Images.
shown "f docs/samples/tux.png"
f docs/samples/tux.png
end_of_scene 4

# 5. lsmd: ls as a Markdown table.
shown "lsmd docs"
if command -v lsmd >/dev/null; then lsmd docs; else dist/lsmd docs; fi
end_of_scene 5

kill "$themes"
wait

#!/usr/bin/env bash
# Change the Omarchy theme; termmd follows it live.
#
#   demos/theme.sh NAME                      # switch to NAME and stay there
#   demos/theme.sh [-s SECONDS] NAME NAME... # each in turn, then back to the theme you had
#   demos/theme.sh [-s SECONDS] --all        # every installed theme in turn, then back
#   demos/theme.sh --loop ...                # round and round until stopped, then back
#   demos/theme.sh -q ...                    # without printing each call
#   demos/theme.sh --list                    # the installed themes
#
# SECONDS is how long each theme shows when stepping through (5). Inside
# termmd, each theme's name shows over the top left while it is on. Ctrl+C
# (or a TERM, as the tour sends) stops it and still goes back to the theme
# you had.
set -euo pipefail

usage() {
    sed -n '2,14s/^# \{0,1\}//p' "${BASH_SOURCE[0]}" >&2
    exit 2
}

command -v omarchy >/dev/null || { echo "theme.sh: needs Omarchy (the omarchy command)" >&2; exit 1; }

seconds=5
loop=
quiet=
themes=()
while (($# > 0)); do
    case $1 in
        -s) [[ ${2:-} =~ ^[0-9]+([.][0-9]+)?$ ]] || usage; seconds=$2; shift 2 ;;
        --all) mapfile -t all < <(omarchy theme list); themes+=("${all[@]}"); shift ;;
        --list) omarchy theme list; exit ;;
        --loop) loop=1; shift ;;
        -q) quiet=1; shift ;;
        -h | --help) usage ;;
        -*) usage ;;
        *) themes+=("$1"); shift ;;
    esac
done
((${#themes[@]} > 0)) || usage

# Check every name before changing anything ("Tokyo Night" or tokyo-night).
declare -A installed
while IFS= read -r t; do
    k=${t,,}
    installed[${k// /-}]=1
done < <(omarchy theme list)
for t in "${themes[@]}"; do
    k=${t,,}
    [[ -n ${installed[${k// /-}]:-} ]] || { echo "theme.sh: no theme \"$t\"; see demos/theme.sh --list" >&2; exit 1; }
done

set_theme() {
    [[ -n $quiet ]] || printf '\e[1m$ omarchy theme set %q\e[0m\n' "$1"
    omarchy theme set "$1" >/dev/null
}

# Inside termmd: name $1 over the top left for $2 seconds, in the theme's own
# colours reversed, in an embedded terminal that closes when it is done.
label() {
    [[ -n ${TERMMD_SURFACES:-} && -t 1 ]] || return 0
    termmd --embed="fixed=2,1 w=$((${#1} + 4)) h=3 focus=no" \
        bash -c 'printf "\e[?25l\n \e[1;7m %s \e[0m" "$1"; sleep "$2"' _ "$1" "$2" &
}

# One theme, once: switch and stay.
if ((${#themes[@]} == 1)) && [[ -z $loop ]]; then
    set_theme "${themes[0]}"
    exit
fi

# Several: step through them, then back to the one you had, however it ends.
# One at a time: a second, started while this one is changing themes, would
# take one of these for yours and put that back.
exec 9>"${XDG_RUNTIME_DIR:-/tmp}/termmd-theme.lock"
flock -n 9 || { echo "theme.sh: already stepping through themes" >&2; exit 1; }
original=$(omarchy theme current)
trap '[[ -n $quiet ]] || echo; set_theme "$original"' EXIT
trap 'exit 130' INT TERM
# Paced by the clock: a theme takes a moment to apply, and that comes out of
# its time rather than adding to it. The sleep runs in the background and is
# waited for, so a signal stops it at once rather than when it ends.
start=$EPOCHREALTIME
k=0
while :; do
    for t in "${themes[@]}"; do
        set_theme "$t"
        k=$((k + 1))
        left=$(awk -v s="$start" -v now="$EPOCHREALTIME" -v d="$seconds" -v k="$k" \
            'BEGIN { w = s + d * k - now; print (w > 0 ? w : 0) }')
        label "$t" "$left"
        sleep "$left" &
        wait $!
    done
    [[ -n $loop ]] || break
done

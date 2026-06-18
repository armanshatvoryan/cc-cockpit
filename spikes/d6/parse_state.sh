#!/usr/bin/env bash
# parse_state.sh — D6 capture-pane state heuristic (Source B fallback).
#
# Classifies a single Claude Code pane snapshot into one of:
#   IDLE | WORKING | NEEDS_INPUT | DEAD | ?
# using the REAL marker strings captured 2026-06-18 (see REAL-MARKERS.md) and
# the backend §3 rules. Pure bash + grep — no API key, no deps.
#
# USAGE:
#   parse_state.sh --dump <file>            # classify a capture-pane dump file
#   tmux capture-pane -p -e -S -120 -t %N | parse_state.sh --dump -   # or stdin
#   parse_state.sh --dead                   # pane_dead==1 short-circuit
#   parse_state.sh --dump <f> --idle-secs N --last-activity-age <sec>
#
# The backend feeds us: pane_dead (from list-panes), the capture-pane text, and
# the seconds since #{pane_activity} last advanced (last-activity-age). This
# script encodes ONLY the text-heuristic + the dead/idle-quiet gating; the Rust
# core layers the hook-event fusion on top (hook wins if <=2s fresh).
#
# RULES (backend §3, in priority order):
#   1. pane_dead==1                                  -> DEAD
#   2. NEEDS_INPUT affordance (perm box / numbered)  -> NEEDS_INPUT
#   3. WORKING marker (spinner glyph / live timer)   -> WORKING
#   4. empty input prompt + quiet >= idle_secs       -> IDLE
#   5. empty input prompt but recently active        -> IDLE (settled turn)
#   6. anything else / signals disagree              -> ?
#
# NOTE: intentionally NOT `set -e` — this classifier is built from conditional
# tests whose "false" branch returns non-zero by design; -e would abort mid-scan.
set -uo pipefail

IDLE_SECS=3
LAST_ACTIVITY_AGE=""   # seconds since pane_activity advanced; optional
DUMP_FILE=""
DEAD=0

while [ $# -gt 0 ]; do
  case "$1" in
    --dump) DUMP_FILE="$2"; shift 2 ;;
    --dead) DEAD=1; shift ;;
    --idle-secs) IDLE_SECS="$2"; shift 2 ;;
    --last-activity-age) LAST_ACTIVITY_AGE="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

# Rule 1: dead short-circuits everything.
if [ "$DEAD" = "1" ]; then echo "DEAD"; exit 0; fi

# Load the snapshot (file or stdin).
if [ -z "$DUMP_FILE" ]; then echo "?"; exit 0; fi
if [ "$DUMP_FILE" = "-" ]; then RAW="$(cat)"; else RAW="$(cat "$DUMP_FILE")"; fi

# Strip ANSI SGR escapes so patterns match on visible text.
# (capture-pane -e embeds \x1b[...m; -e also keeps glyphs intact.)
TEXT="$(printf '%s' "$RAW" | sed $'s/\x1b\\[[0-9;?]*[A-Za-z]//g')"

# Look only at the last ~25 lines — CC's live status region is always the tail.
# Scrollback above can contain stale markers ("Running" from a past turn).
TAIL="$(printf '%s\n' "$TEXT" | tail -n 25)"

has() { printf '%s' "$TAIL" | grep -qaE "$1"; }

# last_line_matching <regex> -> the 1-based line number (within TAIL) of the
# LAST line matching, or 0. Used to order competing affordances: the newest one
# (highest line number) is authoritative — CC's live region is at the bottom.
last_line_matching() {
  printf '%s\n' "$TAIL" | grep -naE "$1" | tail -1 | cut -d: -f1 | { read -r n || true; echo "${n:-0}"; }
}

# CC is a bottom-anchored TUI: the live region is always the LAST thing drawn.
# When several affordances appear in the snapshot (e.g. a permission box from a
# past turn ABOVE a now-empty prompt), the lowest one on screen is authoritative.
# So we score each candidate by the line number of its LAST occurrence and pick
# the largest. This kills the "stale box above a fresh prompt" false positive
# without depending on alt-screen clearing.

# --- Candidate markers (verbatim from REAL-MARKERS.md) ----------------------
WORKING_TIMER='\(([0-9]+m )?[0-9]+s · .*tokens\)'    # live elapsed timer
WORKING_GLYPH='✽'                                     # active spinner frame
WORKING_RUN='(Running|Transmuting|Baking|Cooking|Simmering|Initializing|Resolving)…'
WORKING_BG='ctrl\+b ctrl\+b .*to run in background'
WORKING_ESC='esc to interrupt'                         # other CC builds

NEEDS_QUESTION='Do you want|Would you like|Do you trust|Allow .* to|wants to'
NEEDS_OPTIONS='❯ ?[0-9]\.|^ *[0-9]\. (Yes|No|Allow|Deny)|Yes, and|No, and tell'

IDLE_PROMPT='⏵⏵ auto mode on|shift\+tab to cycle'     # the persistent idle footer

# Line of the LAST WORKING signal (any of the alternatives).
W=0
for re in "$WORKING_TIMER" "$WORKING_GLYPH" "$WORKING_RUN" "$WORKING_BG" "$WORKING_ESC"; do
  n="$(last_line_matching "$re")"; [ "$n" -gt "$W" ] && W="$n"
done

# Line of the LAST NEEDS_INPUT box — require BOTH a question AND an option line
# somewhere in TAIL; score by the lower (later-drawn) of the two so the whole
# box must be the trailing affordance, not a half-scrolled remnant.
NQ="$(last_line_matching "$NEEDS_QUESTION")"
NO="$(last_line_matching "$NEEDS_OPTIONS")"
N=0
if [ "$NQ" -gt 0 ] && [ "$NO" -gt 0 ]; then
  # Both halves present -> score by the lower-on-screen (later-drawn) of the two
  # so a fully-scrolled-off box can't win against a fresh prompt below it.
  N=$NQ; [ "$NO" -gt "$N" ] && N=$NO
fi

# Line of the LAST IDLE prompt/footer.
I="$(last_line_matching "$IDLE_PROMPT")"

# --- Decide by which authoritative marker sits lowest on screen -------------
# DEAD already handled (Rule 1). If nothing matched at all -> ambiguous.
if [ "$W" -eq 0 ] && [ "$N" -eq 0 ] && [ "$I" -eq 0 ]; then echo "?"; exit 0; fi

# WORKING is special: a live spinner/timer overrides a stale prompt even if the
# prompt redraw lands a line lower, because CC suppresses input while working.
# But a NEEDS_INPUT box that sits BELOW the working markers means the turn moved
# on to a prompt -> NEEDS_INPUT wins. Resolve by max line, with WORKING winning
# ties against IDLE (spinner repaints over the just-cleared prompt area).
BEST="?"; BESTLINE=-1
if [ "$N" -gt "$BESTLINE" ]; then BEST="NEEDS_INPUT"; BESTLINE="$N"; fi
if [ "$W" -gt "$BESTLINE" ]; then BEST="WORKING"; BESTLINE="$W"; fi
if [ "$I" -gt "$BESTLINE" ]; then BEST="IDLE"; BESTLINE="$I"; fi

# idle_secs gate: if we landed on IDLE via the prompt but activity advanced
# within idle_secs AND a working marker also exists, hold WORKING (debounce a
# turn that is mid-repaint). Only applies when activity age was supplied.
if [ "$BEST" = "IDLE" ] && [ -n "$LAST_ACTIVITY_AGE" ] && [ "$W" -gt 0 ]; then
  if [ "$LAST_ACTIVITY_AGE" -lt "$IDLE_SECS" ]; then BEST="WORKING"; fi
fi

echo "$BEST"
exit 0

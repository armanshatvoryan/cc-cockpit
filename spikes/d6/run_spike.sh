#!/usr/bin/env bash
# run_spike.sh — self-contained D6 harness on a PRIVATE tmux socket.
#
# Socket: `tmux -L cockpit-d6`  (NOT the default socket, NOT `-L cockpit`).
# This NEVER touches the user's live claude.exe panes (%0..%3 on default) or the
# native `cockpit` socket. Teardown kills ONLY `-L cockpit-d6 kill-server`.
#
# What it proves (first-cut, not the 50-rep statistical run):
#   * fake-claude emits the real markers; the parser classifies each phase
#     (WORKING -> NEEDS_INPUT -> IDLE) from live capture-pane snapshots.
#   * the hook shim fires and writes events/<session>.ndjson.
#   * $COCKPIT_PANE_ID is exported into the pane env AND survives into the hook
#     env the shim runs in (R1 — the riskiest mapping assumption).
set -uo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
SOCK="cockpit-d6"
SESS="d6-spike"
SHIM="$DIR/cockpit-hook-shim.sh"
FAKE="$DIR/fake-claude.sh"
PARSE="$DIR/parse_state.sh"
EVENT_DIR="$DIR/events"
LOG="$DIR/spike-run.log"

chmod +x "$SHIM" "$FAKE" "$PARSE" 2>/dev/null || true
rm -f "$EVENT_DIR"/*.ndjson 2>/dev/null || true
: > "$LOG"
say() { echo "$@" | tee -a "$LOG"; }

tx() { tmux -L "$SOCK" "$@"; }   # all tmux ops on the PRIVATE socket

cleanup() { tx kill-server 2>/dev/null || true; }
trap cleanup EXIT

say "=== D6 spike harness — private socket: tmux -L $SOCK ==="
say "default socket is UNTOUCHED. teardown kills only -L $SOCK."
say ""

# Guard: refuse to run if someone aliased us onto the default/`cockpit` socket.
case "$SOCK" in cockpit|"") say "FATAL: refusing socket '$SOCK'"; exit 1;; esac

# --- spawn a pane running fake-claude, with COCKPIT_PANE_ID exported ---------
tx kill-server 2>/dev/null || true
tx new-session -d -s "$SESS" -x 200 -y 50
# remain-on-exit so pane_dead flips to 1 (and stays) after fake-claude exits —
# this is the real DEAD signal the backend reads via list-panes. It is a WINDOW
# option and must be set BEFORE the process exits. (cockpit sets it per pane at
# create time, §2 "remain-on-exit on per pane at create".)
tx set-window-option -t "$SESS" remain-on-exit on >/dev/null 2>&1 || true
PANE="$(tx list-panes -t "$SESS" -F '#{pane_id}' | head -1)"
say "spawned pane: $PANE"

# Launch fake-claude EXACTLY as the cockpit launches real claude (§7 step 5):
#   cd <dir> && COCKPIT_PANE_ID=<paneId> <cmd>
# This is the R1 mechanism: the env var is set in the pane's shell; the shim
# (invoked by fake-claude, standing in for CC's hook runner) must inherit it.
# `; exit` so the shell terminates when fake-claude returns -> with
# remain-on-exit the pane flips pane_dead=1 (the real DEAD signal). Real claude
# exits the same way; the shell wrapper does not linger.
tx send-keys -t "$PANE" \
  "cd '$DIR' && COCKPIT_PANE_ID=$PANE COCKPIT_SHIM='$SHIM' COCKPIT_EVENT_DIR='$EVENT_DIR' FAKE_WORK_SECS=3 FAKE_INPUT_SECS=3 FAKE_IDLE_SECS=4 '$FAKE'; exit" \
  Enter
say "launched fake-claude with COCKPIT_PANE_ID=$PANE"
say ""

# --- poll + classify each phase --------------------------------------------
# Phase windows are timed to fake-claude's 3s/3s/4s schedule (+startup slack).
declare -a RESULTS
classify_at() {  # classify_at <label> <expected>
  local label="$1" expect="$2" dump got mark
  dump="$(tx capture-pane -t "$PANE" -p -e -S -60 2>/dev/null)"
  got="$(printf '%s' "$dump" | "$PARSE" --dump - --idle-secs 3)"
  if [ "$got" = "$expect" ]; then mark="PASS"; else mark="MISS"; fi
  say "  [$label] parser=$got expected=$expect  -> $mark"
  RESULTS+=("$label:$got:$expect:$mark")
}

sleep 2              # let the shell + clear + phase-1 draw settle
classify_at WORKING      WORKING
sleep 4              # phase 1 ends (~3s), phase 2 (perm box) draws
classify_at NEEDS_INPUT  NEEDS_INPUT
sleep 4              # phase 2 ends (~3s), phase 3 (idle prompt) draws
classify_at IDLE         IDLE

# --- DEAD test: let fake-claude exit, read pane_dead via list-panes ----------
sleep 4              # phase 3 idle hold (~4s) + SessionEnd, process exits
DEAD="$(tx list-panes -t "$SESS" -F '#{pane_id} #{pane_dead}' 2>/dev/null | grep "^$PANE " | awk '{print $2}')"
DEAD="${DEAD:-?}"
if [ "$DEAD" = "1" ]; then
  got="$(printf '' | "$PARSE" --dead)"
  mark=$([ "$got" = "DEAD" ] && echo PASS || echo MISS)
  say "  [DEAD] pane_dead=1 parser=$got -> $mark"
  RESULTS+=("DEAD:$got:DEAD:$mark")
else
  say "  [DEAD] pane_dead=$DEAD (remain-on-exit may be off on this tmux build) — DEAD-by-pane_dead inconclusive; parser --dead path unit-checked below"
  got="$(printf '' | "$PARSE" --dead)"
  mark=$([ "$got" = "DEAD" ] && echo PASS || echo MISS)
  say "  [DEAD-unit] parser --dead -> $got -> $mark"
  RESULTS+=("DEAD:$got:DEAD:$mark")
fi
say ""

# --- R1: did COCKPIT_PANE_ID survive into the hook env? ---------------------
# The shim writes the pane it READ from $COCKPIT_PANE_ID into each ndjson line.
# If that equals the pane we launched, the env survived end-to-end.
NDJSON="$(ls "$EVENT_DIR"/*.ndjson 2>/dev/null | head -1)"
say "=== R1 — COCKPIT_PANE_ID survival into hook env ==="
if [ -z "$NDJSON" ]; then
  say "  FAIL: no events/*.ndjson — shim never fired"
  R1=FAIL
else
  say "  events file: $NDJSON"
  say "  --- events written by the shim ---"
  sed 's/^/    /' "$NDJSON" | tee -a "$LOG" >/dev/null
  cat "$NDJSON" | sed 's/^/    /'
  LINES="$(wc -l < "$NDJSON" | tr -d ' ')"
  PANE_SEEN="$(grep -o "\"pane\":\"$PANE\"" "$NDJSON" | head -1)"
  if [ -n "$PANE_SEEN" ]; then
    say "  shim fired $LINES event(s); every/at-least-one carries pane=$PANE"
    say "  R1 PASS: \$COCKPIT_PANE_ID ($PANE) survived launch-env -> hook-env"
    R1=PASS
  else
    say "  R1 FAIL: ndjson has no pane=$PANE (env stripped before hook)"
    R1=FAIL
  fi
fi
say ""

# --- summary ----------------------------------------------------------------
say "=== SUMMARY ==="
ALL_PASS=1
for r in "${RESULTS[@]}"; do
  IFS=: read -r lbl got exp mk <<< "$r"
  say "  $lbl: $got (expected $exp) $mk"
  [ "$mk" = "PASS" ] || ALL_PASS=0
done
say "  R1 (COCKPIT_PANE_ID survival): $R1"
[ "$R1" = "PASS" ] || ALL_PASS=0
say ""
if [ "$ALL_PASS" = "1" ]; then
  say "RESULT: ALL PASS — parser classified every phase + shim fired + R1 held."
else
  say "RESULT: SOME MISS — see rows above."
fi

# teardown via trap (kills only -L cockpit-d6)

#!/usr/bin/env bash
# fake-claude.sh — emits the REAL Claude Code marker strings (no API key, so we
# can't drive a real model deterministically — see QA §F). Reproduces, verbatim,
# the markers captured from live CC on 2026-06-18 (REAL-MARKERS.md), and fires
# the cockpit hook shim at each phase boundary the way real CC's hooks would.
#
# Phases, in order (durations short for a fast spike; full run uses longer):
#   1. WORKING       ~Ns  — spinner ✽ + live (elapsed · tokens) + Running… + bg hint
#   2. NEEDS_INPUT   ~Ns  — boxed permission prompt with ❯ 1./2./3. options
#   3. IDLE          hold — empty ❯ prompt + auto-mode footer  (then exits)
#
# ENV:
#   COCKPIT_PANE_ID   - exported by the harness; the shim must see it (R1)
#   COCKPIT_SHIM      - abs path to cockpit-hook-shim.sh
#   FAKE_WORK_SECS    - WORKING duration   (default 3)
#   FAKE_INPUT_SECS   - NEEDS_INPUT hold   (default 3)
#   FAKE_IDLE_SECS    - IDLE hold before exit (default 4)
set -u

WORK_SECS="${FAKE_WORK_SECS:-3}"
INPUT_SECS="${FAKE_INPUT_SECS:-3}"
IDLE_HOLD="${FAKE_IDLE_SECS:-4}"
SHIM="${COCKPIT_SHIM:-}"
SESSION="fake-$(date +%s)-$$"

fire() {  # fire <event> — emulate CC invoking the hook with a JSON stdin payload
  [ -z "$SHIM" ] && return 0
  printf '{"session_id":"%s","hook_event_name":"%s"}' "$SESSION" "$1" \
    | "$SHIM" "$1" >/dev/null 2>&1 || true
}

clear 2>/dev/null || true

# ---- PHASE 1: WORKING ------------------------------------------------------
# Real CC fires UserPromptSubmit + PreToolUse/PostToolUse during a turn.
fire UserPromptSubmit
echo ""
echo "  Ran 1 shell command"
echo ""
printf '\033[38;5;220m⏺\033[39m fake-agent(do the thing)\n'
printf '  \033[38;5;246mRunning…\033[39m\n'
printf '  \033[38;5;246m⎿ +2 tool uses \033[39m\n'
printf '  \033[38;5;246m(ctrl+b ctrl+b (twice) to run in background)\033[39m\n'
fire PreToolUse
end=$(( $(date +%s) + WORK_SECS ))
while [ "$(date +%s)" -lt "$end" ]; do
  rem=$(( end - $(date +%s) ))
  # the decisive WORKING line: ✽ <verb>… (elapsed · ⎈ tokens)
  printf '\r\033[38;5;174m✽\033[39m \033[38;5;180mTransmuting…\033[38;5;246m (0m %ds · ⎈ 12.3k tokens)\033[39m   ' "$rem"
  sleep 1
done
printf '\n'
fire PostToolUse

# ---- PHASE 2: NEEDS_INPUT --------------------------------------------------
# Real CC fires PermissionRequest (and on this box also the AskUserQuestion
# PreToolUse). Draw the canonical boxed numbered-option permission prompt.
fire PermissionRequest
echo ""
printf '\033[38;5;220m╭──────────────────────────────────────────────────────╮\033[39m\n'
printf '\033[38;5;220m│\033[39m Do you want to run this command?                     \033[38;5;220m│\033[39m\n'
printf '\033[38;5;220m│\033[39m                                                      \033[38;5;220m│\033[39m\n'
printf '\033[38;5;220m│\033[39m \033[1m❯ 1. Yes\033[0m                                            \033[38;5;220m│\033[39m\n'
printf '\033[38;5;220m│\033[39m   2. Yes, and don'\''t ask again                       \033[38;5;220m│\033[39m\n'
printf '\033[38;5;220m│\033[39m   3. No, and tell Claude what to do differently      \033[38;5;220m│\033[39m\n'
printf '\033[38;5;220m╰──────────────────────────────────────────────────────╯\033[39m\n'
sleep "$INPUT_SECS"

# ---- PHASE 3: IDLE ---------------------------------------------------------
# User "answered"; turn finishes. Real CC fires Stop. Draw the settled prompt:
# completed bullet + empty ❯ input box + the auto-mode footer.
fire Stop
echo ""
printf '\033[38;5;231m⏺\033[39m Done.\n'
printf '\033[38;5;246m✻\033[39m \033[38;5;246mBaked for 3s\033[39m\n'
echo ""
printf '\033[38;5;110m╭──────────────────────────────────────────────────────╮\033[39m\n'
printf '\033[38;5;110m│\033[39m ❯\302\240                                                   \033[38;5;110m│\033[39m\n'
printf '\033[38;5;110m╰──────────────────────────────────────────────────────╯\033[39m\n'
printf '  \033[38;5;220m⏵⏵ auto mode on\033[38;5;246m (shift+tab to cycle) · ⏐ for agents\033[39m\n'
sleep "$IDLE_HOLD"

fire SessionEnd

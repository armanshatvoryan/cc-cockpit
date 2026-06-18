#!/usr/bin/env bash
# cockpit-hook-shim.sh — D6 Source A (authoritative push state).
#
# Wired into CC's settings.json hooks. On each hook event CC invokes this with
# the event payload on stdin (CC hook contract: JSON on stdin). The shim:
#   1. reads $COCKPIT_PANE_ID  (exported into the pane env at launch — R1 test)
#   2. reads the CC session id from the stdin payload (.session_id) if present
#   3. reads the hook event name from $CLAUDE_HOOK_EVENT or arg $1 or payload
#   4. appends ONE NDJSON line to events/<sessionId>.ndjson
#
# Append-only, never blocks CC, always exits 0 (a failing hook must not break
# the user's turn). No deps beyond coreutils; jq used only if available.
#
# NDJSON line shape (backend §3):
#   {"ts":<ms>,"session":"<cc-session-id>","pane":"%3","event":"Stop"}
#
# ENV (set by the cockpit at launch / by CC at hook time):
#   COCKPIT_PANE_ID   - e.g. "%3"  (cockpit exports this; the R1 survival test)
#   COCKPIT_EVENT_DIR - dir for ndjson; default <this-script-dir>/events
#   CLAUDE_HOOK_EVENT - hook name if CC exports it (varies by build)

EVENT_DIR="${COCKPIT_EVENT_DIR:-$(cd "$(dirname "$0")" && pwd)/events}"
mkdir -p "$EVENT_DIR" 2>/dev/null || true

PANE="${COCKPIT_PANE_ID:-unknown}"

# Read stdin payload (may be empty if invoked manually / by a build w/o stdin).
PAYLOAD=""
if [ ! -t 0 ]; then PAYLOAD="$(cat 2>/dev/null || true)"; fi

# Extract a top-level "key":"value" string from the JSON payload. Uses jq when
# present, else a portable grep/sed fallback (jq is absent on this box).
json_str() {  # json_str <key>  -> value or empty
  local key="$1"
  if command -v jq >/dev/null 2>&1; then
    printf '%s' "$PAYLOAD" | jq -r ".${key} // empty" 2>/dev/null
  else
    printf '%s' "$PAYLOAD" \
      | grep -oE "\"${key}\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" \
      | head -1 | sed -E "s/.*:[[:space:]]*\"([^\"]*)\".*/\1/"
  fi
}

# Event name: prefer explicit arg, then CC env, then payload.hook_event_name.
EVENT="${1:-${CLAUDE_HOOK_EVENT:-}}"
SESSION="unknown"
if [ -n "$PAYLOAD" ]; then
  s="$(json_str session_id)"
  [ -n "$s" ] && SESSION="$s"
  if [ -z "$EVENT" ]; then
    e="$(json_str hook_event_name)"
    [ -n "$e" ] && EVENT="$e"
  fi
fi
[ -z "$EVENT" ] && EVENT="unknown"

# Millisecond timestamp (portable: date +%s then *1000; macOS date lacks %3N).
TS="$(( $(date +%s) * 1000 ))"

# Escape the few JSON-special chars that could appear in ids/event.
esc() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }

printf '{"ts":%s,"session":"%s","pane":"%s","event":"%s"}\n' \
  "$TS" "$(esc "$SESSION")" "$(esc "$PANE")" "$(esc "$EVENT")" \
  >> "$EVENT_DIR/${SESSION}.ndjson"

exit 0

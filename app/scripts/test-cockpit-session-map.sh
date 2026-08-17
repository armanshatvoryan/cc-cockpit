#!/bin/bash
# Tests for the cockpit-session-map hook.
#
# The hook is the only bridge between a running Claude session and the cockpit
# (Claude does not announce its session id, so the hook pushes it out keyed by
# tmux pane). Everything it does is observable from a temp dir, so it is tested
# by piping real hook payloads at it and inspecting what lands on disk.
#
# Run: bash app/scripts/test-cockpit-session-map.sh
set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
hook="$script_dir/cockpit-session-map.sh"

root="$(mktemp -d "${TMPDIR:-/tmp}/cockpit-session-map-test.XXXXXX")"
trap 'rm -rf "$root"' EXIT

pass=0
fail=0

ok()   { printf '  ok   %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf '  FAIL %s\n     %s\n' "$1" "$2"; fail=$((fail + 1)); }

# Run the hook with a given payload + environment. Never inherits the real
# tmux/session env, so the suite is identical inside and outside the cockpit.
run_hook() {
  local payload="$1" pane="$2" tmux_env="$3"
  env -u TMUX -u TMUX_PANE \
    COCKPIT_SESSION_MAP_DIR="$root/map" \
    ${pane:+TMUX_PANE="$pane"} \
    ${tmux_env:+TMUX="$tmux_env"} \
    bash "$hook" <<<"$payload" >/dev/null 2>&1
}

start_payload() {
  cat <<JSON
{"hook_event_name":"SessionStart","source":"startup","session_id":"$1",
 "cwd":"/Users/me/Workflows","transcript_path":"/t/$1.jsonl"}
JSON
}

end_payload() {
  cat <<JSON
{"hook_event_name":"SessionEnd","session_id":"$1","cwd":"/Users/me/Workflows",
 "transcript_path":"/t/$1.jsonl"}
JSON
}

# Read one field out of the single map file, or print nothing.
field() {
  python3 - "$root/map" "$1" <<'PY'
import json, os, sys
d, key = sys.argv[1], sys.argv[2]
if not os.path.isdir(d):
    sys.exit()
for name in sorted(os.listdir(d)):
    if name.endswith(".json"):
        try:
            print(json.load(open(os.path.join(d, name))).get(key, ""))
        except Exception:
            pass
        break
PY
}

count_files() {
  ls -1 "$root/map"/*.json 2>/dev/null | wc -l | tr -d ' '
}

echo "cockpit-session-map"

# --- writes the session id for a live pane -----------------------------------
rm -rf "$root/map"
run_hook "$(start_payload aaaa-1111)" "%56" "/private/tmp/tmux-501/cockpit,1497,0"
got="$(field session_id)"
[[ "$got" == "aaaa-1111" ]] \
  && ok "writes session_id for a live pane" \
  || bad "writes session_id for a live pane" "got '$got'"

# --- records the tmux server pid so stale entries can be dropped -------------
got="$(field tmux_server_pid)"
[[ "$got" == "1497" ]] \
  && ok "records the tmux server pid" \
  || bad "records the tmux server pid" "got '$got' want '1497'"

# --- records the pane id with its sigil, matching tmux #{pane_id} ------------
got="$(field tmux_pane)"
[[ "$got" == "%56" ]] \
  && ok "records the pane id with sigil" \
  || bad "records the pane id with sigil" "got '$got' want '%56'"

# --- stamps when the entry was written ---------------------------------------
# The hook payload carries no timestamp, so the hook must stamp it itself; an
# empty field would make a stale-looking entry indistinguishable from a fresh one.
got="$(field started_at)"
[[ "$got" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2} ]] \
  && ok "stamps started_at" \
  || bad "stamps started_at" "got '$got', want an ISO-8601 timestamp"

# --- SessionEnd removes the entry --------------------------------------------
run_hook "$(end_payload aaaa-1111)" "%56" "/private/tmp/tmux-501/cockpit,1497,0"
got="$(count_files)"
[[ "$got" == "0" ]] \
  && ok "SessionEnd clears the entry" \
  || bad "SessionEnd clears the entry" "$got file(s) left"

# --- outside tmux the hook is a no-op ----------------------------------------
rm -rf "$root/map"
run_hook "$(start_payload bbbb-2222)" "" ""
got="$(count_files)"
[[ "$got" == "0" ]] \
  && ok "no-op outside tmux" \
  || bad "no-op outside tmux" "wrote $got file(s) with no TMUX_PANE"

# --- two tmux servers with the same pane number do not collide ---------------
# The cockpit socket and a default-socket tmux both number panes from %0, so a
# pane-number-only filename would let one server silently overwrite the other.
rm -rf "$root/map"
run_hook "$(start_payload cockpit-sess)" "%1" "/private/tmp/tmux-501/cockpit,1497,0"
run_hook "$(start_payload default-sess)" "%1" "/private/tmp/tmux-501/default,2222,0"
got="$(count_files)"
[[ "$got" == "2" ]] \
  && ok "same pane number on two servers keeps both" \
  || bad "same pane number on two servers keeps both" "got $got file(s), want 2"

# --- a malformed pane id is refused, not interpolated ------------------------
# TMUX_PANE reaches a filesystem path, so anything but %<digits> is rejected
# rather than sanitised.
rm -rf "$root/map"
run_hook "$(start_payload evil)" '%1/../../escape' "/private/tmp/tmux-501/cockpit,1497,0"
run_hook "$(start_payload evil2)" 'not-a-pane' "/private/tmp/tmux-501/cockpit,1497,0"
got="$(count_files)"
[[ "$got" == "0" ]] \
  && ok "refuses a malformed pane id" \
  || bad "refuses a malformed pane id" "wrote $got file(s)"
[[ ! -e "$root/escape.json" && ! -e "$root/map/../escape.json" ]] \
  && ok "no path traversal outside the map dir" \
  || bad "no path traversal outside the map dir" "escaped the map dir"

# --- a payload with no session_id writes nothing -----------------------------
rm -rf "$root/map"
run_hook '{"hook_event_name":"SessionStart","cwd":"/w"}' "%56" "/private/tmp/tmux-501/cockpit,1497,0"
got="$(count_files)"
[[ "$got" == "0" ]] \
  && ok "no session_id, no entry" \
  || bad "no session_id, no entry" "wrote $got file(s)"

# --- malformed stdin does not crash or write --------------------------------
rm -rf "$root/map"
run_hook '{ not json at all' "%56" "/private/tmp/tmux-501/cockpit,1497,0"
got="$(count_files)"
[[ "$got" == "0" ]] \
  && ok "malformed payload is ignored" \
  || bad "malformed payload is ignored" "wrote $got file(s)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]

#!/bin/bash
# cockpit-session-map — publish this Claude session's id, keyed by tmux pane.
#
# Claude Code does not expose its session id to the outside, so the cockpit
# cannot ask. This hook inverts that: it runs INSIDE the pane's process tree, so
# it inherits $TMUX_PANE, and it receives `session_id` on stdin. Writing the two
# together gives the cockpit an exact pane -> session mapping with no guessing
# from file mtimes (several panes routinely share one cwd here, so mtime
# heuristics would mis-attribute).
#
# Install: register on BOTH SessionStart and SessionEnd in settings.json.
#   SessionStart -> write/refresh the entry   SessionEnd -> remove it
#
# `/clear` and `--resume` both re-fire SessionStart, so the entry self-corrects;
# `compact` keeps the same id and the rewrite is a no-op in effect.
#
# Always exits 0. A hook that fails must never block a session from starting.
#
# Tests: app/scripts/test-cockpit-session-map.sh
set -uo pipefail

payload="$(cat)"

# Outside tmux there is nothing to key on — the cockpit is the only consumer.
[[ -n "${TMUX_PANE:-}" ]] || exit 0

# TMUX_PANE lands in a filesystem path, so it is validated in full rather than
# sanitised: tmux only ever sets %<digits>, and anything else is a bug or an
# attack, never something to repair.
[[ "$TMUX_PANE" =~ ^%[0-9]+$ ]] || exit 0

# $TMUX is "<socket-path>,<server-pid>,<session-index>". The server pid is what
# makes a stale entry detectable: pane ids restart from %0 when a tmux server
# dies, so without it a leftover file would name the wrong session.
IFS=, read -r _socket server_pid _index <<<"${TMUX:-}"
[[ "${server_pid:-}" =~ ^[0-9]+$ ]] || exit 0

map_dir="${COCKPIT_SESSION_MAP_DIR:-${CLAUDE_CONFIG_DIR:-$HOME/.claude}/cockpit-sessions}"

# Keyed by server pid AND pane number: the cockpit's private `-L cockpit` socket
# and a default-socket tmux both number panes from %0, so a pane-only filename
# would let one server silently overwrite the other's entry.
entry="$map_dir/${server_pid}-${TMUX_PANE#%}.json"

TMUX_PANE="$TMUX_PANE" SERVER_PID="$server_pid" ENTRY="$entry" MAP_DIR="$map_dir" \
python3 - "$payload" <<'PY' || exit 0
import datetime, json, os, sys, tempfile

raw = sys.argv[1] if len(sys.argv) > 1 else ""
try:
    payload = json.loads(raw)
except Exception:
    sys.exit(0)                      # malformed stdin: nothing to publish
if not isinstance(payload, dict):
    sys.exit(0)

entry = os.environ["ENTRY"]

if payload.get("hook_event_name") == "SessionEnd":
    try:
        os.remove(entry)
    except OSError:
        pass                         # already gone is the desired state
    sys.exit(0)

session_id = payload.get("session_id") or ""
if not session_id:
    sys.exit(0)                      # nothing worth publishing

record = {
    "tmux_pane": os.environ["TMUX_PANE"],
    "session_id": session_id,
    "cwd": payload.get("cwd") or "",
    "transcript_path": payload.get("transcript_path") or "",
    "tmux_server_pid": os.environ["SERVER_PID"],
    # The payload carries no timestamp, so stamp it here — otherwise a stale
    # entry is indistinguishable from one written a second ago.
    "started_at": datetime.datetime.now(datetime.timezone.utc)
    .replace(microsecond=0)
    .isoformat()
    .replace("+00:00", "Z"),
}

map_dir = os.environ["MAP_DIR"]
os.makedirs(map_dir, exist_ok=True)

# Written atomically: the cockpit polls this directory continuously, and a
# half-written file would read as corrupt at exactly the wrong moment.
fd, tmp = tempfile.mkstemp(dir=map_dir, suffix=".tmp")
try:
    with os.fdopen(fd, "w") as fh:
        json.dump(record, fh)
    os.replace(tmp, entry)
except Exception:
    try:
        os.remove(tmp)
    except OSError:
        pass
PY

exit 0

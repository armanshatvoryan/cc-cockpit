#!/bin/bash
# Install the cockpit-session-map hook.
#
# The hook is COPIED to ~/.claude/hooks/ rather than referenced in place: hooks
# fire from every Claude session on the machine, and pointing settings.json at a
# git worktree would break the moment that worktree is removed or the branch is
# switched.
#
# Re-run after changing app/scripts/cockpit-session-map.sh to push the update.
#
# Run: bash app/scripts/install-session-map.sh
set -euo pipefail

src_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
src="$src_dir/cockpit-session-map.sh"
[[ -f "$src" ]] || { echo "missing $src" >&2; exit 1; }

config_dir="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
dest_dir="$config_dir/hooks"
dest="$dest_dir/cockpit-session-map.sh"

mkdir -p "$dest_dir"
install -m 0755 "$src" "$dest"
echo "installed $dest"

# Verify the copy actually runs before telling anyone to wire it up.
probe_dir="$(mktemp -d)"
trap 'rm -rf "$probe_dir"' EXIT
env -u TMUX -u TMUX_PANE \
  COCKPIT_SESSION_MAP_DIR="$probe_dir" \
  TMUX_PANE="%0" TMUX="/tmp/probe,$$,0" \
  bash "$dest" <<JSON >/dev/null 2>&1
{"hook_event_name":"SessionStart","session_id":"install-probe","cwd":"/","transcript_path":"/x"}
JSON
if ! grep -q install-probe "$probe_dir"/*.json 2>/dev/null; then
  echo "the installed hook did not write an entry -- not wiring it up" >&2
  exit 1
fi
echo "probe ok: the installed hook writes entries"

# --- the cc-sessions CLI ------------------------------------------------------
# Copied, not symlinked, for the same reason as the hook: a symlink into a git
# worktree dies with the worktree. Re-run this script after a rebuild.
cli_src="$src_dir/../src-tauri/target/release/cc-sessions"
if [[ -x "$cli_src" ]]; then
  mkdir -p "$HOME/bin"
  install -m 0755 "$cli_src" "$HOME/bin/cc-sessions"
  echo "installed $HOME/bin/cc-sessions"
else
  echo "note: no release binary yet -- build it with:" >&2
  echo "  cargo build --release -p claude-sessions --bin cc-sessions" >&2
fi

cat <<EOF

Add to $config_dir/settings.json (both events -- SessionStart publishes,
SessionEnd clears):

  "SessionStart": [{ "hooks": [{ "type": "command",
      "command": "$dest", "timeout": 10 }] }],
  "SessionEnd":   [{ "hooks": [{ "type": "command",
      "command": "$dest", "timeout": 10 }] }]

A SessionStart entry already exists here (brain-session-start.sh); append to its
"hooks" array rather than replacing it.
EOF

#!/bin/bash
# Tests for the `cc-sessions` CLI.
#
# The last test is the important one: it runs the real hook and then reads the
# result back through the real Rust reader. Those are the two halves of the
# feature written in different languages against a shared on-disk contract, and
# a field-name drift between them would be invisible to either side's own suite.
#
# Run: bash app/scripts/test-cc-sessions.sh
set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tauri_dir="$(cd "$script_dir/../src-tauri" && pwd)"
hook="$script_dir/cockpit-session-map.sh"

echo "building cc-sessions..."
if ! (cd "$tauri_dir" && cargo build -q -p claude-sessions --bin cc-sessions 2>&1); then
  echo "build FAILED" >&2
  exit 1
fi
cli="$tauri_dir/target/debug/cc-sessions"

root="$(mktemp -d "${TMPDIR:-/tmp}/cc-sessions-test.XXXXXX")"
trap 'rm -rf "$root"' EXIT

pass=0
fail=0
ok()  { printf '  ok   %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf '  FAIL %s\n     %s\n' "$1" "$2"; fail=$((fail + 1)); }

# A fake ~/.claude the CLI is pointed at, so the real store is never read.
fake_home="$root/claude"
mkdir -p "$fake_home/projects/-Users-me-Workflows-cc-cockpit"

cat > "$fake_home/projects/-Users-me-Workflows-cc-cockpit/aaaa-1111.jsonl" <<'JSON'
{"type":"mode","mode":"normal"}
{"type":"user","cwd":"/Users/me/Workflows/cc-cockpit","message":{"content":"fix the tab naming bug"}}
JSON
sleep 1
cat > "$fake_home/projects/-Users-me-Workflows-cc-cockpit/bbbb-2222.jsonl" <<'JSON'
{"type":"user","cwd":"/Users/me/Workflows","message":{"content":"add a session id chip"}}
JSON

run_cli() {
  env -u TMUX -u TMUX_PANE CLAUDE_CONFIG_DIR="$fake_home" "$cli" "$@"
}

echo "cc-sessions"

# --- default listing shows the newest session first --------------------------
out="$(run_cli)"
[[ "$(head -1 <<<"$out")" == *bbbb-2222* ]] \
  && ok "lists newest session first" \
  || bad "lists newest session first" "first line: $(head -1 <<<"$out")"

# --- listing carries the prompt so a row is identifiable ---------------------
[[ "$out" == *"add a session id chip"* ]] \
  && ok "listing includes the first prompt" \
  || bad "listing includes the first prompt" "prompt missing from output"

# --- --json emits parseable JSON with the expected fields --------------------
json="$(run_cli --json)"
got="$(python3 -c '
import json,sys
rows=json.load(sys.stdin)
print(rows[0]["session_id"], rows[0]["cwd"], sep="|")' <<<"$json" 2>/dev/null)"
[[ "$got" == "bbbb-2222|/Users/me/Workflows" ]] \
  && ok "--json emits session_id and cwd" \
  || bad "--json emits session_id and cwd" "got '$got'"

# --- cwd comes from inside the transcript, not the directory name ------------
# The directory is "-Users-me-Workflows-cc-cockpit"; row aaaa-1111 must report
# the real hyphenated path, which that name cannot be decoded back into.
got="$(python3 -c '
import json,sys
rows=json.load(sys.stdin)
print(next(r["cwd"] for r in rows if r["session_id"]=="aaaa-1111"))' <<<"$json" 2>/dev/null)"
[[ "$got" == "/Users/me/Workflows/cc-cockpit" ]] \
  && ok "cwd read from the transcript, not the dir name" \
  || bad "cwd read from the transcript, not the dir name" "got '$got'"

# --- --limit caps the rows ---------------------------------------------------
got="$(run_cli --json --limit 1 | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')"
[[ "$got" == "1" ]] \
  && ok "--limit caps the rows" \
  || bad "--limit caps the rows" "got $got rows"

# --- --tsv is one record per line, tab separated -----------------------------
got="$(run_cli --tsv | head -1 | awk -F'\t' '{print NF}')"
[[ "${got:-0}" -ge 4 ]] \
  && ok "--tsv emits tab-separated fields" \
  || bad "--tsv emits tab-separated fields" "got $got fields"

# --- END TO END: hook writes, CLI reads it back ------------------------------
# The cross-language contract. Runs the real hook, then asks the real reader.
map_dir="$fake_home/cockpit-sessions"
env -u TMUX -u TMUX_PANE \
  COCKPIT_SESSION_MAP_DIR="$map_dir" \
  TMUX_PANE="%56" TMUX="/private/tmp/tmux-501/cockpit,$$,0" \
  bash "$hook" <<JSON >/dev/null 2>&1
{"hook_event_name":"SessionStart","session_id":"e2e-session-id","cwd":"/w","transcript_path":"/t/e.jsonl"}
JSON

got="$(env -u TMUX -u TMUX_PANE \
  CLAUDE_CONFIG_DIR="$fake_home" \
  TMUX_PANE="%56" TMUX="/private/tmp/tmux-501/cockpit,$$,0" \
  "$cli" --current 2>/dev/null)"
[[ "$got" == "e2e-session-id" ]] \
  && ok "END TO END: hook write -> --current reads it back" \
  || bad "END TO END: hook write -> --current reads it back" "got '$got'"

# --- --panes lists the mapping ----------------------------------------------
got="$(env -u TMUX -u TMUX_PANE \
  CLAUDE_CONFIG_DIR="$fake_home" \
  TMUX_PANE="%56" TMUX="/private/tmp/tmux-501/cockpit,$$,0" \
  "$cli" --panes 2>/dev/null)"
[[ "$got" == *"%56"* && "$got" == *"e2e-session-id"* ]] \
  && ok "--panes lists pane and session" \
  || bad "--panes lists pane and session" "got '$got'"

# --- a stale entry from a dead tmux server is not reported -------------------
got="$(env -u TMUX -u TMUX_PANE \
  CLAUDE_CONFIG_DIR="$fake_home" \
  TMUX_PANE="%56" TMUX="/private/tmp/tmux-501/cockpit,999999,0" \
  "$cli" --current 2>/dev/null)"
[[ -z "$got" ]] \
  && ok "stale server entry is not reported as current" \
  || bad "stale server entry is not reported as current" "got '$got'"

# --- --current outside a mapped pane fails cleanly ---------------------------
run_cli --current >/dev/null 2>&1
[[ "$?" -ne 0 ]] \
  && ok "--current exits non-zero when unmapped" \
  || bad "--current exits non-zero when unmapped" "exited 0 with no mapping"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]

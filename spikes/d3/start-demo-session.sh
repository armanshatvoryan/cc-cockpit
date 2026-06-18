#!/usr/bin/env bash
# Start the private demo session the D3 Tauri app attaches to.
# SAFE: private socket `cockpit-d3` only — never touches default / -L cockpit /
# -L cockpit-d6. Tear down with `tmux -L cockpit-d3 kill-server`.
#
# Run this BEFORE `npm run dev` in tauri-app/. The frontend attaches to
# socket=cockpit-d3 session=d3live (see frontend/src/App.tsx).
set -eu
SOCK=cockpit-d3
SESSION=d3live

tmux -L "$SOCK" kill-server 2>/dev/null || true
sleep 0.2
tmux -L "$SOCK" new-session -d -s "$SESSION" -x 120 -y 32
tmux -L "$SOCK" set-option -g status off
echo "Demo session up:"
tmux -L "$SOCK" list-panes -t "$SESSION" -F '  #{pane_id}  #{pane_width}x#{pane_height}'
echo
echo "Now run, in two terminals:"
echo "  (this socket stays alive)  — leave it"
echo "  cd $(cd "$(dirname "$0")" && pwd)/tauri-app && npm install && npm run dev"
echo
echo "Stress-proxy TUI to type into pane %0 once the app is up:"
echo "  vim   (alt-screen + box-drawing)   |   htop / top   (live full-screen)"
echo
echo "Teardown when done:  tmux -L $SOCK kill-server"

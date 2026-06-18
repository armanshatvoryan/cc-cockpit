#!/usr/bin/env bash
# Second capture: exercise %window-add, %error, %pane-mode-changed, and %exit
# with a reason. Saved to capture/control2.raw.
# SAFE: private socket only.
set -u
SOCK=cockpit-d3
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/capture"
mkdir -p "$OUT"

tmux -L "$SOCK" kill-server 2>/dev/null
sleep 0.3
tmux -L "$SOCK" new-session -d -s d3 -x 80 -y 24
tmux -L "$SOCK" set-option -g status off

CMDFIFO="$OUT/cmd2.fifo"
rm -f "$CMDFIFO"; mkfifo "$CMDFIFO"
( tmux -L "$SOCK" -C attach -t d3 < "$CMDFIFO" > "$OUT/control2.raw" 2> "$OUT/control2.err" ) &
CC_PID=$!
exec 3> "$CMDFIFO"
send() { printf '%s\n' "$1" >&3; sleep "${2:-0.4}"; }

sleep 0.9
# 1) New window -> %window-add + %layout-change + %window-pane-changed.
send 'new-window -t d3' 0.9
# 2) An invalid command -> %begin/%error/%end block.
send 'this-is-not-a-command' 0.7
# 3) Enter copy-mode in a pane -> %pane-mode-changed.
send 'copy-mode -t d3' 0.7
send 'send-keys -t d3 -X cancel' 0.6
sleep 0.5
printf 'detach-client\n' >&3
sleep 0.6
exec 3>&-
wait $CC_PID 2>/dev/null
rm -f "$CMDFIFO"
tmux -L "$SOCK" kill-server 2>/dev/null

echo "=== control2.raw bytes ===" >&2
wc -c "$OUT/control2.raw" >&2

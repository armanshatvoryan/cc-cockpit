#!/usr/bin/env bash
# Capture REAL tmux control-mode (-CC) protocol bytes for the D3 parser unit test.
#
# Drives a private `-L cockpit-d3` session, attaches a control client, runs a
# scripted set of actions (output, split, resize, kill-pane) and saves the raw
# protocol stream verbatim to capture/control.raw so the Rust parser can be
# tested against bytes tmux ACTUALLY produced (octal escapes, %begin/%end,
# topology events).
#
# SAFE: private socket only. Never touches default / -L cockpit / -L cockpit-d6.
set -u
SOCK=cockpit-d3
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/capture"
mkdir -p "$OUT"

tmux -L "$SOCK" kill-server 2>/dev/null
sleep 0.3

# Deterministic geometry.
tmux -L "$SOCK" new-session -d -s d3 -x 80 -y 24
tmux -L "$SOCK" set-option -g status off
PANE0=$(tmux -L "$SOCK" list-panes -t d3 -F '#{pane_id}' | head -1)
echo "pane0=$PANE0" >&2

# ---- Control client driver -------------------------------------------------
# tmux -CC reads commands on stdin, writes protocol to stdout. Pipe a timed
# command script in via a fifo so output has time to flush between actions.
CMDFIFO="$OUT/cmd.fifo"
rm -f "$CMDFIFO"; mkfifo "$CMDFIFO"

( tmux -L "$SOCK" -C attach -t d3 < "$CMDFIFO" > "$OUT/control.raw" 2> "$OUT/control.err" ) &
CC_PID=$!

# Hold the fifo open for writing on fd 3.
exec 3> "$CMDFIFO"
send() { printf '%s\n' "$1" >&3; sleep "${2:-0.4}"; }

# Let the attach handshake + initial %output settle.
sleep 0.9

# 1) Plain text output (exercises %output octal escaping of \r \n + printable).
send 'send-keys -t '"$PANE0"' "printf '\''Hello\\tD3\\r\\n'\''" Enter' 0.7
# 2) ANSI-colour + box-drawing burst (ESC seqs inside %output).
send 'send-keys -t '"$PANE0"' "printf '\''\\033[31mRED\\033[0m box\\r\\n'\''" Enter' 0.7
# 3) Split -> %window-add? + %layout-change (topology).
send 'split-window -t d3 -h' 0.9
# 4) Output in the second pane.
send 'send-keys -t d3 "echo second-pane" Enter' 0.7
# 5) Resize the client (drives %layout-change).
send 'refresh-client -C 100,30' 0.7
# 6) Kill the second pane -> %layout-change / pane removal.
send 'kill-pane -t d3.1' 0.9
# 7) More output, proving the stream survives topology churn.
send 'send-keys -t '"$PANE0"' "echo after-kill" Enter' 0.7

sleep 0.7
printf 'detach-client\n' >&3   # -> %exit
sleep 0.6
exec 3>&-
wait $CC_PID 2>/dev/null

rm -f "$CMDFIFO"
tmux -L "$SOCK" kill-server 2>/dev/null

echo "=== captured bytes (control.raw) ===" >&2
wc -c "$OUT/control.raw" >&2
echo "=== control.err ===" >&2
cat "$OUT/control.err" >&2

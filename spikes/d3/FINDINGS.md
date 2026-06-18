# D3 spike — xterm.js ↔ tmux control-mode via Tauri + SolidJS

**Verdict: GREEN** (2026-06-18). Control mode drives the bridge cleanly. Everything verifiable headless is built + passing; only a live GUI pixel/throughput eyeball remains (confirmation, not open risk).

## Question
Can xterm.js render a real Claude Code TUI via **tmux control mode (`tmux -CC`)** through a **Tauri (Rust) ↔ SolidJS** bridge, with input round-trip + resize? Transport choice = control mode (not pipe-pane).

## What was built + verified (headless, against real tmux)

1. **Control-mode parser (the load-bearing unknown) — PROVEN.**
   `crate/control-mode/src/lib.rs` — streaming `Parser::feed(&[u8]) -> Vec<Event>`. Handles `%begin/%end/%error` blocks (error text captured between `%begin` and `%error`, as tmux actually emits), `%output %<paneid> <data>` with octal decode (`\033`→ESC etc.), `%window-add`, `%window-close`, `%layout-change`, `%window-pane-changed`, `%pane-mode-changed`, `%session-changed`, `%exit`, + a catch-all (nothing silently dropped).
   Real-frame test `crate/control-mode/tests/real_frames.rs` fed by bytes captured verbatim from a live `tmux -CC attach` (`crate/control-mode/tests/fixtures/*.raw`). Asserts decoded pane output, ≥3 layout-changes (split→resize 100x30→kill→collapse), paired begin/end, the `%error` body, window-add/pane-mode/exit, and identical events whether fed whole or one byte at a time.
   **Independently re-run by the lead: 13 tests pass (7 unit + 6 real-frame), 0 fail.**

2. **Engine + live end-to-end proof.** `crate/engine/src/lib.rs` — `ControlClient::attach()` spawns `tmux -L <sock> -C attach`, pumps stdout through the parser on a reader thread, emits `Outbound::{PaneData(base64), Topology, Exit}`. `pane_send_keys` (literal), `interrupt_pane` (binary-safe `send-keys -H`), `pane_resize`. Harness `crate/engine/src/bin/live_bridge.rs` ran live twice, exit 0: output PASS, input round-trip PASS (echoed token observed), resize PASS (100x30 layout-change), split-topology PASS — split driven by a *separate plain* `tmux split-window` (proves the detachable-from-plain-terminal promise / D3-e).

3. **Tauri 2 command layer — `cargo build` succeeds** (50.84s, exit 0, zero warnings). `tauri-app/src-tauri/src/lib.rs` — thin shell over the engine: commands `attach_session`, `pane_send_keys`, `pane_resize`, `interrupt_pane`; forwarder thread maps `Outbound` → `app.emit("pane:data" / "pane:topology")`.

4. **SolidJS frontend — `tsc --noEmit` 0 errors, `vite build` passes** (404KB → 103KB gzip). `frontend/src/XtermHost.tsx` — imperative isolated leaf: xterm + addon-fit + addon-webgl (canvas fallback); `pane:data`→`term.write(base64-decode)`; `onData`→`paneSendKeys` (fire-and-forget); ResizeObserver(50ms)→`fit()`→`paneResize`; `onCleanup`→`dispose()`+unlisten (HMR-safe).

## One real finding (parser is byte-exact)
First fidelity assertion failed on real bytes — root cause: a user typing `printf '\033…'` makes the shell echo literal chars; tmux encodes the echoed backslash as `\134`, decoding to a backslash byte + literal `033` text (real screen content, not a miss). Parser was right; assertion fixed to the correct invariant (no `\134` token survives; 165 real `\033` escapes all decode to 0x1B). Adversarial confirmation the decoder is exact.

## Socket hygiene
Private `-L cockpit-d3` only, torn down after each run. Default sockets (`0`/`1`/`cockpit`) and the sibling `-L cockpit-d6` untouched.

## Still needs YOUR live eyeball (D3-a / D3-d / D3-e) — confirmation, not open risk
```bash
cd ~/Workflows/cc-cockpit/spikes/d3
bash start-demo-session.sh                   # private session cockpit-d3 / d3live, pane %0
cd tauri-app && npm install && npm run dev    # tauri dev — window opens
# teardown: tmux -L cockpit-d3 kill-server
```
- **D3-a (hard-fail gate):** in the pane run `vim` (alt-screen + box-drawing), `htop`/`top` (colors + live redraw), then a real `claude`. If a correct VT stream still corrupts → spec hard-fail, reopen D3. Nothing in the spike points there.
- **D3-d (throughput):** `yes` or `cat 5MB` in %0 while typing another pane; expect >30fps + responsive typing. NOTE: spike forwards every `%output` immediately — no 16ms coalescing yet (that's the planned v1 mitigation), so visible jank under flood is expected work, not a control-mode failure.
- **D3-e (detachability):** from a separate terminal, `tmux -L cockpit-d3 split-window -t d3live -h` then `kill-pane -t d3live.1`; cockpit reflects both within ~2s (topology events already proven by live-bridge).

## Bottom line
Control mode drives the bridge cleanly — parser handles real frames, all three layers build, output+input+resize+topology data path verified live against real tmux. **GREEN.** Only open item: the user's visual pass on a real `claude`.

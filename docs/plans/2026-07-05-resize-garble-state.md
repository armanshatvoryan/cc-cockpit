# Resize/garble debugging — state doc

**Date:** 2026-07-17 → 2026-07-21 (EIGHT root causes over iterations #5-#6)
**Status:** 🟢 GREEN — arc CLOSED, live-verified + merged `aef5008` on 2026-07-21.
See the GREEN section at the end of this file for the final state. Everything
below is the historical investigation log, kept for the evidence; the YELLOW /
"pending verify" markers in it are POINT-IN-TIME and all superseded.

**Historical status at the time of writing:** YELLOW — iteration #5 built + installed + signed, NOT yet live-verified
(user must relaunch → fullscreen toggle + drag-resize + boot must self-heal).
Session closed 2026-07-17 ~5:15pm before verify. TEMP DEBUG still in working
tree AND installed binary. All changes UNCOMMITTED (on stale branch
feat/team-board-filter-cleanup — commit on fresh branch off main after verify).
Core insight: garble = tmux-grid↔xterm-grid seam; Terax immune (one grid, no
tmux). Only the app itself can repaint correctly → auto-^L, not capture-replay.

## ROOT CAUSE #2 (mode 3 — "�� replacement-char garble", found 2026-07-17 4pm)
User screenshots (before/after Ctrl+L): random `��` where multibyte UTF-8
should be (`·`, em-dash, 🚨) — random per chunk, layout width CORRECT.
- Verified live against tmux 3.6b (throwaway `-L dbgtest` server, split-emoji
  printf, raw control-stream capture): tmux octal-escapes ONLY C0 controls and
  backslash in `%output` — high bytes pass through RAW. A UTF-8 char split
  across pty reads arrives as invalid raw bytes, one half per %output line.
- Parser `handle_line` did `String::from_utf8_lossy(&raw)` BEFORE octal decode
  (control-mode/src/lib.rs:128; its "lossy is safe" comment was false for
  %output) → invalid halves → U+FFFD (EF BF BD) → xterm renders "��".
  Rest of pipeline (b64 → forwarder byte-concat → xterm streaming Utf8ToUtf32
  with cross-write interim) verified byte-safe — parser was the only lossy hop.
- FIX: byte-level `%output` fast-path at top of handle_line — payload never
  touches a String. TDD: new test `output_passes_raw_invalid_utf8_bytes_untouched`
  (failed with EF BF BD before fix, green after). Workspace: 124 pass, 0 fail.
- Explains "never happens in Terax": Terax streams pty bytes directly, no
  control-mode text round-trip.

## ROOT CAUSE (mode 1 — "push never landed", found 2026-07-17)
Reproduced 3:50pm: pane stuck at tmux BIRTH size 200x50, control client 80x —
the exact 2026-07-05 5:39pm signature. Chain:
- `cockpit_init` spawns the tmux server at app boot; app process was up since
  2:21pm but the server had started 3:50pm ⇒ a prior server died this app-run
  (last tab closed) and `create_tab_healing` rebuilt it (`cockpit:reconnected`).
- Webview survived with warm `lastGridKey` from the DEAD server. After rebirth,
  cell size unchanged ⇒ pushGrid computed the identical key ⇒ change-guard
  `if (key === lastGridKey) return` swallowed the push ⇒ new server NEVER got
  `refresh-client -C`. CC drew 200 cols into a ~161-col xterm → garble.
- Second hole, same handler: fresh servers reuse pane ids (%0…) so `<For>`
  keyed by id may not remount xterms ⇒ no reportCell either. (Old comment
  claimed "new ids ⇒ remount" — false; comment fixed.)
- Mechanism proven live: manual `refresh-client -t client-30521 -C 161,54`
  resized pane instantly (200x50 → 161x54) — nothing blocked resize, the push
  was simply never sent.

## FIX (uncommitted, in working tree + installed binary)
`app/frontend/src/store.ts`:
- new `gridServerReset()` — clears `lastGridKey`, schedules unconditional
  `pushGrid()`;
- `cockpit:reconnected` handler now runs `refreshState().then(gridServerReset)`
  (push scheduled AFTER panes reload; does not rely on xterm remounts).
Verified: tsc clean; cargo test --workspace 123 pass 0 fail; release binary
(`--features tauri/custom-protocol`) built with new dist + TEMP DEBUG kept,
swapped into /Applications + target bundle copy (unlink-then-copy; running
PID 14362 untouched). /Applications binary now = fixed + instrumented.

## Discovery that unblocked this
The Jul-5 "instrumented" claim was STALE — installed binary had ZERO
`cockpit-dbg` strings (18:40 rebuild lost the patch). That's why mode-1
instances kept appearing with no log. Now truly instrumented.

## Live verify (next launch — do this before declaring GREEN)
1. Quit + relaunch CC Cockpit from Finder.
2. Close last tab (server exits) → ⌘T (server rebirth via healing create_tab).
3. PASS #1 (width fix) = new pane NOT 200x50 (`tmux -L cockpit list-panes -a`),
   and /tmp/cockpit-dbg.log shows `refresh-client -C` fired after the rebirth.
4. PASS #2 (byte fix) = launch `claude` in a pane; banner `·` separators, hook
   em-dash rules, and legend emoji (esp. 🚨) render clean — no `��` — during
   LIVE streaming (not just after Ctrl+L).
5. Then: strip both TEMP DEBUG blocks (engine write_cmd + set_grid cmd),
   rebuild, reinstall, rerun suite, commit BOTH fixes on a fresh branch off
   main, push.

## ROOT CAUSE #3 (mode 4 — "garbles on EVERY window resize", found ~4:40pm)
User confirmed fixes #1/#2 live ("works good") but every window resize
re-garbled until manual Ctrl+L. Dbg log showed the push path CLEAN (single
set_grid, pane landed exact) → failure is the post-resize resync replay.
- CC is a differential renderer: repaints only lines IT thinks changed, so any
  xterm↔frame-model divergence persists until a full repaint (Ctrl+L).
- Old resync captured tmux's grid at a FIXED 320ms after set_grid; a drag's
  SIGWINCH storm outlasts that → replay baked in a stale mid-redraw frame →
  every later diff misaligned. Deterministic on drag-resizes.
- FIX (XtermHost.tsx): quiescence-gated resync — wait until pane output quiet
  250ms (capped 2s for streaming panes) before capture; plus dirty-check that
  retries when output lands while the capture RPC is in flight (else reset()
  wipes newer frames). Verify on live resize while CC busy: settles clean, no
  Ctrl+L.

## ROOT CAUSE #4 (mode 2 CONFIRMED — boot push storm, found ~4:45pm)
Post-relaunch screenshot: scattered layout at boot with CC in the pane. Dbg
log caught the storm: set_grid `154→154→65→118→163→181` (the Jul-5 "bogus
31x46" mechanism, live). Driver: webview settles in steps at boot (window
restore, sidebar mount, async setZoom re-metrics) and EVERY intermediate got
pushed after just 90ms quiet → overlapping SIGWINCH storm → CC's partial
differential redraws pollute TMUX'S OWN GRID → resync (correctly) replays the
polluted grid. Key insight: capture-replay can never fix storm damage — only
prevention (one clean transition) can; single-push resizes are proven clean
(3:53pm manual push, 16:31 single-push log).
- FIX (store.ts): first-push settle gate — while `lastGridKey === ""` (boot or
  post-reconnect), reportCell debounce is 500ms instead of 90ms, so the fitted
  size must hold stable before the FIRST push; settled UI keeps 90ms.
  `gridServerReset` fallback timer aligned to 500ms.
- Residual accepted: a settle step arriving >500ms after the first push (late
  zoom) yields a SECOND well-spaced push — a single clean transition, CC
  redraws fully, resync sweeps. Watch for it in the log if garble recurs.

## ITERATION #5 (~5:05pm) — capture-replay ABANDONED for auto-Ctrl+L
User screen recording: boot now CLEAN (settle gate works, default dir works),
but fullscreen toggles still scatter. Log showed 182↔154 alternation = user
toggling FS, one clean push each — so even single-transition + quiescence
capture-replay leaves scatter: the replay itself is the weak link (capture
races the redraw; trim_blank_edges vertical shift; row-width edge cases).
Meanwhile the user's manual Ctrl+L fixes 100% of cases — the APP is the only
renderer that can correctly repaint its own screen.
- FIX (XtermHost.tsx): resync now sends the pane a literal Ctrl+L (\x0c) after
  the quiescence gate, instead of capture-replay. warmStartScreen no longer
  used by resync (mount warm-start unchanged). CC/vim/less full-repaint;
  zsh clears + redraws prompt (input line preserved).
- FIX (store.ts): settled-UI debounce 90→350ms — discrete resize steps ~200ms
  apart (fullscreen animation reports) coalesce into one push.
- Residual risk to watch: auto-^L lands in a pane where ^L is bound to
  something odd (copy-mode, password prompt) — cosmetic, revisit if reported.

## QoL shipped 2026-07-17 ~4:20pm (same install, uncommitted)
- Default start dir: new-session + new-window get `-c ~/Workflows` (helper
  `tmux::default_cwd()`, falls back $HOME → "/"); split-window inherits source
  pane cwd via `-c '#{pane_current_path}'`. Unit-tested (`default_cwd_impl`).
  125 tests green.
- Recurring TCC permission prompts ROOT-CAUSED: binary swaps left the bundle
  `adhoc, linker-signed` with a per-build hash identifier
  (`cc_cockpit-<hash>`) — macOS saw a new app every rebuild. Now signed with
  the Apple Development identity (justmail01@icloud.com, team BN8BTA42RW) →
  stable identifier `studio.arag.cc-cockpit`. Expect ONE final prompt round
  after this identity change, then remembered.
- **Rebuild ritual from now on:** swap binary → `codesign --force --sign
  "Apple Development: justmail01@icloud.com (76SB26HX56)"` on
  Contents/MacOS/smoke, then on the .app — or prompts return.

## Still open (separate defects, NOT today's cause)
- Mode 2: bogus 31x46 mid-boot push — `reportCell` is global last-writer-wins;
  hidden/mid-transition xterm may report. Fix shape: only ACTIVE tab's visible
  panes report (or PaneGrid single reporter).
- Engine write_cmd is fire-and-forget: tmux `%error` frames swallowed, so a
  rejected push resolves OK and records the guard key. Root-robustness fix =
  reply correlation (%begin/%end/%error) for set_grid.
- Residual keystroke checks from Jul 5: ⌘zoom, relaunch→layout restore, dock
  badge.

## Debug technique that works here (reuse)
Finder-launch bugs: stderr lost — file-append dbglog to /tmp; instrument the
IPC boundary; reproduce env with `env -i` / `open`, never from a shell. Check
`strings <binary> | grep <marker>` before trusting "instrumented build
installed" claims. tmux server start time (`display-message -p '#{start_time}'`)
vs app process start time reveals server rebirths.

## ROOT CAUSE #6 (mode 5 — "tab switch garbles, NO resize", found 2026-07-20)
Tab switch alone corrupts panes. Evidence (live, app PID 91761):
- `PaneGrid` renders `<For each={activePanes()}>` and `activePanes()` returns the
  ACTIVE TAB ONLY (store.ts:109) ⇒ every tab switch UNMOUNTS the old tab's
  XtermHosts and MOUNTS the new tab's fresh+empty. Not a hide — a full teardown.
- Each fresh mount calls `warm_start` = `capture-pane -p -e -S -` (manager.rs:823)
  = the WHOLE scrollback replayed into the empty xterm. Live: %14 capture = 646
  lines (hist=567).
- **54 of those 646 lines are exactly pane width (94 cols).** capture-pane emits
  hard-wrapped lines joined by `\n` and does NOT distinguish soft wrap from real
  newline, so xterm re-wraps each full-width line and inserts a PHANTOM blank
  line ⇒ 54 spurious newlines ⇒ everything below shifts ⇒ garble. Arithmetic,
  not theory.
- CC runs in the NORMAL buffer (`alternate_on=0` for all 4 panes), i.e. it is a
  differential renderer with a row model. After the shifted replay its model no
  longer matches the xterm, so its next partial redraw lands at wrong rows.
- NO REPAIR PATH: both tabs are 2 panes @ 94x53 ⇒ a switch computes an IDENTICAL
  grid key ⇒ `pushGrid` change-guard early-returns (store.ts:428) ⇒
  `scheduleResync()` is never reached ⇒ no auto-^L. Exactly why it garbles
  "even without resizing": the iteration-#5 ^L masked this during resize tests
  but a same-count tab switch issues ZERO tmux commands.
- Note: this is the SAME capture-replay mechanism already abandoned for resync
  in iteration #5 ("only the app can repaint its own screen") — warm_start still
  runs it on every tab switch.

## ROOT CAUSE #7 (mode 6 — "rapid pane close double-^L hits Claude")
Confirmed from /tmp/cockpit-dbg.log WITHOUT user repro: 75 `send-keys -l '\u{c}'`
in one session, incl. back-to-back duplicates to the SAME pane (%14 twice, %16
twice) and fan-out to panes in BOTH tmux windows (%7 %15 @2, %14 %16 @5).
- `scheduleResync()` runs after EVERY successful `pushGrid`; its 320ms debounce
  only coalesces pushes closer than 320ms. Closing panes quickly ⇒ several
  pushes ⇒ several broadcasts ⇒ several ^L per pane.
- `cockpit:resync` is a WINDOW event, so every mounted XtermHost sends ^L to its
  own pane — the repair is fired at panes that never needed it.
- Mechanism is unsound in principle: injecting keystrokes into a live
  interactive session is indistinguishable from the user typing. In Claude Code
  ^L clears the rendered transcript. NOT a timing-constant bug — do not tune
  RESYNC_QUIET_MS/RESYNC_MAX_WAIT_MS.

## STATUS 2026-07-20
Iteration #5 still UNVERIFIED at this point (YELLOW; later superseded — see GREEN section). #6/#7 are consequences of the two-grid
architecture + the ^L band-aid. Proposed direction (needs user go/no-go, it is
an architecture change): keep inactive tabs MOUNTED (hidden) so tab switch stops
tearing down terminals and stops replaying captures; drop keystroke injection as
a repair mechanism. Pending confirmation test: clear log, ONE bare tab switch —
prediction: log stays EMPTY and the pane still garbles.

## CONFIRMATION of #6/#7 (2026-07-20, user natural experiment)
User: switching between the two 2-pane tabs → both garble and STAY garbled;
switching to/from the 1-pane tab → still garbles but auto-heals. "It matters
which tab I come back from." Live tmux + cleared log confirm the mechanism:
  @5 dev-team panes=2 -> key 189x54/2/even-horizontal
  @2 2.1.215  panes=2 -> key 189x54/2/even-horizontal   (IDENTICAL to @5)
  @6 2.1.215  panes=1 -> key 192x54/1/even-horizontal
Log for the session: 10 set_grid, alternating 192<->189 ONLY — i.e. every push
involved the 1-pane tab; the 2<->2 switches emitted ZERO tmux commands.
⇒ garble occurs on EVERY tab switch (warm_start replay, #6); the auto-^L masks
it ONLY when the arriving tab's pane count differs from the last pushed key.
Both root causes confirmed; no further repro needed.

## ROOT CAUSE #8 (found while confirming — sizing targets the WRONG window)
`select-layout` is issued with NO `-t` (engine/src/lib.rs:159) and NOTHING in
the codebase ever issues `select-window` (grep: zero hits in src, crates,
frontend; zero in the debug log). So tmux's current window is simply the
last-created one (@6) and NEVER follows the cockpit tab the user is viewing.
⇒ every `select-layout` re-tiles @6 regardless of which tab is displayed, and
the displayed tab's window is never actually laid out. Live proof that windows
do NOT share one size: @5 panes are 94x53 while @2 panes are 94x54 — the
displayed tab's geometry is stale/accidental, set whenever it last happened to
be tmux-current. `refresh-client -C` sets the CLIENT size (session-global)
while `select-layout` acts per-window — the two halves of set_grid target
different scopes.
Correct shape: `window-size manual` + `resize-window -t <win>` +
`select-layout -t <win>` so each tab's window is sized independently.

## SPIKE 2026-07-20 — per-window sizing validated on tmux 3.6b (socket -L dbgtest)
LANDMINE FOUND: `set-option -g window-size manual` (GLOBAL) makes the very next
`new-window` **kill the tmux server** ("server exited unexpectedly"). Bisected
command-by-command; control run (new-window without the option) stays ALIVE.
The originally approved plan said `set -g window-size manual` — that would have
killed the user's session on every new tab. DO NOT set window-size globally.
WORKS instead — per-window, after the window exists:
    set-option -w -t <win> window-size manual
    resize-window  -t <win> -x <cols> -y <rows>
    select-layout  -t <win> <layout>
Verified WITH a real `-C attach` control client attached (client 80x):
- attached client does NOT override manual per-window sizes (@0 111x33 and
  @1 55x22 both held);
- independent per-window resize works while attached;
- `select-layout -t @0` while tmux current window is @1 correctly tiled @0
  (two 55x33 panes) ⇒ targeted layout fixes #8 without needing select-window.
⇒ `refresh-client -C` (session-global) is not needed at all; drop it.
Note: spiking tmux in the sandbox needs a long-lived pane command
(`new-session -d ... 'sleep 600'`) or the pane shell exits and takes the server
with it; and zsh does NOT word-split unquoted vars (use "$@").

## ITERATION #6 — implemented 2026-07-20 (user approved all three, full fix)
Fixes #6 (tab-switch replay), #7 (^L injection), #8 (wrong-window sizing).

BACKEND
- `engine::set_grid(window_id, cols, rows, layout)` now emits, all targeted:
      set-option -w -t <win> window-size manual
      resize-window -t <win> -x <cols> -y <rows>
      select-layout -t <win> <layout>
  `refresh-client -C` is GONE from this path (it was the session-global coupling).
  Command string split into pure `build_set_grid_cmd()` so its shape is testable.
- 2 new engine tests: `set_grid_targets_exactly_one_window` (asserts every line
  carries `-t @3`) and `set_grid_never_sets_window_size_globally` (asserts no
  `set-option -g` — the tmux-3.6b server-killer — and no `refresh-client`).
- `manager::set_grid` + the `set_grid` tauri command take `window_id`; the TEMP
  DEBUG line now logs `win=`.

FRONTEND
- PaneGrid renders EVERY tab, stacked absolutely in a new `.pane-grid-stack`;
  inactive grids are hidden with `visibility:hidden` + `pointer-events:none`.
  NOT `display:none` — that measures 0x0, so background xterms could not fit and
  would sit at the 80x24 default while tmux streamed full-width lines into them,
  wrapping every line and accumulating garble in any tab not yet opened since
  boot. `visibility` keeps the layout box, so background tabs stay fitted.
- `store.panesForTab(tabId)` extracted; `activePanes()` delegates to it.
- Single global `cellCols/cellRows` replaced by `cellByPane: Map<paneId,{cols,rows}>`.
  Reason found while implementing: with tabs kept mounted, an arriving tab's
  xterms are ALREADY fitted and so report nothing on switch — a last-writer
  global would size the arriving window from the tab you just left (2-pane 94
  vs 1-pane 192). `pushGrid` reads the active tab's own pane size from the map.
- `reportCell(paneId, cols, rows)`: records EVERY pane (hidden included) but only
  the active tab's panes may trigger a push, so a background tab re-fitting
  cannot resize the window you are looking at.
- `lastGridKey` (single string) → `lastGridKeyByWindow: Map`. The old global key
  is what swallowed same-shape tab switches.
- `switchTab` calls new `scheduleGridPush()` (60ms) — with tabs mounted nothing
  else would size an arriving tab whose xterms did not change size.
- `scheduleResync()` DELETED; XtermHost's `cockpit:resync` listener and its
  `paneSendKeys(paneId,"\x0c")` DELETED, with a comment at both sites (and in
  engine near `interrupt_pane`) recording why synthetic input is never a repair.

VERIFIED SO FAR: cargo test --workspace 127 pass / 0 fail (was 125; +2 engine),
tsc clean, vite build clean. NOT yet live-verified at this point (later verified — see GREEN section).

FOLLOW-UPS (not done, out of approved scope)
- `pane_resize` (engine/manager/tauri cmd) is now unreachable from the UI but
  still contains a session-global `refresh-client -C` — a latent re-coupling.
- `warm_start_screen` / `warmStartScreen` likewise unreachable (it existed only
  for the abandoned capture-replay resync).
- `cellByPane` entries are never evicted on pane death (tiny; overwritten on
  re-mount of a reused id).

## ITERATION #6 — two self-inflicted regressions caught in review, fixed pre-deploy
1. FOCUS ROUTING. Tab switch used to destroy the outgoing tab's xterms, so the
   focused textarea died with them. With tabs kept mounted a `visibility:hidden`
   element can retain DOM focus in WebKit ⇒ after a KEYBOARD switch (⌘1-9, no
   click) keystrokes would route to a pane in the tab just left — silent and
   severe. XtermHost now drives focus from the store: a `createEffect` calls
   `term.focus()` when this pane is both focusedPaneId and in the active tab,
   and `term.blur()` if it still holds DOM focus while not.
2. EMPTY-TAB UNMOUNT. The whole per-tab stack sat inside
   `<Show when={hasActivePanes()}>`, so an active tab with zero panes would
   unmount EVERY tab's terminals and the next switch would re-run warm_start —
   resurrecting root cause #6. The stack now always renders; the empty-state is
   an absolutely-positioned overlay (`.grid-empty-overlay`) instead of a
   fallback.

## DEPLOYED 2026-07-20 12:51 — iteration #6, awaiting live verify (YELLOW at the time; verified 2026-07-21)
Release binary built (`--features tauri/custom-protocol`), verified to contain
`window-size manual` + `resize-window -t` and the new dist (index-DGpsE8xb);
the ONE remaining `refresh-client -C` string is the now-unreachable pane_resize.
Swapped into /Applications (unlink-then-copy, running process untouched) and
signed per cc-cockpit-signing-ritual → `studio.arag.cc-cockpit`, team
BN8BTA42RW, `codesign --verify --strict` OK.
Suite: 127 rust tests pass / 0 fail; tsc clean; vite build clean.
TEMP DEBUG deliberately still in tree + binary (kept for this verify round).
USER MUST QUIT + RELAUNCH — a binary swap does not restart the process.

## LIVE VERIFY CHECKLIST (iteration #6)
a. Switch between the two SAME-pane-count tabs (dev-team ↔ the other 2-pane
   tab) → no garble. This is the original repro that had NO repair path.
b. ⌘1-9 switch WITHOUT clicking, then type → text must land in the VISIBLE
   tab (regression guard for the focus fix).
c. Close 3 panes fast, then `grep -c 'u{c}' /tmp/cockpit-dbg.log` → MUST be 0.
   Claude's transcript must survive.
d. Drag-resize + fullscreen toggle → app repaints itself, no manual ^L needed.
e. `tmux -L cockpit list-windows -a -F '#{window_id} #{window_width}x#{window_height}'`
   → tabs may now legitimately have DIFFERENT sizes; that is the fix working.
If a-e pass: strip TEMP DEBUG (engine write_cmd + lib.rs set_grid), rebuild,
re-sign, rerun suite, commit on a fresh branch off main.

## TEMP DEBUG STRIPPED 2026-07-21 (user request, before live verify)
Both instrumentation blocks removed: engine `write_cmd` and the `set_grid`
tauri command. `rg 'TEMP DEBUG|cockpit-dbg'` over src+crates+frontend is clean.
Also corrected the engine module header, which still described `pane_resize`
(`refresh-client -C`) as the sizing path — `set_grid` is, and pane_resize is now
only reachable from the `live_bridge` dev binary.

CONSEQUENCE FOR VERIFY: checklist item (c) can no longer be proven from
/tmp/cockpit-dbg.log (nothing writes it now). #7 is verified by the SYMPTOM
instead — close 3 panes fast and Claude's rendered transcript must survive.
That is the ground truth the log was standing in for. If a regression needs
diagnosing later, re-add the two blocks (see commit ae921f0^..ae921f0 for the
exact shape) and rebuild.

NOT deleted, deliberately: `pane_resize` is still called by
`app/src-tauri/src/bin/live_bridge.rs:77`, so it is NOT dead code and removing
it would break that binary. `warm_start_screen` IS unreachable (frontend +
backend) but removing an exported tauri command is a separate reviewable change,
left for the user to approve.

## 🟢 GREEN — LIVE-VERIFIED + MERGED 2026-07-21
Iteration #6 verified by the user on a real relaunch (app PID 16137, started
after the 17:23 binary install) and merged to main as `aef5008` (PR #4).
Branch `feat/resize-garble-fixes` deleted; docs split to PR #5 (`f8c605a`).

Machine-checked evidence at verify time:
- `@2`/`@6` carry `window-size=manual` PER-WINDOW; global `window-size` is still
  `latest` and the server was alive after new windows ⇒ the `-g` crash landmine
  is genuinely avoided in the shipped code.
- Window sizes DIVERGED: `@2`/`@6` 192x54 vs `@11` 189x54. Baseline before
  relaunch was ALL THREE pinned at 189x54 by the shared client viewport. This is
  the single clearest proof root cause #8 is fixed — it was impossible before.

User-confirmed (visual, no instrumentation — stripped in 3b00c2b):
1. tab switch between same-pane-count tabs → NO garble (the original repro).
2. ⌘1-9 switch without clicking, then type → lands in the VISIBLE tab
   (the focus `createEffect` works; WebKit hidden-focus trap avoided).
3. rapid pane close → Claude Code's transcript SURVIVES (root cause #7 dead).
4. drag-resize + fullscreen → app repaints itself, no manual Ctrl+L needed.

The arc is closed. 8 root causes over iterations #5-#6.

## REMAINING (small, optional, NOT blocking)
- ~~`warm_start_screen` (+ `warmStartScreen` in ipc.ts) unreachable dead code~~
  DONE 2026-07-21: removed (43 lines, 3 files) — the tauri command, its
  `invoke_handler` registration, the `SessionManager` method, and the frontend
  wrapper. `warm_start` (full scrollback) stays: it still runs on GENUINE first
  mounts, which is the only replay path left now that tab switches keep their
  terminals. `trim_blank_edges` + `WarmStartPayload` are shared and stay.
- `pane_resize` is off the app's sizing path but NOT dead — `live_bridge.rs:77`
  calls it. It still contains the last session-global `refresh-client -C`; if
  live_bridge is ever retired, delete both together.
- `cellByPane` entries are never evicted on pane death (bounded, tiny; a reused
  pane id overwrites its entry on remount).
- Engine still swallows tmux `%error` frames (fire-and-forget) — pre-existing,
  unrelated to this arc, worth its own pass.

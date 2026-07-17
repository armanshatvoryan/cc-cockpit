# Resize/garble debugging — state doc

**Date:** 2026-07-17 (FIVE root causes found + fixed; iteration #5 = auto-Ctrl+L)
**Status:** YELLOW — iteration #5 built + installed + signed, NOT yet live-verified
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

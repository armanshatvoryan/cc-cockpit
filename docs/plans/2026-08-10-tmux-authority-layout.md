# tmux-authority layout mirror (bug #10)

Owner ruling 2026-08-10: fix the odd-pane garble by making tmux the layout
authority and mirroring its geometry in CSS — direction 2 from the diagnosis,
inverting the v1 "CSS decides, tmux follows" design.

## Problem (proven on a probe socket)

`select-layout tiled` arranges panes by window aspect and never leaves holes;
the CSS grid (`columnsFor`: 1/2/3 cols) arranges by count. Any pane count that
doesn't fill the rectangle diverges: 3 panes @189×109 → tmux gives the bottom
pane 189 cols vs the 94-col CSS cell (that pane permanently garbled); 5 panes →
tmux picks 2×2+1 (141–142 cols) vs the 3-col CSS 94 → all five mismatch.

## Design

tmux owns pane arrangement. The frontend:

1. **Sizes only the window.** One container-level push: total cols/rows from
   the pane-stack's px box ÷ the measured xterm cell px (minus one toolbar
   height per pane-row). `set_grid(window, cols, rows, "tiled")` as before.
2. **Mirrors the result.** `window_layout` (already collected per tab) is
   parsed in Rust (`layout.rs`) into per-pane rects `{paneId, x, y, w, h}` in
   tmux cells + the window total, shipped on `TabInfo.layoutRects`. Any
   layout change re-enters via the existing `%layout-change` → topology →
   `refreshState()` path — no new events.
3. **Positions panes absolutely** from the rects: `left = x·cellW`,
   `top = y·cellH + rowIndex·toolbarH`, `width = w·cellW`,
   `height = toolbarH + h·cellH` (rowIndex = rank of the pane's distinct y —
   valid for the tiled/even-h layouts tmux is asked for). Falls back to the
   old CSS grid while rects or cell px are still unknown (first frames).
4. **Resizes each xterm explicitly** — `term.resize(w, h)` from its rect.
   FitAddon no longer decides cols/rows; XtermHost only measures the char
   cell px and reports it (`reportCellPx`) for step 1's math.

Convergence: push → tmux applies tiled → layout-change → rects update panes;
a second push happens only if the pane-row count changed the height budget.
By construction every xterm's cols/rows equal its tmux pane's — the mismatch
class dies for ANY count.

## Layout string format (verified live)

`checksum,WxH,X,Y{...}` — `{}` = row of children, `[]` = column of children,
leaf = `WxH,X,Y,<pane-id-number>` (the `%N` number, NOT the pane index —
verified by killing %1 and re-splitting: leaves read `0,2,3`).

## Files

- `app/src-tauri/src/layout.rs` (new) — parser + rects, fixture tests from
  real probe-socket strings.
- `manager.rs` — `TabInfo.layout_rects` + `layout_size`.
- `ipc.ts` — mirror types.
- `store.ts` — drop `cellByPane`/`gridColumns` per-cell math; add
  `reportCellPx` + `reportContainer` (PaneGrid RO) driving `pushGrid`;
  `rectForPane` lookup.
- `PaneGrid.tsx` — absolute positioning from rects; CSS-grid fallback.
- `XtermHost.tsx` — explicit resize from rect; cell-px measurement.

## Gates

- cargo tests (parser fixtures incl. 1/2/3/4/5-pane tiled, narrow, even-h,
  killed-pane numbering) · tsc · vite build.
- Live eyeball is USER-gated: `tauri dev` shares the installed app's tmux
  session — quitting it kills live panes. Smoke: 3 and 5 panes, no garble;
  split/close/switch/zoom still clean.

## Status

- 2026-08-10: BUILT on `feat/tmux-authority-layout` (branched off
  `chore/open-source-prep`, includes the #9 fix; PR after #7/#8 merge).
  Gates green: cargo 125/0 (8 new parser tests) · tsc · vite build ·
  `cargo check --all-targets`. Chrome constants: 6px horizontal
  (border+padding), 34px vertical (toolbar+border+padding), 3px grid pad.
  Known accepted risks: (a) theoretical push↔re-tile oscillation if a ±1-cell
  budget change flips tmux's arrangement at an aspect threshold — damped by
  the per-window key guard, considered vanishingly rare; (b) transient
  CSS-grid fallback frames when rect count ≠ pane count mid-topology-change.
- 🔴 LIVE EYEBALL PENDING (user-gated — tauri dev shares the installed
  app's session): 1/2/3/5 panes render unglitched; split/close/switch/zoom;
  new tab (#9) clean.

## Addendum: manual splits preserved (2026-08-10, `fix/preserve-manual-splits`)

v1 of the mirror pushed `select-layout tiled` on EVERY grid push, so a user's
⌘D/⌘⇧D split survived ~60ms until the split's own refresh re-tiled it. Fix:

- `build_set_grid_cmd` accepts layout `"none"` → resize-window only, no
  select-layout. tmux scales the existing arrangement proportionally (proven
  on a probe socket: nested `{left,[right-stacked]}` keeps its structure
  across a 200×50→150×40 resize-only push).
- `pushGrid` picks the layout: `tiled` only when the pane COUNT changed in a
  window the user hasn't split by hand (external pane adds — e.g. agent
  teams — still get the grid; closes re-balance it). Resizes, boots, tab
  switches, and everything on a user-arranged window push `"none"`.
- `doSplit` flags the window user-arranged before the refresh can push.
  Flags are session-scoped; arrangements survive GUI restarts anyway because
  boot pushes are resize-only. `gridServerReset` clears both new maps.
- The mirror itself (parser/edgePx/explicit resize) was already
  arrangement-agnostic — nested-split fixture added to prove it; the
  "rowIndex rank of distinct y" description above is v1-era, the shipped
  edgePx math handles panes spanning row boundaries.
- Killing the every-push re-tile also kills accepted risk (a), the
  push↔re-tile oscillation.
- Follow-up `00883d4`: ⌘D auto-direction — splits along the focused pane's
  longer RENDERED axis (rect cells × cell px), so repeated ⌘D distributes
  evenly instead of piling skinny columns. ⌘⇧D stays forced-stack.
- ✅ LIVE SMOKE PASSED 2026-08-10 (installed 0.1.3 rebuild, user-confirmed:
  no garble, splits survive + distribute evenly). **MERGED via PR #12** →
  main @ `9c53994`.

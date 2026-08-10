// PaneGrid — renders EVERY tab's panes, showing only the active tab's grid.
//
// tmux is the layout AUTHORITY (bug #10): each tab's `TabInfo.geometry` is the
// parsed `window_layout`, and panes are positioned absolutely from those rects
// (px = tmux cells × the measured char cell + per-pane chrome). The old CSS
// grid guessed its own arrangement (`columnsFor`) and diverged from tmux
// `tiled` on any pane count that doesn't fill the rectangle — the divergent
// pane rendered a wider tmux pane into a narrower xterm and garbled. The CSS
// grid survives only as a fallback for the frames before geometry/cell-px are
// known.
//
// WHY every tab is rendered, not just the active one: this used to be
// `<For each={activePanes()}>`, so a tab switch DISPOSED the outgoing tab's
// terminals and mounted the incoming tab's fresh + empty. Each fresh mount then
// rebuilt its screen from `warm_start` = `capture-pane -p -e -S -`, and that
// replay is lossy: capture-pane hard-wraps at the pane width and joins with
// newlines without marking which breaks were soft, so every captured line that
// is exactly pane-width re-wraps in xterm and injects a phantom blank line
// (measured live 2026-07-20: 54 such lines out of 646). Everything below shifts,
// and since Claude Code renders differentially in the normal buffer its row
// model no longer matches the screen — the "tabs are garbled after switching"
// bug, present with no resize involved (root cause #6).
//
// Keeping tabs mounted removes the replay entirely: hidden panes keep receiving
// `pane:data` and stay live and correct, so switching is a pure CSS visibility
// change. Cost is N terminals in the DOM instead of one tab's worth, which the
// DOM renderer handles fine at cockpit's tab counts.

import { For, Show, onCleanup, onMount, type Component, type JSX } from "solid-js";
import {
  activeTabId,
  cellPx,
  focusedPaneId,
  newTab,
  panesForTab,
  reportContainer,
  store,
  GRID_PAD,
  PANE_CHROME_H,
  PANE_CHROME_W,
} from "../store";
import type { LayoutRect, TabInfo } from "../ipc";
import { Pane } from "./Pane";

/** Fallback column count for n panes (pre-geometry frames only). */
function columnsFor(n: number): number {
  if (n <= 1) return 1;
  if (n <= 4) return 2;
  return 3;
}

/** Map a tmux cell edge to CSS px: cells × cell-px + one chrome width per
 * pane-column/row that STARTS strictly before the edge. A pane's own start
 * excludes its own chrome (the chrome sits inside the pane box); its far edge
 * can never coincide with a later start (tmux puts a border cell between
 * panes), so strict `<` is correct for both, and a pane spanning several
 * pane-columns absorbs the intermediate chrome — edges line up exactly. */
function edgePx(
  cells: number,
  starts: number[],
  cell: number,
  chrome: number,
): number {
  const n = starts.filter((s) => s < cells).length;
  return GRID_PAD + cells * cell + n * chrome;
}

/** Absolute px box for one pane rect. */
function slotStyle(
  rect: LayoutRect,
  xs: number[],
  ys: number[],
  cw: number,
  ch: number,
): JSX.CSSProperties {
  const left = edgePx(rect.x, xs, cw, PANE_CHROME_W);
  const top = edgePx(rect.y, ys, ch, PANE_CHROME_H);
  const right = edgePx(rect.x + rect.w, xs, cw, PANE_CHROME_W);
  const bottom = edgePx(rect.y + rect.h, ys, ch, PANE_CHROME_H);
  return {
    left: `${left}px`,
    top: `${top}px`,
    width: `${right - left}px`,
    height: `${bottom - top}px`,
  };
}

/** The tab's mirror data when tmux geometry is usable, else null. */
function mirrorFor(tab: TabInfo, paneCount: number) {
  const g = tab.geometry;
  const c = cellPx();
  if (!g || !c || g.rects.length === 0 || g.rects.length !== paneCount)
    return null;
  const xs = [...new Set(g.rects.map((r) => r.x))].sort((a, b) => a - b);
  const ys = [...new Set(g.rects.map((r) => r.y))].sort((a, b) => a - b);
  return { g, c, xs, ys };
}

export const PaneGrid: Component = () => {
  const hasActivePanes = () => panesForTab(activeTabId() ?? "").length > 0;
  let stackEl!: HTMLDivElement;

  onMount(() => {
    // The stack box drives the tmux WINDOW size (see store.reportContainer).
    const ro = new ResizeObserver(() =>
      reportContainer(stackEl.clientWidth, stackEl.clientHeight),
    );
    ro.observe(stackEl);
    reportContainer(stackEl.clientWidth, stackEl.clientHeight);
    onCleanup(() => ro.disconnect());
  });

  return (
    // The stack ALWAYS renders. Gating it on the active tab having panes would
    // unmount every tab's terminals whenever the active tab is momentarily
    // empty, and the next switch would remount them and re-run the warm_start
    // replay — resurrecting the very garble this component now avoids. The
    // empty-state is an overlay instead of a fallback.
    <div class="pane-grid-stack" ref={stackEl}>
      <For each={store.tabs}>
        {(tab) => {
          const panes = () => panesForTab(tab.tabId);
          const mirror = () => mirrorFor(tab, panes().length);
          return (
            <Show when={panes().length > 0}>
              <div
                class="pane-grid"
                // Hidden via `visibility`, NOT `display:none`: a display:none
                // grid measures 0x0 and its xterms would have no layout box.
                // Every grid is absolutely stacked on the same element, so
                // background tabs hold correct geometry and are right the
                // instant you switch to them.
                classList={{
                  "pane-grid-hidden": tab.tabId !== activeTabId(),
                  "pane-grid-mirror": !!mirror(),
                }}
                style={
                  mirror()
                    ? undefined
                    : {
                        "grid-template-columns": `repeat(${columnsFor(
                          panes().length,
                        )}, minmax(0, 1fr))`,
                      }
                }
              >
                <For each={panes()}>
                  {(pane) => {
                    const rect = () =>
                      mirror()?.g.rects.find((r) => r.paneId === pane.paneId);
                    const slot = () => {
                      const m = mirror();
                      const r = rect();
                      return m && r
                        ? slotStyle(r, m.xs, m.ys, m.c.w, m.c.h)
                        : undefined;
                    };
                    return (
                      <div class="pane-slot" style={slot()}>
                        <Pane
                          pane={pane}
                          focused={pane.paneId === focusedPaneId()}
                          rect={rect()}
                        />
                      </div>
                    );
                  }}
                </For>
              </div>
            </Show>
          );
        }}
      </For>

      <Show when={!hasActivePanes()}>
        <div class="grid-empty grid-empty-overlay">
          <p class="grid-empty-text">No panes in this tab.</p>
          <button class="btn btn-primary" onClick={() => void newTab()}>
            New tab
          </button>
        </div>
      </Show>
    </div>
  );
};

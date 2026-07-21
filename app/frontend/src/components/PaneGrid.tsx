// PaneGrid — renders EVERY tab's panes, showing only the active tab's grid.
//
// v1 deliberately does NOT parse tmux layout strings (the brief says so). A CSS
// grid with a column count derived from the pane count gives a dense, balanced
// tiling. Panes are keyed by paneId via <For> so each XtermHost instance is
// preserved across re-renders (focus changes, status updates) and only created/
// disposed when a pane actually appears/disappears.
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

import { For, Show, type Component } from "solid-js";
import {
  activeTabId,
  focusedPaneId,
  newTab,
  panesForTab,
  store,
} from "../store";
import { Pane } from "./Pane";

/** Column count for n panes: keeps tiles near-square, capped at 3 wide. */
function columnsFor(n: number): number {
  if (n <= 1) return 1;
  if (n <= 4) return 2;
  return 3;
}

export const PaneGrid: Component = () => {
  const hasActivePanes = () => panesForTab(activeTabId() ?? "").length > 0;

  return (
    // The stack ALWAYS renders. Gating it on the active tab having panes would
    // unmount every tab's terminals whenever the active tab is momentarily
    // empty, and the next switch would remount them and re-run the warm_start
    // replay — resurrecting the very garble this component now avoids. The
    // empty-state is an overlay instead of a fallback.
    <div class="pane-grid-stack">
      <For each={store.tabs}>
        {(tab) => {
          const panes = () => panesForTab(tab.tabId);
          return (
            <Show when={panes().length > 0}>
              <div
                class="pane-grid"
                // Hidden via `visibility`, NOT `display:none`: a display:none
                // grid measures 0x0, so its xterms could not fit and would sit
                // at the 80x24 default while tmux streamed full-width lines
                // into them — every line wrapping, garble accumulating in any
                // tab not yet opened since boot. Every grid is absolutely
                // stacked on the same box, so background tabs stay correctly
                // fitted and are right the instant you switch to them.
                classList={{ "pane-grid-hidden": tab.tabId !== activeTabId() }}
                style={{
                  "grid-template-columns": `repeat(${columnsFor(
                    panes().length,
                  )}, minmax(0, 1fr))`,
                }}
              >
                <For each={panes()}>
                  {(pane) => (
                    <Pane
                      pane={pane}
                      focused={pane.paneId === focusedPaneId()}
                    />
                  )}
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

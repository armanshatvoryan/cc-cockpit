// PaneGrid — renders the ACTIVE tab's panes as an even responsive tiling.
//
// v1 deliberately does NOT parse tmux layout strings (the brief says so). A CSS
// grid with a column count derived from the pane count gives a dense, balanced
// tiling. Panes are keyed by paneId via <For> so each XtermHost instance is
// preserved across re-renders (focus changes, status updates) and only created/
// disposed when a pane actually appears/disappears.

import { For, Show, type Component } from "solid-js";
import { activePanes, focusedPaneId, newTab } from "../store";
import { Pane } from "./Pane";

/** Column count for n panes: keeps tiles near-square, capped at 3 wide. */
function columnsFor(n: number): number {
  if (n <= 1) return 1;
  if (n <= 4) return 2;
  return 3;
}

export const PaneGrid: Component = () => {
  const panes = activePanes;

  return (
    <Show
      when={panes().length > 0}
      fallback={
        <div class="grid-empty">
          <p class="grid-empty-text">No panes in this tab.</p>
          <button class="btn btn-primary" onClick={() => void newTab()}>
            New tab
          </button>
        </div>
      }
    >
      <div
        class="pane-grid"
        style={{
          "grid-template-columns": `repeat(${columnsFor(
            panes().length,
          )}, minmax(0, 1fr))`,
        }}
      >
        <For each={panes()}>
          {(pane) => (
            <Pane pane={pane} focused={pane.paneId === focusedPaneId()} />
          )}
        </For>
      </div>
    </Show>
  );
};

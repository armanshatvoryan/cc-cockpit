// Global keyboard shortcuts (keyboard-first cockpit).
//
//   ⌘T          new tab
//   ⌘1..⌘9      switch to the Nth tab
//   ⌘D          split focused pane horizontally (side-by-side)
//   ⌘⇧D         split focused pane vertically (stacked)
//   ⌘I          toggle the inventory panel
//   ⌘⇧T         toggle the live team board
//   Esc         close the inventory panel / team board (when open)
//
// We bind on the window in capture mode and only act on our combos so terminal
// keystrokes (which xterm handles) are never swallowed.

import { onCleanup } from "solid-js";
import {
  newTab,
  switchTabByIndex,
  doSplit,
  focusedPaneId,
  toggleInventory,
  closeInventory,
  inventoryOpen,
  toggleTeamBoard,
  closeTeamBoard,
  teamBoardOpen,
} from "./store";

export function installKeyboard(): void {
  function onKey(e: KeyboardEvent) {
    // Esc closes the inventory panel if it's open (intercept BEFORE xterm so a
    // stray Esc dismisses the overlay rather than reaching a terminal process).
    if (e.key === "Escape" && (inventoryOpen() || teamBoardOpen())) {
      e.preventDefault();
      if (inventoryOpen()) closeInventory();
      if (teamBoardOpen()) closeTeamBoard();
      return;
    }

    // macOS: Cmd is metaKey. Ignore plain typing / non-meta combos.
    if (!e.metaKey) return;

    // ⌘1..⌘9 — switch tab.
    if (/^[1-9]$/.test(e.key)) {
      e.preventDefault();
      switchTabByIndex(Number(e.key) - 1);
      return;
    }

    const k = e.key.toLowerCase();

    // ⌘T — new tab.  ⌘⇧T — toggle the live team board.
    if (k === "t") {
      e.preventDefault();
      if (e.shiftKey) toggleTeamBoard();
      else void newTab();
      return;
    }

    // ⌘D / ⌘⇧D — split focused pane.
    if (k === "d") {
      e.preventDefault();
      const pid = focusedPaneId();
      if (pid) void doSplit(pid, e.shiftKey ? "v" : "h");
      return;
    }

    // ⌘I — toggle the inventory panel.
    if (k === "i") {
      e.preventDefault();
      toggleInventory();
      return;
    }
  }

  window.addEventListener("keydown", onKey, { capture: true });
  onCleanup(() => window.removeEventListener("keydown", onKey, { capture: true }));
}

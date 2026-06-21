// Global keyboard shortcuts (keyboard-first cockpit).
//
//   ⌘T          new tab
//   ⌘1..⌘9      switch to the Nth tab
//   ⌘D          split focused pane horizontally (side-by-side)
//   ⌘⇧D         split focused pane vertically (stacked)
//   ⌘I          toggle the inventory panel
//   ⌘⇧T         toggle the live team board
//   ⌘= / ⌘+     zoom the whole UI in   (+0.1)
//   ⌘-          zoom the whole UI out  (−0.1)
//   ⌘0          reset zoom to 1.0
//   Ctrl+wheel  zoom the whole UI (±0.1; trackpad pinch lands here too)
//   Esc         close the inventory panel / team board (when open)
//
// We bind on the window in capture mode and only act on our combos so terminal
// keystrokes (which xterm handles) are never swallowed.

import { onCleanup } from "solid-js";
import { getCurrentWebview } from "@tauri-apps/api/webview";
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

// ── C1: whole-UI zoom (webview setZoom, persisted) ───────────────────────────
// One scale factor for the entire cockpit chrome + terminals. Persisted across
// launches via localStorage; applied to the webview on boot. Clamped so a stray
// combo can never zoom the UI into oblivion.

const ZOOM_KEY = "cockpit.zoom";
const ZOOM_MIN = 0.3;
const ZOOM_MAX = 3.0;
const ZOOM_STEP = 0.1;

let zoom = 1.0;

function clampZoom(z: number): number {
  // Round to 2dp first so repeated ±0.1 steps don't accrue float drift.
  const r = Math.round(z * 100) / 100;
  return Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, r));
}

/** Apply + persist a zoom level (clamped). Fire-and-forget on the webview. */
function applyZoom(z: number): void {
  zoom = clampZoom(z);
  void getCurrentWebview()
    .setZoom(zoom)
    .catch((e) => console.warn("setZoom failed", e));
  try {
    localStorage.setItem(ZOOM_KEY, String(zoom));
  } catch {
    /* private mode / disabled storage — zoom still applies this session */
  }
}

function bumpZoom(delta: number): void {
  applyZoom(zoom + delta);
}

function resetZoom(): void {
  applyZoom(1.0);
}

/** Read the saved zoom and apply it to the webview (called on install/boot). */
function restoreZoom(): void {
  let z = 1.0;
  try {
    const raw = localStorage.getItem(ZOOM_KEY);
    if (raw) {
      const n = parseFloat(raw);
      if (Number.isFinite(n)) z = n;
    }
  } catch {
    /* ignore — default to 1.0 */
  }
  applyZoom(z);
}

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

    // ⌘= / ⌘+ — zoom in.  ⌘- — zoom out.  ⌘0 — reset. (=,+,-,0 are unbound.)
    if (e.key === "=" || e.key === "+") {
      e.preventDefault();
      bumpZoom(ZOOM_STEP);
      return;
    }
    if (e.key === "-") {
      e.preventDefault();
      bumpZoom(-ZOOM_STEP);
      return;
    }
    if (e.key === "0") {
      e.preventDefault();
      resetZoom();
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

  // Ctrl+wheel → zoom (±0.1 per event). On macOS a trackpad pinch is delivered
  // as a wheel event with `ctrlKey`, so this captures pinch-to-zoom too. Passive
  // is false so we can preventDefault the browser's native page zoom.
  function onWheel(e: WheelEvent) {
    if (!e.ctrlKey) return;
    e.preventDefault();
    bumpZoom(e.deltaY < 0 ? ZOOM_STEP : -ZOOM_STEP);
  }

  // Restore the persisted zoom before wiring listeners so the UI boots scaled.
  restoreZoom();

  window.addEventListener("keydown", onKey, { capture: true });
  window.addEventListener("wheel", onWheel, { capture: true, passive: false });
  onCleanup(() => {
    window.removeEventListener("keydown", onKey, { capture: true });
    window.removeEventListener("wheel", onWheel, { capture: true });
  });
}

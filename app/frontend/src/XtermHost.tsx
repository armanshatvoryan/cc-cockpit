// XtermHost — imperative xterm.js wrapper, isolated leaf.
//
// Solid's fine-grained reactivity means this component's DOM host div is created
// once and NEVER re-rendered by the framework; the xterm.js canvas/WebGL it owns
// is touched only through the imperative API below. `props.paneId` is read ONCE
// at mount (a pane id is stable for the life of the cell) — the parent keys the
// component by paneId so a different pane means a fresh XtermHost.
//
// Data path:
//   pane:data {paneId,bytesB64}  ->  term.write(decode)        (output)
//   term.onData(data)            ->  paneSendKeys(paneId,data) (input)
//   ResizeObserver -> fit()      ->  paneResize(...)           (resize)
//   onCleanup                    ->  term.dispose() + unlisten (HMR/kill safe)

import { createEffect, onCleanup, onMount, type Component } from "solid-js";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import {
  onPaneData,
  paneSendKeys,
  warmStart,
  type PaneDataPayload,
} from "./ipc";
import { activePanes, focusedPaneId, reportCell } from "./store";
import { termPalette } from "./theme";
import type { UnlistenFn } from "@tauri-apps/api/event";

export interface XtermHostProps {
  /** tmux pane id this host renders, e.g. "%0". Stable for the cell's life. */
  paneId: string;
}

/** Decode a base64 string to a Uint8Array (binary-safe; atob mangles bytes). */
function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export const XtermHost: Component<XtermHostProps> = (props) => {
  let hostEl!: HTMLDivElement;
  // Captured once: the cell is keyed by paneId so this never changes mid-life.
  const paneId = props.paneId;

  onMount(() => {
    const term = new Terminal({
      fontFamily:
        'Menlo, "SF Mono", "Cascadia Code", "JetBrains Mono", monospace',
      fontSize: 12.5,
      lineHeight: 1.0,
      cursorBlink: true,
      scrollback: 5000,
      allowProposedApi: true,
      // Themed from theme.ts, not from CSS: xterm needs a JS colour object and
      // cannot read the stylesheet's custom properties. The effect below keeps
      // it in step when the user flips the theme.
      theme: termPalette(),
    });

    const fit = new FitAddon();
    term.loadAddon(fit);

    term.open(hostEl);

    // NOTE: WebglAddon intentionally NOT used — it renders an all-black canvas in
    // the macOS WKWebView. The default DOM renderer is plenty for v1 (output is
    // already coalesced to ~1 write/pane/16ms in the backend forwarder).

    // Initial fit + report our cell size to the grid coordinator (the single
    // window-size authority). Per-pane resize is gone — it collapsed multi-pane
    // tabs to 1 column.
    //
    // A pane belonging to a NON-active tab now mounts too (tabs are kept mounted
    // so switching never destroys a terminal). Those hosts are hidden with
    // `visibility`, so they DO have a layout box and fit normally — the guard is
    // for the genuinely unmeasurable cases (mount before layout, collapsed
    // sidebar), where fit() throws on a 0x0 box. The ResizeObserver below picks
    // those up as soon as the element has a size.
    let everReported = false;
    if (hostEl.clientWidth > 0 && hostEl.clientHeight > 0) {
      fit.fit();
      reportCell(paneId, term.cols, term.rows);
      everReported = true;
    }

    // Output: write decoded bytes verbatim (xterm is a full VT emulator).
    let unlistenData: UnlistenFn | undefined;
    let disposed = false;
    onPaneData((p: PaneDataPayload) => {
      if (p.paneId !== paneId) return;
      term.write(b64ToBytes(p.bytesB64));
    }).then((u) => {
      if (disposed) {
        u();
        return;
      }
      unlistenData = u;
      // Warm start: only AFTER the live `pane:data` listener is registered do we
      // replay the existing screen + scrollback. The control client streams only
      // `%output` produced after it attached, so a re-attached pane would paint
      // blank without this. Ordering matters: the listener is live first, so any
      // live output during the fetch isn't lost — a little duplicated tail is
      // harmless, a fully blank pane is not. Fire once; guard the unmount race.
      warmStart(paneId)
        .then((w) => {
          if (disposed || !w.bytesB64) return;
          term.write(b64ToBytes(w.bytesB64));
        })
        .catch(() => {});
    });

    // Input: onData yields already-encoded VT (arrows/ctrl/paste) — send literal.
    const dataSub = term.onData((data) => {
      paneSendKeys(paneId, data); // fire-and-forget
    });

    // Resize: ResizeObserver -> fit -> push to tmux when cols/rows change.
    let resizeTimer: number | undefined;
    let lastCols = term.cols;
    let lastRows = term.rows;
    const ro = new ResizeObserver(() => {
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(() => {
        // Skip when the host is hidden (display:none -> 0x0 -> fit throws).
        if (hostEl.clientWidth === 0 || hostEl.clientHeight === 0) return;
        fit.fit();
        // `!everReported` covers a pane that mounted hidden: its very first real
        // fit must be reported even if it happens to land on xterm's 80x24
        // default, or the tab would never size its tmux window.
        if (!everReported || term.cols !== lastCols || term.rows !== lastRows) {
          lastCols = term.cols;
          lastRows = term.rows;
          everReported = true;
          reportCell(paneId, term.cols, term.rows);
        }
      }, 50);
    });
    ro.observe(hostEl);

    // Focus routing. Previously a tab switch DESTROYED the outgoing tab's
    // xterms, so the focused textarea died with them and focus landed on the new
    // tab by construction. Tabs now persist, and a `visibility:hidden` element
    // can keep DOM focus in WebKit — so after a keyboard switch (⌘1-9, which
    // never touches the mouse) keystrokes would silently route to a pane in the
    // tab the user just left. Drive focus from the store instead of relying on
    // xterm's click handler.
    createEffect(() => {
      const isFocusedPane = focusedPaneId() === paneId;
      const isVisible = activePanes().some((p) => p.paneId === paneId);
      if (isFocusedPane && isVisible) {
        term.focus();
      } else if (hostEl.contains(document.activeElement)) {
        // We still hold DOM focus but are no longer the focused/visible pane —
        // release it rather than swallowing the user's keystrokes.
        term.blur();
      }
    });

    // Re-theme in place when the user flips dark/light. Reassigning
    // `options.theme` repaints the existing buffer, so scrollback survives the
    // switch — no reset, no tmux round-trip, no lost transcript. Every mounted
    // pane re-themes, including the ones in background tabs.
    createEffect(() => {
      term.options.theme = termPalette();
    });

    // NOTE: no post-resize repair step here. Iteration #5 listened for a
    // `cockpit:resync` broadcast and sent a synthetic Ctrl+L; tmux cannot tell
    // that from the user typing it, and in Claude Code it wipes the rendered
    // transcript (root cause #7). A resize is now just a resize — the app
    // repaints itself on SIGWINCH, as under any other terminal emulator.

    // Teardown — critical for HMR (else leaked WebGL contexts) and pane kill.
    onCleanup(() => {
      disposed = true;
      if (resizeTimer) clearTimeout(resizeTimer);
      ro.disconnect();
      dataSub.dispose();
      unlistenData?.();
      term.dispose();
    });
  });

  return <div ref={hostEl} class="xterm-host" />;
};

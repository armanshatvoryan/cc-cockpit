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

import { onCleanup, onMount, type Component } from "solid-js";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import {
  onPaneData,
  paneSendKeys,
  warmStart,
  type PaneDataPayload,
} from "./ipc";
import { reportCell } from "./store";
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
      theme: {
        background: "#0d1017",
        foreground: "#dbe2ef",
        cursor: "#60a5fa",
        selectionBackground: "#1e3a5f",
      },
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
    fit.fit();
    reportCell(term.cols, term.rows);

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
        if (term.cols !== lastCols || term.rows !== lastRows) {
          lastCols = term.cols;
          lastRows = term.rows;
          reportCell(term.cols, term.rows);
        }
      }, 50);
    });
    ro.observe(hostEl);

    // Re-sync to tmux's authoritative grid after a pane resize (see
    // store.scheduleResync). xterm's own reflow of a full-screen TUI leaves the
    // old, wider frame wrapped + scattered in scrollback; tmux holds the pane's
    // clean grid re-rendered at the new width, so wipe xterm and replay it. This
    // is the automated form of the Ctrl+L the user would otherwise press. Guard
    // the hidden case (0×0) and the unmount race.
    const onResync = () => {
      if (disposed || hostEl.clientWidth === 0 || hostEl.clientHeight === 0)
        return;
      warmStart(paneId)
        .then((w) => {
          if (disposed || !w.bytesB64) return;
          term.reset();
          term.write(b64ToBytes(w.bytesB64));
        })
        .catch(() => {});
    };
    window.addEventListener("cockpit:resync", onResync);

    // Teardown — critical for HMR (else leaked WebGL contexts) and pane kill.
    onCleanup(() => {
      disposed = true;
      if (resizeTimer) clearTimeout(resizeTimer);
      ro.disconnect();
      window.removeEventListener("cockpit:resync", onResync);
      dataSub.dispose();
      unlistenData?.();
      term.dispose();
    });
  });

  return <div ref={hostEl} class="xterm-host" />;
};

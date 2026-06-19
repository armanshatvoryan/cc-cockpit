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
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import {
  onPaneData,
  paneResize,
  paneSendKeys,
  type PaneDataPayload,
} from "./ipc";
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

    // WebGL is best-effort: fall back to canvas if a context can't be created.
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch (e) {
      console.warn("WebGL addon unavailable, using canvas renderer", e);
    }

    // Initial fit + push size to tmux so the pane lays out for our viewport.
    fit.fit();
    void paneResize(paneId, term.cols, term.rows).catch(() => {});

    // Output: write decoded bytes verbatim (xterm is a full VT emulator).
    let unlistenData: UnlistenFn | undefined;
    let disposed = false;
    onPaneData((p: PaneDataPayload) => {
      if (p.paneId !== paneId) return;
      term.write(b64ToBytes(p.bytesB64));
    }).then((u) => {
      if (disposed) u();
      else unlistenData = u;
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
          void paneResize(paneId, term.cols, term.rows).catch(() => {});
        }
      }, 50);
    });
    ro.observe(hostEl);

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

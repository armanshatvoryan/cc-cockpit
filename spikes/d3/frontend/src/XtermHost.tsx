// XtermHost — imperative xterm.js wrapper, isolated leaf (§2/§3 of the design).
//
// Solid's fine-grained reactivity means this component's DOM host div is created
// once and NEVER re-rendered by the framework; the xterm.js canvas/WebGL it owns
// is touched only through the imperative API below. The only reactive input is
// `props.paneId` (which pane's stream to bind) — changing it re-binds listeners.
//
// Data path wired here:
//   pane:data {paneId,bytesB64}  ->  term.write(base64-decode)     (output)
//   term.onData(data)            ->  paneSendKeys(paneId, data)    (input)
//   ResizeObserver -> fit()      ->  paneResize(paneId, cols, rows)(resize)
//   onCleanup                    ->  term.dispose() + unlisten     (HMR-safe)

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
  /** tmux pane id this host renders, e.g. "%0". */
  paneId: string;
  /** Whether this pane is currently visible (off-screen panes skip WebGL). */
  visible?: boolean;
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

  onMount(() => {
    const term = new Terminal({
      fontFamily:
        'Menlo, "SF Mono", "Cascadia Code", "JetBrains Mono", monospace',
      fontSize: 13,
      cursorBlink: true,
      scrollback: 5000, // §6 cap
      allowProposedApi: true,
      theme: { background: "#11131a", foreground: "#e6e6e6" },
    });

    const fit = new FitAddon();
    term.loadAddon(fit);

    term.open(hostEl);

    // WebGL is best-effort: if the context can't be created (headless CI, too
    // many contexts) we fall back to the canvas renderer rather than crash.
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch (e) {
      console.warn("WebGL addon unavailable, using canvas renderer", e);
    }

    // Initial fit + push size to tmux so the pane lays out for our viewport.
    fit.fit();
    void paneResize(props.paneId, term.cols, term.rows).catch(() => {});

    // Output: write decoded bytes verbatim (xterm is a full VT emulator).
    let unlistenData: UnlistenFn | undefined;
    onPaneData((p: PaneDataPayload) => {
      if (p.paneId !== props.paneId) return;
      term.write(b64ToBytes(p.bytesB64));
    }).then((u) => {
      unlistenData = u;
    });

    // Input: onData yields already-encoded VT (arrows/ctrl/paste) — send literal.
    const dataSub = term.onData((data) => {
      paneSendKeys(props.paneId, data); // fire-and-forget
    });

    // Resize: ResizeObserver -> fit -> push to tmux when cols/rows actually
    // change. Debounced ~50ms to coalesce drag frames (§2 resize path).
    let resizeTimer: number | undefined;
    let lastCols = term.cols;
    let lastRows = term.rows;
    const ro = new ResizeObserver(() => {
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(() => {
        fit.fit();
        if (term.cols !== lastCols || term.rows !== lastRows) {
          lastCols = term.cols;
          lastRows = term.rows;
          void paneResize(props.paneId, term.cols, term.rows).catch(() => {});
        }
      }, 50);
    });
    ro.observe(hostEl);

    // Teardown — critical for HMR (else leaked WebGL contexts) and pane kill.
    onCleanup(() => {
      if (resizeTimer) clearTimeout(resizeTimer);
      ro.disconnect();
      dataSub.dispose();
      unlistenData?.();
      term.dispose();
    });
  });

  return (
    <div
      ref={hostEl}
      class="xterm-host"
      style={{
        width: "100%",
        height: "100%",
        display: props.visible === false ? "none" : "block",
      }}
    />
  );
};

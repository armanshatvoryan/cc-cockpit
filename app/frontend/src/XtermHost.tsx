// XtermHost — imperative xterm.js wrapper, isolated leaf.
//
// Solid's fine-grained reactivity means this component's DOM host div is created
// once and NEVER re-rendered by the framework; the xterm.js canvas/WebGL it owns
// is touched only through the imperative API below. `props.paneId` is read ONCE
// at mount (a pane id is stable for the life of the cell) — the parent keys the
// component by paneId so a different pane means a fresh XtermHost.
//
// SIZING (tmux-authority mirror, bug #10): the terminal's cols/rows come from
// its tmux pane's rect (`props.rect`, reactive) via explicit `term.resize` —
// NOT from FitAddon guessing off the host box. tmux decides the layout; every
// xterm matches its pane by construction, hidden tabs included (no layout box
// needed to resize). This host's only measurement duty is the char cell in CSS
// px (`reportCellPx`), which the grid coordinator needs to size the WINDOW.
//
// Data path:
//   pane:data {paneId,bytesB64}  ->  term.write(decode)        (output)
//   term.onData(data)            ->  paneSendKeys(paneId,data) (input)
//   props.rect change            ->  term.resize(w, h)         (resize)
//   onCleanup                    ->  term.dispose() + unlisten (HMR/kill safe)

import { createEffect, onCleanup, onMount, type Component } from "solid-js";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import {
  onPaneData,
  paneSendKeys,
  warmStart,
  type LayoutRect,
  type PaneDataPayload,
} from "./ipc";
import { activePanes, focusedPaneId, reportCellPx } from "./store";
import type { UnlistenFn } from "@tauri-apps/api/event";

export interface XtermHostProps {
  /** tmux pane id this host renders, e.g. "%0". Stable for the cell's life. */
  paneId: string;
  /** The pane's tmux rect (cols/rows) — the terminal mirrors it exactly. */
  rect?: LayoutRect;
}

/** Measure one char cell in CSS px from xterm's render service. Private API
 * (no public equivalent — FitAddon reads the same fields); returns null until
 * the renderer has measured, so callers just retry. */
function measureCellPx(term: Terminal): { w: number; h: number } | null {
  const dims = (
    term as unknown as {
      _core?: {
        _renderService?: {
          dimensions?: { css?: { cell?: { width: number; height: number } } };
        };
      };
    }
  )._core?._renderService?.dimensions?.css?.cell;
  return dims && dims.width > 0 && dims.height > 0
    ? { w: dims.width, h: dims.height }
    : null;
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

    term.open(hostEl);

    // NOTE: WebglAddon intentionally NOT used — it renders an all-black canvas in
    // the macOS WKWebView. The default DOM renderer is plenty for v1 (output is
    // already coalesced to ~1 write/pane/16ms in the backend forwarder).

    // Report the char cell px to the grid coordinator (identical for every
    // pane — same font/size — so first-reporter wins and the rest are no-ops).
    // The renderer may not have measured yet at open; retry briefly.
    const tryReportCell = () => {
      const c = measureCellPx(term);
      if (c) reportCellPx(c.w, c.h);
      return !!c;
    };
    let measureTimer: number | undefined;
    if (!tryReportCell()) {
      measureTimer = window.setInterval(() => {
        if (tryReportCell()) {
          clearInterval(measureTimer);
          measureTimer = undefined;
        }
      }, 100);
    }

    // Mirror the tmux pane's size exactly. Runs on mount (rect is usually
    // already known from the boot list_state) and on every layout change.
    // Hidden tabs resize too — no layout box needed, unlike the old fit().
    createEffect(() => {
      const r = props.rect;
      if (r && r.w > 0 && r.h > 0 && (term.cols !== r.w || term.rows !== r.h)) {
        term.resize(r.w, r.h);
      }
    });

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

    // Host resizes no longer drive the terminal size (tmux does). The observer
    // survives only to re-measure the char cell after zoom re-metrics change
    // the font geometry — reportCellPx no-ops when nothing moved.
    let resizeTimer: number | undefined;
    const ro = new ResizeObserver(() => {
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(() => {
        tryReportCell();
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

    // NOTE: no post-resize repair step here. Iteration #5 listened for a
    // `cockpit:resync` broadcast and sent a synthetic Ctrl+L; tmux cannot tell
    // that from the user typing it, and in Claude Code it wipes the rendered
    // transcript (root cause #7). A resize is now just a resize — the app
    // repaints itself on SIGWINCH, as under any other terminal emulator.

    // Teardown — critical for HMR (else leaked WebGL contexts) and pane kill.
    onCleanup(() => {
      disposed = true;
      if (resizeTimer) clearTimeout(resizeTimer);
      if (measureTimer) clearInterval(measureTimer);
      ro.disconnect();
      dataSub.dispose();
      unlistenData?.();
      term.dispose();
    });
  });

  return <div ref={hostEl} class="xterm-host" />;
};

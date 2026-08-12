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
  openUrl,
  paneSendKeys,
  warmStart,
  warmStartScreen,
  type LayoutRect,
  type PaneDataPayload,
} from "./ipc";
import { activePanes, focusedPaneId, reportCellPx } from "./store";
import { termPalette } from "./theme";
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
      // Themed from theme.ts, not from CSS: xterm needs a JS colour object and
      // cannot read the stylesheet's custom properties. The effect below keeps
      // it in step when the user flips the theme.
      theme: termPalette(),
      // OSC 8 hyperlinks: xterm's default activate calls confirm() +
      // window.open(), both dead in WKWebView (clicks silently no-op). Route
      // through the backend, which scheme-gates to http(s) and hands the URL
      // to the default browser.
      linkHandler: {
        activate: (_e, uri) => {
          void openUrl(uri).catch((err) => console.error("open_url:", err));
        },
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

    // ── Post-resize resync (bug #11, revisit garble) ──────────────────────
    //
    // A single-shot `resize-window` (typically at tab switch: background
    // windows are sized on arrival) makes the pane's TUI repaint IMMEDIATELY
    // on SIGWINCH, while this terminal's `term.resize` lags behind it by the
    // debounced %layout-change → refreshState round-trip (~150-250ms). The
    // repaint bytes land in a wrong-sized buffer, and a differential renderer
    // like Claude Code never repaints them again — the garble sticks until a
    // manual Ctrl+L. tmux's own grid is clean the whole time, so the repair is
    // to replay it: wait for the pane to go quiet, capture the visible grid
    // (`warm_start_screen`, cursor restored), reset, write.
    //
    // This is NOT iteration #5 coming back: no keystroke injection (root
    // cause #7), pane-scoped (only a pane whose size actually CHANGED, never
    // a broadcast), quiescence-gated with a dirty-retry (root cause #3), and
    // the capture is visible-grid-only (the old full-scrollback replay was
    // root cause #6). Cost: `term.reset()` drops this pane's local scrollback
    // on an actual resize — accepted (owner ruling 2026-08-12).
    const RESYNC_DEBOUNCE_MS = 300; // drag storms collapse into one resync
    const RESYNC_QUIET_MS = 250; // pane output must be quiet this long
    const RESYNC_MAX_WAIT_MS = 2000; // streaming panes: resync anyway
    let lastOutputAt = 0;
    let resyncTimer: number | undefined;
    const scheduleResync = () => {
      if (resyncTimer) clearTimeout(resyncTimer);
      const startedAt = Date.now();
      const attempt = (retriesLeft: number) => {
        if (disposed) return;
        const quietFor = Date.now() - lastOutputAt;
        if (
          quietFor < RESYNC_QUIET_MS &&
          Date.now() - startedAt < RESYNC_MAX_WAIT_MS
        ) {
          resyncTimer = window.setTimeout(() => attempt(retriesLeft), 150);
          return;
        }
        const outputMark = lastOutputAt;
        warmStartScreen(paneId)
          .then((w) => {
            if (disposed) return;
            // Output landed while the capture RPC was in flight — the capture
            // may hold a stale mid-frame. Retry once; then take what we have
            // (a busy pane repaints itself soon anyway).
            if (lastOutputAt !== outputMark && retriesLeft > 0) {
              attempt(retriesLeft - 1);
              return;
            }
            term.reset();
            if (w.bytesB64) term.write(b64ToBytes(w.bytesB64));
          })
          .catch(() => {}); // pane gone mid-resync — cleanup handles it
      };
      resyncTimer = window.setTimeout(() => attempt(1), RESYNC_DEBOUNCE_MS);
    };

    // Mirror the tmux pane's size exactly. Runs on mount (rect is usually
    // already known from the boot list_state) and on every layout change.
    // Hidden tabs resize too — no layout box needed, unlike the old fit().
    // Any size change AFTER the first applied rect schedules the resync
    // above: the first sizing is mount (warm_start replays content for it),
    // every later one is a live tmux resize this buffer just diverged from.
    let sizedOnce = false;
    createEffect(() => {
      const r = props.rect;
      if (!r || !(r.w > 0) || !(r.h > 0)) return;
      const changed = term.cols !== r.w || term.rows !== r.h;
      if (changed) term.resize(r.w, r.h);
      if (changed && sizedOnce) scheduleResync();
      sizedOnce = true;
    });

    // Output: write decoded bytes verbatim (xterm is a full VT emulator).
    let unlistenData: UnlistenFn | undefined;
    let disposed = false;
    onPaneData((p: PaneDataPayload) => {
      if (p.paneId !== paneId) return;
      lastOutputAt = Date.now(); // resync quiescence gate reads this
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

    // Re-theme in place when the user flips dark/light. Reassigning
    // `options.theme` repaints the existing buffer, so scrollback survives the
    // switch — no reset, no tmux round-trip, no lost transcript. Every mounted
    // pane re-themes, including the ones in background tabs.
    createEffect(() => {
      term.options.theme = termPalette();
    });

    // NOTE: the post-resize resync above is CAPTURE-based, never keystrokes.
    // Iteration #5 listened for a `cockpit:resync` broadcast and sent a
    // synthetic Ctrl+L; tmux cannot tell that from the user typing it, and in
    // Claude Code it wipes the rendered transcript (root cause #7). Synthetic
    // input is never a repair mechanism.

    // Teardown — critical for HMR (else leaked WebGL contexts) and pane kill.
    onCleanup(() => {
      disposed = true;
      if (resyncTimer) clearTimeout(resyncTimer);
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

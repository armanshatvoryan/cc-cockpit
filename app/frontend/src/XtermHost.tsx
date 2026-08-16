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
    // (`warm_start_screen`, cursor restored), clear, write.
    //
    // This is NOT iteration #5 coming back: no keystroke injection (root
    // cause #7), pane-scoped (only a pane whose size actually CHANGED, never
    // a broadcast), quiescence-gated with a dirty-retry (root cause #3), and
    // the capture is visible-grid-only (the old full-scrollback replay was
    // root cause #6). Cost: the clear (2J/3J) drops this pane's local
    // scrollback on an actual resize — accepted (owner ruling 2026-08-12).
    let disposed = false;
    const RESYNC_DEBOUNCE_MS = 300; // drag storms collapse into one resync
    const RESYNC_QUIET_MS = 250; // pane output must be quiet this long
    const RESYNC_MAX_WAIT_MS = 2000; // streaming panes: resync anyway
    const RESYNC_STABLE_GAP_MS = 120; // gap between the confirm captures
    // 2 pairs = 3 captures. Each capture takes the global mgr mutex that also
    // serializes keystrokes, so a busy-pane resync is felt as input latency —
    // 4 captures amplified that beyond what the extra confidence is worth
    // (owner ruling 2026-08-14).
    const RESYNC_STABLE_PAIRS = 2; // max consecutive pairs compared
    let lastOutputAt = 0;
    let resyncTimer: number | undefined;
    // Bumped per scheduled resync so an in-flight warm start — or an in-flight
    // capture chain from an EARLIER resync — can tell it went stale (see
    // maybeWarmStart / the `gen !== resyncGen` guards below).
    let resyncGen = 0;
    // "A resync owns this pane's repaint." Set when one is scheduled; stays true
    // after it PAINTS (its repaint supersedes the mount warm start, and the
    // generation counter cannot express that — a resync scheduled BEFORE warm
    // start dispatches shares its generation, so `gen !== resyncGen` never trips
    // and slow warm-start bytes would land AFTER the resync's clear+repaint:
    // appended, no clear, wrong cursor → garble until the next resize).
    //
    // INVARIANT: every terminal outcome of every chain — painted, resolved with
    // an empty capture (writeResync no-ops), or rejected — either paints the
    // pane or calls `releaseResync`, which hands the pane back to warm start.
    // No path may consume both, or the pane stays blank until the next resize.
    let resyncPending = false;
    /** This chain ended without painting — let a deferred warm start proceed. */
    const releaseResync = (gen: number) => {
      if (disposed || gen !== resyncGen) return; // a newer chain owns the pane
      resyncPending = false;
      maybeWarmStart(); // may have deferred on us; re-check now that we're done
    };

    /** Repaint the pane from a capture: escape-level clear, then the bytes.
     *
     * Clear via escape codes, NOT term.reset(): reset() also wipes terminal
     * MODES (application cursor keys, bracketed paste, mouse tracking) that the
     * capture cannot restore — arrows and scroll would break in the resynced
     * pane until its app re-asserted them. 2J = clear screen, 3J = clear
     * scrollback, H = home. Same visible effect as reset, modes untouched
     * (Ctrl+L never touches them either).
     *
     * Returns whether it actually painted — an empty capture is a no-op, and the
     * caller must then release the latch (see the INVARIANT above). */
    const writeResync = (bytesB64: string): boolean => {
      // An empty capture is not "the pane is empty" — it is a capture that told
      // us nothing (RPC returned blank). Clearing on it would DESTROY the pane's
      // real content to paint nothing, so treat it as a no-op.
      if (!bytesB64) return false;
      term.write("\x1b[2J\x1b[3J\x1b[H");
      term.write(b64ToBytes(bytesB64));
      return true;
    };

    const scheduleResync = () => {
      resyncGen++;
      resyncPending = true;
      if (resyncTimer) clearTimeout(resyncTimer);
      const gen = resyncGen;
      const startedAt = Date.now();

      // Busy-pane path: the quiet gate capped out, so the pane is STILL
      // streaming and any single capture may be a half-drawn frame — writing
      // that is the very garble we're repairing. tmux's grid is authoritative,
      // so instead of guessing, confirm it settled: capture repeatedly
      // ~RESYNC_STABLE_GAP_MS apart and only paint once two CONSECUTIVE
      // captures are byte-identical (the pane is between frames). After
      // RESYNC_STABLE_PAIRS pairs we paint the last capture anyway — i.e.
      // exactly today's capped-out behaviour, so this can only be better, never
      // worse. Worst case adds 2×120ms to a resync that already waited 2s.
      // Still zero keystroke injection (standing ruling, root cause #7).
      const confirmStable = (prev: string | undefined, capturesLeft: number) => {
        if (disposed || gen !== resyncGen) return;
        warmStartScreen(paneId)
          .then((w) => {
            if (disposed || gen !== resyncGen) return;
            const stable = prev !== undefined && w.bytesB64 === prev;
            if (stable || capturesLeft <= 1) {
              // Terminal: painted, or the capture was empty and nothing happened.
              if (!writeResync(w.bytesB64)) releaseResync(gen);
              return;
            }
            resyncTimer = window.setTimeout(
              () => confirmStable(w.bytesB64, capturesLeft - 1),
              RESYNC_STABLE_GAP_MS,
            );
          })
          .catch(() => {
            // pane gone mid-resync — cleanup handles it. This chain will never
            // paint, so release the pane back to a deferred warm start.
            releaseResync(gen);
          });
      };

      const attempt = (retriesLeft: number) => {
        // A newer resync was scheduled while this chain was pending: it will
        // capture a fresher grid, so abandon this one rather than painting the
        // old geometry over it.
        if (disposed || gen !== resyncGen) return;
        const quietFor = Date.now() - lastOutputAt;
        const cappedOut = Date.now() - startedAt >= RESYNC_MAX_WAIT_MS;
        if (quietFor < RESYNC_QUIET_MS) {
          if (!cappedOut) {
            resyncTimer = window.setTimeout(() => attempt(retriesLeft), 150);
            return;
          }
          // Never went quiet within MAX_WAIT — confirm by double capture.
          // N pairs = N+1 captures compared consecutively.
          confirmStable(undefined, RESYNC_STABLE_PAIRS + 1);
          return;
        }
        const outputMark = lastOutputAt;
        warmStartScreen(paneId)
          .then((w) => {
            if (disposed || gen !== resyncGen) return;
            // Output landed while the capture RPC was in flight — the capture
            // may hold a stale mid-frame. Retry once; then take what we have
            // (a busy pane repaints itself soon anyway).
            if (lastOutputAt !== outputMark && retriesLeft > 0) {
              attempt(retriesLeft - 1);
              return;
            }
            // Terminal: painted, or the capture was empty and nothing happened.
            if (!writeResync(w.bytesB64)) releaseResync(gen);
          })
          .catch(() => {
            // See confirmStable's catch: a dead chain must not keep a deferred
            // warm start blocked forever.
            releaseResync(gen);
          });
      };
      resyncTimer = window.setTimeout(() => attempt(1), RESYNC_DEBOUNCE_MS);
    };

    // ── Warm start, gated on the first applied rect (bug #4) ──────────────
    //
    // The backend now composes the mount replay grid-exactly: the last
    // `pane_height` captured lines are the visible grid, replayed verbatim
    // with tmux's cursor. That only lands right if THIS terminal is already
    // the pane's size — replaying an 80x40 grid into xterm's default 80x24
    // buffer wraps every over-wide row and the viewport ends up garbled, with
    // no resize afterwards to repair it (the first rect IS the mount rect, so
    // the rectApplied gate below never schedules a resync for it — and we do
    // not force one either: every resync costs this pane's local scrollback,
    // and it is redundant once the mount replay is exact).
    //
    // So warm start waits for BOTH: the `pane:data` listener registered (no
    // live output may be lost) and the terminal sized (buffer is pane-sized).
    // Whichever lands second fires it; it fires exactly once.
    let listenerReady = false;
    let sizeSettled = false;
    let warmStarted = false; // it PAINTED (not merely "was attempted")
    let warmStartInFlight = false;
    // A dispatch that resolves stale or empty does NOT consume the warm start —
    // otherwise a resync that then fails to paint leaves the pane blank forever
    // (both consumed). It stays eligible and any later trigger retries it, so
    // cap the retries: an RPC that keeps returning nothing must not spin.
    let warmStartTries = 0;
    const WARM_START_MAX_TRIES = 3;
    const maybeWarmStart = () => {
      if (disposed || warmStarted || warmStartInFlight) return;
      if (!listenerReady || !sizeSettled) return;
      // A resync owns the repaint: defer WITHOUT consuming the one shot. If that
      // chain paints, `resyncPending` stays true and warm start never fires
      // (correct — the resync's grid is the fresher one). If it ends without
      // painting, `releaseResync` calls back in here and we dispatch then.
      if (resyncPending) return;
      if (warmStartTries >= WARM_START_MAX_TRIES) return;
      warmStartTries++;
      warmStartInFlight = true;
      // A resize can land while the capture RPC is in flight; the resync it
      // schedules paints a fresher grid at the NEW size, so this reply is
      // stale — writing it would repaint the old geometry over the new one.
      // The generation guard alone is not enough: a resync scheduled BEFORE this
      // dispatch shares this generation, so it never trips — hence `resyncPending`.
      const gen = resyncGen;
      warmStart(paneId)
        .then((w) => {
          warmStartInFlight = false;
          if (disposed || gen !== resyncGen || resyncPending || !w.bytesB64)
            return; // stale or nothing to paint — stays eligible for a retry
          warmStarted = true;
          term.write(b64ToBytes(w.bytesB64));
        })
        .catch(() => {
          warmStartInFlight = false;
        });
    };
    // Safety net: a pane whose tmux geometry never parses gets no rect at all
    // (PaneGrid falls back to a CSS grid and passes rect=undefined). Waiting
    // forever would leave it permanently BLANK, which is worse than an
    // unsized replay — so after a grace period we warm start anyway.
    const WARM_START_RECT_WAIT_MS = 1500;
    const rectWaitTimer = window.setTimeout(() => {
      // Firing this net means the pane replays UNSIZED and the first real rect
      // (if one ever arrives) resyncs it, costing its local scrollback. Silent
      // scrollback loss is undiagnosable from a bug report — say so.
      console.warn(
        `[xterm] pane ${paneId}: no rect within ${WARM_START_RECT_WAIT_MS}ms — warm starting unsized (a later rect will resync and drop local scrollback)`,
      );
      sizeSettled = true; // "no rect is coming" — unblock the gate
      maybeWarmStart();
    }, WARM_START_RECT_WAIT_MS);

    // Mirror the tmux pane's size exactly. Runs on mount (rect is usually
    // already known from the boot list_state) and on every layout change.
    // Hidden tabs resize too — no layout box needed, unlike the old fit().
    let rectApplied = false;
    createEffect(() => {
      const r = props.rect;
      if (!r || !(r.w > 0) || !(r.h > 0)) return;
      const changed = term.cols !== r.w || term.rows !== r.h;
      if (changed) term.resize(r.w, r.h);
      // Only a size change AFTER the mount rect is a live tmux resize this
      // buffer diverged from; the mount rect is repaired by warm start below.
      // Exception: if the safety net already warm-started us into an unsized
      // buffer, the FIRST rect is a divergence too and must be resynced.
      if (changed && (rectApplied || warmStarted)) scheduleResync();
      rectApplied = true;
      clearTimeout(rectWaitTimer);
      sizeSettled = true;
      maybeWarmStart();
    });

    // Output: write decoded bytes verbatim (xterm is a full VT emulator).
    let unlistenData: UnlistenFn | undefined;
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
      // Warm start: only AFTER the live `pane:data` listener is registered may
      // we replay the existing screen + scrollback. The control client streams
      // only `%output` produced after it attached, so a re-attached pane would
      // paint blank without this. Ordering matters: the listener is live first,
      // so any live output during the fetch isn't lost — a little duplicated
      // tail is harmless, a fully blank pane is not.
      listenerReady = true;
      maybeWarmStart();
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
      clearTimeout(rectWaitTimer);
      if (measureTimer) clearInterval(measureTimer);
      ro.disconnect();
      dataSub.dispose();
      unlistenData?.();
      term.dispose();
    });
  });

  return <div ref={hostEl} class="xterm-host" />;
};

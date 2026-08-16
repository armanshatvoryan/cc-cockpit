// SessionsPanel (Wave D) — the sessions sidebar: a DOCKED right column listing
// the parked (`_sb:`) tmux windows.
//
// Docked, not an overlay — same shape as the file-tree sidebar on the left, so
// the grid shrinks around it and the parked sessions stay in view while you work
// in a tab. A stored session is fully alive: its panes are still polled, so each
// row's badge is the WORST status across the window (a parked agent that starts
// waiting on you goes red here without a tab existing for it).
//
// Interactions: clicking the row (or ↩) restores the session as a tab; × kills
// it, with the same live-pane confirmation the tab close uses. ⌘S toggles. Each
// of the three is a real <button>, so Tab/Enter/Space work natively — see the
// note on the row markup below.
//
// The kill confirmation lives on the PANEL, not on the row: `store.stored` is
// replaced wholesale on every reconcile (the rows carry no terminal identity, so
// they are deliberately not keyed-reconciled), which re-creates every row — row-
// local state would be dropped by any background topology event mid-confirm.

import { createSignal, For, Show, type Component } from "solid-js";
import type { StoredSessionInfo } from "../ipc";
import {
  store,
  closeSessionsPanel,
  killStoredSession,
  restoreStoredSession,
  storedStatus,
  storedTooltip,
} from "../store";
import { StatusBadge } from "./StatusBadge";

/** A pending kill awaiting confirmation. Holds the window id + label by VALUE —
 *  the `StoredSessionInfo` object it came from does not survive a reconcile. */
interface PendingKill {
  windowId: string;
  label: string;
  livePanes: string[];
}

const SessionRow: Component<{
  session: StoredSessionInfo;
  onNeedsConfirm: (p: PendingKill) => void;
}> = (props) => {
  const restore = () => void restoreStoredSession(props.session.windowId);

  // No stopPropagation needed: the actions are siblings of the row button now,
  // so a click here never reaches a restore handler.
  async function onKill() {
    const res = await killStoredSession(props.session.windowId, false);
    if (res.needsConfirm) {
      props.onNeedsConfirm({
        windowId: props.session.windowId,
        label: props.session.label,
        livePanes: res.livePanes,
      });
    }
  }

  // The row body is a REAL <button>, with the actions as SIBLINGS rather than
  // children. A `role="button"` div wrapping real buttons is invalid for
  // assistive tech, and an ancestor keydown handler would swallow Enter/Space
  // aimed at the action buttons — restoring the session instead of killing it.
  // Three plain buttons side by side need no synthetic keyboard handling at all.
  return (
    <div class="sb-row">
      <button
        class="sb-open"
        title={storedTooltip(props.session)}
        onClick={restore}
      >
        <StatusBadge status={storedStatus(props.session)} />
        <span class="sb-label">{props.session.label}</span>
      </button>
      <span class="sb-actions">
        <button class="sb-act" title="Restore as a tab" onClick={restore}>
          ↩
        </button>
        <button class="sb-act sb-act-danger" title="Kill session" onClick={onKill}>
          ×
        </button>
      </span>
    </div>
  );
};

export const SessionsPanel: Component = () => {
  const [pendingKill, setPendingKill] = createSignal<PendingKill | null>(null);

  async function confirmKill() {
    const p = pendingKill();
    setPendingKill(null);
    if (p) await killStoredSession(p.windowId, true);
  }

  return (
    <>
      <div class="sb-panel">
        <div class="sb-header">
          <span class="sb-title">SESSIONS</span>
          <span class="sb-count">{store.stored.length}</span>
          <span class="sb-spacer" />
          <button
            class="ft-icon-btn"
            title="Hide sessions sidebar (⌘S)"
            onClick={closeSessionsPanel}
          >
            ×
          </button>
        </div>

        <div class="sb-list">
          <Show
            when={store.stored.length > 0}
            fallback={
              <div class="sb-empty">
                No parked sessions. Use ⇥ on a pane toolbar to park it here — it
                keeps running, and its status stays visible.
              </div>
            }
          >
            <For each={store.stored}>
              {(s) => (
                <SessionRow session={s} onNeedsConfirm={(p) => setPendingKill(p)} />
              )}
            </For>
          </Show>
        </div>
      </div>

      <Show when={pendingKill()}>
        {(p) => (
          <div class="modal-overlay" onClick={() => setPendingKill(null)}>
            <div class="modal confirm" onClick={(e) => e.stopPropagation()}>
              <div class="modal-title">Kill session “{p().label}”?</div>
              <p class="confirm-body">
                {p().livePanes.length} live pane
                {p().livePanes.length === 1 ? "" : "s"} will be killed:{" "}
                <span class="mono">{p().livePanes.join(", ")}</span>
              </p>
              <div class="modal-actions">
                <button class="btn btn-ghost" onClick={() => setPendingKill(null)}>
                  Cancel
                </button>
                <button class="btn btn-danger" onClick={() => void confirmKill()}>
                  Kill session
                </button>
              </div>
            </div>
          </div>
        )}
      </Show>
    </>
  );
};

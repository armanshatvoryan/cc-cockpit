// Pane — one grid cell: a PaneToolbar (~28px) over an XtermHost.
//
// Clicking anywhere routes focus to this pane (blue border when focused). The
// XtermHost is keyed by paneId at the call site (PaneGrid) so it stays mounted
// for the pane's whole life and is never re-created on focus changes.

import { createMemo, createSignal, onCleanup, Show, type Component } from "solid-js";
import type { LayoutRect, PaneInfo } from "../ipc";
import {
  focusPane,
  doClosePane,
  activePanes,
  sendPaneToNewTab,
  sendPaneToSidebar,
  paneLabel,
  directLaunchCc,
  copySessionId,
} from "../store";
import { StatusBadge } from "./StatusBadge";
import { LaunchDialog } from "./LaunchDialog";
import { XtermHost } from "../XtermHost";

export const Pane: Component<{
  pane: PaneInfo;
  focused: boolean;
  /** This pane's tmux rect (layout mirror) — XtermHost resizes to match. */
  rect?: LayoutRect;
}> = (props) => {
  const [showLaunch, setShowLaunch] = createSignal(false);
  // Transient "copied" acknowledgement on the session chip. The timer is owned
  // here and cleared on unmount so a pane killed mid-flash cannot fire into a
  // disposed component.
  const [copied, setCopied] = createSignal(false);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(copiedTimer));

  async function doCopySessionId(sessionId: string) {
    await copySessionId(sessionId);
    setCopied(true);
    clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => setCopied(false), 1200);
  }

  // "fresh" panes (a bare shell, not yet running CC) are the launch targets.
  const canLaunch = () =>
    !props.pane.dead &&
    (props.pane.status === "IDLE" || props.pane.status === "UNKNOWN");
  // A2 — team-board member name (falls back to pane title / id) + tooltip.
  // Memoized: read twice per render (text + tooltip) and it joins run rows.
  const label = createMemo(() => paneLabel(props.pane));

  return (
    <div
      class="pane"
      classList={{ focused: props.focused, dead: props.pane.dead }}
      onMouseDown={() => focusPane(props.pane.paneId)}
    >
      <div class="pane-toolbar">
        <StatusBadge status={props.pane.status} />
        <span class="pane-title" title={label().tooltip}>
          {label().text}
        </span>
        <span class="pane-id">{props.pane.paneId}</span>
        <span class="toolbar-spacer" />

        <Show when={canLaunch()}>
          <button
            class="tb-btn"
            title="Launch Claude here (⌥-click for options; refuses a pane already running claude)"
            onClick={(e) => {
              e.stopPropagation();
              if (e.altKey) {
                setShowLaunch(true);
                return;
              }
              void directLaunchCc(props.pane.paneId, props.pane.cwd);
            }}
          >
            Launch CC
          </button>
          <button
            class="tb-btn tb-caret"
            title="Launch options (model / flags / shell)"
            onClick={(e) => {
              e.stopPropagation();
              setShowLaunch(true);
            }}
          >
            ⌄
          </button>
        </Show>

        {/* Claude session id. Rendered only once the cockpit-session-map hook
            has published one for this pane, so a shell pane shows nothing
            rather than an empty slot. Click copies the FULL uuid — the label is
            the short form purely so it fits the toolbar. */}
        <Show when={props.pane.sessionId}>
          {(sessionId) => (
            <button
              class="tb-btn tb-session"
              classList={{ copied: copied() }}
              title={`Claude session ${sessionId()} — click to copy`}
              onClick={(e) => {
                e.stopPropagation();
                void doCopySessionId(sessionId());
              }}
            >
              {copied() ? "copied" : `⧉ ${sessionId().slice(0, 8)}`}
            </button>
          )}
        </Show>

        <Show when={activePanes().length > 1}>
          <button
            class="tb-btn"
            title="Send pane to a new tab"
            onClick={(e) => {
              e.stopPropagation();
              void sendPaneToNewTab(props.pane.paneId);
            }}
          >
            ⤴
          </button>
        </Show>

        {/* Park in the sessions sidebar. NOT gated on pane count: a sole pane is
            exactly the case you want parked (its whole tab becomes the stored
            session), so the button is always available. */}
        <button
          class="tb-btn"
          title="Send pane to the sessions sidebar (keeps running)"
          onClick={(e) => {
            e.stopPropagation();
            void sendPaneToSidebar(props.pane.paneId);
          }}
        >
          ⇥
        </button>

        <button
          class="tb-btn tb-danger"
          title="Kill pane"
          onClick={(e) => {
            e.stopPropagation();
            void doClosePane(props.pane.paneId, "kill");
          }}
        >
          ✕
        </button>
      </div>

      <div class="pane-body">
        <Show
          when={!props.pane.dead}
          fallback={
            <div class="pane-dead-overlay">
              pane {props.pane.paneId} exited
            </div>
          }
        >
          <XtermHost paneId={props.pane.paneId} rect={props.rect} />
        </Show>
      </div>

      <Show when={showLaunch()}>
        <LaunchDialog
          paneId={props.pane.paneId}
          defaultCwd={props.pane.cwd}
          onClose={() => setShowLaunch(false)}
        />
      </Show>
    </div>
  );
};

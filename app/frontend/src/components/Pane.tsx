// Pane — one grid cell: a PaneToolbar (~28px) over an XtermHost.
//
// Clicking anywhere routes focus to this pane (blue border when focused). The
// XtermHost is keyed by paneId at the call site (PaneGrid) so it stays mounted
// for the pane's whole life and is never re-created on focus changes.

import { createSignal, Show, type Component } from "solid-js";
import type { LayoutRect, PaneInfo } from "../ipc";
import { interruptPane } from "../ipc";
import {
  focusPane,
  doClosePane,
  activePanes,
  sendPaneToNewTab,
  paneLabel,
  directLaunchCc,
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

  const isWorking = () => props.pane.status === "WORKING";
  // "fresh" panes (a bare shell, not yet running CC) are the launch targets.
  const canLaunch = () =>
    !props.pane.dead &&
    (props.pane.status === "IDLE" || props.pane.status === "UNKNOWN");
  // A2 — team-board member name (falls back to pane title / id) + tooltip.
  const label = () => paneLabel(props.pane);

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
            title="Launch Claude here (⌥-click for options)"
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

        <button
          class="tb-btn"
          classList={{ active: isWorking() }}
          title="Interrupt (Ctrl+C)"
          disabled={!isWorking()}
          onClick={(e) => {
            e.stopPropagation();
            void interruptPane(props.pane.paneId).catch(() => {});
          }}
        >
          Interrupt
        </button>

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

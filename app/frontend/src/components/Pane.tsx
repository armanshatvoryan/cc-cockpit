// Pane — one grid cell: a PaneToolbar (~28px) over an XtermHost.
//
// Clicking anywhere routes focus to this pane (blue border when focused). The
// XtermHost is keyed by paneId at the call site (PaneGrid) so it stays mounted
// for the pane's whole life and is never re-created on focus changes.

import { createSignal, Show, type Component } from "solid-js";
import type { PaneInfo } from "../ipc";
import { interruptPane } from "../ipc";
import { focusPane, doClosePane } from "../store";
import { StatusBadge } from "./StatusBadge";
import { LaunchDialog } from "./LaunchDialog";
import { XtermHost } from "../XtermHost";

export const Pane: Component<{ pane: PaneInfo; focused: boolean }> = (props) => {
  const [showLaunch, setShowLaunch] = createSignal(false);

  const isWorking = () => props.pane.status === "WORKING";
  // "fresh" panes (a bare shell, not yet running CC) are the launch targets.
  const canLaunch = () =>
    !props.pane.dead &&
    (props.pane.status === "IDLE" || props.pane.status === "UNKNOWN");

  return (
    <div
      class="pane"
      classList={{ focused: props.focused, dead: props.pane.dead }}
      onMouseDown={() => focusPane(props.pane.paneId)}
    >
      <div class="pane-toolbar">
        <StatusBadge status={props.pane.status} />
        <span class="pane-title" title={props.pane.cwd}>
          {props.pane.title || props.pane.paneId}
        </span>
        <span class="pane-id">{props.pane.paneId}</span>
        <span class="toolbar-spacer" />

        <Show when={canLaunch()}>
          <button
            class="tb-btn"
            title="Launch Claude / shell"
            onClick={(e) => {
              e.stopPropagation();
              setShowLaunch(true);
            }}
          >
            Launch CC
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
          <XtermHost paneId={props.pane.paneId} />
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

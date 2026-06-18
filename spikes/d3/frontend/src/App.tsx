// App root for the D3 spike. Attaches the control client on mount, then renders
// XtermHost panes. Default renders the first pane (%0); the "+ pane" button asks
// the backend to split so we can eyeball a 2-up grid (D3-a fidelity / D3-e).

import { createSignal, For, onMount, type Component } from "solid-js";
import { attachSession, onPaneTopology } from "./ipc";
import { XtermHost } from "./XtermHost";

const SOCKET = "cockpit-d3";
const SESSION = "d3live";

export const App: Component = () => {
  // Pane ids we know about. Seeded with %0 (the session's first pane); topology
  // events (window-pane-changed / layout-change) reveal new pane ids on split.
  const [panes, setPanes] = createSignal<string[]>(["%0"]);
  const [status, setStatus] = createSignal("attaching…");

  onMount(() => {
    attachSession(SOCKET, SESSION)
      .then(() => setStatus(`attached: -L ${SOCKET} / ${SESSION}`))
      .catch((e) => setStatus(`attach failed: ${e}`));

    void onPaneTopology((t) => {
      // When tmux tells us a (new) pane became active, ensure it has a host.
      if (t.paneId) {
        setPanes((prev) => (prev.includes(t.paneId!) ? prev : [...prev, t.paneId!]));
      }
    });
  });

  return (
    <div class="app">
      <header class="bar">
        <span class="title">CC Cockpit — D3 control-mode spike</span>
        <span class="status">{status()}</span>
      </header>
      <div class="grid" style={{ "grid-template-columns": `repeat(${Math.min(panes().length, 2)}, 1fr)` }}>
        <For each={panes()}>
          {(paneId) => (
            <div class="pane">
              <div class="pane-label">{paneId}</div>
              <div class="pane-body">
                <XtermHost paneId={paneId} visible={true} />
              </div>
            </div>
          )}
        </For>
      </div>
    </div>
  );
};

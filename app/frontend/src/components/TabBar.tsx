// TabBar — one chip per tab. Active is highlighted; an aggregate attention dot
// shows when any pane in the tab needs input (red) or is working (blue), and a
// red count pill appears while any pane is waiting on the user. The cwd badge
// shows the tab's folder name (branch + git counters live in its tooltip).
//
// Interactions:
//   click           -> switch active tab
//   double-click    -> inline rename (LOCAL ONLY for v1 — no backend rename)
//   close (×)       -> close_tab; if live panes, a confirm modal then force
//   "+"             -> create_tab

import { createSignal, For, Show, type Component } from "solid-js";
import type { TabInfo } from "../ipc";
import {
  store,
  activeTabId,
  switchTab,
  newTab,
  requestCloseTab,
  renameTabLocal,
  tabDisplayName,
  tabAttention,
  tabCwd,
  tabFolderName,
  tabNeedsInputCount,
  gitStatus,
} from "../store";

const TabChip: Component<{ tab: TabInfo }> = (props) => {
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [confirming, setConfirming] = createSignal<string[] | null>(null);

  const active = () => activeTabId() === props.tab.tabId;
  const attention = () => tabAttention(props.tab);
  const needsInput = () => tabNeedsInputCount(props.tab);
  // dev#2 — git status of this tab's first-pane cwd (null/absent ⇒ git extras
  // hidden; the badge itself shows the cwd folder and needs no repo).
  const git = () => gitStatus[props.tab.tmuxWindowId];
  const folder = () => tabFolderName(props.tab);
  const badgeTitle = () => {
    const g = git();
    const gitPart = g
      ? ` · ${g.branch} · ${g.changed} changed · ${g.untracked} untracked${
          g.ahead ? ` · ↑${g.ahead}` : ""
        }${g.behind ? ` · ↓${g.behind}` : ""}`
      : "";
    return `${tabCwd(props.tab)}${gitPart}`;
  };

  function startRename() {
    setDraft(tabDisplayName(props.tab));
    setEditing(true);
  }
  function commitRename() {
    const name = draft().trim();
    if (name) renameTabLocal(props.tab.tmuxWindowId, name);
    setEditing(false);
  }

  async function onCloseClick(e: MouseEvent) {
    e.stopPropagation();
    const res = await requestCloseTab(props.tab.tabId, false);
    if (res.needsConfirm) setConfirming(res.livePanes);
  }
  async function confirmClose() {
    setConfirming(null);
    await requestCloseTab(props.tab.tabId, true);
  }

  return (
    <>
      <div
        class="tab-chip"
        classList={{ active: active() }}
        onClick={() => switchTab(props.tab.tabId)}
        onDblClick={startRename}
        title={props.tab.tabId}
      >
        <span
          class="tab-dot"
          classList={{
            "attn-input": attention() === "needs_input",
            "attn-working": attention() === "working",
            "attn-none": attention() === "none",
          }}
        />
        <Show
          when={!editing()}
          fallback={
            <input
              class="tab-rename-input"
              value={draft()}
              onInput={(e) => setDraft(e.currentTarget.value)}
              onBlur={commitRename}
              onKeyDown={(e) => {
                if (e.key === "Enter") commitRename();
                if (e.key === "Escape") setEditing(false);
              }}
              onClick={(e) => e.stopPropagation()}
              autofocus
            />
          }
        >
          <span class="tab-name">{tabDisplayName(props.tab)}</span>
        </Show>
        <Show when={needsInput() > 0}>
          <span class="tab-attn-badge" title="Claude is waiting for your input">
            {needsInput()}
          </span>
        </Show>
        <Show when={folder()}>
          <span class="git-badge" title={badgeTitle()}>
            <span class="git-branch">{folder()}</span>
            <Show when={git()}>
              {(g) => (
                <>
                  <Show when={g().dirty}>
                    <span class="git-dirty" />
                  </Show>
                  <Show when={g().ahead > 0}>
                    <span class="git-ab">↑{g().ahead}</span>
                  </Show>
                  <Show when={g().behind > 0}>
                    <span class="git-ab">↓{g().behind}</span>
                  </Show>
                </>
              )}
            </Show>
          </span>
        </Show>
        <button class="tab-close" title="Close tab" onClick={onCloseClick}>
          ×
        </button>
      </div>

      <Show when={confirming()}>
        {(panes) => (
          <div class="modal-overlay" onClick={() => setConfirming(null)}>
            <div class="modal confirm" onClick={(e) => e.stopPropagation()}>
              <div class="modal-title">Close tab “{tabDisplayName(props.tab)}”?</div>
              <p class="confirm-body">
                {panes().length} live pane{panes().length === 1 ? "" : "s"} will
                be killed: <span class="mono">{panes().join(", ")}</span>
              </p>
              <div class="modal-actions">
                <button class="btn btn-ghost" onClick={() => setConfirming(null)}>
                  Cancel
                </button>
                <button class="btn btn-danger" onClick={confirmClose}>
                  Close tab
                </button>
              </div>
            </div>
          </div>
        )}
      </Show>
    </>
  );
};

export const TabBar: Component = () => {
  return (
    <div class="tab-bar">
      <span class="brand">CC&nbsp;Cockpit</span>
      <div class="tab-list">
        <For each={store.tabs}>{(tab) => <TabChip tab={tab} />}</For>
      </div>
      <button class="tab-new" title="New tab (⌘T)" onClick={() => void newTab()}>
        +
      </button>
    </div>
  );
};

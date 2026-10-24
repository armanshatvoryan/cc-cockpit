// TabBar — one chip per tab. Active is highlighted; an aggregate attention dot
// shows when any pane in the tab needs input (red) or is working (blue), and a
// red count pill appears while any pane is waiting on the user. The cwd badge
// shows the tab's folder name (branch + git counters live in its tooltip).
//
// Interactions:
//   click           -> switch active tab
//   double-click    -> inline rename (LOCAL ONLY for v1 — no backend rename)
//   drag onto a pane -> join this tab's panes beside it (reverse of ⤴)
//   drag onto a tab  -> reorder: land before/after it (pointer's half of the chip)
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
  openWindowsTab,
  tabDrag,
  setTabDrag,
  joinTabIntoPane,
  reorderTab,
  isNoopTabMove,
} from "../store";

// Windows-laptop launcher: logo button -> menu (Claude Code / plain PowerShell),
// each opening a new tab that SSHes to `win`.
const WindowsButton: Component = () => {
  const [open, setOpen] = createSignal(false);
  const pick = (kind: "claude" | "shell") => {
    setOpen(false);
    void openWindowsTab(kind);
  };
  return (
    <div class="win-launch">
      <button
        class="tab-new win-btn"
        classList={{ "win-btn-on": open() }}
        title="Windows laptop"
        aria-label="Windows laptop"
        onClick={() => setOpen(!open())}
      >
        <svg width="12" height="12" viewBox="0 0 16 16" aria-hidden="true">
          <rect x="0" y="0" width="7.5" height="7.5" fill="currentColor" />
          <rect x="8.5" y="0" width="7.5" height="7.5" fill="currentColor" />
          <rect x="0" y="8.5" width="7.5" height="7.5" fill="currentColor" />
          <rect x="8.5" y="8.5" width="7.5" height="7.5" fill="currentColor" />
        </svg>
      </button>
      <Show when={open()}>
        <div class="ft-menu-backdrop" onClick={() => setOpen(false)} />
        <div class="win-menu">
          <button class="ft-menu-item" onClick={() => pick("claude")}>
            Claude Code on Windows
          </button>
          <button class="ft-menu-item" onClick={() => pick("shell")}>
            PowerShell on Windows
          </button>
        </div>
      </Show>
    </div>
  );
};

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

  // Reorder drop marker: which edge of this chip the dragged tab would land on.
  const dropSide = () => {
    const over = tabDrag()?.overTab;
    if (over?.windowId !== props.tab.tmuxWindowId) return null;
    return over.after ? "after" : "before";
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

  // Drag-to-join / drag-to-reorder. Pointer events, not HTML5 DnD (flaky in
  // WKWebView): past a small threshold the chip is "dragging", the pane or
  // chip under the pointer is tracked for the drop highlight, and release
  // over a pane joins the tab there, over a chip moves it before/after that
  // chip. The click that can trail a drag must not switch tabs.
  let suppressClick = false;
  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0 || editing()) return;
    if ((e.target as HTMLElement).closest(".tab-close")) return;
    const sx = e.clientX;
    const sy = e.clientY;
    let dragging = false;
    const paneAt = (x: number, y: number) =>
      (document.elementFromPoint(x, y)?.closest("[data-pane-id]") as HTMLElement | null)
        ?.dataset.paneId ?? null;
    const tabAt = (x: number, y: number) => {
      const chip = document
        .elementFromPoint(x, y)
        ?.closest("[data-window-id]") as HTMLElement | null;
      const windowId = chip?.dataset.windowId;
      if (!chip || !windowId) return null;
      const r = chip.getBoundingClientRect();
      const after = x > r.left + r.width / 2;
      return isNoopTabMove(props.tab.tmuxWindowId, windowId, after)
        ? null
        : { windowId, after };
    };
    const move = (ev: PointerEvent) => {
      if (!dragging && Math.hypot(ev.clientX - sx, ev.clientY - sy) < 6) return;
      if (!dragging) {
        dragging = true;
        document.body.classList.add("tab-dragging");
      }
      setTabDrag({
        windowId: props.tab.tmuxWindowId,
        overPaneId: paneAt(ev.clientX, ev.clientY),
        overTab: tabAt(ev.clientX, ev.clientY),
      });
    };
    const end = (ev: PointerEvent) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      if (!dragging) return;
      document.body.classList.remove("tab-dragging");
      setTabDrag(null);
      suppressClick = true;
      setTimeout(() => (suppressClick = false), 0);
      if (ev.type !== "pointerup") return;
      const pane = paneAt(ev.clientX, ev.clientY);
      const tab = tabAt(ev.clientX, ev.clientY);
      if (pane) void joinTabIntoPane(props.tab.tmuxWindowId, pane);
      else if (tab) void reorderTab(props.tab.tmuxWindowId, tab.windowId, tab.after);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", end);
    window.addEventListener("pointercancel", end);
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
        classList={{
          active: active(),
          dragging: tabDrag()?.windowId === props.tab.tmuxWindowId,
          "drop-before": dropSide() === "before",
          "drop-after": dropSide() === "after",
        }}
        data-window-id={props.tab.tmuxWindowId}
        onClick={() => {
          if (!suppressClick) switchTab(props.tab.tabId);
        }}
        onPointerDown={onPointerDown}
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
      <WindowsButton />
    </div>
  );
};

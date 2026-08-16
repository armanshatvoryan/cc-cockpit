// App — the cockpit shell. TabBar (top) + PaneGrid (fill) + a status footer.
//
// Boot flow: cockpit_init -> reconcile state -> subscribe to events. While the
// init promise is in flight we show a boot line; with zero tabs we show an
// empty state with a big "New tab" button.

import { onCleanup, onMount, Show, type Component } from "solid-js";
import {
  store,
  bootCockpit,
  shutdownCockpit,
  newTab,
  clearError,
  sidebarVisible,
  ftInitHome,
  settingsOpen,
  awake,
  toggleAwake,
} from "./store";
import { installKeyboard } from "./keyboard";
import { TabBar } from "./components/TabBar";
import { PaneGrid } from "./components/PaneGrid";
import { FileTreePanel } from "./components/FileTreePanel";
import { InventoryPanel } from "./components/InventoryPanel";
import { TeamBoardPanel } from "./components/TeamBoardPanel";
import { SpinupDialog } from "./components/SpinupDialog";
import { SettingsDialog } from "./components/SettingsDialog";

export const App: Component = () => {
  onMount(() => {
    void bootCockpit();
    void ftInitHome(); // resolve $HOME for the file-tree breadcrumb (cd-nav)
    installKeyboard();
  });
  onCleanup(() => shutdownCockpit());

  return (
    <div class="app">
      <Show
        when={store.ready}
        fallback={<div class="boot">cockpit booting…</div>}
      >
        <div class="app-body">
          <Show when={sidebarVisible()}>
            <FileTreePanel />
          </Show>

          <div class="main-col">
            <TabBar />

            <Show
              when={store.tabs.length > 0}
              fallback={
                <div class="empty-state">
                  <h1 class="empty-title">No tabs yet</h1>
                  <p class="empty-sub">
                    Open a tab to get a tmux window with a live terminal.
                  </p>
                  <button class="btn btn-primary btn-lg" onClick={() => void newTab()}>
                    New tab
                  </button>
                  <p class="empty-hint">or press ⌘T</p>
                </div>
              }
            >
              <PaneGrid />
            </Show>

            <footer class="footer">
              <span class="footer-item">
                session <span class="mono">{store.session || "—"}</span>
              </span>
              <span class="footer-item">
                socket <span class="mono">{store.socket || "—"}</span>
              </span>
              <span class="footer-spacer" />
              <button
                class="awake-toggle"
                classList={{ on: awake().on }}
                title={
                  awake().on
                    ? awake().lidProof
                      ? "Keeping the Mac awake, lid close included — click to allow sleep again"
                      : "Keeping the Mac awake while idle — lid close still sleeps (root helper not installed: sudo app/scripts/install-sleeplever.sh)"
                    : "Keep the Mac awake (blocks system sleep; display may still sleep)"
                }
                onClick={() => void toggleAwake()}
              >
                ☕ {awake().on ? (awake().lidProof ? "awake · lid-proof" : "awake · idle-only") : "sleep ok"}
              </button>
              <span class="footer-item">
                {store.tabs.length} tab{store.tabs.length === 1 ? "" : "s"} ·{" "}
                {store.panes.length} pane{store.panes.length === 1 ? "" : "s"}
              </span>
              <span class="footer-keys">
                ⌘B files · ⌘T tab · ⌘1-9 switch · ⌘D split · ⌘I inventory · ⌘⇧T
                teams · ⌘, settings
              </span>
            </footer>
          </div>
        </div>

        <InventoryPanel />
        <TeamBoardPanel />
        <SpinupDialog />
        <Show when={settingsOpen()}>
          <SettingsDialog />
        </Show>
      </Show>

      <Show when={store.error}>
        <div class="toast" onClick={clearError}>
          <span class="toast-msg">{store.error}</span>
          <button class="toast-x" aria-label="Dismiss">
            ×
          </button>
        </div>
      </Show>
    </div>
  );
};

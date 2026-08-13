// App — the cockpit shell. TabBar (top) + PaneGrid (fill) + a status footer.
//
// Boot flow: cockpit_init -> reconcile state -> subscribe to events. While the
// init promise is in flight we show a boot line; with zero tabs we show an
// empty state with a big "New tab" button.

import { onCleanup, onMount, Show, type Component } from "solid-js";
import {
  store,
  decideBoot,
  shutdownCockpit,
  newTab,
  clearError,
  sidebarVisible,
  ftInitHome,
  settingsOpen,
  onboardingOpen,
} from "./store";
import { installKeyboard } from "./keyboard";
import { TabBar } from "./components/TabBar";
import { PaneGrid } from "./components/PaneGrid";
import { FileTreePanel } from "./components/FileTreePanel";
import { InventoryPanel } from "./components/InventoryPanel";
import { TeamBoardPanel } from "./components/TeamBoardPanel";
import { SpinupDialog } from "./components/SpinupDialog";
import { SettingsDialog } from "./components/SettingsDialog";
import { OnboardingWizard } from "./components/OnboardingWizard";

export const App: Component = () => {
  onMount(() => {
    void decideBoot();
    void ftInitHome(); // resolve $HOME for the file-tree breadcrumb (cd-nav)
    installKeyboard();
  });
  onCleanup(() => shutdownCockpit());

  return (
    <div class="app">
      <Show when={onboardingOpen()}>
        <OnboardingWizard />
      </Show>

      <Show
        when={store.ready}
        fallback={
          <Show when={!onboardingOpen()}>
            <div class="boot">cockpit booting…</div>
          </Show>
        }
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

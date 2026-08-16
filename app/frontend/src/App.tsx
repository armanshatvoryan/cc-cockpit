// App — the cockpit shell. TabBar (top) + PaneGrid (fill) + a status footer.
//
// Boot flow: cockpit_init -> reconcile state -> subscribe to events. While the
// init promise is in flight we show a boot line; with zero tabs we show an
// empty state with a big "New tab" button.

import { For, onCleanup, onMount, Show, type Component } from "solid-js";
import {
  store,
  bootCockpit,
  shutdownCockpit,
  newTab,
  clearError,
  sidebarVisible,
  sessionsPanelOpen,
  toggleSessionsPanel,
  reachablePanes,
  storedNeedsInputCount,
  ftInitHome,
  settingsOpen,
  usage,
  usageAgeSec,
} from "./store";
import type { UsageWindow } from "./ipc";
import { installKeyboard } from "./keyboard";
import { TabBar } from "./components/TabBar";
import { PaneGrid } from "./components/PaneGrid";
import { FileTreePanel } from "./components/FileTreePanel";
import { InventoryPanel } from "./components/InventoryPanel";
import { TeamBoardPanel } from "./components/TeamBoardPanel";
import { SessionsPanel } from "./components/SessionsPanel";
import { SpinupDialog } from "./components/SpinupDialog";
import { SettingsDialog } from "./components/SettingsDialog";

// ── C-2: usage footer segment ────────────────────────────────────────────────
// Compact "⛁ 5h <burn> · 7d <burn> · <velocity>/min" readout + a hover tooltip
// with the full breakdown (session/socket, per-model split, in/out/cache,
// message count, "computed Xs ago"). `tokensPerMin` is byte-identical on both
// windows (whole-scan velocity, not a per-window average) — rendered once.
//
// Headline "burn" = output + input tokens, NOT UsageWindow.totalTokens. C-1's
// review flagged totalTokens as cache-read-dominated (the live corpus showed a
// week window of 1.12B tokens, ~99% cache reads) — a footer reading "1.1B
// tokens" reads as a bug. Per C-1's own recommendation, the headline leads
// with output+input; total + the cache figure are still one hover away.

/** Compact "1.2M" / "42k" / "18.4M" style formatting. No decimal is shown when
 *  it would just be ".0" noise. */
function formatCompact(n: number): string {
  if (!Number.isFinite(n)) return "—";
  const abs = Math.abs(n);
  const unit = abs >= 1e9 ? 1e9 : abs >= 1e6 ? 1e6 : abs >= 1e3 ? 1e3 : 1;
  if (unit === 1) return String(Math.round(n));
  const suffix = unit === 1e9 ? "B" : unit === 1e6 ? "M" : "k";
  const s = (n / unit).toFixed(1);
  return (s.endsWith(".0") ? s.slice(0, -2) : s) + suffix;
}

function formatAgo(sec: number | null): string {
  if (sec === null) return "—";
  if (sec < 60) return `${sec}s ago`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m ago`;
  return `${Math.floor(sec / 3600)}h ago`;
}

/** output + input tokens — the headline "burn" figure (excludes cache). */
function windowBurn(w: UsageWindow): number {
  return w.outputTokens + w.inputTokens;
}

/** The compact footer readout text; "⛁ —" pre-first-scan (never "0"). */
function usageSegmentText(): string {
  const u = usage();
  if (!u) return "⛁ —";
  return `⛁ 5h ${formatCompact(windowBurn(u.fiveHour))} · 7d ${formatCompact(
    windowBurn(u.week),
  )} · ${formatCompact(u.fiveHour.tokensPerMin)}/min`;
}

/** One window's tooltip block: header (label + total incl. cache), the
 *  out/in/cache/message breakdown, then a row per model. */
const UsageWindowBlock: Component<{ label: string; w: UsageWindow }> = (props) => (
  <>
    <div class="usage-tt-row usage-tt-head">
      <span>{props.label}</span>
      <span class="mono">{formatCompact(props.w.totalTokens)} total</span>
    </div>
    <div class="usage-tt-row usage-tt-faint">
      <span>
        out {formatCompact(props.w.outputTokens)} · in{" "}
        {formatCompact(props.w.inputTokens)} · cache{" "}
        {formatCompact(props.w.cacheTokens)}
      </span>
      <span>
        {props.w.messages} msg{props.w.messages === 1 ? "" : "s"}
      </span>
    </div>
    <For each={props.w.byModel}>
      {(m) => (
        <div class="usage-tt-row usage-tt-model">
          <span class="usage-tt-model-name">{m.model}</span>
          <span class="mono">
            {formatCompact(m.totalTokens)} · {m.messages}m
          </span>
        </div>
      )}
    </For>
  </>
);

const UsageFooterSegment: Component = () => (
  <div class="footer-usage" tabIndex={0}>
    <span class="footer-item mono">{usageSegmentText()}</span>
    <div class="usage-tooltip" role="tooltip">
      <div class="usage-tt-row">
        <span>session</span>
        <span class="mono">{store.session || "—"}</span>
      </div>
      <div class="usage-tt-row">
        <span>socket</span>
        <span class="mono">{store.socket || "—"}</span>
      </div>
      <div class="usage-tt-divider" />
      <Show
        when={usage()}
        fallback={<div class="usage-tt-row usage-tt-faint">scanning…</div>}
      >
        {(u) => (
          <>
            <UsageWindowBlock label="5h window" w={u().fiveHour} />
            <div class="usage-tt-divider" />
            <UsageWindowBlock label="7d window" w={u().week} />
            <div class="usage-tt-divider" />
            <div class="usage-tt-row">
              <span>velocity</span>
              <span class="mono">{formatCompact(u().fiveHour.tokensPerMin)}/min</span>
            </div>
            <div class="usage-tt-row usage-tt-faint">
              <span>computed</span>
              <span>{formatAgo(usageAgeSec())}</span>
            </div>
          </>
        )}
      </Show>
    </div>
  </div>
);

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
              <UsageFooterSegment />
              <span class="footer-spacer" />
              <Show when={store.stored.length > 0}>
                <button
                  class="footer-sessions"
                  classList={{ attn: storedNeedsInputCount() > 0 }}
                  title={
                    storedNeedsInputCount() > 0
                      ? `${storedNeedsInputCount()} parked session${
                          storedNeedsInputCount() === 1 ? "" : "s"
                        } waiting on you — ⌘S`
                      : "Parked sessions (⌘S)"
                  }
                  onClick={toggleSessionsPanel}
                >
                  ⇥ {store.stored.length}
                  <Show when={storedNeedsInputCount() > 0}>
                    <span class="footer-sessions-attn">
                      {storedNeedsInputCount()}
                    </span>
                  </Show>
                </button>
              </Show>
              <span class="footer-item">
                {store.tabs.length} tab{store.tabs.length === 1 ? "" : "s"} ·{" "}
                {reachablePanes().length} pane
                {reachablePanes().length === 1 ? "" : "s"}
              </span>
              <span class="footer-keys">
                ⌘B files · ⌘S sessions · ⌘T tab · ⌘1-9 switch · ⌘D split · ⌘I
                inventory · ⌘⇧T teams · ⌘, settings
              </span>
            </footer>
          </div>

          <Show when={sessionsPanelOpen()}>
            <SessionsPanel />
          </Show>
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

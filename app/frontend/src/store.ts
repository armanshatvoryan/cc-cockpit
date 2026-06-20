// Cockpit store — the single source of client state.
//
// Holds the reconciled CockpitState (tabs/panes), the active tab, the focused
// pane, and live per-pane status (kept in the panes themselves, refreshed by
// `pane:status` events). Topology events trigger a full `list_state()`
// reconcile (backend confirmed this is the correct, simplest approach).
//
// The store is module-level (one cockpit window = one store). Components read
// the solid `store` proxy and call the action functions.

import { createStore, produce, reconcile as reconcileStore } from "solid-js/store";
import { createSignal, type Accessor } from "solid-js";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  cockpitInit,
  createTab,
  closeTab,
  splitPane,
  closePane,
  listState,
  setGrid,
  onPaneStatus,
  onPaneTopology,
  onCockpitReconnected,
  onCloseRequested,
  type CockpitState,
  type PaneInfo,
  type TabInfo,
} from "./ipc";

interface CockpitStore extends CockpitState {
  /** True once cockpit_init resolved (gates the empty-state vs grid render). */
  ready: boolean;
  /** Last fatal error string from a command, surfaced as a toast. */
  error: string | null;
}

const [store, setStore] = createStore<CockpitStore>({
  socket: "",
  session: "",
  tabs: [],
  panes: [],
  ready: false,
  error: null,
});

// Active tab + focused pane live as signals (pure client UI state).
const [activeTabId, setActiveTabId] = createSignal<string | null>(null);
const [focusedPaneId, setFocusedPaneId] = createSignal<string | null>(null);

// Local-only tab display-name overrides (v1 rename is client-side only).
const [tabNameOverrides, setTabNameOverrides] = createStore<Record<string, string>>({});

export { store, activeTabId, focusedPaneId };

// ── Selectors ─────────────────────────────────────────────────────────────────

/** The currently active tab, or undefined. */
export function activeTab(): TabInfo | undefined {
  const id = activeTabId();
  return store.tabs.find((t) => t.tabId === id);
}

/** Panes belonging to the active tab, in tmux order. */
export function activePanes(): PaneInfo[] {
  const tab = activeTab();
  if (!tab) return [];
  const byId = new Map(store.panes.map((p) => [p.paneId, p]));
  // Prefer the tab's declared pane order; fall back to filtering by tabId.
  const ordered = tab.paneIds
    .map((pid) => byId.get(pid))
    .filter((p): p is PaneInfo => !!p);
  if (ordered.length > 0) return ordered;
  return store.panes.filter((p) => p.tabId === tab.tabId);
}

/** Display name for a tab (local override wins over backend window name). */
export function tabDisplayName(tab: TabInfo): string {
  return tabNameOverrides[tab.tabId] ?? (tab.name || tab.tabId);
}

/** Aggregate attention for a tab: does any pane want input or is it dead? */
export function tabAttention(tab: TabInfo): "needs_input" | "working" | "none" {
  const panes = store.panes.filter(
    (p) => tab.paneIds.includes(p.paneId) || p.tabId === tab.tabId,
  );
  if (panes.some((p) => p.status === "NEEDS_INPUT")) return "needs_input";
  if (panes.some((p) => p.status === "WORKING")) return "working";
  return "none";
}

// ── Reconcile ──────────────────────────────────────────────────────────────────

/** Replace tabs/panes from a fresh CockpitState and repair active/focus refs. */
function reconcile(next: CockpitState): void {
  // Patch by VALUE keyed on id so existing tab/pane objects keep their identity.
  // Replacing the arrays (s.panes = next.panes) gives <For> brand-new references
  // every reconcile, which tears down + rebuilds every XtermHost (terminal dies
  // mid-paint → black panes). reconcile(...,{key}) mutates in place instead.
  setStore("socket", next.socket);
  setStore("session", next.session);
  setStore("tabs", reconcileStore(next.tabs, { key: "tabId" }));
  setStore("panes", reconcileStore(next.panes, { key: "paneId" }));

  // Repair active tab: keep if still present, else first tab, else null.
  const tabIds = next.tabs.map((t) => t.tabId);
  if (!activeTabId() || !tabIds.includes(activeTabId()!)) {
    setActiveTabId(tabIds[0] ?? null);
  }

  // Repair focused pane: keep if still present in the active tab, else first.
  const livePaneIds = new Set(next.panes.map((p) => p.paneId));
  if (!focusedPaneId() || !livePaneIds.has(focusedPaneId()!)) {
    const tab = next.tabs.find((t) => t.tabId === activeTabId());
    setFocusedPaneId(tab?.paneIds[0] ?? null);
  }
}

/** Force a full reload from the backend (used after topology events). */
export async function refreshState(): Promise<void> {
  try {
    reconcile(await listState());
  } catch (e) {
    setStore("error", String(e));
  }
}

// ── Boot ────────────────────────────────────────────────────────────────────

let unlisteners: UnlistenFn[] = [];

/** Call once on mount: init the backend, render state, subscribe to events. */
export async function bootCockpit(): Promise<void> {
  let state: CockpitState;
  try {
    state = await cockpitInit();
  } catch (e) {
    setStore("error", `cockpit_init failed: ${String(e)}`);
    setStore("ready", true); // still show the (empty) shell + the error
    return;
  }
  reconcile(state);
  setStore("ready", true);

  // pane:status — patch the matching pane's status badge in place.
  const unStatus = await onPaneStatus((p) => {
    setStore(
      "panes",
      (pane) => pane.paneId === p.paneId,
      produce((pane) => {
        pane.status = p.status;
        pane.ambiguous = p.ambiguous;
        if (p.status === "DEAD") pane.dead = true;
      }),
    );
  });

  // pane:topology — any structural change → full reconcile.
  // DEBOUNCED: a burst of layout-change events (e.g. an initial resize storm)
  // collapses into a single reload instead of hammering list_state().
  let topoTimer: number | undefined;
  const unTopo = await onPaneTopology(() => {
    if (topoTimer) clearTimeout(topoTimer);
    topoTimer = window.setTimeout(() => void refreshState(), 120);
  });

  // cockpit:reconnected — backend re-healed a vanished server. Reload state; the
  // reconnected session's panes have new ids, so <For> remounts them (warm-start).
  const unReconnect = await onCockpitReconnected(() => {
    setStore("error", null);
    void refreshState();
  });

  // cockpit:close-requested — ⌘W / window close button. Close the focused pane
  // (or the active tab if it's the last pane) instead of the whole window.
  const unCloseReq = await onCloseRequested(() => void closeFocusedPaneOrTab());

  unlisteners = [unStatus, unTopo, unReconnect, unCloseReq];
}

/** Tear down event subscriptions (window close / HMR). */
export function shutdownCockpit(): void {
  for (const u of unlisteners) u();
  unlisteners = [];
}

// ── Actions ────────────────────────────────────────────────────────────────────

// ── Grid sizing coordinator ─────────────────────────────────────────────────
//
// THE single authority for tmux window size. Each xterm reports its fitted cell
// size (cols×rows) here; we compute the window bounding box = grid columns/rows ×
// cell + inter-pane borders, and push ONE `set_grid` (refresh-client + tiled
// select-layout). This replaces the old per-pane resize where every xterm set the
// whole client size to its own width — the last writer shrank the window to one
// pane and the rest collapsed to 1 column ("no space for new pane" on split).

let cellCols = 80;
let cellRows = 24;
let gridTimer: number | undefined;
let lastGridKey = "";

/** Column count for n panes — MUST mirror PaneGrid.columnsFor. */
function gridColumns(n: number): number {
  if (n <= 1) return 1;
  if (n <= 4) return 2;
  return 3;
}

/** An xterm reports its fitted cell size; recompute + push the window grid. */
export function reportCell(cols: number, rows: number): void {
  if (cols > 0) cellCols = cols;
  if (rows > 0) cellRows = rows;
  if (gridTimer) clearTimeout(gridTimer);
  gridTimer = window.setTimeout(() => void pushGrid(), 90);
}

async function pushGrid(): Promise<void> {
  const n = store.panes.length;
  if (n === 0) return;
  const cols = gridColumns(n);
  const rows = Math.ceil(n / cols);
  // Bounding box: tiles plus one border column/row between adjacent panes.
  const winCols = cols * cellCols + (cols - 1);
  const winRows = rows * cellRows + (rows - 1);
  // Layout must MATCH the CSS grid. A single row of panes (n <= cols) is a
  // horizontal split — `tiled` would stack them vertically (mismatch → wrapping),
  // so use `even-horizontal`. Multi-row falls back to `tiled` (≈ the 2-col CSS).
  const layout = rows <= 1 ? "even-horizontal" : "tiled";
  const key = `${winCols}x${winRows}/${n}/${layout}`;
  if (key === lastGridKey) return; // change-guard: no redundant select-layout
  lastGridKey = key;
  try {
    await setGrid(winCols, winRows, layout);
  } catch {
    /* transient; next report retries */
  }
}

export function focusPane(paneId: string): void {
  setFocusedPaneId(paneId);
}

export function switchTab(tabId: string): void {
  setActiveTabId(tabId);
  // Move focus into the newly active tab's first pane.
  const tab = store.tabs.find((t) => t.tabId === tabId);
  setFocusedPaneId(tab?.paneIds[0] ?? null);
}

/** Switch to the Nth tab (0-based) if it exists — for Cmd+1..9. */
export function switchTabByIndex(idx: number): void {
  const tab = store.tabs[idx];
  if (tab) switchTab(tab.tabId);
}

export async function newTab(name?: string): Promise<void> {
  try {
    const res = await createTab(name);
    // Topology event will reconcile; but switch eagerly for snappy UX.
    await refreshState();
    setActiveTabId(res.tabId);
    setFocusedPaneId(res.paneId);
  } catch (e) {
    setStore("error", `create_tab failed: ${String(e)}`);
  }
}

/**
 * Close a tab. Returns the live-pane list when the backend asks for
 * confirmation (ok=false). Caller (TabBar) shows a modal then re-calls force.
 */
export async function requestCloseTab(
  tabId: string,
  force = false,
): Promise<{ needsConfirm: boolean; livePanes: string[] }> {
  try {
    const res = await closeTab(tabId, force);
    if (!res.ok && res.livePanes.length > 0) {
      return { needsConfirm: true, livePanes: res.livePanes };
    }
    await refreshState();
    return { needsConfirm: false, livePanes: [] };
  } catch (e) {
    setStore("error", `close_tab failed: ${String(e)}`);
    return { needsConfirm: false, livePanes: [] };
  }
}

export async function doSplit(paneId: string, dir: "h" | "v"): Promise<void> {
  try {
    const res = await splitPane(paneId, dir);
    await refreshState();
    setFocusedPaneId(res.paneId);
  } catch (e) {
    setStore("error", `split_pane failed: ${String(e)}`);
  }
}

export async function doClosePane(
  paneId: string,
  mode: "kill" | "detach" = "kill",
): Promise<void> {
  try {
    await closePane(paneId, mode);
    await refreshState();
  } catch (e) {
    setStore("error", `close_pane failed: ${String(e)}`);
  }
}

/**
 * ⌘W / window-close handler: close the focused pane, unless it's the only pane in
 * the active tab — then close the tab. Never closes the window/app. If it's the
 * last pane of the last tab, the tab closes and the app shows the empty state
 * (still running; ⌘Q to actually quit).
 */
export async function closeFocusedPaneOrTab(): Promise<void> {
  const tab = activeTab();
  if (!tab) return;
  const pid = focusedPaneId();
  const panes = activePanes();
  if (pid && panes.length > 1) {
    await doClosePane(pid);
  } else {
    await requestCloseTab(tab.tabId, true); // last pane → close the whole tab
  }
}

export function renameTabLocal(tabId: string, name: string): void {
  setTabNameOverrides(tabId, name);
}

export function clearError(): void {
  setStore("error", null);
}

/** Convenience: the focused pane's PaneInfo, for keyboard shortcuts. */
export const focusedPane: Accessor<PaneInfo | undefined> = () =>
  store.panes.find((p) => p.paneId === focusedPaneId());

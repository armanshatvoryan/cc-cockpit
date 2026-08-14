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
import { getCurrentWindow, UserAttentionType } from "@tauri-apps/api/window";
import { writeText as tauriWriteText } from "@tauri-apps/plugin-clipboard-manager";
import {
  cockpitInit,
  createTab,
  closeTab,
  splitPane,
  closePane,
  listState,
  setGrid,
  loadInventory,
  loadAuditMatrix,
  loadTeamRuns,
  cleanupTeamRuns,
  loadCockpitTemplates,
  spinupPreview,
  togglePlugin,
  pluginTogglePreview,
  launchCc,
  launchAgent,
  launchShell,
  paneSendKeys,
  paneRunLine,
  listDir,
  paneCwd,
  paneCommand,
  homeDir,
  discoverRepos,
  revealInFinder,
  createEntry,
  trashPath,
  watchDirs,
  breakPane,
  gitStatusSnapshot,
  saveLayout,
  loadLayout,
  onPaneStatus,
  onPaneTopology,
  onCmdError,
  onCockpitReconnected,
  onCloseRequested,
  onFileTreeChanged,
  usageSnapshot,
  onUsageUpdate,
  type CockpitState,
  type PaneInfo,
  type TabInfo,
  type InventoryItem,
  type InventoryType,
  type AuditMatrix,
  type TeamRun,
  type TeamMember,
  type Roster,
  type Workflow,
  type SpinupPreview,
  type GitStatus,
  type LayoutSnapshot,
  type TabLayout,
  type FileEntry,
  type RepoEntry,
  loadSettings,
  saveSettings,
  effectiveDefaultCwd,
  type CockpitSettings,
  type UsageSnapshot,
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

// dev#2 — per-tab git status of its first-pane cwd. `null` = cwd isn't a repo;
// absent key = not yet polled. Only the ACTIVE tab is refreshed (cheap).
const [gitStatus, setGitStatus] = createStore<Record<string, GitStatus | null>>({});

// C-2 — usage footer: last published token-burn snapshot. `null` until the
// backend's first background pass finishes (never render `0` for that state).
const [usage, setUsage] = createSignal<UsageSnapshot | null>(null);
// Ticks while the cockpit is open so a mounted tooltip's "computed Xs ago"
// stays live without re-fetching. Cheap: one timestamp write per second.
const [usageNow, setUsageNow] = createSignal(Date.now());

export { store, activeTabId, focusedPaneId, gitStatus, usage, usageNow };

/** Seconds since the current snapshot was computed, or `null` before the first
 *  scan pass lands. */
export function usageAgeSec(): number | null {
  const u = usage();
  if (!u) return null;
  return Math.max(0, Math.round((usageNow() - u.computedAtMs) / 1000));
}

// ── Selectors ─────────────────────────────────────────────────────────────────

/** The currently active tab, or undefined. */
export function activeTab(): TabInfo | undefined {
  const id = activeTabId();
  return store.tabs.find((t) => t.tabId === id);
}

/** Panes belonging to `tabId`, in tmux order. PaneGrid renders every tab (see
 * its header), so this is needed for tabs other than the active one. */
export function panesForTab(tabId: string): PaneInfo[] {
  const tab = store.tabs.find((t) => t.tabId === tabId);
  if (!tab) return [];
  const byId = new Map(store.panes.map((p) => [p.paneId, p]));
  // Prefer the tab's declared pane order; fall back to filtering by tabId.
  const ordered = tab.paneIds
    .map((pid) => byId.get(pid))
    .filter((p): p is PaneInfo => !!p);
  if (ordered.length > 0) return ordered;
  return store.panes.filter((p) => p.tabId === tab.tabId);
}

/** Panes belonging to the active tab, in tmux order. */
export function activePanes(): PaneInfo[] {
  const id = activeTabId();
  return id ? panesForTab(id) : [];
}

/** Display name for a tab (local override wins over backend window name). */
export function tabDisplayName(tab: TabInfo): string {
  return tabNameOverrides[tab.tabId] ?? (tab.name || tab.tabId);
}

/** First-pane cwd for a tab, "" when no pane / cwd yet. */
export function tabCwd(tab: TabInfo): string {
  return store.panes.find((p) => p.paneId === tab.paneIds[0])?.cwd ?? "";
}

/** Folder basename of the tab's working directory ("" when unknown). */
export function tabFolderName(tab: TabInfo): string {
  const cwd = tabCwd(tab).replace(/\/+$/, "");
  if (!cwd) return "";
  const base = cwd.slice(cwd.lastIndexOf("/") + 1);
  return base || cwd;
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

/** How many panes in this tab are waiting on the user right now. */
export function tabNeedsInputCount(tab: TabInfo): number {
  return store.panes.filter(
    (p) =>
      (tab.paneIds.includes(p.paneId) || p.tabId === tab.tabId) &&
      p.status === "NEEDS_INPUT",
  ).length;
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
    // The fresh geometry may change the pane-column/row chrome budget (e.g.
    // a split turned 1 pane-row into 2) — re-key the window push. The
    // change-guard makes this a no-op when nothing relevant moved, so no
    // push→layout-change→push loop: it converges in one extra round at most.
    scheduleGridPush();
  } catch (e) {
    setStore("error", String(e));
  }
}

// ── C2: OS attention (dock bounce + badge) ──────────────────────────────────
// When a pane needs input while the cockpit is in the background, bounce the
// dock and badge the window with the count. Cleared the moment the window
// regains focus. Best-effort: every call swallows its own rejection so a missing
// permission / platform quirk never escalates into a store error.

/** Number of panes currently waiting on the user. */
function needsInputCount(): number {
  return store.panes.filter((p) => p.status === "NEEDS_INPUT").length;
}

/** Bounce + badge when backgrounded and something needs input. */
function signalAttention(): void {
  const count = needsInputCount();
  if (count <= 0 || document.hasFocus()) return;
  const win = getCurrentWindow();
  void win.requestUserAttention(UserAttentionType.Critical).catch(() => {});
  void win.setBadgeCount(count).catch(() => {});
}

/** Drop the bounce + clear the badge (on window focus). `undefined` ⇒ None ⇒
 *  clears the badge — the type excludes `null` and `0` would render a "0" badge. */
function clearAttention(): void {
  const win = getCurrentWindow();
  void win.requestUserAttention(null).catch(() => {});
  void win.setBadgeCount(undefined).catch(() => {});
}

// ── dev#1: disk-persisted layout ────────────────────────────────────────────
// Mirror the open tabs (position + first-pane cwd + local rename) + the active
// tab to disk, debounced. On boot we replay saved renames onto the live tabs
// matched by (index, cwd). Best-effort throughout — a write/read failure logs
// and is dropped; it must never block a UI action or boot.

/** Build the snapshot from current store state. */
function buildLayoutSnapshot(): LayoutSnapshot {
  const tabs: TabLayout[] = store.tabs.map((t) => {
    const cwd = store.panes.find((p) => p.paneId === t.paneIds[0])?.cwd ?? "";
    return { index: t.index, cwd, customTitle: tabNameOverrides[t.tabId] ?? null };
  });
  return { schemaVersion: 1, activeTabId: activeTabId(), tabs };
}

let persistTimer: number | undefined;
/** Debounced persist (~300ms) so a burst of tab ops coalesces into one write. */
function persistLayout(): void {
  if (persistTimer) clearTimeout(persistTimer);
  persistTimer = window.setTimeout(() => {
    void saveLayout(buildLayoutSnapshot()).catch((e) =>
      console.warn("save_layout failed", e),
    );
  }, 300);
}

/** Replay persisted renames + active tab onto the freshly reconciled state. */
async function restoreLayout(): Promise<void> {
  try {
    const saved = await loadLayout();
    if (!saved) return;
    for (const tl of saved.tabs) {
      if (!tl.customTitle) continue;
      const liveTab = store.tabs.find(
        (t) =>
          t.index === tl.index &&
          store.panes.find((p) => p.paneId === t.paneIds[0])?.cwd === tl.cwd,
      );
      if (liveTab) renameTabLocal(liveTab.tabId, tl.customTitle);
    }
    if (saved.activeTabId && store.tabs.some((t) => t.tabId === saved.activeTabId)) {
      setActiveTabId(saved.activeTabId);
    }
  } catch (e) {
    console.warn("load_layout/restore failed", e);
  }
}

// ── dev#2: per-worktree git status ──────────────────────────────────────────
// Poll only the ACTIVE tab's first-pane cwd (a switch + an 8s interval). Cheap:
// one `git status` per cycle, never an all-tabs hammer.

/** Refresh the active tab's git badge from its first-pane cwd. */
async function refreshActiveGitStatus(): Promise<void> {
  const tab = activeTab();
  if (!tab) return;
  const cwd = store.panes.find((p) => p.paneId === tab.paneIds[0])?.cwd;
  if (!cwd) return;
  try {
    setGitStatus(tab.tabId, await gitStatusSnapshot(cwd));
  } catch {
    /* git missing / transient — keep the last known badge */
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

  // dev#1 — best-effort replay of persisted renames + active tab. Never throws.
  await restoreLayout();
  // dev#2 — paint the active tab's git badge immediately, then poll it.
  void refreshActiveGitStatus();

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
    // C2 — after the patch (setStore is sync), bounce + badge if we're in the
    // background and any pane now needs input.
    signalAttention();
  });

  // pane:topology — any structural change → full reconcile.
  // DEBOUNCED: a burst of layout-change events (e.g. an initial resize storm)
  // collapses into a single reload instead of hammering list_state().
  let topoTimer: number | undefined;
  // A2 — a new pane may be a freshly-launched teammate; debounce-reload the
  // team board so `paneLabel` can resolve its name/task without the user
  // opening the board. Separate timer/delay from the state reconcile above —
  // this is a "nice to have eventually", not on the topology critical path.
  let teamTopoTimer: number | undefined;
  const unTopo = await onPaneTopology(() => {
    if (topoTimer) clearTimeout(topoTimer);
    topoTimer = window.setTimeout(() => void refreshState(), 120);
    if (teamTopoTimer) clearTimeout(teamTopoTimer);
    teamTopoTimer = window.setTimeout(() => void loadTeamRunsNow(), 400);
  });

  // cockpit:reconnected — backend re-healed a vanished server. Reload state, then
  // re-push the grid: the replacement server was born at default size, but the
  // recorded grid key describes a push only the DEAD server applied, so an
  // ordinary push would hit the change-guard. gridServerReset drops the guard
  // and pushes unconditionally — else panes sit at tmux birth size (200×50).
  const unReconnect = await onCockpitReconnected(() => {
    setStore("error", null);
    void refreshState().then(gridServerReset);
  });

  // cockpit:cmd-error — tmux REJECTED a control-mode command. Log it (it was
  // silently swallowed before) and re-arm the grid push: the rejected command is
  // most likely the `set_grid` whose key we already recorded as pushed.
  const unCmdError = await onCmdError((p) => {
    const msg = p.lines.join(" ").trim();
    console.error("tmux rejected a command:", msg || "(no message)");
    gridCommandRejected();
  });

  // cockpit:close-requested — ⌘W / window close button. Close the focused pane
  // (or the active tab if it's the last pane) instead of the whole window.
  const unCloseReq = await onCloseRequested(() => void closeFocusedPaneOrTab());

  // C2 — clear the dock bounce + badge the moment the cockpit regains focus.
  const onWinFocus = () => clearAttention();
  window.addEventListener("focus", onWinFocus);
  const unFocus: UnlistenFn = () => window.removeEventListener("focus", onWinFocus);

  // dev#2 — refresh the active tab's git badge every 8s (active tab only).
  const gitInterval = window.setInterval(() => void refreshActiveGitStatus(), 8000);
  const unGit: UnlistenFn = () => clearInterval(gitInterval);

  // v1.1 — root the file-tree on the active pane now, then follow it (a shell cd
  // emits no topology event, so poll the focused pane's cwd every 1.5s).
  void syncFileTreeRoot();
  const ftInterval = window.setInterval(() => void syncFileTreeRoot(), 1500);
  const unFt: UnlistenFn = () => clearInterval(ftInterval);

  // v1.1 — live fs-watch: reload a visible dir when the backend reports it changed.
  const unFtChange = await onFileTreeChanged((p) => ftOnChanged(p.dir));

  // C-2 — usage footer: pull whatever the poller last computed (may be `null`
  // pre-first-pass — the footer renders `—` for that), then stay live via
  // usage:update (fired immediately by the backend, then every 45s).
  void usageSnapshot()
    .then(setUsage)
    .catch((e) => console.warn("usage_snapshot failed", e));
  const unUsage = await onUsageUpdate((snap) => setUsage(snap));
  // Tick the "computed Xs ago" clock every second while the cockpit is open.
  const usageTickInterval = window.setInterval(() => setUsageNow(Date.now()), 1000);
  const unUsageTick: UnlistenFn = () => clearInterval(usageTickInterval);

  unlisteners = [
    unStatus,
    unTopo,
    unReconnect,
    unCmdError,
    unCloseReq,
    unFocus,
    unGit,
    unFt,
    unFtChange,
    unUsage,
    unUsageTick,
  ];
}

/** Tear down event subscriptions (window close / HMR). */
export function shutdownCockpit(): void {
  for (const u of unlisteners) u();
  unlisteners = [];
}

// ── Actions ────────────────────────────────────────────────────────────────────

// ── Grid sizing coordinator (tmux-authority mirror, bug #10) ────────────────
//
// tmux owns pane ARRANGEMENT; the frontend owns only the WINDOW size. One
// container-level push tells tmux how many total cols/rows fit the pane
// stack; tmux tiles the panes and reports the result in `window_layout`,
// which the backend parses (`TabInfo.geometry`) and the frontend mirrors 1:1
// — PaneGrid positions panes from the rects, XtermHost `term.resize`s each
// terminal to its tmux pane's exact cols/rows.
//
// This replaces the per-pane FitAddon math, which diverged from tmux `tiled`
// on any pane count that doesn't fill the rectangle: 3 panes @189×109 → tmux
// gives the bottom pane 189 cols vs the 94-col CSS cell — that pane was
// permanently garbled (root cause #10, 2026-08-10).
//
// Sizing still targets the ACTIVE TAB'S OWN tmux window (`resize-window -t` +
// `select-layout -t`) — windows are `window-size manual` and sized
// individually, never via the session-global `refresh-client -C`.

/** .pane-toolbar height (styles.css) — vertical chrome each pane-row costs. */
export const PANE_TOOLBAR_PX = 28;
/** Horizontal per-pane chrome: 1px border ×2 + 4px .xterm-host left padding. */
export const PANE_CHROME_W = 6;
/** Vertical per-pane chrome: toolbar + 1px border ×2 + 4px top padding. */
export const PANE_CHROME_H = PANE_TOOLBAR_PX + 6;
/** .pane-grid padding (each side). */
export const GRID_PAD = 3;

// One xterm char cell in CSS px — measured by the first mounted XtermHost
// (identical for all panes: same font, same size). A signal so PaneGrid's
// mirror positioning recomputes when zoom re-metrics change it.
const [cellPx, setCellPx] = createSignal<{ w: number; h: number } | null>(null);
export { cellPx };

// The shared pane-stack box in CSS px (all tabs stack on the same element).
let containerPx: { w: number; h: number } | null = null;

let gridTimer: number | undefined;
// Per tmux WINDOW, the last grid key successfully pushed for it. Keyed by
// window, not global: two tabs with identical geometry still each need their
// own push (root cause #6/#8, 2026-07-20).
const lastGridKeyByWindow = new Map<string, string>();
// Per tmux WINDOW, the pane count seen at the last push. `select-layout tiled`
// fires only when the count GREW (a new pane appeared) — a resize, tab switch,
// boot, or pane close is pushed resize-only, so tmux scales the existing
// arrangement proportionally instead of flattening it.
const lastPaneCountByWindow = new Map<string, number>();
// Windows the user arranged by hand (⌘D/⌘⇧D split). Never auto-tiled again —
// even when external splits later add panes (e.g. an agent team spawning) the
// user's arrangement wins. Session-scoped: tmux itself preserves the layout
// across GUI restarts because boot pushes are resize-only (count unchanged).
const manualLayoutWindows = new Set<string>();

/** An XtermHost measured its char cell in CSS px. */
export function reportCellPx(w: number, h: number): void {
  if (!(w > 0) || !(h > 0)) return;
  const cur = cellPx();
  if (cur && Math.abs(cur.w - w) < 0.01 && Math.abs(cur.h - h) < 0.01) return;
  setCellPx({ w, h });
  scheduleGridPush();
}

/** PaneGrid's stack element resized (window resize, sidebar toggle, zoom).
 *
 * SETTLE GATE: during boot/reconnect the webview settles in steps (window
 * restore, sidebar mount, async setZoom re-metrics). Pushing each step gave
 * the pane's TUI an overlapping SIGWINCH storm whose partial differential
 * redraws pollute tmux's own grid. Until the first push lands for the active
 * window require a longer stretch of stability (500ms) so boot collapses into
 * ONE clean transition; settled UI coalesces at 350ms. */
export function reportContainer(w: number, h: number): void {
  if (!(w > 0) || !(h > 0)) return;
  containerPx = { w, h };
  if (gridTimer) clearTimeout(gridTimer);
  const firstPushForActiveWindow = !lastGridKeyByWindow.has(
    activeTab()?.tmuxWindowId ?? "",
  );
  gridTimer = window.setTimeout(
    () => void pushGrid(),
    firstPushForActiveWindow ? 500 : 350,
  );
}

/** The tmux server was replaced (mid-op re-heal or healing create_tab). The
 * recorded grid key describes a push only the DEAD server ever applied, so the
 * change-guard would swallow the next push and leave the new server's panes at
 * birth size. Drop the guard and push the current grid unconditionally. */
export function gridServerReset(): void {
  lastGridKeyByWindow.clear();
  // Window ids belong to the dead server — the counts and manual-arrangement
  // flags keyed by them are meaningless for its replacement.
  lastPaneCountByWindow.clear();
  manualLayoutWindows.clear();
  if (gridTimer) clearTimeout(gridTimer);
  gridTimer = window.setTimeout(() => void pushGrid(), 500);
}

// A rejected command (`cockpit:cmd-error`) re-arms the push, but the push may be
// what tmux is rejecting — clear-and-push on every error would then be a
// self-feeding loop. So this is a RATE LIMIT: at most GRID_ERROR_MAX_PUSHES
// re-pushes per GRID_ERROR_WINDOW_MS of wall clock, budget refilled when the
// window rolls over.
//
// It deliberately does NOT reset on a gap between errors: `cmd-error` fires for
// ANY rejected command, so a recurring unrelated rejection (say every 2s) never
// leaves a 5s gap — a silence-based reset would burn the budget in one burst and
// then disarm grid repair permanently, silently reinstating the wrong-size
// window this whole listener exists to fix.
const GRID_ERROR_MAX_PUSHES = 3;
const GRID_ERROR_WINDOW_MS = 5000;
let gridErrorPushes = 0;
let gridErrorWindowStartedAt = 0;

/** tmux REJECTED a control-mode command (`%error` on the control stream).
 *
 * `setGrid` resolves as soon as the command bytes reach the server's stdin, so a
 * rejected sizing command still recorded its grid key — and the per-window
 * change-guard in `pushGrid` then swallowed every retry at that size, leaving the
 * window silently at the WRONG geometry until the user resized by hand. Drop the
 * key guard and re-push.
 *
 * Unlike `gridServerReset` this does NOT clear the pane counts or the
 * manual-arrangement set: the window ids are still valid, and forgetting them
 * would re-tile a window the user split by hand. We also can't tell WHICH
 * command tmux rejected (control mode gives no command echo), so this is
 * deliberately a cheap idempotent re-push, not a targeted repair. */
export function gridCommandRejected(): void {
  const now = Date.now();
  // Roll the window forward from ITS start, not from the last error, so a
  // steady error stream still gets its budget back every GRID_ERROR_WINDOW_MS.
  if (now - gridErrorWindowStartedAt > GRID_ERROR_WINDOW_MS) {
    gridErrorWindowStartedAt = now;
    gridErrorPushes = 0;
  }
  if (gridErrorPushes >= GRID_ERROR_MAX_PUSHES) return; // storm — stop feeding it
  gridErrorPushes++;
  lastGridKeyByWindow.clear();
  if (gridTimer) clearTimeout(gridTimer);
  gridTimer = window.setTimeout(() => void pushGrid(), 350);
}

/** Pane-columns/rows the tab's CURRENT layout occupies (distinct x/y starts),
 * for the chrome budget. Before geometry exists, estimate from the old grid
 * formula so the first push is roughly right; the next layout-change refines
 * it (pushGrid re-keys and converges in one extra round-trip at most). */
function paneGridShape(tab: TabInfo | undefined): { cols: number; rows: number } {
  const g = tab?.geometry;
  if (g && g.rects.length > 0) {
    return {
      cols: new Set(g.rects.map((r) => r.x)).size,
      rows: new Set(g.rects.map((r) => r.y)).size,
    };
  }
  const n = tab ? panesForTab(tab.tabId).length : 1;
  const cols = n <= 1 ? 1 : n <= 4 ? 2 : 3;
  return { cols, rows: Math.ceil(Math.max(n, 1) / cols) };
}

async function pushGrid(): Promise<void> {
  // Active tab ONLY: PaneGrid shows one tab; background windows keep their
  // own size and get sized on switch.
  const tab = activeTab();
  if (!tab) return;
  const windowId = tab.tmuxWindowId;
  const n = panesForTab(tab.tabId).length;
  if (n === 0) return;
  const cell = cellPx();
  const box = containerPx;
  // Not measurable yet — the reports that set these re-drive the push.
  if (!cell || !box) return;
  // Window size = what fits the box after per-pane-column/row chrome. tmux
  // cells include the 1-cell inter-pane borders, so no extra gap math.
  const shape = paneGridShape(tab);
  const winCols = Math.floor(
    (box.w - 2 * GRID_PAD - shape.cols * PANE_CHROME_W) / cell.w,
  );
  const winRows = Math.floor(
    (box.h - 2 * GRID_PAD - shape.rows * PANE_CHROME_H) / cell.h,
  );
  if (winCols < 20 || winRows < 5) return; // degenerate box; wait for layout
  const key = `${winCols}x${winRows}/${n}`;
  // Change-guard is per-window: an identical key on a DIFFERENT tab is still a
  // push that must happen, because that tab's tmux window has its own size.
  if (key === lastGridKeyByWindow.get(windowId)) return;
  // tmux owns the arrangement, the frontend mirrors it (bug #10) — but re-tile
  // ONLY when the pane count changed in a window the user hasn't arranged by
  // hand (external split added a pane / a pane closed → re-balance the grid).
  // Everything else (resize, boot, tab switch) — and EVERYTHING on a manually
  // arranged window — is pushed resize-only ("none"): tmux scales the existing
  // layout proportionally, so a ⌘D/⌘⇧D split is never flattened into a grid.
  const prevCount = lastPaneCountByWindow.get(windowId);
  const countChanged = prevCount !== undefined && n !== prevCount;
  const layout =
    countChanged && !manualLayoutWindows.has(windowId) ? "tiled" : "none";
  try {
    await setGrid(windowId, winCols, winRows, layout);
    // Record the key ONLY after the push lands. The control client may still be
    // attaching at first mount, so an early set_grid rejects with "not attached".
    // If we recorded the key before awaiting (the old bug), the change-guard
    // above would then block every retry at this same size — the window stayed
    // stuck at its tmux birth size (200×50) until the user happened to resize.
    lastGridKeyByWindow.set(windowId, key);
    lastPaneCountByWindow.set(windowId, n);
  } catch {
    // Not attached yet (or transient). No fresh report may follow once the UI
    // settles, so self-heal: retry until the client accepts the size.
    if (gridTimer) clearTimeout(gridTimer);
    gridTimer = window.setTimeout(() => void pushGrid(), 400);
  }
}

// NO post-resize "resync" step exists any more, deliberately.
//
// Iteration #5 broadcast a synthetic Ctrl+L to every mounted pane after each
// successful push. It fired on every grid change (not just resizes), fanned out
// to panes that were fine, and — since injected keys are indistinguishable from
// typed ones — cleared Claude Code's rendered transcript. Closing several panes
// quickly produced several pushes and so several ^L per pane (75 in one session,
// including back-to-back duplicates; root cause #7).
//
// The two things it was papering over are gone at the source: tab switches no
// longer tear down and replay terminals (PaneGrid keeps tabs mounted), and each
// tab's window is sized on its own (set_grid targets one window). What remains
// is an ordinary terminal resize, which the app repaints itself via SIGWINCH —
// exactly as it does under any other emulator.
export function focusPane(paneId: string): void {
  setFocusedPaneId(paneId);
  void syncFileTreeRoot(); // v1.1 — tree follows the newly focused pane's cwd
}

/** Schedule a grid push for whatever tab is active now.
 *
 * Needed on tab switch because tabs stay mounted: an xterm that is already
 * fitted reports nothing when it becomes visible again, so without this the
 * arriving tab's tmux window would keep whatever size it last had — including
 * its 200x50 birth size if it has never been active. The per-window
 * change-guard makes a redundant call a no-op. */
function scheduleGridPush(): void {
  if (gridTimer) clearTimeout(gridTimer);
  gridTimer = window.setTimeout(() => void pushGrid(), 60);
}

export function switchTab(tabId: string): void {
  setActiveTabId(tabId);
  scheduleGridPush(); // size the arriving tab's own window
  // Move focus into the newly active tab's first pane.
  const tab = store.tabs.find((t) => t.tabId === tabId);
  setFocusedPaneId(tab?.paneIds[0] ?? null);
  void refreshActiveGitStatus(); // dev#2 — repaint badge for the new active tab
  void syncFileTreeRoot(); // v1.1 — tree follows the new active tab's cwd
  persistLayout(); // dev#1 — active tab changed
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
    scheduleGridPush(); // size the newborn window — refreshState() above
    // scheduled a push while the OLD tab was still active, so without this the
    // new window keeps its ~200x50 tmux birth size and garbles (bug #9).
    setFocusedPaneId(res.paneId);
    void refreshActiveGitStatus(); // dev#2 — badge the freshly created tab
    persistLayout(); // dev#1
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
  // Resolve to the STABLE tmux window id. tab_id encodes the mutable window
  // index, which tmux reuses for new windows — closing by index can hit the
  // wrong window or a gone one. If the tab already reconciled away (its window
  // closed under us), there's nothing to kill; just resync.
  const tab = store.tabs.find((t) => t.tabId === tabId);
  if (!tab) {
    await refreshState();
    return { needsConfirm: false, livePanes: [] };
  }
  try {
    const res = await closeTab(tab.tmuxWindowId, force);
    if (!res.ok && res.livePanes.length > 0) {
      return { needsConfirm: true, livePanes: res.livePanes };
    }
    await refreshState();
    persistLayout(); // dev#1 — tab set changed (stale gitStatus keys are never
    // read: chips only render for live tabs, so no cleanup needed)
    return { needsConfirm: false, livePanes: [] };
  } catch (e) {
    setStore("error", `close_tab failed: ${String(e)}`);
    return { needsConfirm: false, livePanes: [] };
  }
}

/** Split direction for "auto": along the focused pane's LONGER rendered axis.
 * Wider than tall → side-by-side ("h"), taller → stacked ("v"). A full-width
 * window splits into columns first; each (now taller-than-wide) half then
 * stacks — repeated ⌘D distributes evenly instead of piling up skinny columns.
 * Compared in px (tmux cells × measured cell px), since a char cell is ~2×
 * taller than wide; falls back to "h" while geometry/cell px are unknown. */
function autoSplitDir(paneId: string): "h" | "v" {
  const tab = store.tabs.find((t) => t.paneIds.includes(paneId));
  const rect = tab?.geometry?.rects.find((r) => r.paneId === paneId);
  const cell = cellPx();
  if (!rect || !cell) return "h";
  return rect.w * cell.w >= rect.h * cell.h ? "h" : "v";
}

export async function doSplit(
  paneId: string,
  dir: "h" | "v" | "auto",
): Promise<void> {
  try {
    // Mark the window user-arranged BEFORE the split's refresh schedules a
    // grid push, so the push sees the flag and never re-tiles this window.
    const winId = store.tabs.find((t) => t.paneIds.includes(paneId))
      ?.tmuxWindowId;
    if (winId) manualLayoutWindows.add(winId);
    const res = await splitPane(paneId, dir === "auto" ? autoSplitDir(paneId) : dir);
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
  persistLayout(); // dev#1 — local rename is part of the persisted layout
}

export function clearError(): void {
  setStore("error", null);
}

/** Convenience: the focused pane's PaneInfo, for keyboard shortcuts. */
export const focusedPane: Accessor<PaneInfo | undefined> = () =>
  store.panes.find((p) => p.paneId === focusedPaneId());

// ── Inventory mission-control (P2-F1) ────────────────────────────────────────
//
// The unified read-only browser. Opened with ⌘I as a right-side panel over the
// pane grid. Loads on open (and on demand) from `load_inventory`, scoped to the
// active tab's project root. Filters (type / scope / text) are pure client-side.

interface InventoryStore {
  items: InventoryItem[];
  loading: boolean;
  error: string | null;
}
const [inventory, setInventory] = createStore<InventoryStore>({
  items: [],
  loading: false,
  error: null,
});
const [inventoryOpen, setInventoryOpen] = createSignal(false);
// Empty type set = "all types". Scope is a single segmented choice.
const [invTypes, setInvTypes] = createSignal<Set<InventoryType>>(new Set());
const [invScope, setInvScope] = createSignal<"all" | "global" | "project">("all");
const [invQuery, setInvQuery] = createSignal("");
// Which inventory view: the browse list (F1) or the cross-project matrix (F5).
const [invView, setInvView] = createSignal<"browse" | "audit">("browse");

// Audit matrix (P2-F5) state.
interface AuditStore {
  data: AuditMatrix | null;
  loading: boolean;
  error: string | null;
}
const [audit, setAudit] = createStore<AuditStore>({ data: null, loading: false, error: null });

export { inventory, inventoryOpen, invTypes, invScope, invQuery, invView, audit };

/** Project root for project-scope inventory = the active tab's first pane cwd. */
function activeProjectPath(): string | undefined {
  const cwd = activePanes()[0]?.cwd;
  return cwd && cwd.trim() ? cwd : undefined;
}

/** Each open tab's first-pane cwd (the backend resolves + dedupes to roots). */
function openTabProjectPaths(): string[] {
  return store.tabs
    .map((t) => store.panes.find((p) => p.paneId === t.paneIds[0])?.cwd)
    .filter((c): c is string => !!c && c.trim().length > 0);
}

/** Reload the inventory from the backend for the current project context. */
export async function loadInventoryNow(): Promise<void> {
  setInventory("loading", true);
  setInventory("error", null);
  try {
    const items = await loadInventory(activeProjectPath());
    setInventory("items", items);
  } catch (e) {
    setInventory("error", String(e));
    setInventory("items", []);
  } finally {
    setInventory("loading", false);
  }
}

/** Reload the cross-project audit matrix for the current open tabs. */
export async function loadAuditNow(): Promise<void> {
  setAudit("loading", true);
  setAudit("error", null);
  try {
    const data = await loadAuditMatrix(openTabProjectPaths());
    setAudit("data", data);
  } catch (e) {
    setAudit("error", String(e));
    setAudit("data", null);
  } finally {
    setAudit("loading", false);
  }
}

/** Reload whichever inventory view is active. */
export function reloadInventoryView(): void {
  if (invView() === "audit") void loadAuditNow();
  else void loadInventoryNow();
}

/** Switch between the browse list and the audit matrix (loads on switch). */
export function setInventoryView(mode: "browse" | "audit"): void {
  if (invView() === mode) return;
  setInvView(mode);
  reloadInventoryView();
}

export function openInventory(): void {
  setInventoryOpen(true);
  reloadInventoryView();
}
export function closeInventory(): void {
  setInventoryOpen(false);
}
export function toggleInventory(): void {
  if (inventoryOpen()) closeInventory();
  else openInventory();
}

// ── Settings (⌘,) ────────────────────────────────────────────────────────────
// One preference today: the start directory for new tabs. The dialog is loaded
// lazily on open — settings.json is read from disk, never cached across opens,
// so an external edit can't leave a stale value in the UI.

interface SettingsStore {
  /** Configured start dir, or "" when unset (⇒ built-in default). */
  defaultCwd: string;
  /** What `default_cwd()` actually resolves to right now. Differs from
   *  `defaultCwd` when the configured folder was deleted or was never set. */
  effectiveCwd: string;
  loading: boolean;
  saving: boolean;
  error: string | null;
}
const [settings, setSettings] = createStore<SettingsStore>({
  defaultCwd: "",
  effectiveCwd: "",
  loading: false,
  saving: false,
  error: null,
});
const [settingsOpen, setSettingsOpen] = createSignal(false);
export { settings, settingsOpen };

/** Surface a settings-dialog error raised outside the store's own actions. */
export function setSettingsError(msg: string | null): void {
  setSettings("error", msg);
}

/** Read settings + the current effective dir off disk into the dialog. */
async function reloadSettings(): Promise<void> {
  setSettings("loading", true);
  setSettings("error", null);
  try {
    const [s, effective] = await Promise.all([loadSettings(), effectiveDefaultCwd()]);
    setSettings("defaultCwd", s.defaultCwd ?? "");
    setSettings("effectiveCwd", effective);
  } catch (e) {
    setSettings("error", String(e));
  } finally {
    setSettings("loading", false);
  }
}

/** Persist a new start directory. Empty string clears it (back to default). */
export async function setDefaultCwd(dir: string): Promise<void> {
  setSettings("saving", true);
  setSettings("error", null);
  try {
    const next: CockpitSettings = { schemaVersion: 1 };
    if (dir.trim()) next.defaultCwd = dir.trim();
    const effective = await saveSettings(next);
    setSettings("defaultCwd", next.defaultCwd ?? "");
    setSettings("effectiveCwd", effective);
  } catch (e) {
    setSettings("error", String(e));
  } finally {
    setSettings("saving", false);
  }
}

export function openSettings(): void {
  setSettingsOpen(true);
  void reloadSettings();
}
export function closeSettings(): void {
  setSettingsOpen(false);
}
export function toggleSettings(): void {
  if (settingsOpen()) closeSettings();
  else openSettings();
}

// ── Live team board (P3 step 3) ──────────────────────────────────────────────
// Read-only view of native Agent Teams sessions on disk. Newest-first list of
// runs; each run a roster of members. A live member with a real `%N` pane links
// to that pane (the socket spike confirmed teammates land on `-L cockpit`).

interface TeamBoardStore {
  runs: TeamRun[];
  loading: boolean;
  error: string | null;
}
const [teamBoard, setTeamBoard] = createStore<TeamBoardStore>({
  runs: [],
  loading: false,
  error: null,
});
const [teamBoardOpen, setTeamBoardOpen] = createSignal(false);
export { teamBoard, teamBoardOpen };

/** Reload the live team runs from native session files. */
export async function loadTeamRunsNow(): Promise<void> {
  setTeamBoard("loading", true);
  setTeamBoard("error", null);
  try {
    const runs = await loadTeamRuns();
    setTeamBoard("runs", runs);
  } catch (e) {
    setTeamBoard("error", String(e));
    setTeamBoard("runs", []);
  } finally {
    setTeamBoard("loading", false);
  }
}

export function openTeamBoard(): void {
  setTeamBoardOpen(true);
  void loadTeamRunsNow();
}
export function closeTeamBoard(): void {
  setTeamBoardOpen(false);
}
export function toggleTeamBoard(): void {
  if (teamBoardOpen()) closeTeamBoard();
  else openTeamBoard();
}

/** True when this member's `%N` pane is one the cockpit currently tracks — only
 *  then can a click focus it (the team was launched in this cockpit). */
export function memberPaneIsLive(paneId?: string): boolean {
  if (!paneId || !paneId.startsWith("%")) return false;
  return store.panes.some((p) => p.paneId === paneId);
}

/** Link a board row to its pane: switch to the pane's tab and focus it. No-op
 *  (returns false) if the pane isn't tracked here (e.g. a team run elsewhere). */
export function focusTeamMemberPane(paneId?: string): boolean {
  if (!paneId) return false;
  const pane = store.panes.find((p) => p.paneId === paneId);
  if (!pane) return false;
  setActiveTabId(pane.tabId);
  focusPane(paneId);
  closeTeamBoard();
  return true;
}

// ── Team board filter + cleanup (P3 step 3.1) ────────────────────────────────
// The board accretes one dir per Claude session on disk; most are lead-only
// stubs that never spawned a team. Default view hides that graveyard — show only
// a REAL team (>=2 members) created within STALE_DAYS — and a toggle reveals all.
// Cleanup deletes the dead runs, guarding anything live or freshly written.

const STALE_DAYS = 7;
const FRESH_MIN = 10; // a run written this recently is an active session — never delete

const [teamBoardShowAll, setTeamBoardShowAll] = createSignal(false);
export { teamBoardShowAll };
export function toggleTeamBoardShowAll(): void {
  setTeamBoardShowAll((v) => !v);
}

/** A run that actually spawned teammates (more than just the lead). */
function isRealTeam(run: TeamRun): boolean {
  return run.members.length >= 2;
}
/** Created within the staleness window (missing createdAt → treated as old). */
function isRecentRun(run: TeamRun): boolean {
  if (run.createdAt == null) return false;
  return Date.now() - run.createdAt < STALE_DAYS * 24 * 60 * 60 * 1000;
}
/** Default-view predicate: a real, recent team. A parse-error row always shows so
 *  a broken dir stays visible (it never joins the hidden set, so bulk cleanup
 *  leaves it alone — deal with a garbled dir by hand). */
function runPassesDefaultFilter(run: TeamRun): boolean {
  if (run.parseError) return true;
  return isRealTeam(run) && isRecentRun(run);
}

/** Runs shown in the board, honoring the show-all toggle. */
export function visibleTeamRuns(): TeamRun[] {
  if (teamBoardShowAll()) return teamBoard.runs;
  return teamBoard.runs.filter(runPassesDefaultFilter);
}
/** Runs hidden by the default filter (empty while show-all is on). */
function hiddenTeamRuns(): TeamRun[] {
  return teamBoard.runs.filter((r) => !runPassesDefaultFilter(r));
}
/** Any member's pane is live in this cockpit → never delete this run. */
function runHasLivePane(run: TeamRun): boolean {
  return run.members.some((m) => memberPaneIsLive(m.tmuxPaneId));
}
/** config.json written within FRESH_MIN → an active session, never delete. */
function runIsFresh(run: TeamRun): boolean {
  if (run.modifiedAt == null) return false;
  return Date.now() - run.modifiedAt < FRESH_MIN * 60 * 1000;
}
/** Dead runs safe to delete: hidden, no live pane, not freshly written. Mirrors
 *  the backend guard so the confirm count matches what actually deletes. */
export function deletableTeamRuns(): TeamRun[] {
  return hiddenTeamRuns().filter((r) => !runHasLivePane(r) && !runIsFresh(r));
}

/** Delete every currently-deletable dead run, then reload. Returns the count the
 *  backend actually removed (it re-validates + protects fresh runs). */
export async function cleanupDeadRuns(): Promise<number> {
  const ids = deletableTeamRuns().map((r) => r.sessionId);
  if (ids.length === 0) return 0;
  try {
    const deleted = await cleanupTeamRuns(ids);
    await loadTeamRunsNow();
    return deleted.length;
  } catch (e) {
    setTeamBoard("error", String(e));
    return 0;
  }
}

/** Open a member's project in a new pane cd'd to its cwd — the action for a
 *  dead/headless row that has no live pane to jump to. No-op without a cwd. */
export async function openMemberCwd(cwd?: string): Promise<void> {
  if (!cwd) return;
  try {
    const res = await createTab();
    await refreshState();
    setActiveTabId(res.tabId);
    setFocusedPaneId(res.paneId);
    await launchShell(res.paneId, cwd);
    closeTeamBoard();
  } catch (e) {
    setStore("error", `open cwd failed: ${String(e)}`);
  }
}

// ── Spin-up (P3 step 2) ──────────────────────────────────────────────────────
// Pair a saved roster + workflow + task → review the generated lead prompt →
// launch: new tab, boot claude, send the prompt. New run shows on the board.

interface TemplatesStore {
  teams: Roster[];
  workflows: Workflow[];
  loading: boolean;
  error: string | null;
}
const [templates, setTemplates] = createStore<TemplatesStore>({
  teams: [],
  workflows: [],
  loading: false,
  error: null,
});

interface SpinupPrevStore {
  data: SpinupPreview | null;
  loading: boolean;
  error: string | null;
}
const [spinupPrev, setSpinupPrev] = createStore<SpinupPrevStore>({
  data: null,
  loading: false,
  error: null,
});
const [spinupOpen, setSpinupOpen] = createSignal(false);
const [spinupRosterId, setSpinupRosterId] = createSignal<string | null>(null);
const [spinupWorkflowId, setSpinupWorkflowId] = createSignal<string | null>(null);
const [spinupTask, setSpinupTaskSig] = createSignal("");
export { templates, spinupPrev, spinupOpen, spinupRosterId, spinupWorkflowId, spinupTask };

/** Load saved roster + workflow templates for the dropdowns. */
export async function loadTemplatesNow(): Promise<void> {
  setTemplates("loading", true);
  setTemplates("error", null);
  try {
    const t = await loadCockpitTemplates(activeProjectPath());
    setTemplates("teams", t.teams);
    setTemplates("workflows", t.workflows);
  } catch (e) {
    setTemplates("error", String(e));
  } finally {
    setTemplates("loading", false);
  }
}

/** Recompute the spin-up preview (prompt + coverage) for the current selection. */
export async function refreshSpinupPreview(): Promise<void> {
  const rid = spinupRosterId();
  const wid = spinupWorkflowId();
  if (!rid || !wid) {
    setSpinupPrev("data", null);
    setSpinupPrev("error", null);
    return;
  }
  setSpinupPrev("loading", true);
  setSpinupPrev("error", null);
  try {
    const data = await spinupPreview(rid, wid, spinupTask(), activeProjectPath());
    setSpinupPrev("data", data);
  } catch (e) {
    setSpinupPrev("error", String(e));
    setSpinupPrev("data", null);
  } finally {
    setSpinupPrev("loading", false);
  }
}

export function setSpinupRoster(id: string): void {
  setSpinupRosterId(id);
  void refreshSpinupPreview();
}
export function setSpinupWorkflow(id: string): void {
  setSpinupWorkflowId(id);
  void refreshSpinupPreview();
}
export function setSpinupTask(t: string): void {
  setSpinupTaskSig(t);
  void refreshSpinupPreview();
}

export function openSpinupDialog(): void {
  setSpinupOpen(true);
  setSpinupRosterId(null);
  setSpinupWorkflowId(null);
  setSpinupTaskSig("");
  setSpinupPrev("data", null);
  setSpinupPrev("error", null);
  void loadTemplatesNow();
}
export function closeSpinupDialog(): void {
  setSpinupOpen(false);
}

/** True when the selection is valid and the roster covers the workflow's roles. */
export function canLaunchTeam(): boolean {
  const p = spinupPrev.data;
  return (
    !!spinupRosterId() &&
    !!spinupWorkflowId() &&
    spinupTask().trim().length > 0 &&
    !!p &&
    p.coverageProblems.length === 0 &&
    !spinupPrev.loading
  );
}

/** Launch the team: new tab → boot claude → (after boot) send the prompt + CR.
 *  Mirrors `launchFromInventory`'s plumbing; the boot delay is the grill's
 *  "review doubles as the timing fix" — send only once the lead is ready. */
export async function launchTeam(): Promise<void> {
  if (!canLaunchTeam()) return;
  const prompt = spinupPrev.data!.prompt;
  const teamName = spinupPrev.data!.rosterName;
  const preCwd = activeProjectPath();
  try {
    const res = await createTab(teamName);
    await refreshState();
    setActiveTabId(res.tabId);
    setFocusedPaneId(res.paneId);
    const cwd = preCwd ?? store.panes.find((p) => p.paneId === res.paneId)?.cwd;
    if (!cwd) {
      setStore("error", "spin-up failed — no working directory");
      return;
    }
    await launchCc(res.paneId, cwd);
    const pid = res.paneId;
    // Boot delay, then paste the single-line prompt and submit with a CR (the
    // same data path as run_line_in_pane). A short gap lets the text settle.
    window.setTimeout(() => {
      void paneSendKeys(pid, prompt);
      window.setTimeout(() => {
        void paneSendKeys(pid, "\r");
        void loadTeamRunsNow();
      }, 450);
    }, 3500);
    closeSpinupDialog();
    closeTeamBoard();
  } catch (e) {
    setStore("error", `spin-up failed: ${String(e)}`);
  }
}

/** Toggle a type filter chip (multi-select; empty set shows all). */
export function toggleInvType(t: InventoryType): void {
  setInvTypes((prev) => {
    const next = new Set(prev);
    if (next.has(t)) next.delete(t);
    else next.add(t);
    return next;
  });
}
export function setInvScopeFilter(s: "all" | "global" | "project"): void {
  setInvScope(s);
}
export function setInvQueryFilter(q: string): void {
  setInvQuery(q);
}

/** Items after type + scope + text filters, sorted type→scope→name. */
export function filteredInventory(): InventoryItem[] {
  const types = invTypes();
  const scope = invScope();
  const q = invQuery().trim().toLowerCase();
  const order: Record<InventoryType, number> = { skill: 0, subagent: 1, plugin: 2, mcp: 3 };
  return inventory.items
    .filter((i) => (types.size === 0 ? true : types.has(i.type)))
    .filter((i) => (scope === "all" ? true : i.scope === scope))
    .filter((i) =>
      q === ""
        ? true
        : i.name.toLowerCase().includes(q) ||
          i.desc.toLowerCase().includes(q) ||
          (i.detail ?? "").toLowerCase().includes(q),
    )
    .slice()
    .sort(
      (a, b) =>
        order[a.type] - order[b.type] ||
        a.scope.localeCompare(b.scope) ||
        a.name.localeCompare(b.name),
    );
}

/** Per-type counts of the FULL (unfiltered) inventory, for the filter chips. */
export function inventoryCounts(): Record<InventoryType, number> {
  const c: Record<InventoryType, number> = { skill: 0, subagent: 0, plugin: 0, mcp: 0 };
  for (const i of inventory.items) c[i.type]++;
  return c;
}

// ── Plugin toggle (P2-F2) — confirm-first, delegated to native, read-back ────
//
// Config writes are NEVER optimistic (spec hard rule): a click opens a confirm
// modal showing the exact `claude plugin …` command; on confirm we run it via
// the backend (which shells out to native CC), then reload the inventory so the
// row reflects what's actually on disk. Failure surfaces a toast; the row never
// moves on its own. Only plugins toggle here — MCP has no safe native disable.

interface PendingToggle {
  item: InventoryItem;
  enable: boolean;
  preview: string;
}
const [pendingToggle, setPendingToggle] = createSignal<PendingToggle | null>(null);
const [togglingId, setTogglingId] = createSignal<string | null>(null);

export { pendingToggle, togglingId };

/** Open the confirm modal for flipping a plugin row (no write yet). */
export async function requestTogglePlugin(item: InventoryItem): Promise<void> {
  if (item.type !== "plugin" || !item.toggleable) return;
  const enable = !item.enabled;
  let preview = `claude plugin ${enable ? "enable" : "disable"} ${item.name}`;
  try {
    preview = await pluginTogglePreview(item.id, enable);
  } catch {
    /* fall back to the approximate preview above */
  }
  setPendingToggle({ item, enable, preview });
}

export function cancelToggle(): void {
  setPendingToggle(null);
}

// ── Launch from inventory (P2-F4) ────────────────────────────────────────────
//
// A skill/subagent row's ▶ opens it in a NEW tab (never clobbers a live pane).
// Subagent → `claude --agent <name>` (backend-validated). Skill → `claude` at the
// project cwd, then best-effort pre-type `/<skill> ` once it boots (no Enter, so
// the user adds args + runs it). Plugins/MCP aren't launchable.

const NAME_OK = /^[A-Za-z0-9._-]+$/;

/** A1 — the pane toolbar's zero-friction "Launch CC" path: launch straight
 *  into an existing pane (no dialog). Surfaces a failure the same way every
 *  other launch path here does — `setStore("error", ...)` — so a bad cwd or
 *  backend error is visible, not indistinguishable from success. */
export async function directLaunchCc(paneId: string, cwd: string): Promise<void> {
  try {
    await launchCc(paneId, cwd);
  } catch (e) {
    setStore("error", `launch failed: ${String(e)}`);
  }
}

export async function launchFromInventory(item: InventoryItem): Promise<void> {
  if (item.type !== "skill" && item.type !== "subagent") return;
  if (!NAME_OK.test(item.name)) {
    setStore("error", `cannot launch — unsafe name: ${item.name}`);
    return;
  }
  const preCwd = activeProjectPath();
  try {
    const res = await createTab(item.name);
    await refreshState();
    setActiveTabId(res.tabId);
    setFocusedPaneId(res.paneId);
    // The new pane's own cwd is always a real absolute path — a safe fallback.
    const cwd = preCwd ?? store.panes.find((p) => p.paneId === res.paneId)?.cwd;
    if (!cwd) {
      setStore("error", "launch failed — no working directory");
      return;
    }
    if (item.type === "subagent") {
      await launchAgent(res.paneId, cwd, item.name);
    } else {
      await launchCc(res.paneId, cwd);
      window.setTimeout(() => paneSendKeys(res.paneId, `/${item.name} `), 2500);
    }
    closeInventory(); // reveal the new pane
  } catch (e) {
    setStore("error", `launch failed: ${String(e)}`);
  }
}

/** Confirm the pending toggle: run it, reload on success, toast on failure. */
export async function confirmToggle(): Promise<void> {
  const pending = pendingToggle();
  if (!pending) return;
  setPendingToggle(null);
  setTogglingId(pending.item.id);
  try {
    await togglePlugin(pending.item.id, pending.enable);
    reloadInventoryView(); // read-back: the row reflects disk truth, not a guess
  } catch (e) {
    setStore("error", `plugin toggle failed: ${String(e)}`);
  } finally {
    setTogglingId(null);
  }
}

// ── File-tree sidebar (v1.1) ──────────────────────────────────────────────────
//
// A DOCKED left sidebar (⌘B), not an overlay — a navigation + path helper for the
// terminals/agents, NOT an editor. The tree FOLLOWS the active pane's cwd: we
// PROBE the focused pane's `pane_current_path` on a poll, because a shell `cd`
// fires no topology event so the reconciled `PaneInfo.cwd` goes stale. Lazy —
// one `list_dir` per opened folder, cached in `entries` (keyed by abs path).

interface FileTreeStore {
  /** Current root = the active pane's cwd. */
  root: string;
  /** dir path → its immediate children (load cache). */
  entries: Record<string, FileEntry[]>;
  /** dir path → mid-load (for a spinner; avoids double-fetch). */
  loading: Record<string, boolean>;
  error: string | null;
}
const [fileTree, setFileTree] = createStore<FileTreeStore>({
  root: "",
  entries: {},
  loading: {},
  error: null,
});
/** Sidebar shown? Docked + default on; ⌘B toggles. */
const [sidebarVisible, setSidebarVisible] = createSignal(true);
/** Which folders are expanded (keyed by abs path). */
const [ftExpanded, setFtExpanded] = createStore<Record<string, boolean>>({});
/** Show dotfiles? (⚙ toggle). Independent of the gitignore toggle below. */
const [ftShowHidden, setFtShowHidden] = createSignal(false);
/** Hide .gitignored entries? (⊘ toggle). OFF by default — the tree shows all
 *  projects (incl. gitignored sub-repos); ON re-applies .gitignore to declutter. */
const [ftHideIgnored, setFtHideIgnored] = createSignal(false);
export { fileTree, sidebarVisible, ftExpanded, ftShowHidden, ftHideIgnored };

/** Children of the current root (the top level the tree renders). */
export function ftRootEntries(): FileEntry[] {
  return fileTree.entries[fileTree.root] ?? [];
}

/** Load (or reload) one directory's children into the cache. Never throws — a
 *  permission error surfaces as the panel error, not a blank tree. */
export async function ftLoadDir(path: string): Promise<void> {
  if (!path) return;
  setFileTree("loading", path, true);
  try {
    const entries = await listDir(path, ftShowHidden(), ftHideIgnored());
    setFileTree("entries", path, entries);
    setFileTree("error", null);
  } catch (e) {
    setFileTree("error", String(e));
  } finally {
    setFileTree("loading", path, false);
  }
}

/** Expand/collapse a folder; lazy-load its children on first expand. */
export function ftToggleExpand(path: string): void {
  const open = !!ftExpanded[path];
  setFtExpanded(path, !open);
  if (!open && !fileTree.entries[path]) void ftLoadDir(path);
  ftSyncWatched(); // visible set changed → update the watcher
}

/** Re-root the tree (the active pane's cwd changed). Always reloads the new root
 *  fresh; expansion state is kept (keyed by abs path, so it's harmless). */
export function ftSetRoot(path: string): void {
  if (!path || path === fileTree.root) return;
  setFileTree("root", path);
  void ftLoadDir(path);
  ftSyncWatched(); // root changed → re-watch
}

/** ⌘B — show/hide the docked sidebar. Re-syncs the root + watcher when toggled. */
export function toggleSidebar(): void {
  setSidebarVisible((v) => !v);
  if (sidebarVisible()) void syncFileTreeRoot();
  ftSyncWatched(); // watch the visible set, or clear it when hidden
}

/** Flip hide-.gitignored and re-list every cached dir so the filter re-applies. */
export function ftToggleHideIgnored(): void {
  setFtHideIgnored((v) => !v);
  for (const dir of Object.keys(fileTree.entries)) void ftLoadDir(dir);
}

/** Flip show-dotfiles and re-list every cached dir so the filter re-applies. */
export function ftToggleHidden(): void {
  setFtShowHidden((v) => !v);
  for (const dir of Object.keys(fileTree.entries)) void ftLoadDir(dir);
}

/** Manual ⟳: reload the root + every currently-expanded dir from disk. */
export function ftRefresh(): void {
  void ftLoadDir(fileTree.root);
  for (const [path, open] of Object.entries(ftExpanded)) {
    if (open) void ftLoadDir(path);
  }
}

/** Probe the focused pane's live cwd and re-root the tree there if it moved.
 *  The tree follows the active pane; shell `cd`s don't emit topology, so this
 *  runs on a poll + on focus/tab change. Cheap (one `display-message`). */
export async function syncFileTreeRoot(): Promise<void> {
  if (!sidebarVisible()) return; // don't probe a hidden sidebar
  const pid = focusedPaneId();
  let cwd: string | undefined;
  if (pid) {
    try {
      cwd = await paneCwd(pid);
    } catch {
      cwd = undefined; // dead/odd pane — fall back below
    }
  }
  if (!cwd) cwd = focusedPane()?.cwd || store.panes[0]?.cwd;
  if (cwd && cwd !== fileTree.root) ftSetRoot(cwd);
}

// ── File-tree actions (v1.1 Phase C) ──────────────────────────────────────────
// All actions DRIVE the terminals/agents — there is no editor. Path inserts use
// `paneSendKeys` (same channel as typing); creates/trash hit the validated
// backend; "open in terminal" reuses the tab/shell launch plumbing.

/** Path of `abs` relative to `base` (else `abs` if it isn't under `base`). */
function relativeTo(base: string, abs: string): string {
  if (!base) return abs;
  if (abs === base) return ".";
  const b = base.endsWith("/") ? base : base + "/";
  return abs.startsWith(b) ? abs.slice(b.length) : abs;
}

function parentDir(p: string): string {
  const t = p.replace(/\/+$/, "");
  const i = t.lastIndexOf("/");
  return i <= 0 ? "/" : t.slice(0, i);
}

function baseName(p: string): string {
  const parts = p.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || p;
}

/** Shell-quote a path that has anything beyond a safe charset (defense at the
 *  send boundary — a path with spaces/specials must reach the shell intact). */
function shellQuoteIfNeeded(s: string): string {
  return /[^A-Za-z0-9._/-]/.test(s) ? `'${s.replace(/'/g, `'\\''`)}'` : s;
}

/** Insert a path into a pane, formatted for the pane's kind: a claude pane gets
 *  an `@<relpath>` mention, a shell gets a (shell-quoted) raw relpath. Relative
 *  to the pane's own cwd; NO trailing Enter (you prefix a command / edit first). */
async function insertPathInto(paneId: string, absPath: string): Promise<void> {
  let base = fileTree.root;
  try {
    base = await paneCwd(paneId);
  } catch {
    /* keep tree root as the relativity base */
  }
  const rel = relativeTo(base, absPath);
  let isClaude = false;
  try {
    isClaude = (await paneCommand(paneId)).toLowerCase().includes("claude");
  } catch {
    /* default to shell formatting */
  }
  paneSendKeys(paneId, isClaude ? `@${rel}` : shellQuoteIfNeeded(rel));
}

/** Double-click a file → insert its path into the ACTIVE pane (D1/D5). */
export async function ftInsertIntoActivePane(entry: FileEntry): Promise<void> {
  if (entry.isDir) return;
  const pid = focusedPaneId();
  if (pid) await insertPathInto(pid, entry.path);
}

/** Right-click "Open in Terminal": a NEW tab with a shell cd'd into the folder
 *  (a file → its parent dir). */
export async function ftOpenInTerminal(entry: FileEntry): Promise<void> {
  const dir = entry.isDir ? entry.path : parentDir(entry.path);
  try {
    const res = await createTab(baseName(dir));
    await refreshState();
    setActiveTabId(res.tabId);
    setFocusedPaneId(res.paneId);
    await launchShell(res.paneId, dir);
  } catch (e) {
    setStore("error", `open in terminal failed: ${String(e)}`);
  }
}

export async function ftRevealInFinder(path: string): Promise<void> {
  try {
    await revealInFinder(path);
  } catch (e) {
    setStore("error", `reveal failed: ${String(e)}`);
  }
}

// ── cd navigation (v1.1 cd-nav): drive the active pane from the tree ───────────
// The sidebar stops being one-way: a double-click on a folder, a breadcrumb
// segment, or a repo pick all `cd` the ACTIVE pane. We only type `cd` into a
// recognized SHELL — a claude REPL / editor / test-runner would mis-eat the
// keystrokes — so anything else falls back to opening the dir in a NEW tab.

/** $HOME, resolved once on boot (the "Home" breadcrumb cd's here). */
const [ftHome, setFtHome] = createSignal<string>("");
export { ftHome };
export async function ftInitHome(): Promise<void> {
  try {
    setFtHome(await homeDir());
  } catch {
    /* leave empty — breadcrumb falls back to absolute segments from "/" */
  }
}

/** Roots visited via a click this session (most-recent first), for the picker. */
const [ftRecents, setFtRecents] = createSignal<string[]>([]);
export { ftRecents };
function pushRecent(path: string): void {
  setFtRecents((r) => [path, ...r.filter((p) => p !== path)].slice(0, 8));
}

/** Sibling project dirs for the repo picker (refreshed when it opens). */
const [ftRepos, setFtRepos] = createSignal<RepoEntry[]>([]);
export { ftRepos };
export async function ftLoadRepos(): Promise<void> {
  const from = fileTree.root;
  if (!from) {
    setFtRepos([]);
    return;
  }
  try {
    setFtRepos(await discoverRepos(from));
  } catch {
    setFtRepos([]);
  }
}

// `pane_current_command` reports a basename like `zsh`, `-zsh` (login), `bash`,
// `claude`, `node`, `vim`. cd is only safe to type into an actual shell.
const SHELLS = new Set([
  "zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh", "csh",
]);
function isShellCommand(cmd: string): boolean {
  const c = cmd.trim().toLowerCase().replace(/^-/, ""); // strip login-shell dash
  return SHELLS.has(c);
}

/** cd the active pane into `dir`. Shell pane → `cd <quoted>` + Enter, then a
 *  snappy proactive re-root (the cwd poll would catch up anyway) and remember the
 *  dir as a recent. NON-shell pane (claude/editor/runner) → don't type into it;
 *  open the dir in a new terminal tab instead. Shared by folder-dblclick,
 *  breadcrumb segments, and the repo picker. */
export async function ftCdActivePane(dir: string): Promise<void> {
  if (!dir) return;
  const pid = focusedPaneId();
  if (!pid) {
    // No active pane — open a fresh terminal there.
    await ftOpenInTerminal({ name: baseName(dir), path: dir, isDir: true });
    return;
  }
  let cmd = "";
  try {
    cmd = await paneCommand(pid);
  } catch {
    /* unknown command — treat as non-shell, fall back to a new tab */
  }
  if (!isShellCommand(cmd)) {
    await ftOpenInTerminal({ name: baseName(dir), path: dir, isDir: true });
    return;
  }
  // Fire-and-forget, but self-catch: paneRunLine returns a raw Promise (unlike
  // the old self-catching paneSendKeys), so an un-caught rejection on a failed cd
  // would surface as an unhandled promise rejection. Log and move on.
  void paneRunLine(pid, `cd ${shellQuoteIfNeeded(dir)}`).catch((e) =>
    console.warn("pane_run_line (cd) failed", e),
  );
  pushRecent(dir);
  ftSetRoot(dir); // snappy re-root; syncFileTreeRoot would also catch it
}

async function copyText(t: string): Promise<void> {
  // WKWebView blocks navigator.clipboard.writeText over the tauri:// scheme, so
  // Copy Path silently failed. Write through the native clipboard-manager plugin;
  // fall back to navigator.clipboard only for plain-browser (vite) dev.
  try {
    await tauriWriteText(t);
  } catch {
    try {
      await navigator.clipboard.writeText(t);
    } catch {
      setStore("error", "clipboard write failed");
    }
  }
}
/** Copy Path = absolute. */
export function ftCopyPath(path: string): void {
  void copyText(path);
}
/** Copy Relative Path = relative to the tree root (the active cwd). */
export function ftCopyRelPath(path: string): void {
  void copyText(relativeTo(fileTree.root, path));
}

// ── Inline New File / New Folder ──────────────────────────────────────────────
interface FtNewEntry {
  parent: string;
  isDir: boolean;
}
const [ftNewEntry, setFtNewEntry] = createSignal<FtNewEntry | null>(null);
export { ftNewEntry };

/** Begin an inline new file/folder under `parent` (default = the root). */
export function ftBeginNew(isDir: boolean, parent?: string): void {
  const dir = parent ?? fileTree.root;
  if (parent && !ftExpanded[parent]) {
    setFtExpanded(parent, true);
    if (!fileTree.entries[parent]) void ftLoadDir(parent);
  }
  setFtNewEntry({ parent: dir, isDir });
}
export function ftCancelNew(): void {
  setFtNewEntry(null);
}
/** Commit the inline new entry (Enter): create on disk + reload the parent. */
export async function ftCommitNew(name: string): Promise<void> {
  const ne = ftNewEntry();
  setFtNewEntry(null);
  if (!ne) return;
  const trimmed = name.trim();
  if (!trimmed) return;
  try {
    await createEntry(ne.parent, trimmed, ne.isDir);
    await ftLoadDir(ne.parent); // surface the new entry
  } catch (e) {
    setStore("error", `create failed: ${String(e)}`);
  }
}

// ── Delete (confirm → Trash) ──────────────────────────────────────────────────
const [ftPendingDelete, setFtPendingDelete] = createSignal<FileEntry | null>(null);
export { ftPendingDelete };
export function ftRequestDelete(entry: FileEntry): void {
  setFtPendingDelete(entry);
}
export function ftCancelDelete(): void {
  setFtPendingDelete(null);
}
/** Confirm: move to Trash (recoverable) + reload the parent dir. */
export async function ftConfirmDelete(): Promise<void> {
  const e = ftPendingDelete();
  setFtPendingDelete(null);
  if (!e) return;
  try {
    await trashPath(e.path);
    await ftLoadDir(parentDir(e.path));
  } catch (err) {
    setStore("error", `delete failed: ${String(err)}`);
  }
}

// ── Attach to Agent ───────────────────────────────────────────────────────────
/** `%N` pane id → the live team-board member occupying it (first match wins;
 *  a pane belongs to at most one live member in practice). The pane↔member
 *  join, shared by `ftLiveAgents` (Attach-to-Agent submenu) and `paneLabel`
 *  (toolbar name). */
function liveMembersByPane(): Map<string, TeamMember> {
  const out = new Map<string, TeamMember>();
  for (const run of teamBoard.runs) {
    for (const m of run.members) {
      if (m.tmuxPaneId && memberPaneIsLive(m.tmuxPaneId) && !out.has(m.tmuxPaneId)) {
        out.set(m.tmuxPaneId, m);
      }
    }
  }
  return out;
}

/** Live agent panes for the Attach-to-Agent submenu: team-board members whose
 *  `%N` pane this cockpit currently tracks (deduped). */
export function ftLiveAgents(): { paneId: string; label: string }[] {
  return Array.from(liveMembersByPane(), ([paneId, m]) => ({
    paneId,
    label: `${m.name || m.agentId} ${paneId}`,
  }));
}

/** Best display name for a pane's toolbar: a live team-board member's `name`
 *  (Claude Code's own OSC title overwrite otherwise yields "general-purpose"
 *  for every teammate pane), else the pane's own title, else its raw id. The
 *  tooltip surfaces the member's task summary (if any) plus the pane's cwd. */
export function paneLabel(pane: PaneInfo): { text: string; tooltip: string } {
  const member = liveMembersByPane().get(pane.paneId);
  const text = member?.name || pane.title || pane.paneId;
  const tooltipParts = [member?.taskSummary, pane.cwd].filter(
    (p): p is string => !!p,
  );
  return { text, tooltip: tooltipParts.join(" — ") || pane.paneId };
}
/** Attach a file to a chosen agent: insert its path into that agent's pane.
 *  (insertPathInto detects the claude pane → `@path` mention.) */
export async function ftAttachToAgent(entry: FileEntry, paneId: string): Promise<void> {
  await insertPathInto(paneId, entry.path);
}

// ── Context menu (right-click) ────────────────────────────────────────────────
interface FtMenu {
  entry: FileEntry;
  x: number;
  y: number;
}
const [ftMenu, setFtMenu] = createSignal<FtMenu | null>(null);
export { ftMenu };
/** Open the right-click menu at (x,y); refresh live team runs so the
 *  Attach-to-Agent submenu is populated. */
export function ftOpenMenu(entry: FileEntry, x: number, y: number): void {
  setFtMenu({ entry, x, y });
  void loadTeamRunsNow();
}
export function ftCloseMenu(): void {
  setFtMenu(null);
}

// ── Live fs-watch (Phase D) ───────────────────────────────────────────────────
// The visible dirs (root + expanded) are watched non-recursively. On a change the
// backend emits `filetree:changed { dir }`; we debounce-reload that one dir.

/** The dirs currently visible (root + expanded folders) — the watch set. */
function ftWatchedDirs(): string[] {
  const dirs = new Set<string>();
  if (fileTree.root) dirs.add(fileTree.root);
  for (const [p, open] of Object.entries(ftExpanded)) if (open) dirs.add(p);
  return [...dirs];
}

/** Push the current visible set to the backend watcher (or clear it when the
 *  sidebar is hidden). Cheap diff server-side; safe to call on every change. */
export function ftSyncWatched(): void {
  void watchDirs(sidebarVisible() ? ftWatchedDirs() : []);
}

// One debounce timer per changed dir (agents write bursts; coalesce reloads).
const ftReloadTimers: Record<string, number> = {};
/** A watched dir changed → reload it (debounced) if it's currently shown. */
function ftOnChanged(dir: string): void {
  if (dir !== fileTree.root && !(dir in fileTree.entries)) return; // not visible
  if (ftReloadTimers[dir]) clearTimeout(ftReloadTimers[dir]);
  ftReloadTimers[dir] = window.setTimeout(() => void ftLoadDir(dir), 150);
}
export { ftOnChanged };

// ── Send pane → new tab (Phase E) ─────────────────────────────────────────────
/** Break a pane out into its own new tab (kept running) + switch to it. The
 *  caller (pane chrome) only offers this when the tab has >1 pane. */
export async function sendPaneToNewTab(paneId: string): Promise<void> {
  try {
    const windowId = await breakPane(paneId);
    await refreshState();
    const tab = store.tabs.find((t) => t.tmuxWindowId === windowId);
    if (tab) {
      setActiveTabId(tab.tabId);
      setFocusedPaneId(paneId);
    }
  } catch (e) {
    setStore("error", `send to new tab failed: ${String(e)}`);
  }
}

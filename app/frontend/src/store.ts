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
  loadInventory,
  loadAuditMatrix,
  loadTeamRuns,
  loadCockpitTemplates,
  spinupPreview,
  togglePlugin,
  pluginTogglePreview,
  launchCc,
  launchAgent,
  paneSendKeys,
  onPaneStatus,
  onPaneTopology,
  onCockpitReconnected,
  onCloseRequested,
  type CockpitState,
  type PaneInfo,
  type TabInfo,
  type InventoryItem,
  type InventoryType,
  type AuditMatrix,
  type TeamRun,
  type Roster,
  type Workflow,
  type SpinupPreview,
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
let resyncTimer: number | undefined;
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
  // Active tab ONLY. The viewport (refresh-client -C) is shared across tmux
  // windows, and PaneGrid renders just the active tab's panes — so the grid
  // must mirror activePanes(), not the global pane count. Using store.panes
  // here counted panes in OTHER tabs too: 2 tabs × 1 pane → n=2 → a 2-col
  // viewport ~2× the real width, so a single-pane tab's CC laid out for double
  // width and wrapped/scattered in the half-width xterm.
  const n = activePanes().length;
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
  try {
    await setGrid(winCols, winRows, layout);
    // Record the key ONLY after the push lands. The control client may still be
    // attaching at first mount, so an early set_grid rejects with "not attached".
    // If we recorded the key before awaiting (the old bug), the change-guard
    // above would then block every retry at this same size — the window stayed
    // stuck at its tmux birth size (200×50) until the user happened to resize.
    lastGridKey = key;
    scheduleResync();
  } catch {
    // Not attached yet (or transient). No fresh reportCell may follow once the
    // xterm settles, so self-heal: retry until the client accepts the size.
    if (gridTimer) clearTimeout(gridTimer);
    gridTimer = window.setTimeout(() => void pushGrid(), 400);
  }
}

// After a tmux pane resize, xterm's own reflow of a full-screen TUI is lossy: a
// frame the app (e.g. `claude`) drew at the OLD width keeps its now-too-wide lines
// in xterm's buffer, which wrap and scatter at the new width — and the app's live
// SIGWINCH redraw only repaints the visible viewport, not the polluted scrollback
// above it (the exact garble Ctrl+L fixes by hand). tmux holds each pane's CLEAN
// grid re-rendered at the new width, so the cure is to wipe xterm and replay
// tmux's capture. Panes are born 200 cols (tmux.rs) and settle to the fitted
// width, so this fires on the birth→fit transition too — fixing a CC launched
// into a just-born pane as well as any later user window-resize. Broadcast +
// debounced: every visible XtermHost re-syncs its own pane once the size settles.
function scheduleResync(): void {
  if (resyncTimer) clearTimeout(resyncTimer);
  resyncTimer = window.setTimeout(() => {
    window.dispatchEvent(new CustomEvent("cockpit:resync"));
  }, 320);
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

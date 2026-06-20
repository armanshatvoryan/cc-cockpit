// IPC layer — typed wrappers over the full CC Cockpit backend contract.
//
// Command names + arg shapes match the `#[tauri::command]` fns in
// `app/src-tauri/src/lib.rs` verbatim. Tauri v2's JS bridge converts camelCase
// arg keys to the snake_case Rust params, so we pass camelCase here. Every
// command rejects with a `string` on error.
//
// Events (Rust -> FE):
//   pane:data     { paneId, bytesB64 }        raw VT for one pane
//   pane:topology { kind, tabId?, windowId?, paneId?, layout? }
//   pane:status   { paneId, status, ambiguous, recencyMs }

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// ── State / DTO types (mirror manager.rs camelCase serde) ───────────────────

export type PaneStatus =
  | "IDLE"
  | "WORKING"
  | "NEEDS_INPUT"
  | "DEAD"
  | "UNKNOWN";

export interface PaneInfo {
  paneId: string;
  tabId: string;
  cwd: string;
  title: string;
  dead: boolean;
  status: PaneStatus;
  ambiguous: boolean;
}

export interface TabInfo {
  tabId: string;
  tmuxWindowId: string;
  index: number;
  name: string;
  layout: string;
  paneIds: string[];
}

export interface CockpitState {
  socket: string;
  session: string;
  tabs: TabInfo[];
  panes: PaneInfo[];
}

// ── Inventory (P2-F1) ────────────────────────────────────────────────────────

export type InventoryType = "skill" | "subagent" | "plugin" | "mcp";

export interface InventoryItem {
  id: string;
  name: string;
  type: InventoryType;
  scope: "global" | "project";
  enabled: boolean;
  toggleable: boolean;
  desc: string;
  detail?: string;
  path?: string;
  parseError?: string;
}

export interface CreateTabResult {
  tabId: string;
  tmuxWindowId: string;
  paneId: string;
}

export interface CloseTabResult {
  ok: boolean;
  livePanes: string[];
}

export interface SplitPaneResult {
  paneId: string;
  layout: string;
}

export interface WarmStartPayload {
  bytesB64: string;
}

// ── Event payloads ──────────────────────────────────────────────────────────

export interface PaneDataPayload {
  paneId: string;
  bytesB64: string;
}

export type TopologyKind =
  | "windowAdd"
  | "windowClose"
  | "layoutChange"
  | "activePaneChanged"
  | "paneModeChanged"
  | "exit";

export interface PaneTopologyPayload {
  kind: TopologyKind;
  tabId?: string;
  windowId?: string;
  paneId?: string;
  layout?: string;
}

export interface PaneStatusPayload {
  paneId: string;
  status: PaneStatus;
  ambiguous: boolean;
  recencyMs: number;
}

// ── Commands (FE -> Rust) ────────────────────────────────────────────────────

/** Ensure socket + session, attach control client, start forwarder+poller. */
export function cockpitInit(): Promise<CockpitState> {
  return invoke<CockpitState>("cockpit_init");
}

/** Create a new tab (tmux window). */
export function createTab(name?: string): Promise<CreateTabResult> {
  return invoke<CreateTabResult>("create_tab", { name: name ?? null });
}

/**
 * Inspect/close a tab. If `force` is false and the result has `ok:false` with
 * non-empty `livePanes`, confirm with the user then re-call with `force:true`.
 */
export function closeTab(tabId: string, force: boolean): Promise<CloseTabResult> {
  return invoke<CloseTabResult>("close_tab", { tabId, force });
}

/** Split a pane horizontally ('h', side-by-side) or vertically ('v', stacked). */
export function splitPane(paneId: string, dir: "h" | "v"): Promise<SplitPaneResult> {
  return invoke<SplitPaneResult>("split_pane", { paneId, dir });
}

/** Close a pane: 'kill' (process gone) or 'detach' (break out, keep running). */
export function closePane(paneId: string, mode: "kill" | "detach"): Promise<void> {
  return invoke<void>("close_pane", { paneId, mode });
}

/** Launch a real `claude` in a pane. NEVER pass an api-key flag. */
export function launchCc(
  paneId: string,
  cwd: string,
  model?: string,
  flags?: string,
): Promise<void> {
  return invoke<void>("launch_cc", {
    paneId,
    cwd,
    model: model && model.trim() ? model.trim() : null,
    flags: flags && flags.trim() ? flags.trim() : null,
  });
}

/** Launch a plain shell context (`cd <cwd>`) in a pane. */
export function launchShell(paneId: string, cwd: string): Promise<void> {
  return invoke<void>("launch_shell", { paneId, cwd });
}

/**
 * Send literal VT input to a pane. Fire-and-forget: we deliberately do NOT
 * await at call sites so typing never blocks on IPC.
 */
export function paneSendKeys(paneId: string, data: string): void {
  void invoke("pane_send_keys", { paneId, data }).catch((e) =>
    console.warn("pane_send_keys failed", e),
  );
}

/** Push the xterm-fit cols/rows to tmux (authoritative resize). */
export function paneResize(paneId: string, cols: number, rows: number): Promise<void> {
  return invoke<void>("pane_resize", { paneId, cols, rows });
}

/**
 * Size the WHOLE tmux window to the grid's bounding box and re-tile. This is the
 * single authority for window size (the control client's size IS the window
 * size), replacing per-pane resizes that collapsed multi-pane tabs to 1 column.
 */
export function setGrid(cols: number, rows: number, layout: string): Promise<void> {
  return invoke<void>("set_grid", { cols, rows, layout });
}

/** Ctrl+C interrupt for a pane. */
export function interruptPane(paneId: string): Promise<void> {
  return invoke<void>("interrupt_pane", { paneId });
}

/** Full state snapshot (called on every topology event to reconcile). */
export function listState(): Promise<CockpitState> {
  return invoke<CockpitState>("list_state");
}

/**
 * Read-only unified inventory (P2-F1): skills/subagents/plugins/MCP across the
 * global `~/.claude` scope plus the per-project `.claude/` scope rooted at
 * `projectPath` (the active pane's cwd; the backend walks up to the repo root).
 * Pure config reads — never opens `.env`, never returns MCP env values.
 */
export function loadInventory(projectPath?: string): Promise<InventoryItem[]> {
  return invoke<InventoryItem[]>("load_inventory", {
    projectPath: projectPath ?? null,
  });
}

/**
 * Warm-start replay for a pane: returns its current screen + scrollback
 * (escape-aware) base64-encoded. Called once on XtermHost mount so a pane the
 * GUI re-attaches to paints its existing content instead of staying blank (the
 * control client only streams `%output` produced AFTER it attaches).
 */
export function warmStart(paneId: string): Promise<WarmStartPayload> {
  return invoke<WarmStartPayload>("warm_start", { paneId });
}

// ── Events (Rust -> FE) ───────────────────────────────────────────────────────

export function onPaneData(handler: (p: PaneDataPayload) => void): Promise<UnlistenFn> {
  return listen<PaneDataPayload>("pane:data", (e) => handler(e.payload));
}

export function onPaneTopology(
  handler: (p: PaneTopologyPayload) => void,
): Promise<UnlistenFn> {
  return listen<PaneTopologyPayload>("pane:topology", (e) => handler(e.payload));
}

export function onPaneStatus(
  handler: (p: PaneStatusPayload) => void,
): Promise<UnlistenFn> {
  return listen<PaneStatusPayload>("pane:status", (e) => handler(e.payload));
}

/**
 * Fired after the backend re-heals a vanished tmux server mid-run (re-attached a
 * fresh control client). The frontend should reload state — the reconnected
 * session has new panes/tabs, which remount and warm-start.
 */
export function onCockpitReconnected(handler: () => void): Promise<UnlistenFn> {
  return listen("cockpit:reconnected", () => handler());
}

/**
 * Fired when the user hits ⌘W / the window close button. The backend prevents the
 * actual window close; the frontend closes the focused pane (or active tab) so a
 * stray ⌘W never kills the whole cockpit.
 */
export function onCloseRequested(handler: () => void): Promise<UnlistenFn> {
  return listen("cockpit:close-requested", () => handler());
}

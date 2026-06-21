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

export type AuditCellState = "on" | "off" | "absent" | "error";

export interface AuditColumn {
  label: string;
  projectPath: string;
}

export interface AuditRow {
  id: string;
  name: string;
  type: "plugin" | "mcp";
  detail?: string;
  cells: AuditCellState[];
}

export interface AuditMatrix {
  columns: AuditColumn[];
  rows: AuditRow[];
}

// ── Cockpit team templates (P3 step 1) ───────────────────────────────────────
// Saved, reusable ROSTER (who) + WORKFLOW (how) artifacts under
// `~/.claude/cockpit/{teams,workflows}/*.yaml` (global) and the project mirror.

export interface RoleSpec {
  /** Role name (the YAML map key); workflows refer to roles by this. */
  role: string;
  /** Agent that fills the role — a `~/.claude/agents/<x>.md` or built-in type. */
  agent: string;
  model?: string;
  worktree: boolean;
  mode: "live" | "headless" | string;
}

export interface Roster {
  /** `"team:<scope>:<name>"`. */
  id: string;
  scope: "global" | "project";
  name: string;
  description: string;
  path: string;
  roles: RoleSpec[];
  defaultCwd?: string;
  /** Non-fatal validation warnings (bad mode, unknown agent ref, …). */
  problems?: string[];
  parseError?: string;
}

export interface PhaseSpec {
  id: string;
  /** Unified from `role:` and `roles: [..]`. `lead` allowed. */
  roles?: string[];
  parallel: boolean;
  gate?: "user" | string;
}

export interface Workflow {
  /** `"workflow:<scope>:<name>"`. */
  id: string;
  scope: "global" | "project";
  name: string;
  description: string;
  path: string;
  leadHint?: string;
  phases: PhaseSpec[];
  problems?: string[];
  parseError?: string;
}

export interface CockpitTemplates {
  teams: Roster[];
  workflows: Workflow[];
}

// ── Live team runs (P3 step 3 — the team board) ──────────────────────────────
// READ-ONLY view of native Agent Teams sessions on disk
// (`~/.claude/teams/session-*/` config + inboxes + tasks).

export interface TeamMember {
  agentId: string;
  name: string;
  agentType: string;
  /** `tmux` → "live", `in-process` → "headless". */
  mode: "live" | "headless" | string;
  backendType: "tmux" | "in-process" | string;
  /** `"%1"` real pane on the lead's socket, `"leader"`, or absent. */
  tmuxPaneId?: string;
  model?: string;
  cwd?: string;
  color?: string;
  isActive: boolean;
  isLead: boolean;
}

export interface TeamRun {
  /** Session dir name, e.g. `session-41b57ff1`. */
  sessionId: string;
  name: string;
  leadAgentId: string;
  createdAt?: number;
  members: TeamMember[];
  /** Undelivered messages summed across the file mailbox. */
  inboxDepth: number;
  taskCount: number;
  parseError?: string;
}

// ── Spin-up (P3 step 2) ──────────────────────────────────────────────────────

export interface SpinupPreview {
  rosterName: string;
  workflowName: string;
  /** The single-line lead prompt the cockpit will send after launch. */
  prompt: string;
  /** Workflow roles the roster doesn't cover. Non-empty ⇒ launch blocked. */
  coverageProblems: string[];
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

// ── Terax tier-1 (git status + disk-persisted layout) ────────────────────────
// Mirrors `gitstatus.rs` / `persist.rs` DTOs (camelCase serde). See the backend
// contract: `git_status_snapshot(cwd)`, `save_layout(snapshot)`, `load_layout()`.

export interface GitStatus {
  branch: string;
  ahead: number;
  behind: number;
  dirty: boolean;
  changed: number;
  untracked: number;
}

/** One persisted tab: position + first-pane cwd + optional local rename. */
export interface TabLayout {
  index: number;
  cwd: string;
  customTitle?: string | null;
}

/** The whole disk-persisted cockpit layout (`<app_config_dir>/cockpit/layout.json`). */
export interface LayoutSnapshot {
  schemaVersion: number;
  activeTabId?: string | null;
  tabs: TabLayout[];
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
 * Launch `claude --agent <name>` in a pane (P2-F4 launch-from-inventory). The
 * backend validates + shell-quotes the agent name, so a config-derived name
 * can't inject a flag or shell payload.
 */
export function launchAgent(paneId: string, cwd: string, agent: string): Promise<void> {
  return invoke<void>("launch_agent", { paneId, cwd, agent });
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
 * Toggle a plugin on/off (P2-F2). DELEGATES to `claude plugin enable|disable`
 * — the cockpit never hand-patches config. `id` is the inventory item id. After
 * it resolves, reload the inventory so the row reflects disk truth (never flip
 * optimistically — config writes are confirm-first + read-back only).
 */
export function togglePlugin(id: string, enable: boolean): Promise<void> {
  return invoke<void>("toggle_plugin", { id, enable });
}

/** The exact `claude …` command a confirm modal shows before a toggle runs. */
export function pluginTogglePreview(id: string, enable: boolean): Promise<string> {
  return invoke<string>("plugin_toggle_preview", { id, enable });
}

/**
 * Cross-project audit matrix (P2-F5): effective on/off of every plugin + MCP
 * server across the open tabs' project roots. `projectPaths` = the open tabs'
 * cwds (the backend resolves + dedupes them to project roots). Pure read.
 */
export function loadAuditMatrix(projectPaths: string[]): Promise<AuditMatrix> {
  return invoke<AuditMatrix>("load_audit_matrix", { projectPaths });
}

/**
 * Cockpit team templates (P3 step 1): the saved roster + workflow YAML under
 * `~/.claude/cockpit/{teams,workflows}/` (global) + the active project mirror.
 * Pure read + validate; a malformed file degrades to one row with `parseError`,
 * never a blank panel.
 */
export function loadCockpitTemplates(projectPath?: string): Promise<CockpitTemplates> {
  return invoke<CockpitTemplates>("load_cockpit_templates", {
    projectPath: projectPath ?? null,
  });
}

/**
 * Live team board (P3 step 3): native Agent Teams sessions on disk, newest
 * first. Pure read — a rotated/garbled run degrades to an empty list or one
 * `parseError` row, never an error.
 */
export function loadTeamRuns(): Promise<TeamRun[]> {
  return invoke<TeamRun[]>("load_team_runs");
}

/**
 * Spin-up review (P3 step 2): pair a saved roster + workflow + task → the
 * generated lead prompt + role-coverage problems. Pure compose — the launch
 * itself is orchestrated in the store from existing plumbing.
 */
export function spinupPreview(
  rosterId: string,
  workflowId: string,
  task: string,
  projectPath?: string,
): Promise<SpinupPreview> {
  return invoke<SpinupPreview>("spinup_preview", {
    rosterId,
    workflowId,
    task,
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

/**
 * Per-worktree git status for a cwd (dev#2). Resolves to `null` when `cwd` is not
 * a git repo (the backend returns `Ok(None)`); rejects only if `git` is missing.
 */
export function gitStatusSnapshot(cwd: string): Promise<GitStatus | null> {
  return invoke<GitStatus | null>("git_status_snapshot", { cwd });
}

/** Persist the cockpit layout atomically (dev#1). Backend forces schemaVersion=1. */
export function saveLayout(snapshot: LayoutSnapshot): Promise<void> {
  return invoke<void>("save_layout", { snapshot });
}

/** Load the persisted layout, or `null` on first run / absent file (dev#1). */
export function loadLayout(): Promise<LayoutSnapshot | null> {
  return invoke<LayoutSnapshot | null>("load_layout");
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

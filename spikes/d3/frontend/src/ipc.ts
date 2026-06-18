// IPC layer — typed wrappers over the Tauri command/event surface for the D3
// spike. Mirrors the §5 IPC contract (subset relevant to the terminal path):
//   invoke: attach_session, pane_send_keys, pane_resize, interrupt_pane
//   event:  pane:data { paneId, bytesB64 }, pane:topology { ... }
//
// These names match the `#[tauri::command]` fns + `app.emit` keys in
// `tauri-app/src-tauri/src/lib.rs`. Keep them in sync.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface PaneDataPayload {
  paneId: string;
  bytesB64: string;
}

export interface PaneTopologyPayload {
  kind: "windowAdd" | "windowClose" | "layoutChange" | "activePaneChanged" | "paneModeChanged";
  windowId?: string;
  paneId?: string;
  layout?: string;
}

// ── commands (FE -> Rust) ──────────────────────────────────────────────────

/** Spawn the single `tmux -CC` control client for a session; starts data flow. */
export function attachSession(socket: string, session: string): Promise<void> {
  return invoke("attach_session", { socket, session });
}

/**
 * Send literal VT input to a pane. Fire-and-forget: we deliberately do NOT
 * await at call sites so typing never blocks on IPC (§2 input path).
 */
export function paneSendKeys(paneId: string, data: string): void {
  // void the promise; errors surface via a console warning only.
  void invoke("pane_send_keys", { paneId, data }).catch((e) =>
    console.warn("pane_send_keys failed", e),
  );
}

/** Push the xterm-fit cols/rows to tmux (authoritative resize). */
export function paneResize(paneId: string, cols: number, rows: number): Promise<void> {
  return invoke("pane_resize", { paneId, cols, rows });
}

/** Ctrl+C interrupt for a pane (P1-F5). */
export function interruptPane(paneId: string): Promise<void> {
  return invoke("interrupt_pane", { paneId });
}

// ── events (Rust -> FE) ─────────────────────────────────────────────────────

/** Subscribe to coalesced `%output` for all panes. Returns an unlisten fn. */
export function onPaneData(handler: (p: PaneDataPayload) => void): Promise<UnlistenFn> {
  return listen<PaneDataPayload>("pane:data", (e) => handler(e.payload));
}

/** Subscribe to topology / lifecycle events. */
export function onPaneTopology(
  handler: (p: PaneTopologyPayload) => void,
): Promise<UnlistenFn> {
  return listen<PaneTopologyPayload>("pane:topology", (e) => handler(e.payload));
}

//! CC Cockpit — Tauri command layer over the SessionManager.
//!
//! Thin shell: every `#[tauri::command]` delegates to `SessionManager` (which
//! owns the PROVEN `cockpit_engine::ControlClient` for streaming and drives
//! `tmux -L cockpit` for admin). On `cockpit_init` we spin up two background
//! tasks:
//!   * an **event forwarder** that maps the engine's `Outbound` channel onto the
//!     `pane:data` / `pane:topology` Tauri events; and
//!   * a **status poller** (~1Hz) that runs the ported D6 heuristic and emits
//!     `pane:status` on change.
//!
//! The IPC contract this exposes is documented in the final report; command
//! names + payloads are stable for the frontend to build against.

pub mod filetree;
pub mod gitstatus;
pub mod inventory;
pub mod manager;
pub mod persist;
pub mod status;
pub mod teamruns;
pub mod templates;
pub mod tmux;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use cockpit_engine::{Outbound, TopologyEvent};
use manager::{
    CloseTabResult, CockpitState, CreateTabResult, SessionManager, SplitPaneResult,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

/// Shared base64 engine for re-encoding coalesced `pane:data` payloads (must
/// match the engine's STANDARD alphabet so the frontend decodes identically).
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// Output coalescing window. `spawn_forwarder` buffers per-pane `%output` and
/// flushes at most one `pane:data` per pane per tick, instead of one emit per
/// control-mode frame (which floods the IPC bridge and causes UI lag).
const COALESCE_MS: u64 = 16;

/// Managed state: the SessionManager behind a mutex (Tauri commands are sync).
#[derive(Clone)]
pub struct AppState {
    pub mgr: Arc<Mutex<SessionManager>>,
    /// Set once the forwarder + poller are running, so re-init doesn't dup them.
    pub started: Arc<Mutex<bool>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            mgr: Arc::new(Mutex::new(SessionManager::new())),
            started: Arc::new(Mutex::new(false)),
        }
    }
}

// ── Event payloads (camelCase) ───────────────────────────────────────────────

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaneDataPayload {
    pane_id: String,
    bytes_b64: String,
}

/// Return value of the `warm_start` command: the pane's current screen +
/// scrollback (escape-aware), base64-encoded, for the frontend to `term.write`
/// on mount so a re-attached pane isn't blank.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WarmStartPayload {
    bytes_b64: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaneTopologyPayload {
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tab_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    layout: Option<String>,
}

// ── Commands ─────────────────────────────────────────────────────────────────

/// Ensure socket + `cockpit-main`, attach the control client, start the event
/// forwarder + status poller, and return the current full state.
#[tauri::command]
fn cockpit_init(app: AppHandle, state: State<'_, AppState>) -> Result<CockpitState, String> {
    // Attach (idempotent on the session; fresh client each call).
    let rx = {
        let mut mgr = state.mgr.lock().unwrap();
        mgr.init()?
    };

    // ONE forwarder per attach. A later reconnect spawns another and the prior one
    // ends on its own when its dropped client's stream disconnects, so this is not
    // gated by `started` — only the singleton poller is.
    spawn_forwarder(app.clone(), rx);
    {
        let mut started = state.started.lock().unwrap();
        if !*started {
            spawn_status_poller(app.clone(), state.mgr.clone());
            *started = true;
        }
    }

    let mgr = state.mgr.lock().unwrap();
    mgr.list_state()
}

/// Run a SessionManager op with automatic mid-run reconnect. On a server-gone
/// error the manager re-heals + re-attaches and retries the op once; we then
/// rebind the event forwarder to the fresh `Outbound` stream and notify the
/// frontend (`cockpit:reconnected`) so it reloads state and panes repaint via
/// warm-start. The happy path adds zero overhead (only the failure branch heals).
fn with_reconnect<T>(
    app: &AppHandle,
    state: &AppState,
    op: impl Fn(&mut SessionManager) -> Result<T, String>,
) -> Result<T, String> {
    let (res, new_rx) = {
        let mut mgr = state.mgr.lock().unwrap();
        mgr.with_reattach(op)?
    };
    if let Some(rx) = new_rx {
        spawn_forwarder(app.clone(), rx);
        let _ = app.emit("cockpit:reconnected", ());
    }
    Ok(res)
}

#[tauri::command]
fn create_tab(
    app: AppHandle,
    state: State<'_, AppState>,
    name: Option<String>,
) -> Result<CreateTabResult, String> {
    with_reconnect(&app, &state, |mgr| mgr.create_tab(name.as_deref()))
}

#[tauri::command]
fn close_tab(
    app: AppHandle,
    state: State<'_, AppState>,
    tab_id: String,
    force: bool,
) -> Result<CloseTabResult, String> {
    with_reconnect(&app, &state, |mgr| mgr.close_tab(&tab_id, force))
}

#[tauri::command]
fn split_pane(
    app: AppHandle,
    state: State<'_, AppState>,
    pane_id: String,
    dir: String,
) -> Result<SplitPaneResult, String> {
    with_reconnect(&app, &state, |mgr| mgr.split_pane(&pane_id, &dir))
}

#[tauri::command]
fn close_pane(
    app: AppHandle,
    state: State<'_, AppState>,
    pane_id: String,
    mode: String,
) -> Result<(), String> {
    with_reconnect(&app, &state, |mgr| mgr.close_pane(&pane_id, &mode))
}

#[tauri::command]
fn launch_cc(
    state: State<'_, AppState>,
    pane_id: String,
    cwd: String,
    model: Option<String>,
    flags: Option<String>,
) -> Result<(), String> {
    let mut mgr = state.mgr.lock().unwrap();
    mgr.launch_cc(&pane_id, &cwd, model.as_deref(), flags.as_deref())
}

#[tauri::command]
fn launch_shell(state: State<'_, AppState>, pane_id: String, cwd: String) -> Result<(), String> {
    let mut mgr = state.mgr.lock().unwrap();
    mgr.launch_shell(&pane_id, &cwd)
}

/// Launch `claude --agent <name>` in a pane (P2-F4). The backend validates +
/// shell-quotes the agent name (security boundary), so a config-derived name
/// can't inject. Used by launch-from-inventory on a subagent row.
#[tauri::command]
fn launch_agent(
    state: State<'_, AppState>,
    pane_id: String,
    cwd: String,
    agent: String,
) -> Result<(), String> {
    let mut mgr = state.mgr.lock().unwrap();
    mgr.launch_agent(&pane_id, &cwd, &agent)
}

#[tauri::command]
fn pane_send_keys(state: State<'_, AppState>, pane_id: String, data: String) -> Result<(), String> {
    let mut mgr = state.mgr.lock().unwrap();
    mgr.pane_send_keys(&pane_id, &data)
}

#[tauri::command]
fn pane_resize(state: State<'_, AppState>, pane_id: String, cols: u16, rows: u16) -> Result<(), String> {
    let mut mgr = state.mgr.lock().unwrap();
    mgr.pane_resize(&pane_id, cols, rows)
}

/// Size the whole window to the grid bounding box + re-tile. The frontend grid
/// coordinator is the SINGLE authority for window size (one call per layout
/// change), replacing the per-pane client resize that collapsed multi-pane tabs.
#[tauri::command]
fn set_grid(state: State<'_, AppState>, cols: u16, rows: u16, layout: String) -> Result<(), String> {
    let mut mgr = state.mgr.lock().unwrap();
    mgr.set_grid(cols, rows, &layout)
}

#[tauri::command]
fn interrupt_pane(state: State<'_, AppState>, pane_id: String) -> Result<(), String> {
    let mut mgr = state.mgr.lock().unwrap();
    mgr.interrupt_pane(&pane_id)
}

#[tauri::command]
fn list_state(app: AppHandle, state: State<'_, AppState>) -> Result<CockpitState, String> {
    // Routed through reconnect: a passive refresh after the server died re-heals
    // the session and reloads, so the UI recovers even without a structural action.
    with_reconnect(&app, &state, |mgr| mgr.list_state())
}

/// Warm-start replay for one pane: return the pane's current screen + scrollback
/// (escape-aware) base64-encoded. The frontend calls this once on mount so a
/// re-attached pane paints its existing content instead of staying blank (the
/// control client only streams `%output` produced after it attaches).
#[tauri::command]
fn warm_start(state: State<'_, AppState>, pane_id: String) -> Result<WarmStartPayload, String> {
    let mgr = state.mgr.lock().unwrap();
    let bytes_b64 = mgr.warm_start(&pane_id)?;
    Ok(WarmStartPayload { bytes_b64 })
}

/// Visible-screen-only replay for the post-resize re-sync (no scrollback). A
/// shell leaves a trail of redrawn prompts in scrollback on resize; replaying the
/// full history surfaces that garble. The visible grid is clean (= what Ctrl+L
/// shows), so the re-sync replays just that.
#[tauri::command]
fn warm_start_screen(
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<WarmStartPayload, String> {
    let mgr = state.mgr.lock().unwrap();
    let bytes_b64 = mgr.warm_start_screen(&pane_id)?;
    Ok(WarmStartPayload { bytes_b64 })
}

/// Inventory mission-control (P2-F1): the unified read-only browser of skills,
/// subagents, plugins, and MCP servers across the global `~/.claude` scope and
/// (when `project_path` is the active tab's cwd) the per-project `.claude/`
/// scope. Pure config reads — no tmux, no SessionManager. SECURITY: never opens
/// `.env`, never emits MCP env values.
#[tauri::command]
fn load_inventory(
    project_path: Option<String>,
) -> Result<Vec<inventory::InventoryItem>, String> {
    inventory::load_inventory(project_path.as_deref())
}

/// Inventory toggle (P2-F2): enable/disable a plugin by DELEGATING to
/// `claude plugin enable|disable <key> --scope …`. We never hand-patch the
/// config — native CC owns that write. `id` is the inventory item id
/// (`plugin:<scope>:<name@marketplace>`). The frontend re-reads on success.
#[tauri::command]
fn toggle_plugin(id: String, enable: bool) -> Result<(), String> {
    inventory::toggle_plugin(&id, enable)
}

/// The exact `claude …` command a confirm modal shows before a toggle runs
/// (display only; the real exec uses a validated argv array, not this string).
#[tauri::command]
fn plugin_toggle_preview(id: String, enable: bool) -> Result<String, String> {
    inventory::plugin_toggle_preview(&id, enable)
}

/// Cross-project audit matrix (P2-F5): for each open tab's project root, the
/// effective on/off of every plugin + MCP server. Pure read (reuses the
/// inventory readers per project). `project_paths` = the open tabs' cwds.
#[tauri::command]
fn load_audit_matrix(project_paths: Vec<String>) -> Result<inventory::AuditMatrix, String> {
    inventory::load_audit_matrix(project_paths)
}

/// Cockpit team templates (P3 step 1): the saved **roster** (WHO) + **workflow**
/// (HOW) YAML artifacts under `~/.claude/cockpit/{teams,workflows}/` (global) and
/// `<project>/.claude/cockpit/...` (project). Pure read + validate; the loader is
/// fault-tolerant (a bad file = one row with `parseError`, never a blank panel).
#[tauri::command]
fn load_cockpit_templates(
    project_path: Option<String>,
) -> Result<templates::CockpitTemplates, String> {
    templates::load_cockpit_templates(project_path.as_deref())
}

/// Live team board (P3 step 3): READ-ONLY view of native Agent Teams sessions on
/// disk (`~/.claude/teams/session-*/` config + inboxes + tasks), newest first.
/// Pure read — writes nothing, spawns nothing; fault-tolerant to rotated dirs.
#[tauri::command]
fn load_team_runs() -> Result<Vec<teamruns::TeamRun>, String> {
    teamruns::load_team_runs()
}

/// Spin-up review (P3 step 2): pair a saved roster + workflow + task → the
/// generated lead prompt + role-coverage problems for the review dialog. Pure
/// read/compose — the actual launch (createTab + launch claude + send) is
/// orchestrated frontend-side, reusing the existing launch plumbing.
#[tauri::command]
fn spinup_preview(
    project_path: Option<String>,
    roster_id: String,
    workflow_id: String,
    task: String,
) -> Result<templates::SpinupPreview, String> {
    templates::spinup_preview(project_path.as_deref(), &roster_id, &workflow_id, &task)
}

/// File-tree sidebar (v1.1): the immediate children of one directory, filtered
/// (.gitignore always; dotfiles hidden unless `show_hidden`) and sorted
/// dirs-first. The tree expands lazily — one call per opened folder. Pure read.
#[tauri::command]
fn list_dir(path: String, show_hidden: bool) -> Result<Vec<filetree::FileEntry>, String> {
    filetree::list_dir(&path, show_hidden)
}

/// File-tree root probe (v1.1): a tmux pane's current working dir. The tree
/// follows the active pane — re-roots here when the active tab/pane changes or a
/// shell pane `cd`s. `pane_id` is validated to `%<n>` at the boundary.
#[tauri::command]
fn pane_cwd(pane_id: String) -> Result<String, String> {
    filetree::active_pane_cwd(&pane_id)
}

/// File-tree (v1.1): a pane's current command, so a double-click / Attach-to-Agent
/// can pick the insert format (claude → `@path`, shell → raw path).
#[tauri::command]
fn pane_command(pane_id: String) -> Result<String, String> {
    filetree::pane_command(&pane_id)
}

/// File-tree right-click "Reveal in Finder" (v1.1): `open -R <path>`. Path passed
/// as its own argv element (no shell); existence verified first.
#[tauri::command]
fn reveal_in_finder(path: String) -> Result<(), String> {
    filetree::reveal_in_finder(&path)
}

/// File-tree New File / New Folder (v1.1): create `name` under `parent`. `name`
/// is validated to a single safe path segment (no traversal); never clobbers.
#[tauri::command]
fn create_entry(parent: String, name: String, is_dir: bool) -> Result<String, String> {
    filetree::create_entry(&parent, &name, is_dir)
}

/// File-tree Delete (v1.1): move a path to the macOS Trash (recoverable) — never
/// an unlink. Existence verified first.
#[tauri::command]
fn trash_path(path: String) -> Result<(), String> {
    filetree::trash_path(&path)
}

/// File-tree live watch (v1.1): set the exact dirs watched for changes (the
/// sidebar root + expanded folders), each non-recursive. Emits `filetree:changed`
/// on any change; an empty list unwatches everything (sidebar hidden).
#[tauri::command]
fn watch_dirs(app: AppHandle, dirs: Vec<String>) -> Result<(), String> {
    filetree::set_watched(&app, dirs)
}

/// "Send pane → new tab" (v1.1): break a pane out into its own new window/tab,
/// keeping it running. Returns the new window id so the frontend can switch to it.
#[tauri::command]
fn break_pane(
    app: AppHandle,
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<String, String> {
    with_reconnect(&app, &state, |mgr| mgr.break_pane(&pane_id))
}

// ── Background tasks ─────────────────────────────────────────────────────────

/// Forward engine `Outbound` -> Tauri events, with output coalescing.
///
/// Per-pane `%output` is buffered (decoded bytes appended) and flushed at most
/// once per `COALESCE_MS` as a SINGLE `pane:data` per pane. Under heavy output
/// this collapses thousands of tiny IPC emits per second into ~60/pane/s,
/// killing the UI lag (D3 spike named 16ms coalescing as the v1 mitigation).
///
/// Ordering is preserved: a `Topology`/`Exit` event flushes the pending pane
/// buffers FIRST, so output that arrived before a topology change is delivered
/// before it. Runs until the channel disconnects.
fn spawn_forwarder(app: AppHandle, rx: std::sync::mpsc::Receiver<Outbound>) {
    std::thread::spawn(move || {
        // Pending per-pane bytes, accumulated between flushes. Insertion order is
        // not load-bearing (each pane's xterm is independent); within a pane the
        // appended Vec preserves byte order.
        let mut pending: HashMap<String, Vec<u8>> = HashMap::new();

        loop {
            match rx.recv_timeout(Duration::from_millis(COALESCE_MS)) {
                Ok(Outbound::PaneData { pane_id, bytes_b64 }) => {
                    // Decode here and re-encode on flush; concatenating decoded
                    // bytes is the only correct way to merge base64 chunks.
                    match B64.decode(bytes_b64.as_bytes()) {
                        Ok(bytes) => pending.entry(pane_id).or_default().extend_from_slice(&bytes),
                        // Should not happen (engine produces it), but never drop
                        // the frame on a decode error — emit it standalone.
                        Err(_) => {
                            let _ = app.emit("pane:data", PaneDataPayload { pane_id, bytes_b64 });
                        }
                    }
                }
                Ok(Outbound::Topology(t)) => {
                    flush_pending(&app, &mut pending);
                    let _ = app.emit("pane:topology", topology_payload(t));
                }
                Ok(Outbound::Exit { reason }) => {
                    flush_pending(&app, &mut pending);
                    let _ = app.emit(
                        "pane:topology",
                        PaneTopologyPayload {
                            kind: "exit".into(),
                            tab_id: None,
                            window_id: None,
                            pane_id: None,
                            layout: reason,
                        },
                    );
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Tick boundary: emit one coalesced `pane:data` per pane.
                    flush_pending(&app, &mut pending);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    flush_pending(&app, &mut pending);
                    break;
                }
            }
        }
    });
}

/// Drain `pending`, emitting one coalesced `pane:data` per pane with buffered
/// bytes. No-op for empty buffers. Clears the map.
fn flush_pending(app: &AppHandle, pending: &mut HashMap<String, Vec<u8>>) {
    if pending.is_empty() {
        return;
    }
    for (pane_id, bytes) in pending.drain() {
        if bytes.is_empty() {
            continue;
        }
        let _ = app.emit(
            "pane:data",
            PaneDataPayload {
                pane_id,
                bytes_b64: B64.encode(&bytes),
            },
        );
    }
}

fn topology_payload(t: TopologyEvent) -> PaneTopologyPayload {
    let base = PaneTopologyPayload {
        kind: String::new(),
        tab_id: None,
        window_id: None,
        pane_id: None,
        layout: None,
    };
    match t {
        TopologyEvent::WindowAdd { window_id } => PaneTopologyPayload {
            kind: "windowAdd".into(),
            window_id: Some(window_id),
            ..base
        },
        TopologyEvent::WindowClose { window_id } => PaneTopologyPayload {
            kind: "windowClose".into(),
            window_id: Some(window_id),
            ..base
        },
        TopologyEvent::LayoutChange { window_id, layout } => PaneTopologyPayload {
            kind: "layoutChange".into(),
            window_id: Some(window_id),
            layout: Some(layout),
            ..base
        },
        TopologyEvent::ActivePaneChanged { window_id, pane_id } => PaneTopologyPayload {
            kind: "activePaneChanged".into(),
            window_id: Some(window_id),
            pane_id: Some(pane_id),
            ..base
        },
        TopologyEvent::PaneModeChanged { pane_id } => PaneTopologyPayload {
            kind: "paneModeChanged".into(),
            pane_id: Some(pane_id),
            ..base
        },
    }
}

/// ~1Hz status poller: classify changed panes, emit `pane:status` on change.
fn spawn_status_poller(app: AppHandle, mgr: Arc<Mutex<SessionManager>>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(1000));
        let changed = {
            let mut m = mgr.lock().unwrap();
            if !m.is_attached() {
                break;
            }
            m.poll_statuses()
        };
        for payload in changed {
            let _ = app.emit("pane:status", payload);
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            cockpit_init,
            create_tab,
            close_tab,
            split_pane,
            close_pane,
            launch_cc,
            launch_shell,
            launch_agent,
            pane_send_keys,
            pane_resize,
            set_grid,
            interrupt_pane,
            list_state,
            warm_start,
            warm_start_screen,
            load_inventory,
            toggle_plugin,
            plugin_toggle_preview,
            load_audit_matrix,
            load_cockpit_templates,
            load_team_runs,
            spinup_preview,
            list_dir,
            pane_cwd,
            pane_command,
            reveal_in_finder,
            create_entry,
            trash_path,
            watch_dirs,
            break_pane,
            persist::save_layout,
            persist::load_layout,
            gitstatus::git_status_snapshot,
        ])
        .on_window_event(|window, event| {
            // ⌘W (and the red close button) fire CloseRequested, which would close
            // the whole window — the user's whole cockpit — on a single keystroke.
            // Redirect it: DON'T close the window; tell the frontend to close the
            // focused pane (or the active tab if it's that pane's last one). The app
            // quits only via ⌘Q. The tmux session survives regardless, so even a
            // real quit loses nothing — `cockpit_init` re-attaches on next launch.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.emit("cockpit:close-requested", ());
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

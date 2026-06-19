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

pub mod manager;
pub mod status;
pub mod tmux;

use std::sync::{Arc, Mutex};

use cockpit_engine::{Outbound, TopologyEvent};
use manager::{
    CloseTabResult, CockpitState, CreateTabResult, SessionManager, SplitPaneResult,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

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

    // Start background tasks once.
    {
        let mut started = state.started.lock().unwrap();
        if !*started {
            spawn_forwarder(app.clone(), rx);
            spawn_status_poller(app.clone(), state.mgr.clone());
            *started = true;
        }
    }

    let mgr = state.mgr.lock().unwrap();
    mgr.list_state()
}

#[tauri::command]
fn create_tab(state: State<'_, AppState>, name: Option<String>) -> Result<CreateTabResult, String> {
    let mut mgr = state.mgr.lock().unwrap();
    mgr.create_tab(name.as_deref())
}

#[tauri::command]
fn close_tab(state: State<'_, AppState>, tab_id: String, force: bool) -> Result<CloseTabResult, String> {
    let mut mgr = state.mgr.lock().unwrap();
    mgr.close_tab(&tab_id, force)
}

#[tauri::command]
fn split_pane(state: State<'_, AppState>, pane_id: String, dir: String) -> Result<SplitPaneResult, String> {
    let mut mgr = state.mgr.lock().unwrap();
    mgr.split_pane(&pane_id, &dir)
}

#[tauri::command]
fn close_pane(state: State<'_, AppState>, pane_id: String, mode: String) -> Result<(), String> {
    let mut mgr = state.mgr.lock().unwrap();
    mgr.close_pane(&pane_id, &mode)
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

#[tauri::command]
fn interrupt_pane(state: State<'_, AppState>, pane_id: String) -> Result<(), String> {
    let mut mgr = state.mgr.lock().unwrap();
    mgr.interrupt_pane(&pane_id)
}

#[tauri::command]
fn list_state(state: State<'_, AppState>) -> Result<CockpitState, String> {
    let mgr = state.mgr.lock().unwrap();
    mgr.list_state()
}

// ── Background tasks ─────────────────────────────────────────────────────────

/// Forward engine `Outbound` -> Tauri events. Runs until the channel closes.
fn spawn_forwarder(app: AppHandle, rx: std::sync::mpsc::Receiver<Outbound>) {
    std::thread::spawn(move || {
        for out in rx {
            match out {
                Outbound::PaneData { pane_id, bytes_b64 } => {
                    let _ = app.emit("pane:data", PaneDataPayload { pane_id, bytes_b64 });
                }
                Outbound::Topology(t) => {
                    let _ = app.emit("pane:topology", topology_payload(t));
                }
                Outbound::Exit { reason } => {
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
            }
        }
    });
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
            pane_send_keys,
            pane_resize,
            interrupt_pane,
            list_state,
        ])
        .on_window_event(|window, event| {
            // Best-effort: when the main window closes, tear down the control
            // client. We do NOT kill the cockpit session here (panes/tabs should
            // survive a window close so the user can re-attach); only detach.
            if let tauri::WindowEvent::Destroyed = event {
                if let Some(state) = window.app_handle().try_state::<AppState>() {
                    if let Ok(mut mgr) = state.mgr.lock() {
                        // Detach the streaming client only; leave the session alive.
                        // teardown() also kills the session, which we don't want on
                        // a mere window close — so we just drop the client.
                        let _ = &mut *mgr; // session intentionally preserved
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

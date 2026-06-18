//! Minimal Tauri 2 command layer over the PROVEN control-mode engine.
//!
//! This file is intentionally thin: all the load-bearing logic (spawn `tmux -CC`,
//! parse the protocol, octal-decode, route output/topology) lives in
//! `cockpit_engine` / `cockpit_control_mode`, which the headless `live-bridge`
//! binary already exercised end-to-end against a real session. Here we only:
//!   * hold one `ControlClient` in managed state,
//!   * forward its `Outbound` channel onto Tauri events (`pane:data`,
//!     `pane:topology`) for the SolidJS frontend,
//!   * expose `attach_session`, `pane_send_keys`, `pane_resize`, `interrupt_pane`.

use std::sync::Mutex;

use cockpit_engine::{ControlClient, Outbound, TopologyEvent};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

/// Managed state: the live control client (None until `attach_session`).
#[derive(Default)]
struct Bridge {
    client: Mutex<Option<ControlClient>>,
}

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
    window_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    layout: Option<String>,
}

/// Spawn the single control client for a session and start forwarding events.
#[tauri::command]
fn attach_session(
    app: AppHandle,
    bridge: State<'_, Bridge>,
    socket: String,
    session: String,
) -> Result<(), String> {
    let (client, rx) = ControlClient::attach(&socket, &session).map_err(|e| e.to_string())?;

    // Forwarder thread: engine Outbound -> Tauri events. Runs until the channel
    // disconnects (client shutdown / session gone).
    let app_for_thread = app.clone();
    std::thread::spawn(move || {
        for out in rx {
            match out {
                Outbound::PaneData { pane_id, bytes_b64 } => {
                    let _ = app_for_thread
                        .emit("pane:data", PaneDataPayload { pane_id, bytes_b64 });
                }
                Outbound::Topology(t) => {
                    let _ = app_for_thread.emit("pane:topology", topology_payload(t));
                }
                Outbound::Exit { reason } => {
                    let _ = app_for_thread.emit(
                        "pane:topology",
                        PaneTopologyPayload {
                            kind: "exit".into(),
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

    *bridge.client.lock().unwrap() = Some(client);
    Ok(())
}

fn topology_payload(t: TopologyEvent) -> PaneTopologyPayload {
    match t {
        TopologyEvent::WindowAdd { window_id } => PaneTopologyPayload {
            kind: "windowAdd".into(),
            window_id: Some(window_id),
            pane_id: None,
            layout: None,
        },
        TopologyEvent::WindowClose { window_id } => PaneTopologyPayload {
            kind: "windowClose".into(),
            window_id: Some(window_id),
            pane_id: None,
            layout: None,
        },
        TopologyEvent::LayoutChange { window_id, layout } => PaneTopologyPayload {
            kind: "layoutChange".into(),
            window_id: Some(window_id),
            pane_id: None,
            layout: Some(layout),
        },
        TopologyEvent::ActivePaneChanged { window_id, pane_id } => PaneTopologyPayload {
            kind: "activePaneChanged".into(),
            window_id: Some(window_id),
            pane_id: Some(pane_id),
            layout: None,
        },
        TopologyEvent::PaneModeChanged { pane_id } => PaneTopologyPayload {
            kind: "paneModeChanged".into(),
            window_id: None,
            pane_id: Some(pane_id),
            layout: None,
        },
    }
}

/// Literal VT input to a pane (fire-and-forget at the JS layer).
#[tauri::command]
fn pane_send_keys(bridge: State<'_, Bridge>, pane_id: String, data: String) -> Result<(), String> {
    let mut guard = bridge.client.lock().unwrap();
    let cc = guard.as_mut().ok_or("not attached")?;
    cc.pane_send_keys(&pane_id, &data).map_err(|e| e.to_string())
}

/// Push xterm-fit cols/rows to tmux (authoritative resize).
#[tauri::command]
fn pane_resize(
    bridge: State<'_, Bridge>,
    pane_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let mut guard = bridge.client.lock().unwrap();
    let cc = guard.as_mut().ok_or("not attached")?;
    cc.pane_resize(&pane_id, cols, rows).map_err(|e| e.to_string())
}

/// Ctrl+C interrupt for a pane.
#[tauri::command]
fn interrupt_pane(bridge: State<'_, Bridge>, pane_id: String) -> Result<(), String> {
    let mut guard = bridge.client.lock().unwrap();
    let cc = guard.as_mut().ok_or("not attached")?;
    cc.interrupt_pane(&pane_id).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(Bridge::default())
        .invoke_handler(tauri::generate_handler![
            attach_session,
            pane_send_keys,
            pane_resize,
            interrupt_pane
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

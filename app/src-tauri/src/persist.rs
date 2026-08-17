//! Disk-persisted UI layout (Terax Tier-1 C/dev#1).
//!
//! The frontend owns tab order, per-tab cwd, and tab-name overrides; this module
//! just durably snapshots that to `<app_config_dir>/cockpit/layout.json` so a
//! restart restores tab titles + the active tab. tmux itself already survives a
//! restart (the session lives on `-L cockpit`), so this is purely the *UI* layer
//! the engine can't reconstruct (custom titles, active selection).
//!
//! Write is crash-safe: serialize to `layout.json.tmp` then atomic `rename` over
//! `layout.json`, so a kill mid-write never leaves a half-written file. Read is
//! best-effort: a missing file is `Ok(None)` (first run — never an error), and the
//! caller treats any error as "no restore" rather than blocking boot.
//!
//! `schemaVersion` is always `1` (Terax omits versioning; we keep it so a future
//! format change can migrate instead of mis-parsing). The persisted file's
//! version is forced to `SCHEMA_VERSION` on every write regardless of input.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// Current on-disk layout schema version. Bump only with a migration path.
const SCHEMA_VERSION: u32 = 1;

/// One persisted tab: position, its first-pane working dir (the match key on
/// restore), and an optional user-set title override.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TabLayout {
    /// 0-based position in the tab strip.
    pub index: u32,
    /// First-pane cwd — legacy match key, used only when `window_id` is absent.
    pub cwd: String,
    /// tmux window id (`@<n>`) — the STABLE key a restore matches on. Window
    /// INDEXES are recycled by tmux, so matching on index made a new tab inherit
    /// a closed tab's title. Absent in snapshots written before this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    /// User-renamed title, if any. Absent ⇒ engine/default name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
}

/// Full UI layout snapshot persisted between sessions.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LayoutSnapshot {
    /// On-disk schema version (always `1` once written; see `SCHEMA_VERSION`).
    pub schema_version: u32,
    /// The tab that was active at save time, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tab_id: Option<String>,
    /// Tabs in strip order.
    pub tabs: Vec<TabLayout>,
}

/// `<app_config_dir>/cockpit` — the dir holding `layout.json`. Created on save.
fn cockpit_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("resolve app config dir: {e}"))?;
    Ok(base.join("cockpit"))
}

/// Persist the layout snapshot atomically. Forces `schemaVersion = SCHEMA_VERSION`
/// on write, creates the cockpit dir if needed, writes `layout.json.tmp`, then
/// atomically renames it over `layout.json`.
#[tauri::command]
pub fn save_layout(app: AppHandle, snapshot: LayoutSnapshot) -> Result<(), String> {
    let dir = cockpit_dir(&app)?;
    fs::create_dir_all(&dir).map_err(|e| format!("create cockpit dir: {e}"))?;

    let snapshot = LayoutSnapshot {
        schema_version: SCHEMA_VERSION,
        ..snapshot
    };
    let json =
        serde_json::to_string_pretty(&snapshot).map_err(|e| format!("serialize layout: {e}"))?;

    let final_path = dir.join("layout.json");
    let tmp_path = dir.join("layout.json.tmp");
    fs::write(&tmp_path, json.as_bytes()).map_err(|e| format!("write tmp layout: {e}"))?;
    fs::rename(&tmp_path, &final_path).map_err(|e| format!("rename layout: {e}"))?;
    Ok(())
}

/// Load the persisted layout. `Ok(None)` when the file is absent (first run) —
/// never an error. A corrupt/unparseable file is a real `Err` the caller can
/// choose to ignore (best-effort restore).
#[tauri::command]
pub fn load_layout(app: AppHandle) -> Result<Option<LayoutSnapshot>, String> {
    let path = cockpit_dir(&app)?.join("layout.json");
    match fs::read_to_string(&path) {
        Ok(s) => {
            let snap = serde_json::from_str(&s).map_err(|e| format!("parse layout: {e}"))?;
            Ok(Some(snap))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read layout: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_full() {
        let snap = LayoutSnapshot {
            schema_version: 1,
            active_tab_id: Some("tab-2".into()),
            tabs: vec![
                TabLayout {
                    index: 0,
                    cwd: "/repo/a".into(),
                    window_id: Some("@4".into()),
                    custom_title: Some("Alpha".into()),
                },
                TabLayout {
                    index: 1,
                    cwd: "/repo/b".into(),
                    window_id: Some("@9".into()),
                    custom_title: None,
                },
            ],
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: LayoutSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }

    #[test]
    fn round_trip_empty() {
        let snap = LayoutSnapshot {
            schema_version: 1,
            active_tab_id: None,
            tabs: vec![],
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: LayoutSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }

    #[test]
    fn serializes_camel_case_keys() {
        let snap = LayoutSnapshot {
            schema_version: 1,
            active_tab_id: Some("t".into()),
            tabs: vec![TabLayout {
                index: 0,
                cwd: "/x".into(),
                window_id: Some("@1".into()),
                custom_title: Some("T".into()),
            }],
        };
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("\"schemaVersion\""), "json: {json}");
        assert!(json.contains("\"activeTabId\""), "json: {json}");
        assert!(json.contains("\"customTitle\""), "json: {json}");
        assert!(json.contains("\"windowId\""), "json: {json}");
    }

    #[test]
    fn none_options_omitted_then_default_back_to_none() {
        // A persisted tab without a customTitle must round-trip to None even
        // though the key is absent from the JSON.
        let json = r#"{"schemaVersion":1,"tabs":[{"index":0,"cwd":"/x"}]}"#;
        let snap: LayoutSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snap.active_tab_id, None);
        assert_eq!(snap.tabs[0].custom_title, None);
    }

    #[test]
    fn legacy_snapshot_without_window_id_still_loads() {
        // Files written before `windowId` existed must keep loading (the frontend
        // falls back to the old (index, cwd) match for exactly these rows).
        let json = r#"{"schemaVersion":1,"tabs":[{"index":2,"cwd":"/x","customTitle":"Zorik"}]}"#;
        let snap: LayoutSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snap.tabs[0].window_id, None);
        assert_eq!(snap.tabs[0].custom_title.as_deref(), Some("Zorik"));
    }
}

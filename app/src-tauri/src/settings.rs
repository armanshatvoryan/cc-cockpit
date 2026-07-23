//! Disk-persisted user settings (`<app_config_dir>/cockpit/settings.json`).
//!
//! Deliberately a SIBLING of `layout.json`, not a field inside it: layout has
//! its own schema version and tab-match semantics, and a user preference has no
//! business sharing a migration path with reconstructable window state.
//!
//! Same durability contract as `persist.rs`: write is crash-safe (serialize to
//! `settings.json.tmp`, then atomic `rename`), read is best-effort (a missing
//! file is `Ok(None)` — first run, never an error).
//!
//! Today this holds exactly one preference — `defaultCwd`, the directory new
//! tabs and the bootstrap session start in. Absent ⇒ the built-in fallback
//! chain in `tmux::default_cwd` (`$HOME/Workflows` → `$HOME` → `/`), which is
//! why a fresh install on a machine without `~/Workflows` still behaves.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// Current on-disk settings schema version. Bump only with a migration path.
const SCHEMA_VERSION: u32 = 1;

/// User preferences persisted between launches.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CockpitSettings {
    /// On-disk schema version (always `SCHEMA_VERSION` once written).
    #[serde(default)]
    pub schema_version: u32,
    /// Absolute start directory for new tabs/panes. `None` ⇒ use the built-in
    /// fallback chain. A path that no longer exists is NOT an error here — the
    /// `is_dir` gate in `tmux::default_cwd` degrades it to `$HOME` at use time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cwd: Option<String>,
}

/// `<app_config_dir>/cockpit` — the dir holding `settings.json`. Created on save.
fn cockpit_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("resolve app config dir: {e}"))?;
    Ok(base.join("cockpit"))
}

/// Read settings from an explicit cockpit dir. `Ok(None)` when absent (first
/// run). A corrupt file is a real `Err` so the caller can surface it rather than
/// silently resetting the user's preferences.
///
/// Split out from `read_settings` so the disk→struct path is testable without a
/// Tauri `AppHandle` (which only exists inside a running app).
pub fn read_settings_in(dir: &std::path::Path) -> Result<Option<CockpitSettings>, String> {
    match fs::read_to_string(dir.join("settings.json")) {
        Ok(s) => {
            let parsed = serde_json::from_str(&s).map_err(|e| format!("parse settings: {e}"))?;
            Ok(Some(parsed))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read settings: {e}")),
    }
}

/// Read settings from the app's config dir.
pub fn read_settings(app: &AppHandle) -> Result<Option<CockpitSettings>, String> {
    read_settings_in(&cockpit_dir(app)?)
}

/// Load settings for the frontend. Missing file ⇒ defaults, not an error, so the
/// settings dialog always opens on first run.
#[tauri::command]
pub fn load_settings(app: AppHandle) -> Result<CockpitSettings, String> {
    Ok(read_settings(&app)?.unwrap_or_default())
}

/// The directory a new tab would actually open in right now, after the whole
/// fallback chain. The dialog shows this next to the configured value so a
/// folder that was deleted since it was picked is visible rather than silent.
#[tauri::command]
pub fn effective_default_cwd() -> String {
    crate::tmux::default_cwd()
}

/// Persist settings atomically AND apply them to the live process, so the next
/// `create_tab` uses the new directory without a restart. Returns the directory
/// that will actually be used (post-`is_dir`-gate), which is what the dialog
/// shows back to the user — a path that silently falls back must be visible.
#[tauri::command]
pub fn save_settings(app: AppHandle, settings: CockpitSettings) -> Result<String, String> {
    let dir = cockpit_dir(&app)?;
    fs::create_dir_all(&dir).map_err(|e| format!("create cockpit dir: {e}"))?;

    let settings = CockpitSettings {
        schema_version: SCHEMA_VERSION,
        ..settings
    };
    let json =
        serde_json::to_string_pretty(&settings).map_err(|e| format!("serialize settings: {e}"))?;

    let final_path = dir.join("settings.json");
    let tmp_path = dir.join("settings.json.tmp");
    fs::write(&tmp_path, json.as_bytes()).map_err(|e| format!("write tmp settings: {e}"))?;
    fs::rename(&tmp_path, &final_path).map_err(|e| format!("rename settings: {e}"))?;

    crate::tmux::set_configured_cwd(settings.default_cwd.clone());
    Ok(crate::tmux::default_cwd())
}

/// Load persisted settings into process state at startup. Best-effort: a
/// corrupt or unreadable file must never block boot — the cockpit just starts
/// with the built-in default directory.
pub fn apply_at_startup(app: &AppHandle) {
    let configured = read_settings(app)
        .ok()
        .flatten()
        .and_then(|s| s.default_cwd);
    crate::tmux::set_configured_cwd(configured);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_path() {
        let s = CockpitSettings {
            schema_version: 1,
            default_cwd: Some("/Users/u/Projects".into()),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: CockpitSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn round_trip_unset() {
        let s = CockpitSettings {
            schema_version: 1,
            default_cwd: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            !json.contains("defaultCwd"),
            "unset path must be omitted, got: {json}"
        );
        let back: CockpitSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn serializes_camel_case_keys() {
        let s = CockpitSettings {
            schema_version: 1,
            default_cwd: Some("/x".into()),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"schemaVersion\""), "json: {json}");
        assert!(json.contains("\"defaultCwd\""), "json: {json}");
    }

    /// Real files, real IO — the three states `apply_at_startup` can encounter
    /// on a user's disk. Uses a uniquely-named temp dir so it can't collide with
    /// a parallel test or the developer's actual config.
    #[test]
    fn reads_the_three_real_disk_states() {
        let dir = std::env::temp_dir().join("cc-cockpit-settings-test-4b71");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // 1. Absent — first run. Not an error.
        assert_eq!(read_settings_in(&dir).unwrap(), None);

        // 2. Present and valid.
        fs::write(
            dir.join("settings.json"),
            r#"{"schemaVersion":1,"defaultCwd":"/Users/u/Code"}"#,
        )
        .unwrap();
        assert_eq!(
            read_settings_in(&dir).unwrap().unwrap().default_cwd,
            Some("/Users/u/Code".into())
        );

        // 3. Corrupt — a real error, so a truncated write is visible rather than
        //    silently resetting the user's preference to the default.
        fs::write(dir.join("settings.json"), "{not json").unwrap();
        assert!(read_settings_in(&dir).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_file_written_by_an_older_build() {
        // No schemaVersion, no defaultCwd — must not fail, must mean "unset".
        let s: CockpitSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.default_cwd, None);
        assert_eq!(s.schema_version, 0);
    }
}

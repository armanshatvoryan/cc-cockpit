//! First-run onboarding: environment probe + one-click prereq installs.
//!
//! Security invariant: NO user input ever reaches a command line. The only
//! installable things are the variants of `PrereqTool` (Task 4), each mapping
//! to a hardcoded argv; serde rejects anything else at the IPC boundary.
//!
//! Probing uses `zsh -lc` for the same reason as the PATH capture in
//! `lib.rs`: a Finder-launched app has a stripped PATH, and the login shell
//! sources /etc/zprofile + ~/.zprofile (Homebrew shellenv) — where tmux and
//! brew actually live on a normal setup.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

/// Minimum tmux version the cockpit supports.
const MIN_TMUX: (u32, u32) = (3, 3);

/// Presence/health of one probed tool.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub found: bool,
    /// Raw version line (e.g. `"tmux 3.4"`), `None` when not found.
    pub version: Option<String>,
    /// tmux: found AND version >= 3.3. claude: same as `found` (presence-only).
    pub ok: bool,
}

/// Everything Step 1 of the wizard renders.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrereqReport {
    pub tmux: ToolStatus,
    pub claude: ToolStatus,
    pub brew: bool,
    pub npm: bool,
}

/// `"tmux 3.4"` → `(3, 4)`; `"tmux 3.3a"` → `(3, 3)`; `"tmux next-3.6"` →
/// `(3, 6)`; garbage → `None`.
fn parse_tmux_version(s: &str) -> Option<(u32, u32)> {
    let rest = s.trim().strip_prefix("tmux ").unwrap_or_else(|| s.trim());
    let rest = rest.strip_prefix("next-").unwrap_or(rest);
    let digits = |p: &str| -> Option<u32> {
        let d: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
        d.parse().ok()
    };
    let mut parts = rest.split('.');
    let major = digits(parts.next()?)?;
    let minor = parts.next().and_then(digits).unwrap_or(0);
    Some((major, minor))
}

/// Parse the sentinel-keyed probe output (Task 3 emits `CC_TMUX=` etc.). A
/// login shell can print rc noise, so only lines starting with a known key
/// count, and the LAST occurrence wins. An empty value ⇒ tool not found.
fn parse_probe_output(out: &str) -> PrereqReport {
    let mut tmux_v = String::new();
    let mut claude_v = String::new();
    let mut brew = String::new();
    let mut npm = String::new();
    for line in out.lines() {
        if let Some(v) = line.strip_prefix("CC_TMUX=") {
            tmux_v = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("CC_CLAUDE=") {
            claude_v = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("CC_BREW=") {
            brew = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("CC_NPM=") {
            npm = v.trim().to_string();
        }
    }
    PrereqReport {
        tmux: ToolStatus {
            found: !tmux_v.is_empty(),
            ok: parse_tmux_version(&tmux_v).is_some_and(|v| v >= MIN_TMUX),
            version: (!tmux_v.is_empty()).then(|| tmux_v.clone()),
        },
        claude: ToolStatus {
            found: !claude_v.is_empty(),
            ok: !claude_v.is_empty(),
            version: (!claude_v.is_empty()).then(|| claude_v.clone()),
        },
        brew: !brew.is_empty(),
        npm: !npm.is_empty(),
    }
}

/// The only things the wizard can install. `install_prereq` takes this enum —
/// never a string — so no user-controlled text can reach a shell. Serde
/// (kebab-case) rejects unknown values at the IPC boundary.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum PrereqTool {
    Tmux,
    ClaudeCli,
}

impl PrereqTool {
    /// Hardcoded install command. `2>&1` merges stderr into the streamed log
    /// so a single reader thread sees everything in order.
    fn install_script(self) -> &'static str {
        match self {
            PrereqTool::Tmux => "brew install tmux 2>&1",
            PrereqTool::ClaudeCli => "npm install -g @anthropic-ai/claude-code 2>&1",
        }
    }
}

/// Process-group id of the running install, if any. One install at a time —
/// the wizard disables the other Install button while this is `Some`.
#[derive(Default)]
pub struct InstallGuard(pub Mutex<Option<u32>>);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallLine {
    tool: PrereqTool,
    line: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallDone {
    tool: PrereqTool,
    exit_code: i32,
}

/// One login-shell round trip probing all four tools. `2>/dev/null` per tool:
/// a missing binary prints an empty value instead of shell noise.
const PROBE_SCRIPT: &str = "printf 'CC_TMUX=%s\\n' \"$(tmux -V 2>/dev/null)\"; \
printf 'CC_CLAUDE=%s\\n' \"$(claude --version 2>/dev/null)\"; \
printf 'CC_BREW=%s\\n' \"$(command -v brew 2>/dev/null)\"; \
printf 'CC_NPM=%s\\n' \"$(command -v npm 2>/dev/null)\"";

/// Probe the login-shell environment for the cockpit's prerequisites. Async so
/// the (up to ~2 s — `claude --version` pays node startup) probe never blocks
/// the async runtime; wrapped in `spawn_blocking` to avoid hogging a worker thread.
/// The wizard shows a spinner meanwhile.
#[tauri::command]
pub async fn check_prereqs() -> Result<PrereqReport, String> {
    let out = tauri::async_runtime::spawn_blocking(|| {
        Command::new("zsh").args(["-lc", PROBE_SCRIPT]).output()
    })
    .await
    .map_err(|e| format!("probe task: {e}"))?
    .map_err(|e| format!("spawn zsh probe: {e}"))?;
    Ok(parse_probe_output(&String::from_utf8_lossy(&out.stdout)))
}

/// Start an install. Returns immediately; output streams via
/// `onboarding:install-line` and completion via `onboarding:install-done`.
#[tauri::command]
pub fn install_prereq(
    app: AppHandle,
    guard: State<InstallGuard>,
    tool: PrereqTool,
) -> Result<(), String> {
    let mut running = guard.0.lock().unwrap();
    if running.is_some() {
        return Err("an install is already running".into());
    }

    use std::os::unix::process::CommandExt;
    let mut child = Command::new("zsh")
        .args(["-lc", tool.install_script()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null()) // merged into stdout by the script's 2>&1
        .process_group(0) // own group ⇒ cancel kills brew/npm's children too
        .spawn()
        .map_err(|e| format!("spawn installer: {e}"))?;
    // With process_group(0) the child's pid IS its pgid.
    *running = Some(child.id());
    drop(running);

    let stdout = child.stdout.take().expect("stdout was piped");
    let app2 = app.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let _ = app2.emit("onboarding:install-line", InstallLine { tool, line });
        }
        // Killed-by-signal (cancel) has no exit code — report -1, non-zero.
        let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
        *app2.state::<InstallGuard>().0.lock().unwrap() = None;
        let _ = app2.emit("onboarding:install-done", InstallDone { tool, exit_code: code });
    });
    Ok(())
}

/// Kill the whole install process group (zsh + brew/npm + their children).
/// SIGTERM first; if the group is still alive after a short grace, SIGKILL —
/// a child that ignores TERM must not outlive the app or wedge the
/// one-install guard. Best-effort: the reader thread sees EOF and emits
/// `install-done` itself. `/bin/kill` with a negative pgid avoids a libc
/// dependency.
pub fn kill_running_install(guard: &InstallGuard) {
    let pgid = *guard.0.lock().unwrap();
    let Some(pgid) = pgid else { return };
    let group = format!("-{pgid}");
    let _ = Command::new("/bin/kill").args(["-TERM", &group]).status();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    // `kill -0` probes liveness without sending a signal.
    let alive = Command::new("/bin/kill")
        .args(["-0", &group])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if alive {
        let _ = Command::new("/bin/kill").args(["-KILL", &group]).status();
    }
}

/// Frontend cancel button / wizard-close hook.
#[tauri::command]
pub fn cancel_install(guard: State<InstallGuard>) {
    kill_running_install(&guard);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_enum_rejects_everything_but_the_two_variants() {
        assert_eq!(
            serde_json::from_str::<PrereqTool>("\"tmux\"").unwrap(),
            PrereqTool::Tmux
        );
        assert_eq!(
            serde_json::from_str::<PrereqTool>("\"claude-cli\"").unwrap(),
            PrereqTool::ClaudeCli
        );
        assert!(serde_json::from_str::<PrereqTool>("\"tmux; rm -rf /\"").is_err());
        assert!(serde_json::from_str::<PrereqTool>("\"brew\"").is_err());
        assert!(serde_json::from_str::<PrereqTool>("\"\"").is_err());
    }

    #[test]
    fn install_scripts_are_exactly_the_two_known_commands() {
        assert_eq!(PrereqTool::Tmux.install_script(), "brew install tmux 2>&1");
        assert_eq!(
            PrereqTool::ClaudeCli.install_script(),
            "npm install -g @anthropic-ai/claude-code 2>&1"
        );
    }

    #[test]
    fn tmux_version_parse() {
        assert_eq!(parse_tmux_version("tmux 3.4"), Some((3, 4)));
        assert_eq!(parse_tmux_version("tmux 3.3a"), Some((3, 3)));
        assert_eq!(parse_tmux_version("tmux next-3.6"), Some((3, 6)));
        assert_eq!(parse_tmux_version("tmux 3.2"), Some((3, 2)));
        assert_eq!(parse_tmux_version(""), None);
        assert_eq!(parse_tmux_version("command not found"), None);
    }

    #[test]
    fn tmux_ok_gate_is_3_3() {
        let ok = |s: &str| parse_tmux_version(s).is_some_and(|v| v >= MIN_TMUX);
        assert!(ok("tmux 3.3"));
        assert!(ok("tmux 3.4"));
        assert!(ok("tmux 4.0"));
        assert!(!ok("tmux 3.2"));
        assert!(!ok("garbage"));
    }

    #[test]
    fn probe_output_full_and_noisy() {
        let out = "rc-noise: welcome\nCC_TMUX=tmux 3.4\nCC_CLAUDE=1.0.72 (Claude Code)\nCC_BREW=/opt/homebrew/bin/brew\nCC_NPM=/Users/u/.nvm/versions/node/v22/bin/npm\n";
        let r = parse_probe_output(out);
        assert!(r.tmux.found && r.tmux.ok);
        assert_eq!(r.tmux.version.as_deref(), Some("tmux 3.4"));
        assert!(r.claude.found && r.claude.ok);
        assert!(r.brew && r.npm);
    }

    #[test]
    fn probe_output_all_missing() {
        let r = parse_probe_output("CC_TMUX=\nCC_CLAUDE=\nCC_BREW=\nCC_NPM=\n");
        assert!(!r.tmux.found && !r.tmux.ok && r.tmux.version.is_none());
        assert!(!r.claude.found && !r.claude.ok);
        assert!(!r.brew && !r.npm);
    }

    #[test]
    fn probe_output_old_tmux_found_but_not_ok() {
        let r = parse_probe_output("CC_TMUX=tmux 3.2\n");
        assert!(r.tmux.found);
        assert!(!r.tmux.ok);
    }

    #[test]
    fn report_serializes_camel_case() {
        let r = parse_probe_output("CC_TMUX=tmux 3.4\n");
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"tmux\""), "json: {json}");
        assert!(json.contains("\"found\":true"), "json: {json}");
        // Option<String> None serializes as null (frontend types it string|null).
        assert!(json.contains("\"version\":null"), "json: {json}");
    }
}

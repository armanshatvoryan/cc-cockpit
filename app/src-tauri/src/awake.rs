//! Keep-awake lever: hold the Mac awake while long runs finish.
//!
//! Two layers, engaged together on toggle-on:
//!  * `caffeinate -i -s -w <our-pid>` — blocks *idle* system sleep. `-w` ties
//!    the power assertion to the cockpit's own pid, so even a crash or
//!    force-quit releases it. Display sleep stays allowed on purpose: the
//!    point is agents running overnight, not a lit screen.
//!  * root helper (`sudo -n /usr/local/bin/cc-cockpit-sleeplever on <pid>`) —
//!    `pmset disablesleep 1`, the only thing macOS honors on LID CLOSE
//!    (assertions never survive clamshell sleep). The helper spawns a root
//!    watchdog that resets `disablesleep 0` when our pid dies, so this layer
//!    can't orphan either. Helper not installed (no sudoers entry) ⇒ the
//!    lever degrades to idle-only and the UI says so.

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use serde::Serialize;
use tauri::State;

/// Root helper installed by `app/scripts/install-sleeplever.sh` (one-time,
/// with sudo). Absent ⇒ lid-proofing silently degrades, never errors.
const HELPER: &str = "/usr/local/bin/cc-cockpit-sleeplever";

/// Managed state behind the lever.
#[derive(Default)]
pub struct AwakeState(pub Mutex<Inner>);

#[derive(Default)]
pub struct Inner {
    /// Live caffeinate child while the lever is on.
    child: Option<Child>,
    /// Whether the root helper accepted `on` — i.e. lid close is covered too.
    lid_proof: bool,
}

/// What the footer renders: lever on/off + whether lid close is covered.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AwakePayload {
    pub on: bool,
    pub lid_proof: bool,
}

/// Args for the caffeinate child. `-i` blocks idle system sleep, `-s` blocks
/// system sleep on AC power, `-w` exits with the watched pid.
fn caffeinate_args(pid: u32) -> [String; 3] {
    ["-is".into(), "-w".into(), pid.to_string()]
}

/// `sudo -n` argv for the helper. `-n` = never prompt: no sudoers entry ⇒
/// clean failure ⇒ idle-only degrade.
fn helper_args(action: &str, pid: Option<u32>) -> Vec<String> {
    let mut v = vec!["-n".into(), HELPER.into(), action.into()];
    if let Some(pid) = pid {
        v.push(pid.to_string());
    }
    v
}

/// Best-effort helper invocation; true iff it ran and exited 0.
fn run_helper(action: &str, pid: Option<u32>) -> bool {
    Command::new("sudo")
        .args(helper_args(action, pid))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

impl Inner {
    /// Drop a caffeinate child that already exited on its own (killed
    /// externally, crashed), so stale state can't report "on" or block a
    /// re-enable. If lid-proofing was active, release it too — the root
    /// watchdog only fires on APP death, not on caffeinate's, and a lever
    /// that reads "off" must never leave `disablesleep 1` behind.
    fn reap_dead(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if matches!(child.try_wait(), Ok(Some(_))) {
                self.child = None;
                if self.lid_proof {
                    run_helper("off", None);
                    self.lid_proof = false;
                }
            }
        }
    }

    fn payload(&self) -> AwakePayload {
        AwakePayload { on: self.child.is_some(), lid_proof: self.lid_proof }
    }
}

/// Turn the lever on/off. Idempotent in both directions; returns the actual
/// state afterwards, which is what the footer toggle renders.
#[tauri::command]
pub fn awake_set(state: State<'_, AwakeState>, on: bool) -> Result<AwakePayload, String> {
    let mut inner = state.0.lock().unwrap();
    inner.reap_dead();
    if on && inner.child.is_none() {
        let pid = std::process::id();
        let child = Command::new("caffeinate")
            .args(caffeinate_args(pid))
            .spawn()
            .map_err(|e| format!("caffeinate spawn failed: {e}"))?;
        inner.child = Some(child);
        inner.lid_proof = run_helper("on", Some(pid));
    } else if !on {
        if let Some(mut child) = inner.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if inner.lid_proof {
            run_helper("off", None);
            inner.lid_proof = false;
        }
    }
    Ok(inner.payload())
}

/// Current lever state, for the frontend to sync on boot (the backend
/// survives a webview reload, so the toggle must ask rather than assume off).
#[tauri::command]
pub fn awake_get(state: State<'_, AwakeState>) -> AwakePayload {
    let mut inner = state.0.lock().unwrap();
    inner.reap_dead();
    inner.payload()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_bind_assertion_to_watched_pid() {
        assert_eq!(caffeinate_args(4242), ["-is", "-w", "4242"]);
    }

    #[test]
    fn helper_args_never_prompt_and_pass_pid_only_for_on() {
        assert_eq!(helper_args("on", Some(7)), ["-n", HELPER, "on", "7"]);
        assert_eq!(helper_args("off", None), ["-n", HELPER, "off"]);
    }

    #[test]
    fn reap_clears_exited_child_and_keeps_live_one() {
        // Exited child: `true` terminates immediately.
        let mut done = Command::new("true").spawn().expect("spawn true");
        done.wait().expect("wait true");
        let mut inner = Inner { child: Some(done), lid_proof: false };
        inner.reap_dead();
        assert!(inner.child.is_none(), "exited child must be reaped");

        // Live child: `sleep 30` is still running when we reap.
        let live = Command::new("sleep").arg("30").spawn().expect("spawn sleep");
        let mut inner = Inner { child: Some(live), lid_proof: false };
        inner.reap_dead();
        assert!(inner.child.is_some(), "live child must survive reap");
        let mut child = inner.child.take().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

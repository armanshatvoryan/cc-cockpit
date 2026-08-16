//! Keep-awake lever: hold the Mac's system sleep off while long runs finish.
//!
//! Backed by a spawned `caffeinate -i -s -w <our-pid>` child. `-w` ties the
//! power assertion to the cockpit's own pid, so even a crash or force-quit
//! releases it — the assertion can never outlive the app. Toggling off kills
//! the child (and reaps it). Display sleep stays allowed on purpose: the point
//! is agents running overnight, not a lit screen.

use std::process::{Child, Command};
use std::sync::Mutex;

use tauri::State;

/// Managed state: the live caffeinate child while the lever is on.
#[derive(Default)]
pub struct AwakeState(pub Mutex<Option<Child>>);

/// Args for the caffeinate child. `-i` blocks idle system sleep, `-s` blocks
/// system sleep on AC power, `-w` exits with the watched pid.
fn caffeinate_args(pid: u32) -> [String; 3] {
    ["-is".into(), "-w".into(), pid.to_string()]
}

/// Drop a child that already exited on its own (killed externally, crashed),
/// so stale state can't report "on" or block a re-enable. Best-effort: a
/// try_wait error leaves the child in place.
fn reap_dead(slot: &mut Option<Child>) {
    if let Some(child) = slot.as_mut() {
        if matches!(child.try_wait(), Ok(Some(_))) {
            *slot = None;
        }
    }
}

/// Turn the lever on/off. Idempotent in both directions; returns the actual
/// state afterwards, which is what the footer toggle renders.
#[tauri::command]
pub fn awake_set(state: State<'_, AwakeState>, on: bool) -> Result<bool, String> {
    let mut slot = state.0.lock().unwrap();
    reap_dead(&mut slot);
    if on && slot.is_none() {
        let child = Command::new("caffeinate")
            .args(caffeinate_args(std::process::id()))
            .spawn()
            .map_err(|e| format!("caffeinate spawn failed: {e}"))?;
        *slot = Some(child);
    } else if !on {
        if let Some(mut child) = slot.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    Ok(slot.is_some())
}

/// Current lever state, for the frontend to sync on boot (the backend child
/// survives a webview reload, so the toggle must ask rather than assume off).
#[tauri::command]
pub fn awake_get(state: State<'_, AwakeState>) -> bool {
    let mut slot = state.0.lock().unwrap();
    reap_dead(&mut slot);
    slot.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_bind_assertion_to_watched_pid() {
        assert_eq!(caffeinate_args(4242), ["-is", "-w", "4242"]);
    }

    #[test]
    fn reap_clears_exited_child_and_keeps_live_one() {
        // Exited child: `true` terminates immediately.
        let mut done = Command::new("true").spawn().expect("spawn true");
        done.wait().expect("wait true");
        let mut slot = Some(done);
        reap_dead(&mut slot);
        assert!(slot.is_none(), "exited child must be reaped");

        // Live child: `sleep 30` is still running when we reap.
        let live = Command::new("sleep").arg("30").spawn().expect("spawn sleep");
        let mut slot = Some(live);
        reap_dead(&mut slot);
        assert!(slot.is_some(), "live child must survive reap");
        let mut child = slot.take().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

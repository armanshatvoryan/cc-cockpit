//! Thin synchronous wrapper over the `tmux -L cockpit` CLI for *admin* ops.
//!
//! Two data paths exist in the cockpit backend:
//!   * **Streaming** — one long-lived `tmux -CC attach` control client per
//!     session (the PROVEN `cockpit_engine::ControlClient`) carries live
//!     `%output` + topology. Never used for one-shot queries.
//!   * **Admin** — short-lived `tmux -L cockpit <cmd>` subprocesses for
//!     create/split/kill/list/capture. These return stdout synchronously and,
//!     because they hit the SAME socket, any topology change they cause is
//!     reflected back to the attached control client as `%window-add` /
//!     `%layout-change` / etc. (validated by the D3 live-bridge spike).
//!
//! Everything here is scoped to the PRIVATE `-L cockpit` socket. We NEVER touch
//! the default socket (native CC's `cockpit` session lives there).

use std::process::Command;

/// The private tmux socket name. NEVER the default socket.
pub const SOCKET: &str = "cockpit";
/// The single cockpit session that holds all tabs (windows) and panes.
pub const SESSION: &str = "cockpit-main";

/// Result of a tmux admin invocation.
pub struct TmuxOut {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl TmuxOut {
    pub fn ok(&self) -> bool {
        self.status == 0
    }
    /// Trimmed stdout as an owned String (convenience for single-value queries).
    pub fn trimmed(&self) -> String {
        self.stdout.trim().to_string()
    }
}

/// Run `tmux -L cockpit <args...>` and capture output. Synchronous.
pub fn tmux(args: &[&str]) -> Result<TmuxOut, String> {
    let mut full: Vec<&str> = Vec::with_capacity(args.len() + 2);
    full.push("-L");
    full.push(SOCKET);
    full.extend_from_slice(args);

    let out = Command::new("tmux")
        .args(&full)
        .output()
        .map_err(|e| format!("spawn tmux: {e}"))?;

    Ok(TmuxOut {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Run a tmux admin command and require success, surfacing stderr on failure.
pub fn tmux_ok(args: &[&str]) -> Result<TmuxOut, String> {
    let o = tmux(args)?;
    if o.ok() {
        Ok(o)
    } else {
        Err(format!(
            "tmux {:?} failed (exit {}): {}",
            args,
            o.status,
            o.stderr.trim()
        ))
    }
}

/// Does the cockpit session exist on the private socket?
pub fn has_session() -> bool {
    // `has-session` exits 0 if present, 1 otherwise; a missing server is also
    // non-zero (it tries to start one for has-session, so guard with -L only).
    tmux(&["has-session", "-t", SESSION])
        .map(|o| o.ok())
        .unwrap_or(false)
}

/// Ensure the socket + `cockpit-main` session exist. Idempotent. The bootstrap
/// window/pane is created detached so no client is required.
pub fn ensure_session() -> Result<(), String> {
    if has_session() {
        return Ok(());
    }
    // -d detached, -s session name. The default first window is the bootstrap
    // tab; the SessionManager renames/derives tab ids from window ids.
    tmux_ok(&[
        "new-session",
        "-d",
        "-s",
        SESSION,
        "-x",
        "200",
        "-y",
        "50",
    ])?;
    // Don't let panes vanish the instant a command exits — we want pane_dead so
    // the status heuristic can report DEAD instead of the pane just disappearing.
    let _ = tmux(&["set-option", "-t", SESSION, "remain-on-exit", "on"]);
    Ok(())
}

/// Is the cockpit server alive AND responsive? A *poisoned* socket (left behind
/// by a crashed/force-quit prior run + orphaned `-CC` control clients) answers
/// every command with "server exited unexpectedly" while looking present — so we
/// probe with a real query that only succeeds against a live server.
pub fn server_healthy() -> bool {
    match tmux(&["list-panes", "-t", SESSION, "-F", "#{pane_id}"]) {
        Ok(o) => o.ok() && !o.trimmed().is_empty(),
        Err(_) => false,
    }
}

/// Forcibly reset a poisoned `-L cockpit` socket: kill orphaned control clients,
/// kill the (possibly half-dead) server, and remove the stale socket file. Scoped
/// to our private socket only — never the default socket.
pub fn reset_server() {
    let _ = Command::new("sh")
        .arg("-c")
        .arg(
            "pkill -f 'tmux -L cockpit -C' 2>/dev/null; \
             tmux -L cockpit kill-server 2>/dev/null; \
             rm -f \"${TMPDIR:-/tmp}tmux-$(id -u)/cockpit\" 2>/dev/null; true",
        )
        .status();
}

/// Ensure a HEALTHY cockpit session exists, self-healing a poisoned socket.
/// Create → verify responsive → on failure hard-reset and retry. This is what
/// `init` calls so a prior force-quit/crash can't brick startup.
pub fn ensure_healthy_session() -> Result<(), String> {
    for _ in 0..3 {
        let _ = ensure_session();
        if server_healthy() {
            return Ok(());
        }
        reset_server();
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    ensure_session()?;
    if server_healthy() {
        Ok(())
    } else {
        Err("cockpit tmux server is unhealthy after reset attempts".into())
    }
}

/// Shell-single-quote a string for safe interpolation into a `send-keys`
/// command line (cwd, flags). Escapes embedded single quotes with the `'\''`
/// idiom. Mirrors the engine's quoting so behaviour is consistent.
pub fn shq(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shq_escapes_quotes_and_spaces() {
        assert_eq!(shq("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(shq("it's"), "'it'\\''s'");
        assert_eq!(shq(""), "''");
    }
}

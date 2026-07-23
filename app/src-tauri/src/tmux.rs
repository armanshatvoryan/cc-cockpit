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
use std::sync::RwLock;

/// The private tmux socket name. NEVER the default socket.
pub const SOCKET: &str = "cockpit";
/// The single cockpit session that holds all tabs (windows) and panes.
pub const SESSION: &str = "cockpit-main";
/// Fallback directory (under $HOME) where new tabs and the bootstrap session
/// start when the user has configured nothing. Finder-launched apps inherit cwd
/// `/`, so without an explicit `-c` every fresh pane opened at the filesystem
/// root. This is the author's own layout; it is a FALLBACK, not a rule — an
/// install on a machine without `~/Workflows` lands on `$HOME` instead.
pub const DEFAULT_DIR_UNDER_HOME: &str = "Workflows";

/// The user-configured start directory, or `None` when unset.
///
/// `default_cwd` is a free fn called from both the boot path (`ensure_session`)
/// and per-tab (`SessionManager::create_tab`), neither of which holds an
/// `AppHandle` — but the persisted preference lives in `app_config_dir`, which
/// needs one. Process-global state is the seam between the two: Tauri's `setup`
/// hook loads the file once (see `settings::apply_at_startup`) and the settings
/// dialog re-sets it on save, so a change takes effect without a restart.
static CONFIGURED_CWD: RwLock<Option<String>> = RwLock::new(None);

/// Install the configured start directory. `None` clears it (revert to default).
/// Poisoned-lock safe: a panic elsewhere must not brick tab creation.
pub fn set_configured_cwd(dir: Option<String>) {
    let mut guard = match CONFIGURED_CWD.write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = dir.filter(|d| !d.trim().is_empty());
}

/// Read the configured start directory.
fn configured_cwd() -> Option<String> {
    match CONFIGURED_CWD.read() {
        Ok(g) => g.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Start directory for new sessions/tabs, in order: the user-configured dir (if
/// it still exists), else `$HOME/Workflows` (if it exists), else `$HOME`, else
/// `/` (no HOME — shouldn't happen in a GUI launch).
///
/// The existence gate on the configured path is deliberate: a user who renames
/// or deletes the folder they picked gets a working cockpit rooted at `$HOME`,
/// not a tmux that refuses to spawn a pane into a missing `-c` directory.
pub fn default_cwd() -> String {
    default_cwd_impl(configured_cwd(), std::env::var("HOME").ok(), |p| {
        std::path::Path::new(p).is_dir()
    })
}

fn default_cwd_impl(
    configured: Option<String>,
    home: Option<String>,
    is_dir: impl Fn(&str) -> bool,
) -> String {
    if let Some(c) = configured {
        if is_dir(&c) {
            return c;
        }
    }
    match home {
        Some(h) => {
            let preferred = format!("{h}/{DEFAULT_DIR_UNDER_HOME}");
            if is_dir(&preferred) {
                preferred
            } else {
                h
            }
        }
        None => "/".into(),
    }
}

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
    let cwd = default_cwd();
    tmux_ok(&[
        "new-session",
        "-d",
        "-s",
        SESSION,
        "-x",
        "200",
        "-y",
        "50",
        "-c",
        &cwd,
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
    // NOTE socket path: tmux stores its socket at `${TMUX_TMPDIR:-/tmp}/tmux-<uid>/<name>`
    // — it uses /tmp, NOT $TMPDIR (which on macOS is a per-process /var/folders dir).
    // The earlier `${TMPDIR}tmux-…` target never matched the real file; kill-server
    // already unlinks the socket on a live server, and tmux unlinks a stale socket
    // itself on the next new-session, so this rm is only belt-and-suspenders — but
    // point it at the CORRECT path so it actually helps when it must.
    let _ = Command::new("sh")
        .arg("-c")
        .arg(
            "pkill -f 'tmux -L cockpit -C' 2>/dev/null; \
             tmux -L cockpit kill-server 2>/dev/null; \
             rm -f \"${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)/cockpit\" 2>/dev/null; true",
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

    #[test]
    fn default_cwd_prefers_workflows_then_home_then_root() {
        let home = Some("/Users/u".to_string());
        assert_eq!(
            default_cwd_impl(None, home.clone(), |p| p == "/Users/u/Workflows"),
            "/Users/u/Workflows"
        );
        assert_eq!(default_cwd_impl(None, home, |_| false), "/Users/u");
        assert_eq!(default_cwd_impl(None, None, |_| true), "/");
    }

    #[test]
    fn configured_cwd_wins_over_workflows_and_home() {
        let home = Some("/Users/u".to_string());
        assert_eq!(
            default_cwd_impl(Some("/Users/u/Code".into()), home, |_| true),
            "/Users/u/Code"
        );
    }

    #[test]
    fn stale_configured_cwd_falls_back_instead_of_breaking_tmux() {
        // The user picked a folder, then deleted or renamed it. tmux would
        // refuse `-c <missing dir>`, so the chain must skip it silently.
        let home = Some("/Users/u".to_string());
        assert_eq!(
            default_cwd_impl(Some("/Users/u/Gone".into()), home.clone(), |p| p
                == "/Users/u/Workflows"),
            "/Users/u/Workflows"
        );
        assert_eq!(
            default_cwd_impl(Some("/Users/u/Gone".into()), home, |_| false),
            "/Users/u"
        );
    }

    /// Everything that touches the `CONFIGURED_CWD` global lives in ONE test on
    /// purpose: `cargo test` runs tests in parallel threads of a single process,
    /// so two tests mutating the same static would race and flake.
    ///
    /// The mocked-`is_dir` tests above cover the branch logic; this drives the
    /// REAL `default_cwd()` against the REAL filesystem, so a mis-wire between
    /// the global and the resolver can't pass on mocks alone.
    #[test]
    fn configured_cwd_global_round_trip_and_real_resolution() {
        // Blank/whitespace is "unset", not a path.
        set_configured_cwd(Some("   ".into()));
        assert_eq!(configured_cwd(), None);
        set_configured_cwd(Some("/Users/u/Code".into()));
        assert_eq!(configured_cwd(), Some("/Users/u/Code".into()));
        set_configured_cwd(None);
        assert_eq!(configured_cwd(), None);

        // A real, existing directory must win outright.
        let real = std::env::temp_dir()
            .to_string_lossy()
            .trim_end_matches('/')
            .to_string();
        set_configured_cwd(Some(real.clone()));
        assert_eq!(default_cwd(), real, "an existing configured dir must win");

        // A configured directory that has since vanished must never reach
        // `tmux -c` — tmux refuses to spawn and the tab creation would fail.
        let gone = format!("{real}/cc-cockpit-does-not-exist-9f3a");
        set_configured_cwd(Some(gone.clone()));
        assert_ne!(default_cwd(), gone, "missing dir must not be handed to tmux");
        assert!(
            std::path::Path::new(&default_cwd()).is_dir(),
            "the fallback must itself be a real directory"
        );

        set_configured_cwd(None);
    }
}

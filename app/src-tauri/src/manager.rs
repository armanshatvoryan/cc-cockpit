//! SessionManager — the v1 cockpit backbone (P1-F1…F5, backend half).
//!
//! Model (per the brief):
//!   * ONE tmux session `cockpit-main` on the PRIVATE `-L cockpit` socket.
//!   * Tabs   = tmux **windows**  (tab id `tab-<window-number>`, e.g. `tab-0`).
//!   * Panes  = tmux **panes**    (pane id `%N`, the tmux pane id verbatim).
//!   * ONE control-mode client (`cockpit_engine::ControlClient`) multiplexes all
//!     panes' live `%output` + topology for the whole session.
//!
//! Admin ops (create/split/kill/list/capture/launch) go through short-lived
//! `tmux -L cockpit` subprocesses (`tmux.rs`); the live stream comes from the
//! single attached control client. Status is derived by a background poller that
//! runs the ported D6 heuristic (`status.rs`) over `capture-pane`.

use std::collections::HashMap;

use base64::Engine as _;
use cockpit_engine::ControlClient;
use serde::Serialize;

use crate::status::{classify, Status};
use crate::tmux::{self, SESSION};

/// Shared base64 engine for warm-start payload encoding (mirrors the engine's
/// STANDARD alphabet so the frontend decodes identically to `pane:data`).
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// Trim leading + trailing blank lines from a `capture-pane -e` dump (blank =
/// empty after stripping ANSI escapes + whitespace). A fresh pane captures as a
/// single prompt line padded with ~45 empty rows; replaying that padding paints
/// a tall void above the prompt in the re-attached xterm. Interior blanks (real
/// output spacing) are preserved — only the edges are trimmed.
fn trim_blank_edges(s: &str) -> String {
    let lines: Vec<&str> = s.split('\n').collect();
    let is_blank = |l: &&str| crate::status::strip_ansi(l).trim().is_empty();
    let first = lines.iter().position(|l| !is_blank(l));
    let last = lines.iter().rposition(|l| !is_blank(l));
    match (first, last) {
        (Some(a), Some(b)) => lines[a..=b].join("\n"),
        _ => String::new(), // all-blank capture → nothing to replay
    }
}

/// Build the byte stream that repaints an xterm from a visible-grid capture.
///
/// capture-pane joins rows with bare `\n`; xterm is not in convertEol mode, so
/// rows are rejoined with `\r\n` (a bare LF keeps the column and stairsteps).
/// The capture is NOT edge-trimmed: after a clear every visible row — blank
/// ones included — must land on its own grid row or everything below shifts.
/// `cursor` is tmux's real (x, y) for the pane, re-asserted with a 1-based CUP
/// so the next differential frame starts from the right cell.
///
/// Bug #5 (the void): `capture-pane -p` terminates its LAST row with `\n` too,
/// so an R-row grid arrives as R rows + R line separators. Replaying all R
/// separators into an R-row viewport scrolls it one row — the top row is pushed
/// into scrollback (a phantom history line) and the bottom row is left blank
/// (the void). Exactly ONE trailing `\n` is stripped: it is a terminator, not a
/// row. A genuine trailing blank ROW arrives as `\n\n` and survives.
fn compose_screen_replay(capture: &str, cursor: Option<(u32, u32)>) -> String {
    let grid = capture.strip_suffix('\n').unwrap_or(capture);
    let mut buf = grid.replace('\n', "\r\n");
    if let Some((x, y)) = cursor {
        buf.push_str(&format!("\x1b[{};{}H", y + 1, x + 1));
    }
    buf
}

/// Build the mount-time warm-start stream from a FULL-scrollback capture
/// (`capture-pane -e -S -`) plus the pane's real height and cursor.
///
/// The old warm start replayed the whole capture edge-trimmed, with no cursor
/// and no idea where the viewport started — so the visible grid landed wherever
/// the write happened to end up (bug #4, warm-start garble). The capture is
/// ordered history-then-screen, so the split is exact: the LAST `pane_height`
/// lines ARE the visible grid; everything above is scrollback.
///
///   * history → `trim_blank_edges` (drops the fresh-pane padding void) then
///     `\r\n` join + one terminator so the grid starts on its own row. An
///     all-blank history contributes nothing.
///   * grid → `compose_screen_replay` (verbatim rows, H-1 line endings, real
///     cursor CUP), so the H rows fill the H-row viewport without scrolling it
///     and the cursor's viewport-relative CUP is correct.
///
/// Soft-wrapped history lines still gain a phantom blank row when xterm
/// re-wraps them — cosmetic, scrollback-only, accepted. The viewport (input
/// line included) is exact.
fn compose_warm_start(capture: &str, pane_height: usize, cursor: Option<(u32, u32)>) -> String {
    let body = capture.strip_suffix('\n').unwrap_or(capture);
    let lines: Vec<&str> = body.split('\n').collect();
    let split = lines.len().saturating_sub(pane_height.max(1));
    let mut out = String::new();
    if split > 0 {
        let history = trim_blank_edges(&lines[..split].join("\n"));
        if !history.is_empty() {
            out.push_str(&history.replace('\n', "\r\n"));
            out.push_str("\r\n");
        }
    }
    // The grid slice is handed over in capture-pane's own shape — rows joined
    // by \n WITH the terminator — because that is what compose_screen_replay
    // consumes. Re-adding it matters when the last visible row is blank: the
    // join's trailing \n is then a real separator, not the terminator.
    let grid = lines[split..].join("\n") + "\n";
    out.push_str(&compose_screen_replay(&grid, cursor));
    out
}

/// Parse the first `n` whitespace-separated u32 fields of a `display-message -p`
/// reply. Any short / non-numeric / empty reply (older tmux, dead pane) yields
/// None so the caller can fall back instead of guessing a geometry.
fn parse_u32_fields(s: &str, n: usize) -> Option<Vec<u32>> {
    let v: Vec<u32> = s
        .split_whitespace()
        .take(n)
        .map(|f| f.parse::<u32>().ok())
        .collect::<Option<Vec<u32>>>()?;
    (v.len() == n).then_some(v)
}

/// Ask tmux for `n` numeric `#{...}` fields of one pane. Best-effort: a failed
/// or unparsable query is None, never an error — every caller has a fallback.
fn pane_numbers(pane_id: &str, format: &str, n: usize) -> Option<Vec<u32>> {
    tmux::tmux(&["display-message", "-p", "-t", pane_id, format])
        .ok()
        .filter(|o| o.ok())
        .and_then(|o| parse_u32_fields(&o.stdout, n))
}

/// tmux's real cursor (x, y) for a pane, or None when the query fails (the
/// replay is still aligned; only the cursor cell is off until the next output).
fn pane_cursor(pane_id: &str) -> Option<(u32, u32)> {
    pane_numbers(pane_id, "#{cursor_x} #{cursor_y}", 2).map(|v| (v[0], v[1]))
}

// ── Snapshot / return DTOs (camelCase for the JS frontend) ───────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneInfo {
    /// tmux pane id, e.g. `%3`. This is THE pane id the frontend uses everywhere.
    pub pane_id: String,
    /// The tab (window) this pane belongs to, e.g. `tab-0`.
    pub tab_id: String,
    /// Current working directory of the pane's foreground process.
    pub cwd: String,
    /// Pane title (tmux `#{pane_title}`), best-effort label.
    pub title: String,
    /// Whether tmux reports the pane as dead (process exited, remain-on-exit).
    pub dead: bool,
    /// Last classified status. UNKNOWN until the poller first runs.
    pub status: String,
    /// Whether the last classification was ambiguous (status == UNKNOWN).
    pub ambiguous: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabInfo {
    /// Cockpit tab id `tab-<n>`.
    pub tab_id: String,
    /// tmux window id `@<n>`.
    pub tmux_window_id: String,
    /// tmux window index (number).
    pub index: u32,
    /// Window name.
    pub name: String,
    /// tmux layout string for the window (for the frontend's pane geometry).
    pub layout: String,
    /// Parsed pane geometry (tmux is the layout authority — bug #10). None
    /// when the layout string fails to parse; the frontend then falls back
    /// to its own grid.
    pub geometry: Option<crate::layout::WindowLayout>,
    /// Pane ids in this tab (tmux order).
    pub pane_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CockpitState {
    pub socket: String,
    pub session: String,
    pub tabs: Vec<TabInfo>,
    pub panes: Vec<PaneInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTabResult {
    pub tab_id: String,
    pub tmux_window_id: String,
    pub pane_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseTabResult {
    pub ok: bool,
    /// Live (non-dead) panes in the tab. If non-empty and `force` was false, the
    /// frontend should confirm before re-calling with force=true.
    pub live_panes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitPaneResult {
    pub pane_id: String,
    pub layout: String,
}

/// Status of one pane, as emitted on `pane:status`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneStatusPayload {
    pub pane_id: String,
    pub status: String,
    pub ambiguous: bool,
    /// Milliseconds since the pane's `#{pane_activity}` last advanced.
    pub recency_ms: u64,
}

// ── tab id <-> window mapping ────────────────────────────────────────────────

/// Cockpit tab id from a tmux window index. `tab-<index>`.
pub fn tab_id_for_index(index: u32) -> String {
    format!("tab-{index}")
}

/// A tmux window id is `@` followed by ASCII digits (e.g. `@3`). Validated before
/// it reaches a tmux `-t` target so a stale/config string can never smuggle an
/// extra flag or shell payload into the argv. Closing addresses windows by this
/// STABLE id, never the mutable window index (which tmux reuses for new windows).
pub fn is_window_id(s: &str) -> bool {
    s.len() >= 2 && s.starts_with('@') && s[1..].bytes().all(|b| b.is_ascii_digit())
}

/// A tmux pane id is `%` followed by ASCII digits (e.g. `%3`). List-parsers
/// validate every id before serving it to the frontend: a C-locale tmux
/// sanitizes the TAB field delimiters to `_`, fusing a whole list-panes line
/// into one garbage "id" — that must die here, not surface as a broken pane.
pub fn is_pane_id(s: &str) -> bool {
    s.len() >= 2 && s.starts_with('%') && s[1..].bytes().all(|b| b.is_ascii_digit())
}

/// tmux's "target window/pane doesn't exist" stderr — treated as idempotent
/// success when closing a tab: a window that's already gone IS closed, so a no-op
/// kill must not surface a scary error.
fn is_missing_target(stderr: &str) -> bool {
    let e = stderr.to_ascii_lowercase();
    e.contains("can't find window") || e.contains("can't find pane")
}

/// Does this tmux error mean the server or our session is gone — so a re-heal +
/// re-attach would recover? Matches the admin-path failure strings that warrant a
/// reconnect, and deliberately does NOT match "can't find window/pane" (a stale
/// target on a live server, where reconnecting wouldn't help).
pub fn is_server_gone(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("no server running")
        || e.contains("can't find session")
        || e.contains("session not found")
        || e.contains("no current session")
        || e.contains("lost server")
        || e.contains("server exited")
}

/// A `~/.claude/agents/*.md` `name` is config-derived; before it reaches a
/// `claude --agent <name>` command line we require a strict identifier charset
/// so it can never inject an extra flag or shell payload (P2-F4 launch).
pub fn is_valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

// ── SessionManager ───────────────────────────────────────────────────────────

/// Owns the live control client. The in-memory model is intentionally derived
/// fresh from tmux on each `list_state()` (tmux is the source of truth), so the
/// manager stays correct even when external clients mutate the session.
pub struct SessionManager {
    client: Option<ControlClient>,
    /// Last status emitted per pane, so the poller only emits on change.
    last_status: HashMap<String, Status>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            client: None,
            last_status: HashMap::new(),
        }
    }

    /// Ensure socket + session, attach the control client. Returns nothing here;
    /// the channel (engine Outbound) is returned so the caller can spin up the
    /// event forwarder. Idempotent on the session; re-attaches a fresh client.
    pub fn init(
        &mut self,
    ) -> Result<std::sync::mpsc::Receiver<cockpit_engine::Outbound>, String> {
        tmux::ensure_healthy_session()?;
        let (client, rx) =
            ControlClient::attach(tmux::SOCKET, SESSION).map_err(|e| e.to_string())?;
        self.client = Some(client);
        Ok(rx)
    }

    pub fn is_attached(&self) -> bool {
        self.client.is_some()
    }

    /// Run an admin/control op; if it fails because the tmux server or our session
    /// vanished mid-run (force-quit, crash, external `kill`), re-heal the session,
    /// RE-ATTACH a fresh control client, and retry the op ONCE. Returns the op
    /// result plus — when a reconnect happened — the NEW `Outbound` receiver so the
    /// caller can rebind the event forwarder to it (the old forwarder ends on its
    /// own when the dropped client's stream disconnects).
    ///
    /// `op` may run twice, so it must be safe to retry: the first run failed before
    /// mutating tmux (the server was already gone), so a single retry is sound.
    pub fn with_reattach<T>(
        &mut self,
        op: impl Fn(&mut Self) -> Result<T, String>,
    ) -> Result<(T, Option<std::sync::mpsc::Receiver<cockpit_engine::Outbound>>), String> {
        match op(self) {
            Ok(v) => Ok((v, None)),
            Err(e) if is_server_gone(&e) => {
                // The server vanished, so our control client is now orphaned. A
                // lingering `tmux -C` client keeps the dead socket "present but
                // broken" and poisons a freshly-created server ("server exited
                // unexpectedly" on the next command) — and its graceful Drop would
                // block on child.wait(). Force-kill it FIRST so re-heal sees a clean
                // socket and the drop returns immediately, THEN re-attach + retry.
                if let Some(mut dead) = self.client.take() {
                    dead.kill();
                }
                let rx = self.init()?;
                let v = op(self)?;
                Ok((v, Some(rx)))
            }
            Err(e) => Err(e),
        }
    }

    fn client_mut(&mut self) -> Result<&mut ControlClient, String> {
        self.client.as_mut().ok_or_else(|| "not attached".to_string())
    }

    // ── F1: tabs ─────────────────────────────────────────────────────────────

    /// Create a new tab (tmux window). Returns the new tab id, tmux window id,
    /// and the id of its initial pane.
    pub fn create_tab(&mut self, name: Option<&str>) -> Result<CreateTabResult, String> {
        // new-window -P -F prints the new window/pane ids we want, atomically.
        // Format: "@<win> <index> %<pane>"
        let mut args: Vec<String> = vec![
            "new-window".into(),
            "-t".into(),
            format!("{SESSION}:"),
            "-c".into(),
            tmux::default_cwd(),
            "-P".into(),
            "-F".into(),
            "#{window_id} #{window_index} #{pane_id}".into(),
        ];
        if let Some(n) = name {
            args.push("-n".into());
            args.push(n.to_string());
        }
        let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let out = tmux::tmux_ok(&argv)?;
        let line = out.trimmed();
        let mut it = line.split_whitespace();
        let win = it.next().unwrap_or("").to_string();
        let index: u32 = it.next().unwrap_or("0").parse().unwrap_or(0);
        let pane = it.next().unwrap_or("").to_string();
        Ok(CreateTabResult {
            tab_id: tab_id_for_index(index),
            tmux_window_id: win,
            pane_id: pane,
        })
    }

    /// After the cockpit session was destroyed (the user closed the last tab —
    /// tmux can't hold a 0-window session) and then re-created, surface its single
    /// bootstrap window as the "new" tab. Every fresh tmux session is born with one
    /// window, so calling `create_tab` (new-window) here would sit a SECOND window
    /// on top of that bootstrap → one ⌘T from the empty state yields two tabs.
    /// Mirrors what boot's `list_state` already does for the bootstrap window.
    /// Best-effort rename when a name is given.
    pub fn adopt_bootstrap_tab(&mut self, name: Option<&str>) -> Result<CreateTabResult, String> {
        let tab = self
            .collect_tabs()?
            .into_iter()
            .next()
            .ok_or_else(|| "re-healed session has no window to adopt".to_string())?;
        if let Some(n) = name {
            if !n.is_empty() {
                let _ = tmux::tmux(&["rename-window", "-t", &tab.tmux_window_id, n]);
            }
        }
        let pane_id = tab.pane_ids.first().cloned().unwrap_or_default();
        Ok(CreateTabResult {
            tab_id: tab.tab_id,
            tmux_window_id: tab.tmux_window_id,
            pane_id,
        })
    }

    /// Create a new tab, healing a destroyed session correctly. When the session is
    /// ALIVE, add a fresh window (`create_tab`). When it's GONE (the last tab was
    /// closed, which destroys the tmux session) or poisoned, re-create + re-attach
    /// the control client and ADOPT the lone bootstrap window — never `new-window` a
    /// SECOND window on top of it. Returns the new `Outbound` receiver when a
    /// re-attach happened, so the caller rebinds the event forwarder (same contract
    /// as `with_reattach`).
    pub fn create_tab_healing(
        &mut self,
        name: Option<&str>,
    ) -> Result<
        (
            CreateTabResult,
            Option<std::sync::mpsc::Receiver<cockpit_engine::Outbound>>,
        ),
        String,
    > {
        if tmux::has_session() && tmux::server_healthy() {
            let tab = self.create_tab(name)?;
            Ok((tab, None))
        } else {
            let rx = self.init()?;
            let tab = self.adopt_bootstrap_tab(name)?;
            Ok((tab, Some(rx)))
        }
    }

    /// Inspect a tab's live panes (for close confirmation). On `force`, kill it.
    ///
    /// Targets the tab by its STABLE tmux window id (`@n`), never the window
    /// *index*. Indices are mutable and tmux reuses a freed index for the next
    /// new/broken-out window, so a close-by-index can hit the WRONG window — or a
    /// window that's already gone, which used to surface a scary `can't find
    /// window: N` error. Closing an already-absent window is idempotent success:
    /// the tab IS gone, which is exactly what "close" means.
    pub fn close_tab(&mut self, window_id: &str, force: bool) -> Result<CloseTabResult, String> {
        if !is_window_id(window_id) {
            return Err(format!("bad window id {window_id:?}, want @<n>"));
        }

        // List live (non-dead) panes in the window.
        let live = self.live_panes_in_window(window_id)?;

        if !force && !live.is_empty() {
            // Frontend should confirm; do NOT kill yet.
            return Ok(CloseTabResult {
                ok: false,
                live_panes: live,
            });
        }

        let out = tmux::tmux(&["kill-window", "-t", window_id])?;
        if !out.ok() && !is_missing_target(&out.stderr) {
            return Err(format!(
                "tmux kill-window -t {window_id} failed (exit {}): {}",
                out.status,
                out.stderr.trim()
            ));
        }
        Ok(CloseTabResult {
            ok: true,
            live_panes: vec![],
        })
    }

    /// Live (non-dead) pane ids inside a window target.
    fn live_panes_in_window(&self, win_target: &str) -> Result<Vec<String>, String> {
        let out = tmux::tmux(&[
            "list-panes",
            "-t",
            win_target,
            "-F",
            "#{pane_id} #{pane_dead}",
        ])?;
        if !out.ok() {
            // Window may already be gone.
            return Ok(vec![]);
        }
        let mut live = vec![];
        for line in out.stdout.lines() {
            let mut it = line.split_whitespace();
            let pane = it.next().unwrap_or("");
            let dead = it.next().unwrap_or("0");
            if !pane.is_empty() && dead != "1" {
                live.push(pane.to_string());
            }
        }
        Ok(live)
    }

    // ── F2: panes ────────────────────────────────────────────────────────────

    /// Split a pane horizontally (`h`, side-by-side) or vertically (`v`, stacked).
    /// Returns the new pane id and the window's resulting layout string.
    pub fn split_pane(&mut self, pane_id: &str, dir: &str) -> Result<SplitPaneResult, String> {
        let flag = match dir {
            "h" => "-h",
            "v" => "-v",
            other => return Err(format!("bad split dir {other:?}, want 'h' or 'v'")),
        };
        // -c inherits the source pane's cwd (server-side format expansion) —
        // splitting a pane you've cd'd somewhere keeps you there.
        let out = tmux::tmux_ok(&[
            "split-window",
            flag,
            "-t",
            pane_id,
            "-c",
            "#{pane_current_path}",
            "-P",
            "-F",
            "#{pane_id}",
        ])?;
        let new_pane = out.trimmed();
        let layout = self.window_layout_for_pane(&new_pane).unwrap_or_default();
        Ok(SplitPaneResult {
            pane_id: new_pane,
            layout,
        })
    }

    /// Close a pane. `kill` = kill-pane (process gone). `detach` = break the pane
    /// out into its own background window so it keeps running but leaves the tab.
    pub fn close_pane(&mut self, pane_id: &str, mode: &str) -> Result<(), String> {
        match mode {
            "kill" => {
                tmux::tmux_ok(&["kill-pane", "-t", pane_id])?;
            }
            "detach" => {
                // break-pane -d: move the pane to a new (detached) window so the
                // process survives; -d keeps focus where it was.
                tmux::tmux_ok(&["break-pane", "-d", "-s", pane_id])?;
            }
            other => return Err(format!("bad close mode {other:?}, want 'kill' or 'detach'")),
        }
        self.last_status.remove(pane_id);
        Ok(())
    }

    /// "Send pane → new tab": break a pane out of its current window into a brand
    /// new window (tmux `break-pane`), which the cockpit reconciles into a new tab.
    /// The pane keeps running. Returns the new `#{window_id}` so the caller can
    /// switch the active tab to it. (No `-d`: tmux focuses the new window; the
    /// cockpit's active tab is frontend-driven and follows on reconcile.) Only
    /// meaningful when the source tab has >1 pane — the frontend gates on that.
    pub fn break_pane(&mut self, pane_id: &str) -> Result<String, String> {
        let out = tmux::tmux_ok(&["break-pane", "-s", pane_id, "-P", "-F", "#{window_id}"])?;
        Ok(out.trimmed())
    }

    // ── F3/F4: launch ────────────────────────────────────────────────────────

    /// Launch Claude Code in a pane: `cd <cwd> && COCKPIT_PANE_ID=<pane> claude
    /// [--model X] [flags]` then Enter. cwd/flags are shell-quoted. NEVER an
    /// API-key flag (user is on Claude Max).
    pub fn launch_cc(
        &mut self,
        pane_id: &str,
        cwd: &str,
        model: Option<&str>,
        flags: Option<&str>,
    ) -> Result<(), String> {
        let mut cmd = format!(
            "cd {cwd} && COCKPIT_PANE_ID={pane} claude",
            cwd = tmux::shq(cwd),
            pane = pane_id
        );
        if let Some(m) = model {
            cmd.push_str(" --model ");
            cmd.push_str(&tmux::shq(m));
        }
        if let Some(f) = flags {
            let f = f.trim();
            if !f.is_empty() {
                // Reject any attempt to smuggle an api-key flag.
                if f.contains("api-key") || f.contains("apiKey") || f.contains("ANTHROPIC_API_KEY") {
                    return Err("api-key flags are not permitted".into());
                }
                cmd.push(' ');
                cmd.push_str(f);
            }
        }
        self.run_line_in_pane(pane_id, &cmd)
    }

    /// Launch a plain shell command-line context in a pane: just `cd <cwd>`.
    pub fn launch_shell(&mut self, pane_id: &str, cwd: &str) -> Result<(), String> {
        let cmd = format!("cd {}", tmux::shq(cwd));
        self.run_line_in_pane(pane_id, &cmd)
    }

    /// Launch `claude --agent <name>` in a pane (P2-F4 launch-from-inventory).
    /// The agent name is config-derived (a `~/.claude/agents/*.md` `name`), so we
    /// validate it against a strict charset AND shell-quote it before it reaches
    /// the command line — a name can never inject an extra flag or shell payload.
    pub fn launch_agent(&mut self, pane_id: &str, cwd: &str, agent: &str) -> Result<(), String> {
        if !is_valid_agent_name(agent) {
            return Err(format!("invalid agent name: {agent}"));
        }
        let cmd = format!(
            "cd {cwd} && COCKPIT_PANE_ID={pane} claude --agent {agent}",
            cwd = tmux::shq(cwd),
            pane = pane_id,
            agent = tmux::shq(agent),
        );
        self.run_line_in_pane(pane_id, &cmd)
    }

    /// Type a command line into a pane and press Enter. Uses the control client
    /// (literal send-keys) then a hex CR so the data path is identical to the
    /// frontend's keystrokes.
    fn run_line_in_pane(&mut self, pane_id: &str, line: &str) -> Result<(), String> {
        let cc = self.client_mut()?;
        cc.pane_send_keys(pane_id, line).map_err(|e| e.to_string())?;
        cc.pane_send_keys_hex(pane_id, &[0x0d])
            .map_err(|e| e.to_string())
    }

    /// Public entry to type a full command line + Enter atomically (one control-
    /// client round-trip). Used by the file-tree `cd` so the line and its CR can't
    /// be split into two racy IPC calls.
    pub fn pane_run_line(&mut self, pane_id: &str, line: &str) -> Result<(), String> {
        self.run_line_in_pane(pane_id, line)
    }

    // ── F5: raw IO ───────────────────────────────────────────────────────────

    pub fn pane_send_keys(&mut self, pane_id: &str, data: &str) -> Result<(), String> {
        self.client_mut()?
            .pane_send_keys(pane_id, data)
            .map_err(|e| e.to_string())
    }

    pub fn pane_resize(&mut self, pane_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        self.client_mut()?
            .pane_resize(pane_id, cols, rows)
            .map_err(|e| e.to_string())
    }

    /// Size the whole window to the grid bounding box + re-tile (multi-pane safe).
    /// `layout` is validated to a known tmux layout name so a bad value can't be
    /// injected into the control stream.
    pub fn set_grid(
        &mut self,
        window_id: &str,
        cols: u16,
        rows: u16,
        layout: &str,
    ) -> Result<(), String> {
        // "none" = resize-only (no select-layout): preserves a manual split.
        let layout = match layout {
            "tiled" | "even-horizontal" | "even-vertical" | "main-horizontal"
            | "main-vertical" | "none" => layout,
            _ => "tiled",
        };
        // Clamp to sane bounds; tmux rejects absurd sizes and a 0 would be invalid.
        let cols = cols.clamp(20, 2000);
        let rows = rows.clamp(5, 500);
        self.client_mut()?
            .set_grid(window_id, cols, rows, layout)
            .map_err(|e| e.to_string())
    }

    pub fn interrupt_pane(&mut self, pane_id: &str) -> Result<(), String> {
        self.client_mut()?
            .interrupt_pane(pane_id)
            .map_err(|e| e.to_string())
    }

    // ── State snapshot ───────────────────────────────────────────────────────

    /// Full snapshot of tabs + panes (each pane's cwd + status), derived live
    /// from tmux. tmux is the source of truth; the in-memory model only caches
    /// last-status for change detection.
    pub fn list_state(&self) -> Result<CockpitState, String> {
        let tabs = self.collect_tabs()?;
        let panes = self.collect_panes()?;
        Ok(CockpitState {
            socket: tmux::SOCKET.into(),
            session: SESSION.into(),
            tabs,
            panes,
        })
    }

    fn collect_tabs(&self) -> Result<Vec<TabInfo>, String> {
        // One line per window: id, index, name, layout.
        let out = tmux::tmux(&[
            "list-windows",
            "-t",
            SESSION,
            "-F",
            "#{window_id}\t#{window_index}\t#{window_name}\t#{window_layout}",
        ])?;
        if !out.ok() {
            return Ok(vec![]);
        }
        // Pre-fetch pane->window mapping for pane_ids per tab.
        let pane_map = self.window_to_panes()?;
        let mut tabs = vec![];
        for line in out.stdout.lines() {
            let mut it = line.split('\t');
            let win = it.next().unwrap_or("").to_string();
            let index: u32 = it.next().unwrap_or("0").parse().unwrap_or(0);
            let name = it.next().unwrap_or("").to_string();
            let layout = it.next().unwrap_or("").to_string();
            let pane_ids = pane_map.get(&win).cloned().unwrap_or_default();
            let geometry = crate::layout::parse_window_layout(&layout);
            tabs.push(TabInfo {
                tab_id: tab_id_for_index(index),
                tmux_window_id: win,
                index,
                name,
                layout,
                geometry,
                pane_ids,
            });
        }
        Ok(tabs)
    }

    fn window_to_panes(&self) -> Result<HashMap<String, Vec<String>>, String> {
        let out = tmux::tmux(&[
            "list-panes",
            "-s",
            "-t",
            SESSION,
            "-F",
            "#{window_id}\t#{pane_id}",
        ])?;
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        if out.ok() {
            for line in out.stdout.lines() {
                let mut it = line.split('\t');
                let win = it.next().unwrap_or("").to_string();
                let pane = it.next().unwrap_or("").to_string();
                if is_window_id(&win) && is_pane_id(&pane) {
                    map.entry(win).or_default().push(pane);
                }
            }
        }
        Ok(map)
    }

    fn collect_panes(&self) -> Result<Vec<PaneInfo>, String> {
        // Session-wide pane list with the fields the frontend + poller need.
        let out = tmux::tmux(&[
            "list-panes",
            "-s",
            "-t",
            SESSION,
            "-F",
            "#{pane_id}\t#{window_index}\t#{pane_current_path}\t#{pane_title}\t#{pane_dead}",
        ])?;
        if !out.ok() {
            return Ok(vec![]);
        }
        let mut panes = vec![];
        for line in out.stdout.lines() {
            let mut it = line.split('\t');
            let pane = it.next().unwrap_or("").to_string();
            let win_index: u32 = it.next().unwrap_or("0").parse().unwrap_or(0);
            let cwd = it.next().unwrap_or("").to_string();
            let title = it.next().unwrap_or("").to_string();
            let dead = it.next().unwrap_or("0") == "1";
            if !is_pane_id(&pane) {
                continue; // mangled/fused line (e.g. C-locale tab sanitization)
            }
            let status = self
                .last_status
                .get(&pane)
                .copied()
                .unwrap_or(if dead { Status::Dead } else { Status::Unknown });
            panes.push(PaneInfo {
                pane_id: pane,
                tab_id: tab_id_for_index(win_index),
                cwd,
                title,
                dead,
                status: status.as_str().into(),
                ambiguous: status == Status::Unknown,
            });
        }
        Ok(panes)
    }

    /// The tmux layout string of the window that owns `pane_id`.
    fn window_layout_for_pane(&self, pane_id: &str) -> Option<String> {
        let out = tmux::tmux(&[
            "display-message",
            "-p",
            "-t",
            pane_id,
            "-F",
            "#{window_layout}",
        ])
        .ok()?;
        if out.ok() {
            Some(out.trimmed())
        } else {
            None
        }
    }

    // ── Status polling support ───────────────────────────────────────────────

    /// One poll pass: for each live pane whose activity advanced, capture +
    /// classify. Returns the (pane, payload) pairs whose status CHANGED since
    /// last poll (so the caller emits only on change). Updates last_status.
    pub fn poll_statuses(&mut self) -> Vec<PaneStatusPayload> {
        let rows = match self.pane_poll_rows() {
            Ok(r) => r,
            Err(_) => return vec![],
        };
        let mut changed = vec![];
        for row in rows {
            let new_status = if row.dead {
                Status::Dead
            } else {
                // capture-pane is light; we capture every live pane and let the
                // classifier + change-gate decide what to emit. The activity age
                // (when known) feeds the idle-debounce in the heuristic.
                let text = self.capture_pane(&row.pane_id).unwrap_or_default();
                classify(false, &text, row.activity_age_secs)
            };
            let prev = self.last_status.get(&row.pane_id).copied();
            if prev != Some(new_status) {
                self.last_status.insert(row.pane_id.clone(), new_status);
                changed.push(PaneStatusPayload {
                    pane_id: row.pane_id,
                    status: new_status.as_str().into(),
                    ambiguous: new_status == Status::Unknown,
                    // recencyMs: 0 when the activity timestamp is unavailable on
                    // this tmux build (the frontend reads 0 as "unknown recency").
                    recency_ms: row
                        .activity_age_secs
                        .map(|s| s.saturating_mul(1000))
                        .unwrap_or(0),
                });
            }
        }
        changed
    }

    /// Drop cached status for a pane (e.g. after it is removed) so a future
    /// reincarnation of the id re-emits.
    pub fn forget_pane(&mut self, pane_id: &str) {
        self.last_status.remove(pane_id);
    }

    fn pane_poll_rows(&self) -> Result<Vec<PollRow>, String> {
        // #{pane_dead} + #{pane_activity} (epoch of last activity). NOTE: on some
        // tmux builds (e.g. 3.6b observed here) pane_activity is EMPTY — we treat
        // that as "age unknown" (None) and capture unconditionally. When present
        // it may be seconds or milliseconds; we autodetect and normalise.
        let out = tmux::tmux(&[
            "list-panes",
            "-s",
            "-t",
            SESSION,
            "-F",
            "#{pane_id}\t#{pane_dead}\t#{pane_activity}",
        ])?;
        if !out.ok() {
            return Ok(vec![]);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut rows = vec![];
        for line in out.stdout.lines() {
            let mut it = line.split('\t');
            let pane = it.next().unwrap_or("").to_string();
            let dead = it.next().unwrap_or("0") == "1";
            let activity_raw = it.next().unwrap_or("").trim();
            if !is_pane_id(&pane) {
                continue; // mangled/fused line (e.g. C-locale tab sanitization)
            }
            let age = activity_epoch_secs(activity_raw).map(|epoch| now.saturating_sub(epoch));
            rows.push(PollRow {
                pane_id: pane,
                dead,
                activity_age_secs: age,
            });
        }
        Ok(rows)
    }

    fn capture_pane(&self, pane_id: &str) -> Result<String, String> {
        let out = tmux::tmux(&[
            "capture-pane",
            "-p",
            "-e",
            "-S",
            "-120",
            "-t",
            pane_id,
        ])?;
        if out.ok() {
            Ok(out.stdout)
        } else {
            Err(out.stderr)
        }
    }

    /// Warm-start replay: capture the pane's current screen + full scrollback
    /// WITH escape sequences (`-e`) so colors/cursor styling survive, then return
    /// the raw bytes base64-encoded. The control client only streams `%output`
    /// produced AFTER it attaches, so a pane the GUI re-attaches to (e.g. after a
    /// window close+reopen) would otherwise paint blank — this replays whatever
    /// is already on screen. `-S -` includes the whole scrollback history.
    ///
    /// The capture is split at the pane's real height (`compose_warm_start`) so
    /// the visible grid is replayed grid-exactly with tmux's cursor, instead of
    /// landing wherever the scrollback write happened to end (bug #4). If the
    /// geometry query fails we fall back to the old edge-trimmed whole-capture
    /// replay — approximate, but never blank.
    ///
    /// Distinct from `capture_pane` (the poller's plain, bounded capture used for
    /// status classification): warm-start is escape-aware and unbounded.
    pub fn warm_start(&self, pane_id: &str) -> Result<String, String> {
        let out = tmux::tmux(&[
            "capture-pane",
            "-p",
            "-e",
            "-S",
            "-",
            "-t",
            pane_id,
        ])?;
        if !out.ok() {
            return Err(out.stderr.trim().to_string());
        }
        // One query for both halves of the split: height picks the viewport,
        // cursor re-asserts the caret inside it.
        let replay = match pane_numbers(pane_id, "#{pane_height} #{cursor_x} #{cursor_y}", 3) {
            Some(v) => compose_warm_start(&out.stdout, v[0] as usize, Some((v[1], v[2]))),
            None => trim_blank_edges(&out.stdout).replace('\n', "\r\n"),
        };
        Ok(B64.encode(replay.as_bytes()))
    }

    /// Like `warm_start` but the VISIBLE grid ONLY (no `-S -`, no scrollback),
    /// verbatim (no edge-trim) and with tmux's real cursor position re-asserted.
    /// Used by the post-resize resync (bug #11, revisit garble): a single-shot
    /// `resize-window` at tab switch makes the pane's TUI repaint into an xterm
    /// that is still at its OLD size (`term.resize` lags via the debounced
    /// %layout-change → refreshState round-trip), so the xterm buffer diverges
    /// from tmux's grid and — for a differential renderer like Claude Code —
    /// stays diverged until a full repaint. tmux's own grid is clean (= what
    /// Ctrl+L shows); this replays exactly that, without keystroke injection.
    pub fn warm_start_screen(&self, pane_id: &str) -> Result<String, String> {
        let out = tmux::tmux(&["capture-pane", "-p", "-e", "-t", pane_id])?;
        if !out.ok() {
            return Err(out.stderr.trim().to_string());
        }
        // Best-effort cursor: a failed query just skips the CUP.
        let cursor = pane_cursor(pane_id);
        Ok(B64.encode(compose_screen_replay(&out.stdout, cursor).as_bytes()))
    }

    /// Tear down: detach the control client, then kill the cockpit session on the
    /// PRIVATE socket only. NEVER `kill-server` (would also kill the default
    /// socket's server if mis-targeted — we always pass -L cockpit, but kill the
    /// session specifically to be safe).
    pub fn teardown(&mut self) {
        if let Some(mut c) = self.client.take() {
            c.shutdown();
        }
        // kill-session on the private socket. If it was the last session the
        // server exits on its own; we never call kill-server.
        let _ = tmux::tmux(&["kill-session", "-t", SESSION]);
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

struct PollRow {
    pane_id: String,
    dead: bool,
    /// Seconds since the pane was last active, or None if tmux didn't report a
    /// usable `#{pane_activity}` value on this build.
    activity_age_secs: Option<u64>,
}

/// Normalise a raw `#{pane_activity}` field to an epoch in SECONDS, or None if
/// it's empty / unparseable / implausible. Autodetects ms vs s: a value past
/// the year-5138 mark in seconds (>1e11) is treated as milliseconds.
fn activity_epoch_secs(raw: &str) -> Option<u64> {
    let v: u64 = raw.parse().ok()?;
    if v == 0 {
        return None;
    }
    if v > 100_000_000_000 {
        Some(v / 1000)
    } else {
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_id_roundtrip() {
        assert_eq!(tab_id_for_index(0), "tab-0");
        assert_eq!(tab_id_for_index(7), "tab-7");
    }

    #[test]
    fn pane_id_validation() {
        assert!(is_pane_id("%0"));
        assert!(is_pane_id("%137"));
        assert!(!is_pane_id("%"));
        assert!(!is_pane_id("0"));
        assert!(!is_pane_id(""));
        // A C-locale tmux sanitizes the tab delimiters in list-panes output to
        // `_`, fusing the whole line into one string. That garbage must never
        // be served to the frontend as a pane id.
        assert!(!is_pane_id("%2_2_/_example-host.local_0"));
        // Nor a line whose tabs survived but split failed upstream somehow.
        assert!(!is_pane_id("%2\t2\t/"));
    }

    #[test]
    fn window_id_validation() {
        assert!(is_window_id("@0"));
        assert!(is_window_id("@42"));
        // Reject anything that isn't `@<digits>` so it can't smuggle a tmux flag
        // or a stale index-style target into the kill argv.
        assert!(!is_window_id("@"));
        assert!(!is_window_id("0"));
        assert!(!is_window_id("tab-1"));
        assert!(!is_window_id("cockpit-main:1"));
        assert!(!is_window_id("@1 -k"));
        assert!(!is_window_id("@-1"));
        assert!(!is_window_id(""));
    }

    #[test]
    fn missing_target_is_idempotent_close() {
        // The exact tmux stderr the close used to choke on — now a no-op success.
        assert!(is_missing_target("can't find window: 1"));
        assert!(is_missing_target("can't find pane %9"));
        assert!(is_missing_target("can't find window @7"));
        // A real failure (e.g. server gone) must still propagate.
        assert!(!is_missing_target("no server running on /tmp/tmux/cockpit"));
        assert!(!is_missing_target("server exited unexpectedly"));
    }

    #[test]
    fn agent_name_validation_blocks_injection() {
        assert!(is_valid_agent_name("dev-agent"));
        assert!(is_valid_agent_name("qa_agent.v2"));
        // Anything with shell metacharacters / spaces / flags is rejected.
        assert!(!is_valid_agent_name(""));
        assert!(!is_valid_agent_name("dev --dangerously-skip-permissions"));
        assert!(!is_valid_agent_name("a; rm -rf ~"));
        assert!(!is_valid_agent_name("a$(id)"));
        assert!(!is_valid_agent_name("a`whoami`"));
        assert!(!is_valid_agent_name("a b"));
    }

    #[test]
    fn activity_epoch_normalises() {
        assert_eq!(activity_epoch_secs(""), None);
        assert_eq!(activity_epoch_secs("0"), None);
        assert_eq!(activity_epoch_secs("1781869938"), Some(1781869938)); // seconds
        assert_eq!(activity_epoch_secs("1781869938370"), Some(1781869938)); // ms -> s
    }

    #[test]
    fn trim_blank_edges_drops_fresh_pane_padding() {
        // Fresh pane: 1 leading blank, prompt, 45 trailing blanks (the void).
        let mut s = String::from("\nuser@host repo % ");
        for _ in 0..45 {
            s.push('\n');
        }
        // Blank LINES are dropped; the prompt line keeps its trailing space
        // (that's where the cursor sits — only edge blank rows are noise).
        assert_eq!(trim_blank_edges(&s), "user@host repo % ");
    }

    #[test]
    fn trim_blank_edges_keeps_interior_blanks() {
        assert_eq!(trim_blank_edges("\n\na\n\nb\n\n"), "a\n\nb");
    }

    #[test]
    fn trim_blank_edges_all_blank_is_empty() {
        assert_eq!(trim_blank_edges("\n\n\n"), "");
        assert_eq!(trim_blank_edges("\x1b[0m\n  \n"), "");
    }

    #[test]
    fn compose_screen_replay_converts_lf_and_restores_cursor() {
        // capture-pane joins rows with bare \n; xterm has no convertEol, so a
        // bare LF would stairstep (next row starts at the previous row's end
        // column). Rows must be rejoined with \r\n. capture-pane's own trailing
        // \n is NOT a row separator — it is dropped (bug #5, the void). The
        // cursor lands wherever the write ends, so tmux's real cursor position
        // is re-asserted with a 1-based CUP.
        let out = compose_screen_replay("a\nb\n", Some((3, 1)));
        assert_eq!(out, "a\r\nb\x1b[2;4H");
    }

    #[test]
    fn compose_screen_replay_keeps_leading_blank_rows() {
        // The visible grid is replayed VERBATIM after a clear: a blank top row
        // is a real grid row (e.g. a TUI's margin) — trimming it would shift
        // every subsequent row up and misalign the next differential frame.
        // (trim_blank_edges is for the scrollback warm_start only.)
        let out = compose_screen_replay("\n\nprompt %\n", None);
        assert_eq!(out, "\r\n\r\nprompt %");
    }

    #[test]
    fn compose_screen_replay_never_scrolls_full_grid() {
        // Bug #5 (the void): a full-height grid replayed with R line endings
        // scrolls the xterm one row — the top row leaves for scrollback and the
        // bottom row is blank. R rows must produce exactly R-1 line endings.
        const R: usize = 24;
        let capture = (0..R).map(|i| format!("row{i}")).collect::<Vec<_>>().join("\n") + "\n";
        let out = compose_screen_replay(&capture, None);
        assert_eq!(out.matches("\r\n").count(), R - 1);
        assert!(!out.ends_with('\n'));
        assert!(out.starts_with("row0\r\n"));
        assert!(out.ends_with("row23"));
    }

    #[test]
    fn compose_screen_replay_without_trailing_newline_is_unchanged() {
        // Only ONE trailing \n is stripped, and only if present: a capture that
        // already ends on its last row keeps every row.
        let out = compose_screen_replay("a\nb", Some((0, 0)));
        assert_eq!(out, "a\r\nb\x1b[1;1H");
        // Two trailing newlines = one real trailing blank row + the separator.
        let out = compose_screen_replay("a\nb\n\n", None);
        assert_eq!(out, "a\r\nb\r\n");
    }

    #[test]
    fn compose_warm_start_splits_history_and_grid() {
        // 5 captured lines, pane_height 3 -> h1,h2 are scrollback history and
        // g1,g2,g3 are the visible grid. History is joined with \r\n and
        // terminated so the grid starts on its own row; the grid goes through
        // compose_screen_replay (no trailing newline) with the real cursor.
        let capture = "h1\nh2\ng1\ng2\ng3\n";
        let out = compose_warm_start(capture, 3, Some((2, 2)));
        assert_eq!(out, "h1\r\nh2\r\ng1\r\ng2\r\ng3\x1b[3;3H");
    }

    #[test]
    fn compose_warm_start_short_capture_is_all_grid() {
        // Fewer captured lines than the pane is tall (fresh pane, no history):
        // everything is grid, nothing is trimmed, no history terminator.
        let out = compose_warm_start("a\nb\n", 24, Some((1, 1)));
        assert_eq!(out, "a\r\nb\x1b[2;2H");
        // Exactly pane_height lines -> still all grid.
        let out = compose_warm_start("a\nb\nc\n", 3, None);
        assert_eq!(out, "a\r\nb\r\nc");
    }

    #[test]
    fn compose_warm_start_trims_history_blanks_keeps_grid_blanks() {
        // History edge blanks are the fresh-pane padding void — trimmed.
        // Grid blanks are real rows of the viewport — kept verbatim, including
        // a leading blank row, or every row below shifts up.
        let capture = "\n\nreal history\n\n\n\ntop\n\nbottom\n";
        let out = compose_warm_start(capture, 4, None);
        assert_eq!(out, "real history\r\n\r\ntop\r\n\r\nbottom");
        // An all-blank history contributes nothing at all (no stray newline).
        let out = compose_warm_start("\n\n\ngrid\n", 1, None);
        assert_eq!(out, "grid");
    }

    #[test]
    fn compose_warm_start_never_scrolls() {
        // The grid half must emit exactly H-1 line endings so the H visible
        // rows fill the H-row viewport without scrolling it (bug #5 again) —
        // INCLUDING the blank rows below the prompt, which are real viewport
        // rows. Only the history's edges may be trimmed; treating the whole
        // capture as one blob (the old warm start) eats those grid blanks and
        // the prompt lands H-2 rows too low.
        const H: usize = 24;
        const GRID_BLANKS: usize = 22; // fresh-ish pane: prompt + padding
        let mut capture = String::from("h0\nh1\n\n\n\n\n"); // 6 history rows, 4 blank
        capture.push_str("g0\ng1"); // first 2 of the H grid rows
        capture.push_str(&"\n".repeat(GRID_BLANKS)); // GRID_BLANKS blank rows
        capture.push('\n'); // capture-pane's terminator
        let out = compose_warm_start(&capture, H, None);
        // 2 kept history rows (1 separator) + 1 terminator + (H-1) grid rows.
        assert_eq!(out.matches("\r\n").count(), 1 + 1 + H - 1);
        // History edge blanks gone; grid blanks all present, in order.
        assert_eq!(
            out,
            format!("h0\r\nh1\r\ng0\r\ng1{}", "\r\n".repeat(GRID_BLANKS))
        );
    }

    #[test]
    fn parse_u32_fields_needs_every_field() {
        assert_eq!(parse_u32_fields("3 7\n", 2), Some(vec![3, 7]));
        assert_eq!(parse_u32_fields(" 24 0 12 \n", 3), Some(vec![24, 0, 12]));
        // Short, non-numeric, or empty replies (older tmux, dead pane) -> None,
        // so the caller falls back instead of guessing.
        assert_eq!(parse_u32_fields("3", 2), None);
        assert_eq!(parse_u32_fields("", 1), None);
        assert_eq!(parse_u32_fields("x y", 2), None);
        // Extra trailing fields are ignored, not an error.
        assert_eq!(parse_u32_fields("1 2 3", 2), Some(vec![1, 2]));
    }

    #[test]
    fn server_gone_detection() {
        // Real admin-path failures that warrant a reconnect.
        assert!(is_server_gone(
            "tmux [\"new-window\"] failed (exit 1): no server running on /private/tmp/tmux-501/cockpit"
        ));
        assert!(is_server_gone("can't find session: cockpit-main"));
        assert!(is_server_gone("server exited unexpectedly"));
        // NOT server-gone: a stale target on a live server — reconnect won't help.
        assert!(!is_server_gone("can't find window: cockpit-main:7"));
        assert!(!is_server_gone("can't find pane: %42"));
        assert!(!is_server_gone("duplicate session: cockpit-main"));
    }
}

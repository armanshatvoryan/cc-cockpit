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

    /// Inspect a tab's live panes (for close confirmation). On `force`, kill it.
    pub fn close_tab(&mut self, tab_id: &str, force: bool) -> Result<CloseTabResult, String> {
        let win_index = parse_tab_index(tab_id)?;
        let target = format!("{SESSION}:{win_index}");

        // List live (non-dead) panes in the tab.
        let live = self.live_panes_in_window(&target)?;

        if !force && !live.is_empty() {
            // Frontend should confirm; do NOT kill yet.
            return Ok(CloseTabResult {
                ok: false,
                live_panes: live,
            });
        }

        tmux::tmux_ok(&["kill-window", "-t", &target])?;
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
        let out = tmux::tmux_ok(&[
            "split-window",
            flag,
            "-t",
            pane_id,
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

    /// Type a command line into a pane and press Enter. Uses the control client
    /// (literal send-keys) then a hex CR so the data path is identical to the
    /// frontend's keystrokes.
    fn run_line_in_pane(&mut self, pane_id: &str, line: &str) -> Result<(), String> {
        let cc = self.client_mut()?;
        cc.pane_send_keys(pane_id, line).map_err(|e| e.to_string())?;
        cc.pane_send_keys_hex(pane_id, &[0x0d])
            .map_err(|e| e.to_string())
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
            tabs.push(TabInfo {
                tab_id: tab_id_for_index(index),
                tmux_window_id: win,
                index,
                name,
                layout,
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
                if !win.is_empty() && !pane.is_empty() {
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
            if pane.is_empty() {
                continue;
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
            if pane.is_empty() {
                continue;
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
        if out.ok() {
            Ok(B64.encode(trim_blank_edges(&out.stdout).as_bytes()))
        } else {
            Err(out.stderr.trim().to_string())
        }
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

/// Parse `tab-<n>` -> n. Errors on a malformed tab id.
fn parse_tab_index(tab_id: &str) -> Result<u32, String> {
    tab_id
        .strip_prefix("tab-")
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| format!("bad tab id {tab_id:?}, want tab-<n>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_id_roundtrip() {
        assert_eq!(tab_id_for_index(0), "tab-0");
        assert_eq!(parse_tab_index("tab-7").unwrap(), 7);
        assert!(parse_tab_index("window-3").is_err());
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
        let mut s = String::from("\narmanshatvoran@host repo % ");
        for _ in 0..45 {
            s.push('\n');
        }
        // Blank LINES are dropped; the prompt line keeps its trailing space
        // (that's where the cursor sits — only edge blank rows are noise).
        assert_eq!(trim_blank_edges(&s), "armanshatvoran@host repo % ");
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

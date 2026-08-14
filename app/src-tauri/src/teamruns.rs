//! Live team runs (P3 step 3 — the team board) — a READ-ONLY view of native
//! Agent Teams sessions currently (or recently) on disk.
//!
//! Native owns the runtime: when a lead spins up teammates (`teammateMode=tmux`),
//! it writes a session dir under `~/.claude/teams/session-<id>/` with a
//! `config.json` (lead + member roster, each with `tmuxPaneId`/`backendType`/
//! `cwd`/`model`) and a `inboxes/<role>.json` file mailbox, plus a task list at
//! `~/.claude/tasks/session-<id>/`. The socket spike (2026-06-21) confirmed the
//! teammate panes live on the SAME tmux socket as the lead — so for a team the
//! cockpit launched, every `tmuxPaneId` is an ordinary pane on `-L cockpit` (no
//! bridge needed); the board can link a row straight to its pane.
//!
//! This module just READS those files into typed rows for the board. It writes
//! nothing and spawns nothing. Native rotates/cleans these dirs (R-CLEANUP), so
//! the reader is fully fault-tolerant: a missing/rotated/garbled file degrades to
//! an empty list or a single `parseError` row — never a panic, never a blank UI.
//!
//! Defensive parsing (R-FILE-SCHEMA-DRIFT): we read via `serde_json::Value` with
//! `.get(...)` lookups, not a rigid `#[derive(Deserialize)]` struct, so an added
//! or renamed native field can't break the whole read.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde_json::Value;

/// One member (the lead or a teammate) of a live team run.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TeamMember {
    /// `"po@session-…"` / `"team-lead@session-…"`.
    pub agent_id: String,
    /// Short role/name, e.g. `worker`, `team-lead`.
    pub name: String,
    /// The agent type backing the role, e.g. `dev-agent`, `team-lead`.
    pub agent_type: String,
    /// Derived display mode: `tmux` → `"live"`, `in-process` → `"headless"`,
    /// else the raw backend string.
    pub mode: String,
    /// Raw native backend: `"tmux" | "in-process"`.
    pub backend_type: String,
    /// `"%1"` (a real pane on the lead's socket), `"leader"`, or absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Raw native `prompt` (the task the teammate was spun up with), if any.
    /// Trimmed; an empty/whitespace-only prompt is treated as absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Derived display summary of `prompt` (first sentence of the first line,
    /// trimmed to ~80 chars) — the pane-label fallback for a generic
    /// `agentType` like `general-purpose`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_summary: Option<String>,
    /// Native `isActive` flag (the lead is implicitly active).
    pub is_active: bool,
    /// True for the member whose `agentId == leadAgentId`.
    pub is_lead: bool,
}

/// One live (or recent) team session.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TeamRun {
    /// The session dir name, e.g. `session-41b57ff1` — stable id.
    pub session_id: String,
    /// Team name from config (falls back to the session id).
    pub name: String,
    pub lead_agent_id: String,
    /// Native epoch-ms create time, if present (used for newest-first ordering).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    /// Epoch-ms mtime of `config.json` — the last time this session wrote. The
    /// board uses it to protect an actively-writing session from cleanup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<u64>,
    pub members: Vec<TeamMember>,
    /// Total undelivered messages summed across `inboxes/*.json`.
    pub inbox_depth: usize,
    /// Count of task entries under `~/.claude/tasks/<session_id>/` (dotfiles excluded).
    pub task_count: usize,
    /// Set when `config.json` couldn't be read/parsed — surfaced as a `!PARSE` row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
}

// ── Entry points ──────────────────────────────────────────────────────────────

/// Resolve `$HOME`, then read every team run, newest first.
pub fn load_team_runs() -> Result<Vec<TeamRun>, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME not set".to_string())?;
    Ok(load_team_runs_at(&home))
}

/// Testable core: read all `~/.claude/teams/session-*/` under an injectable
/// `home`. Never fails as a whole; sorted newest-first by `createdAt` (then id).
pub fn load_team_runs_at(home: &Path) -> Vec<TeamRun> {
    let teams_dir = home.join(".claude").join("teams");
    let tasks_dir = home.join(".claude").join("tasks");
    let mut runs = Vec::new();

    let Ok(entries) = fs::read_dir(&teams_dir) else {
        return runs;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let session_id = entry.file_name().to_string_lossy().to_string();
        if !session_id.starts_with("session-") {
            continue;
        }
        runs.push(read_one_run(&path, &session_id, &tasks_dir));
    }

    // Newest first: createdAt desc, then session_id desc as a stable tiebreak.
    runs.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.session_id.cmp(&a.session_id))
    });
    runs
}

fn read_one_run(dir: &Path, session_id: &str, tasks_dir: &Path) -> TeamRun {
    let mut run = TeamRun {
        session_id: session_id.to_string(),
        name: session_id.to_string(),
        lead_agent_id: String::new(),
        created_at: None,
        modified_at: None,
        members: Vec::new(),
        inbox_depth: 0,
        task_count: 0,
        parse_error: None,
    };

    let config_path = dir.join("config.json");
    // Stat before parsing so even a garbled-config row carries an mtime (a broken
    // dir is still cleanable, and cleanup wants the freshness signal).
    run.modified_at = fs::metadata(&config_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|mt| mt.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64);
    let text = match fs::read_to_string(&config_path) {
        Ok(t) => t,
        Err(e) => {
            run.parse_error = Some(format!("config.json read failed: {e}"));
            return run;
        }
    };
    let cfg: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            run.parse_error = Some(format!("config.json invalid JSON: {e}"));
            return run;
        }
    };

    if let Some(n) = cfg.get("name").and_then(Value::as_str) {
        run.name = n.to_string();
    }
    let lead = cfg
        .get("leadAgentId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    run.lead_agent_id = lead.clone();
    run.created_at = cfg.get("createdAt").and_then(Value::as_u64);

    if let Some(members) = cfg.get("members").and_then(Value::as_array) {
        for m in members {
            run.members.push(read_member(m, &lead));
        }
    }

    run.inbox_depth = inbox_depth(&dir.join("inboxes"));
    run.task_count = task_count(&tasks_dir.join(session_id));
    run
}

fn read_member(m: &Value, lead_agent_id: &str) -> TeamMember {
    let s = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_string);
    let agent_id = s("agentId").unwrap_or_default();
    let backend_type = s("backendType").unwrap_or_else(|| "in-process".into());
    let mode = match backend_type.as_str() {
        "tmux" => "live",
        "in-process" => "headless",
        other => other,
    }
    .to_string();
    // Trim, and drop an empty/whitespace-only prompt to `None` — treat it the
    // same as a missing field (schema-drift safe, no blank tooltips).
    let prompt = s("prompt").and_then(|p| {
        let t = p.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    });
    let task_summary = prompt.as_deref().and_then(derive_task_summary);
    TeamMember {
        is_lead: !agent_id.is_empty() && agent_id == lead_agent_id,
        is_active: m
            .get("isActive")
            .and_then(Value::as_bool)
            // The lead has no isActive flag; treat it as active.
            .unwrap_or_else(|| !agent_id.is_empty() && agent_id == lead_agent_id),
        name: s("name").unwrap_or_default(),
        agent_type: s("agentType").unwrap_or_default(),
        mode,
        backend_type,
        tmux_pane_id: s("tmuxPaneId"),
        model: s("model"),
        cwd: s("cwd"),
        color: s("color"),
        prompt,
        task_summary,
        agent_id,
    }
}

/// Max chars of a derived `task_summary` (a `…` is appended when the source
/// text is cut, so a full result is at most this many chars).
const SUMMARY_MAX_CHARS: usize = 80;

/// Derive a short pane-label summary from a raw teammate `prompt`: the first
/// sentence of the first line (a `.`/`!`/`?` not at position 0, so it doesn't
/// cut on an "e.g." abbreviation at the very start), else the whole first
/// line — either way trimmed to `SUMMARY_MAX_CHARS`. `None` only if the first
/// line is itself empty (the caller already filters blank prompts, but this
/// stays defensive for a prompt that's all newlines).
fn derive_task_summary(prompt: &str) -> Option<String> {
    let first_line = prompt.split('\n').next().unwrap_or("").trim();
    if first_line.is_empty() {
        return None;
    }
    let candidate = match first_line.find(['.', '!', '?']) {
        Some(i) if i > 0 => first_line[..=i].trim(),
        _ => first_line,
    };
    Some(truncate_chars(candidate, SUMMARY_MAX_CHARS))
}

/// Truncate to at most `max` chars (UTF-8 safe, via `chars()`), appending `…`
/// in place of the last char when a cut happens.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", truncated.trim_end())
}

/// Sum of message-array lengths across `inboxes/*.json`. A bad/empty inbox file
/// contributes 0 (never errors the whole run).
fn inbox_depth(inboxes_dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(inboxes_dir) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            if let Ok(Value::Array(msgs)) = serde_json::from_str::<Value>(&text) {
                total += msgs.len();
            }
        }
    }
    total
}

/// Count of task entries (any non-dot file/dir) under the run's tasks dir.
fn task_count(tasks_session_dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(tasks_session_dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            !e.file_name()
                .to_string_lossy()
                .starts_with('.')
        })
        .count()
}

// ── Cleanup (the ONE write path — delete dead run dirs) ───────────────────────
//
// The board is otherwise read-only; this is the sole mutation. It is deliberately
// paranoid: the frontend hands us a list of session ids to drop, but we NEVER
// trust it blindly. Each id is re-validated here (shape + containment) and each
// run is re-checked against a freshness guard so an actively-writing session
// (including the cockpit user's own current session) can never be deleted out
// from under a live Claude process.

/// A session id is safe to act on only if it is a plain `session-*` dir name with
/// no path-traversal or separator tricks. Rejects `..`, `/`, `\`, NUL, bare
/// `session-`.
fn valid_session_id(id: &str) -> bool {
    id.starts_with("session-")
        && id.len() > "session-".len()
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains("..")
        && !id.contains('\0')
}

/// True when the run's `config.json` was modified within `fresh` of now — i.e. a
/// session that is (or just was) actively writing. Such runs are PROTECTED from
/// deletion. A missing/unstattable config is treated as NOT fresh (a run with no
/// live config is junk, safe to drop).
fn is_fresh(config_path: &Path, fresh: Duration) -> bool {
    fs::metadata(config_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|mt| SystemTime::now().duration_since(mt).ok())
        .map(|age| age < fresh)
        .unwrap_or(false)
}

/// Delete the given team-run session dirs under `home` — both the
/// `~/.claude/teams/session-<id>/` config dir and the matching
/// `~/.claude/tasks/session-<id>/` task dir. Returns the ids actually deleted.
///
/// Safety guards, applied per id (a rejected id is silently skipped, never an
/// error — the caller only learns which ids were removed):
///   * `valid_session_id` — shape + no traversal.
///   * containment — the resolved teams target must sit directly under the teams
///     dir (belt-and-suspenders on top of id validation).
///   * freshness — skip any run whose `config.json` mtime is within `fresh`.
///
/// Never touches anything outside the teams/tasks roots. Absent target → skipped.
pub fn cleanup_team_runs_at(
    home: &Path,
    ids: &[String],
    fresh: Duration,
) -> Result<Vec<String>, String> {
    let teams_dir = home.join(".claude").join("teams");
    let tasks_dir = home.join(".claude").join("tasks");
    let mut deleted = Vec::new();

    for id in ids {
        if !valid_session_id(id) {
            continue;
        }
        let teams_target = teams_dir.join(id);
        // Containment belt: parent must be exactly the teams dir.
        if teams_target.parent() != Some(teams_dir.as_path()) {
            continue;
        }
        if is_fresh(&teams_target.join("config.json"), fresh) {
            continue; // an actively-writing session — protected
        }
        // The teams dir is the source of truth; only claim a delete if it went.
        if fs::remove_dir_all(&teams_target).is_ok() {
            let _ = fs::remove_dir_all(tasks_dir.join(id)); // best-effort sibling
            deleted.push(id.clone());
        }
    }
    Ok(deleted)
}

/// Resolve `$HOME`, then delete the given runs. `fresh` is fixed at 10 minutes:
/// long enough to cover a session mid-write, short enough that genuinely dead
/// runs are always removable.
pub fn cleanup_team_runs(ids: &[String]) -> Result<Vec<String>, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME not set".to_string())?;
    cleanup_team_runs_at(&home, ids, Duration::from_secs(600))
}

// ── Tests (sandbox-only; never touch the real ~/.claude) ──────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct Sandbox {
        root: PathBuf,
    }
    impl Sandbox {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("cockpit-teamrun-test-{tag}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Sandbox { root }
        }
        fn home(&self) -> PathBuf {
            self.root.join("home")
        }
        fn write(&self, rel: &str, contents: &str) {
            let p = self.root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, contents).unwrap();
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    // The exact 2-member shape captured live from the socket spike.
    const LIVE_CONFIG: &str = r#"{
      "name": "session-41b57ff1",
      "createdAt": 1781988000000,
      "leadAgentId": "team-lead@session-41b57ff1",
      "leadSessionId": "41b57ff1-aaaa",
      "members": [
        { "agentId": "team-lead@session-41b57ff1", "name": "team-lead",
          "agentType": "team-lead", "joinedAt": 1781988000000,
          "tmuxPaneId": "leader", "cwd": "/Users/x/Workflows",
          "subscriptions": [], "backendType": "in-process" },
        { "agentId": "worker@session-41b57ff1", "name": "worker", "color": "blue",
          "joinedAt": 1781988050000, "tmuxPaneId": "%1", "subscriptions": [],
          "agentType": "dev-agent", "model": "claude-sonnet-4-6",
          "cwd": "/Users/x/Workflows", "backendType": "tmux", "isActive": false }
      ]
    }"#;

    #[test]
    fn reads_live_two_member_run_with_derived_modes() {
        let sb = Sandbox::new("live");
        sb.write("home/.claude/teams/session-41b57ff1/config.json", LIVE_CONFIG);
        sb.write("home/.claude/teams/session-41b57ff1/inboxes/worker.json", "[]");
        sb.write("home/.claude/teams/session-41b57ff1/inboxes/team-lead.json", "[]");

        let runs = load_team_runs_at(&sb.home());
        assert_eq!(runs.len(), 1);
        let r = &runs[0];
        assert!(r.parse_error.is_none());
        assert_eq!(r.session_id, "session-41b57ff1");
        assert_eq!(r.name, "session-41b57ff1");
        assert_eq!(r.members.len(), 2);

        let lead = r.members.iter().find(|m| m.name == "team-lead").unwrap();
        assert!(lead.is_lead);
        assert!(lead.is_active, "lead has no isActive flag → treated active");
        assert_eq!(lead.mode, "headless"); // in-process
        assert_eq!(lead.tmux_pane_id.as_deref(), Some("leader"));

        let worker = r.members.iter().find(|m| m.name == "worker").unwrap();
        assert!(!worker.is_lead);
        assert!(!worker.is_active);
        assert_eq!(worker.mode, "live"); // tmux → live
        assert_eq!(worker.backend_type, "tmux");
        assert_eq!(worker.tmux_pane_id.as_deref(), Some("%1"));
        assert_eq!(worker.agent_type, "dev-agent");
        assert_eq!(worker.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(worker.color.as_deref(), Some("blue"));
    }

    #[test]
    fn counts_inbox_depth_and_tasks() {
        let sb = Sandbox::new("counts");
        sb.write("home/.claude/teams/session-aaa/config.json", LIVE_CONFIG);
        // 2 pending messages for worker, 1 for lead = depth 3
        sb.write(
            "home/.claude/teams/session-aaa/inboxes/worker.json",
            r#"[{"id":1},{"id":2}]"#,
        );
        sb.write(
            "home/.claude/teams/session-aaa/inboxes/team-lead.json",
            r#"[{"id":3}]"#,
        );
        // tasks: 2 real files + a dotfile that must NOT count
        sb.write("home/.claude/tasks/session-aaa/task-1.json", "{}");
        sb.write("home/.claude/tasks/session-aaa/task-2.json", "{}");
        sb.write("home/.claude/tasks/session-aaa/.lock", "");

        let runs = load_team_runs_at(&sb.home());
        let r = &runs[0];
        assert_eq!(r.inbox_depth, 3);
        assert_eq!(r.task_count, 2);
    }

    #[test]
    fn newest_first_by_created_at() {
        let sb = Sandbox::new("order");
        sb.write(
            "home/.claude/teams/session-old/config.json",
            r#"{"name":"old","createdAt":1000,"leadAgentId":"l@old","members":[]}"#,
        );
        sb.write(
            "home/.claude/teams/session-new/config.json",
            r#"{"name":"new","createdAt":2000,"leadAgentId":"l@new","members":[]}"#,
        );
        let runs = load_team_runs_at(&sb.home());
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].name, "new"); // newest first
        assert_eq!(runs[1].name, "old");
    }

    #[test]
    fn malformed_config_becomes_parse_error_not_panic() {
        let sb = Sandbox::new("malformed");
        sb.write("home/.claude/teams/session-bad/config.json", "{ not json");
        let runs = load_team_runs_at(&sb.home());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].session_id, "session-bad");
        assert!(runs[0].parse_error.as_deref().unwrap().contains("invalid JSON"));
    }

    #[test]
    fn lead_only_run_loads_clean() {
        // The common case: teams enabled, no teammate spawned (just the lead).
        let sb = Sandbox::new("leadonly");
        sb.write(
            "home/.claude/teams/session-solo/config.json",
            r#"{"name":"session-solo","createdAt":5,"leadAgentId":"team-lead@session-solo","members":[{"agentId":"team-lead@session-solo","name":"team-lead","agentType":"team-lead","tmuxPaneId":"leader","backendType":"in-process"}]}"#,
        );
        let runs = load_team_runs_at(&sb.home());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].members.len(), 1);
        assert!(runs[0].members[0].is_lead);
        assert_eq!(runs[0].inbox_depth, 0);
        assert_eq!(runs[0].task_count, 0);
    }

    #[test]
    fn absent_teams_dir_yields_empty_not_error() {
        let sb = Sandbox::new("none");
        let runs = load_team_runs_at(&sb.home());
        assert!(runs.is_empty());
    }

    /// Real-target smoke: read the actual `~/.claude/teams/`. Not a sandbox —
    /// `#[ignore]` by default; run with `cargo test --lib real_corpus -- --ignored
    /// --nocapture`. Asserts only that the read never panics; prints what it found
    /// so a human can eyeball the live shape (the team board's data source).
    #[test]
    #[ignore]
    fn real_corpus_smoke() {
        let runs = load_team_runs().expect("HOME set");
        eprintln!("== {} live team run(s) in ~/.claude/teams ==", runs.len());
        for r in &runs {
            eprintln!(
                "  {} \"{}\"  members={} inbox={} tasks={} {}",
                r.session_id,
                r.name,
                r.members.len(),
                r.inbox_depth,
                r.task_count,
                r.parse_error.as_deref().unwrap_or("")
            );
            for m in &r.members {
                eprintln!(
                    "      - {:<12} {:<20} {:<8} pane={}",
                    m.name,
                    m.agent_type,
                    m.mode,
                    m.tmux_pane_id.as_deref().unwrap_or("—")
                );
            }
        }
    }

    // ── read_member: prompt / task_summary parsing ──────────────────────────

    fn member_with_prompt(prompt: Value) -> Value {
        serde_json::json!({
            "agentId": "worker@session-x",
            "name": "worker",
            "agentType": "general-purpose",
            "backendType": "tmux",
            "tmuxPaneId": "%1",
            "prompt": prompt,
        })
    }

    #[test]
    fn task_summary_takes_first_sentence_of_first_line() {
        let m = member_with_prompt(serde_json::json!(
            "Fix the OAuth timeout bug in the login flow. Also check the refresh path.\n\nMore context below."
        ));
        let tm = read_member(&m, "lead@session-x");
        assert_eq!(
            tm.prompt.as_deref(),
            Some("Fix the OAuth timeout bug in the login flow. Also check the refresh path.\n\nMore context below.")
        );
        assert_eq!(
            tm.task_summary.as_deref(),
            Some("Fix the OAuth timeout bug in the login flow.")
        );
    }

    #[test]
    fn task_summary_and_prompt_absent_when_prompt_field_missing() {
        let m = serde_json::json!({
            "agentId": "worker@session-x", "name": "worker",
            "agentType": "general-purpose", "backendType": "tmux", "tmuxPaneId": "%1",
        });
        let tm = read_member(&m, "lead@session-x");
        assert!(tm.prompt.is_none());
        assert!(tm.task_summary.is_none());
    }

    #[test]
    fn task_summary_and_prompt_absent_when_prompt_is_not_a_string() {
        // Schema drift: a malformed/mistyped `prompt` (e.g. native ships a
        // number or object) must degrade to None, never panic.
        let m = member_with_prompt(serde_json::json!(12345));
        let tm = read_member(&m, "lead@session-x");
        assert!(tm.prompt.is_none());
        assert!(tm.task_summary.is_none());

        let m_obj = member_with_prompt(serde_json::json!({"nested": "value"}));
        let tm_obj = read_member(&m_obj, "lead@session-x");
        assert!(tm_obj.prompt.is_none());
        assert!(tm_obj.task_summary.is_none());
    }

    #[test]
    fn task_summary_and_prompt_absent_for_blank_prompt() {
        let m = member_with_prompt(serde_json::json!("   \n\t  "));
        let tm = read_member(&m, "lead@session-x");
        assert!(tm.prompt.is_none());
        assert!(tm.task_summary.is_none());
    }

    #[test]
    fn task_summary_truncates_long_terminator_less_first_line() {
        let long = "a".repeat(120);
        let m = member_with_prompt(serde_json::json!(long));
        let tm = read_member(&m, "lead@session-x");
        let summary = tm.task_summary.expect("summary present");
        assert_eq!(summary.chars().count(), 80);
        assert_eq!(summary.chars().last(), Some('…'));
        assert_eq!(
            summary.chars().take(79).collect::<String>(),
            "a".repeat(79)
        );
    }

    #[test]
    fn task_summary_leading_punctuation_does_not_cut_at_position_zero() {
        // A first line that STARTS with a sentence-ending char (e.g. "! ...")
        // must not cut to a 1-char summary — the `i > 0` guard falls back to
        // the whole first line instead.
        let m = member_with_prompt(serde_json::json!("! urgent: fix prod outage now"));
        let tm = read_member(&m, "lead@session-x");
        assert_eq!(tm.task_summary.as_deref(), Some("! urgent: fix prod outage now"));
    }

    #[test]
    fn ignores_non_session_dirs_and_files() {
        let sb = Sandbox::new("junk");
        sb.write("home/.claude/teams/session-ok/config.json",
            r#"{"name":"ok","createdAt":1,"leadAgentId":"l","members":[]}"#);
        sb.write("home/.claude/teams/README.md", "not a session");
        fs::create_dir_all(sb.root.join("home/.claude/teams/random-dir")).unwrap();
        let runs = load_team_runs_at(&sb.home());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].name, "ok");
    }

    // ── cleanup_team_runs_at ──────────────────────────────────────────────────

    /// `fresh = ZERO` makes nothing count as "fresh" (age < 0 is impossible), so
    /// every well-formed id is deletable — the deterministic delete path.
    const DELETE_ALL: Duration = Duration::ZERO;

    fn exists(sb: &Sandbox, rel: &str) -> bool {
        sb.root.join(rel).exists()
    }

    #[test]
    fn cleanup_removes_both_teams_and_tasks_dirs() {
        let sb = Sandbox::new("cln-both");
        sb.write("home/.claude/teams/session-dead/config.json", "{}");
        sb.write("home/.claude/tasks/session-dead/task-1.json", "{}");

        let deleted =
            cleanup_team_runs_at(&sb.home(), &["session-dead".into()], DELETE_ALL).unwrap();

        assert_eq!(deleted, vec!["session-dead".to_string()]);
        assert!(!exists(&sb, "home/.claude/teams/session-dead"));
        assert!(!exists(&sb, "home/.claude/tasks/session-dead"));
    }

    #[test]
    fn cleanup_protects_a_freshly_written_run() {
        let sb = Sandbox::new("cln-fresh");
        sb.write("home/.claude/teams/session-live/config.json", "{}");

        // A 1-hour freshness window: the file we just wrote is well within it, so
        // the run is protected — this is the guard for an active session (incl.
        // the cockpit user's own current session).
        let deleted = cleanup_team_runs_at(
            &sb.home(),
            &["session-live".into()],
            Duration::from_secs(3600),
        )
        .unwrap();

        assert!(deleted.is_empty());
        assert!(exists(&sb, "home/.claude/teams/session-live"));
    }

    #[test]
    fn cleanup_deletes_a_configless_stub() {
        // No config.json → cannot be "fresh" → junk, removable even under a wide
        // freshness window.
        let sb = Sandbox::new("cln-nocfg");
        fs::create_dir_all(sb.root.join("home/.claude/teams/session-stub")).unwrap();

        let deleted = cleanup_team_runs_at(
            &sb.home(),
            &["session-stub".into()],
            Duration::from_secs(3600),
        )
        .unwrap();

        assert_eq!(deleted, vec!["session-stub".to_string()]);
        assert!(!exists(&sb, "home/.claude/teams/session-stub"));
    }

    #[test]
    fn cleanup_rejects_traversal_and_malformed_ids() {
        let sb = Sandbox::new("cln-evil");
        sb.write("home/.claude/teams/session-ok/config.json", "{}");
        // A sentinel OUTSIDE the teams dir that a traversal id would target.
        fs::create_dir_all(sb.root.join("home/.claude/secrets")).unwrap();

        let evil = vec![
            "../secrets".to_string(),
            "session-../secrets".to_string(),
            "session-ok/../../secrets".to_string(),
            "session-".to_string(),
            "not-a-session".to_string(),
            "session-a\0b".to_string(),
        ];
        let deleted = cleanup_team_runs_at(&sb.home(), &evil, DELETE_ALL).unwrap();

        assert!(deleted.is_empty());
        assert!(exists(&sb, "home/.claude/secrets"));
        assert!(exists(&sb, "home/.claude/teams/session-ok"));
    }

    #[test]
    fn cleanup_only_touches_named_ids_and_skips_missing() {
        let sb = Sandbox::new("cln-scope");
        sb.write("home/.claude/teams/session-a/config.json", "{}");
        sb.write("home/.claude/teams/session-b/config.json", "{}");

        // Ask to delete `a` and a nonexistent `ghost`; `b` must survive untouched.
        let deleted = cleanup_team_runs_at(
            &sb.home(),
            &["session-a".into(), "session-ghost".into()],
            DELETE_ALL,
        )
        .unwrap();

        assert_eq!(deleted, vec!["session-a".to_string()]);
        assert!(!exists(&sb, "home/.claude/teams/session-a"));
        assert!(exists(&sb, "home/.claude/teams/session-b"));
    }
}

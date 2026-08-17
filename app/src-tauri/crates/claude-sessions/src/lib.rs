//! Claude Code session discovery.
//!
//! Two independent readers, both pure functions over a directory root so they
//! are testable without touching the real `~/.claude`:
//!
//!   1. [`read_pane_map`] — the tmux-pane → session-id map written by the
//!      `cockpit-session-map` SessionStart/SessionEnd hook. This is how the
//!      cockpit learns which Claude session is running in which pane; Claude
//!      does not announce its session id, so the hook pushes it out instead.
//!
//!   2. [`index_sessions`] — an index over `~/.claude/projects/<dir>/<uuid>.jsonl`.
//!
//! ## Why the project dir name is never decoded
//!
//! `~/.claude/projects/` encodes a cwd by replacing `/` with `-`, which is NOT
//! reversible: `-Users-me-Workflows-cc-cockpit` could be `.../Workflows/cc-cockpit`
//! or `.../Workflows/cc/cockpit`, and hyphenated directory names are the norm
//! here. So the cwd is read from the `cwd` field INSIDE the transcript, which is
//! authoritative, and the directory name is kept only as an opaque grouping key.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One pane→session entry, as written by the hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSession {
    /// tmux pane id including the sigil, e.g. `%56`.
    pub tmux_pane: String,
    /// The Claude Code session UUID.
    pub session_id: String,
    /// cwd Claude was launched in.
    pub cwd: String,
    /// Absolute path to the session transcript `.jsonl`.
    pub transcript_path: String,
    /// PID of the tmux SERVER that owned this pane.
    ///
    /// tmux pane ids (`%N`) are monotonic per server but restart from a low
    /// number when the server dies, so `%56` after a restart is a DIFFERENT
    /// pane than `%56` before it. Without this field a stale entry would show a
    /// confidently wrong session id in the toolbar.
    pub tmux_server_pid: String,
    /// RFC3339-ish timestamp the entry was written.
    pub started_at: String,
}

/// Read the pane→session map, keyed by tmux pane id (`%56`).
///
/// Entries are dropped when they came from a different tmux server than the one
/// now running (see [`PaneSession::tmux_server_pid`]). Unreadable or malformed
/// files are skipped individually — one bad file must not blank the whole map.
pub fn read_pane_map(dir: &Path, live_server_pid: &str) -> HashMap<String, PaneSession> {
    let mut out = HashMap::new();
    // A missing map dir is the normal cold-start state, not an error.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(session) = serde_json::from_str::<PaneSession>(&text) else {
            continue; // one half-written file must not blank the map
        };
        if session.tmux_server_pid != live_server_pid {
            continue; // left over from a previous tmux server — `%N` is reused
        }
        out.insert(session.tmux_pane.clone(), session);
    }
    out
}

/// One row of the transcript index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRow {
    /// Session UUID (the transcript filename stem).
    pub session_id: String,
    /// cwd read from inside the transcript. Empty when the transcript has none.
    pub cwd: String,
    /// The opaque `~/.claude/projects/` directory name. NOT a decoded path.
    pub project_dir: String,
    /// First real user prompt, trimmed to a single line. Empty when none found.
    pub first_prompt: String,
    /// Transcript mtime, epoch seconds.
    pub modified_epoch: u64,
    /// Transcript size in bytes.
    pub bytes: u64,
}

/// Index every `<projects_root>/<dir>/<uuid>.jsonl`, newest first.
///
/// Transcripts are stat-ed, sorted and TRUNCATED to `limit` before any of them
/// is opened — there are ~1600 of them on a working machine and only the kept
/// rows need their `cwd` / first prompt read.
pub fn index_sessions(projects_root: &Path, limit: usize) -> Vec<SessionRow> {
    struct Candidate {
        path: std::path::PathBuf,
        project_dir: String,
        session_id: String,
        modified_epoch: u64,
        bytes: u64,
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    let Ok(dirs) = std::fs::read_dir(projects_root) else {
        return Vec::new();
    };
    for dir in dirs.flatten() {
        if !dir.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let project_dir = dir.file_name().to_string_lossy().to_string();
        let Ok(files) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(stem) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
            else {
                continue;
            };
            let Ok(meta) = file.metadata() else {
                continue;
            };
            let modified_epoch = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            candidates.push(Candidate {
                path,
                project_dir: project_dir.clone(),
                session_id: stem,
                modified_epoch,
                bytes: meta.len(),
            });
        }
    }

    // Newest first; session id as a stable tiebreak so equal mtimes don't
    // shuffle between calls.
    candidates.sort_by(|a, b| {
        b.modified_epoch
            .cmp(&a.modified_epoch)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    candidates.truncate(limit);

    candidates
        .into_iter()
        .map(|c| {
            let (cwd, first_prompt) = scan_transcript(&c.path);
            SessionRow {
                session_id: c.session_id,
                cwd,
                project_dir: c.project_dir,
                first_prompt,
                modified_epoch: c.modified_epoch,
                bytes: c.bytes,
            }
        })
        .collect()
}

/// Pull `cwd` and the first user prompt out of a transcript.
///
/// Bounded on both axes: only the head of the file is scanned, and absurdly long
/// lines (base64 image blocks) are skipped rather than parsed.
fn scan_transcript(path: &Path) -> (String, String) {
    use std::io::{BufRead, BufReader};

    /// Both facts live in the opening records; scanning further is wasted IO.
    const MAX_LINES: usize = 400;
    const MAX_LINE_BYTES: usize = 64 * 1024;

    let mut cwd = String::new();
    let mut first_prompt = String::new();

    let Ok(file) = std::fs::File::open(path) else {
        return (cwd, first_prompt);
    };
    for line in BufReader::new(file)
        .lines()
        .take(MAX_LINES)
        .map_while(Result::ok)
    {
        if line.len() > MAX_LINE_BYTES {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if cwd.is_empty() {
            if let Some(found) = value.get("cwd").and_then(|c| c.as_str()) {
                cwd = found.to_string();
            }
        }
        if first_prompt.is_empty() && value.get("type").and_then(|t| t.as_str()) == Some("user") {
            if let Some(text) = user_text(&value) {
                first_prompt = text;
            }
        }
        if !cwd.is_empty() && !first_prompt.is_empty() {
            break;
        }
    }
    (cwd, first_prompt)
}

/// Openers that mark a `type: "user"` record as machinery rather than something
/// the human typed: slash-command wrappers, injected context, agent-to-agent
/// traffic, and the auto-continuation nudge. Measured against the real
/// transcript store — without this filter the majority of rows show plumbing.
const SYNTHETIC_PROMPT_PREFIXES: &[&str] = &[
    "<local-command-caveat>",
    "<command-name>",
    "<command-message>",
    "<command-args>",
    "<system-reminder>",
    "[MESSAGE FROM NON-USER SOURCE",
    "[Your previous response had no visible output",
    "Caveat: The messages below",
];

/// The prompt text of a `type: "user"` record, flattened to one short line.
///
/// `message.content` is either a bare string or an array of blocks, so both
/// shapes are handled. Returns `None` for synthetic records so the caller keeps
/// scanning for a real prompt — a blank summary beats a wrapper tag.
fn user_text(value: &serde_json::Value) -> Option<String> {
    const MAX_CHARS: usize = 200;

    let content = value.get("message")?.get("content")?;
    let raw = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .and_then(|b| b.get("text").and_then(|t| t.as_str()))
            .map(|s| s.to_string())?,
        _ => return None,
    };

    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    if SYNTHETIC_PROMPT_PREFIXES
        .iter()
        .any(|prefix| flat.starts_with(prefix))
    {
        return None;
    }
    Some(flat.chars().take(MAX_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Same throwaway-root pattern the teamruns tests use — no tempfile dep.
    struct Sandbox {
        root: PathBuf,
    }
    impl Sandbox {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("cockpit-sessions-test-{tag}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Sandbox { root }
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

    fn entry(pane: &str, session: &str, pid: &str) -> String {
        format!(
            r#"{{"tmux_pane":"{pane}","session_id":"{session}","cwd":"/w",
                 "transcript_path":"/t/{session}.jsonl","tmux_server_pid":"{pid}",
                 "started_at":"2026-08-17T12:00:00Z"}}"#
        )
    }

    #[test]
    fn maps_a_live_pane_to_its_session_id() {
        let sb = Sandbox::new("live");
        sb.write("map/56.json", &entry("%56", "aaaa-1111", "1497"));

        let map = read_pane_map(&sb.root.join("map"), "1497");

        assert_eq!(map.len(), 1);
        assert_eq!(map["%56"].session_id, "aaaa-1111");
    }

    #[test]
    fn drops_entries_left_behind_by_a_dead_tmux_server() {
        let sb = Sandbox::new("stale");
        sb.write("map/56.json", &entry("%56", "old-session", "1111"));
        sb.write("map/57.json", &entry("%57", "new-session", "2222"));

        let map = read_pane_map(&sb.root.join("map"), "2222");

        assert!(!map.contains_key("%56"), "stale server entry must not survive");
        assert_eq!(map["%57"].session_id, "new-session");
    }

    #[test]
    fn one_corrupt_file_does_not_blank_the_map() {
        let sb = Sandbox::new("corrupt");
        sb.write("map/56.json", "{ this is not json");
        sb.write("map/57.json", &entry("%57", "good", "1497"));

        let map = read_pane_map(&sb.root.join("map"), "1497");

        assert_eq!(map.len(), 1);
        assert_eq!(map["%57"].session_id, "good");
    }

    #[test]
    fn missing_map_dir_is_empty_not_an_error() {
        let sb = Sandbox::new("nodir");
        let map = read_pane_map(&sb.root.join("does-not-exist"), "1497");
        assert!(map.is_empty());
    }

    #[test]
    fn reads_cwd_from_inside_the_transcript_not_the_directory_name() {
        let sb = Sandbox::new("cwd");
        // Directory name is ambiguous on purpose: hyphens that are really path
        // separators, next to hyphens that are part of a real folder name.
        sb.write(
            "projects/-Users-me-Workflows-cc-cockpit/uuid-1.jsonl",
            "{\"type\":\"mode\",\"mode\":\"normal\"}\n\
             {\"cwd\":\"/Users/me/Workflows/cc-cockpit\",\"type\":\"user\"}\n",
        );

        let rows = index_sessions(&sb.root.join("projects"), 10);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, "uuid-1");
        assert_eq!(rows[0].cwd, "/Users/me/Workflows/cc-cockpit");
        assert_eq!(rows[0].project_dir, "-Users-me-Workflows-cc-cockpit");
    }

    #[test]
    fn first_prompt_is_the_first_real_user_message() {
        let sb = Sandbox::new("prompt");
        sb.write(
            "projects/p/uuid-2.jsonl",
            "{\"type\":\"mode\",\"mode\":\"normal\"}\n\
             {\"type\":\"assistant\",\"message\":{\"content\":[]}}\n\
             {\"type\":\"user\",\"cwd\":\"/w\",\"message\":{\"role\":\"user\",\"content\":\"fix the booking form\"}}\n\
             {\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"second one\"}}\n",
        );

        let rows = index_sessions(&sb.root.join("projects"), 10);

        assert_eq!(rows[0].first_prompt, "fix the booking form");
    }

    #[test]
    fn skips_slash_command_wrappers_to_reach_the_real_prompt() {
        let sb = Sandbox::new("wrapper");
        sb.write(
            "projects/p/uuid-4.jsonl",
            "{\"type\":\"user\",\"message\":{\"content\":\"<local-command-caveat>Caveat: the messages below were generated while running local commands.</local-command-caveat>\"}}\n\
             {\"type\":\"user\",\"message\":{\"content\":\"research what makes a good website\"}}\n",
        );

        let rows = index_sessions(&sb.root.join("projects"), 10);

        assert_eq!(rows[0].first_prompt, "research what makes a good website");
    }

    #[test]
    fn skips_injected_non_user_messages() {
        let sb = Sandbox::new("injected");
        sb.write(
            "projects/p/uuid-5.jsonl",
            "{\"type\":\"user\",\"message\":{\"content\":\"[MESSAGE FROM NON-USER SOURCE - NOT USER INPUT] Hello memory agent\"}}\n\
             {\"type\":\"user\",\"message\":{\"content\":\"<system-reminder>background context</system-reminder>\"}}\n\
             {\"type\":\"user\",\"message\":{\"content\":\"actually fix the booking form\"}}\n",
        );

        let rows = index_sessions(&sb.root.join("projects"), 10);

        assert_eq!(rows[0].first_prompt, "actually fix the booking form");
    }

    #[test]
    fn an_all_noise_transcript_reports_no_prompt_rather_than_noise() {
        let sb = Sandbox::new("allnoise");
        sb.write(
            "projects/p/uuid-6.jsonl",
            "{\"type\":\"user\",\"message\":{\"content\":\"<system-reminder>only noise here</system-reminder>\"}}\n",
        );

        let rows = index_sessions(&sb.root.join("projects"), 10);

        assert_eq!(rows[0].first_prompt, "", "a wrapper is worse than a blank");
    }

    #[test]
    fn newest_transcript_sorts_first() {
        let sb = Sandbox::new("sort");
        sb.write("projects/p/older.jsonl", "{\"cwd\":\"/w\"}\n");
        // Give the second file a clearly later mtime.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        sb.write("projects/p/newer.jsonl", "{\"cwd\":\"/w\"}\n");

        let rows = index_sessions(&sb.root.join("projects"), 10);

        assert_eq!(rows[0].session_id, "newer");
        assert_eq!(rows[1].session_id, "older");
    }

    #[test]
    fn limit_caps_the_row_count() {
        let sb = Sandbox::new("limit");
        for i in 0..5 {
            sb.write(&format!("projects/p/s{i}.jsonl"), "{\"cwd\":\"/w\"}\n");
        }

        let rows = index_sessions(&sb.root.join("projects"), 2);

        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn non_jsonl_files_are_ignored() {
        let sb = Sandbox::new("ext");
        sb.write("projects/p/notes.md", "hello");
        sb.write("projects/p/uuid-3.jsonl", "{\"cwd\":\"/w\"}\n");

        let rows = index_sessions(&sb.root.join("projects"), 10);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, "uuid-3");
    }
}

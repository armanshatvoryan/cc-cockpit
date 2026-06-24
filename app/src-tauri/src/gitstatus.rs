//! Per-worktree git status (Terax Tier-1 C/dev#2).
//!
//! A tiny, read-only probe: run `git -C <cwd> status --porcelain=v2 --branch`
//! via `std::process::Command` (NOT the shell plugin — this is a structural read,
//! not a user-launched command) and fold the porcelain-v2 output into a compact
//! `GitStatus` the tab bar renders as `branch ● ↑ahead ↓behind`.
//!
//! Design rules:
//!   * **Never panic.** A non-repo cwd, a missing dir, or a git error degrades to
//!     `Ok(None)` (exit≠0 ⇒ "no status"); the only `Err` is `git` failing to spawn
//!     (e.g. not installed), which the frontend can surface once.
//!   * **Pure parser, then thin command.** `parse_porcelain_v2` is total over any
//!     `&str` so it's unit-tested in isolation (TDD) without spawning git.
//!
//! Porcelain v2 (`--branch`) we care about:
//!   * `# branch.head <name>`   → branch (literally `(detached)` when detached)
//!   * `# branch.ab +A -B`      → ahead / behind (absent when no upstream ⇒ 0/0)
//!   * `1 …` / `2 …` / `u …`    → a tracked change (staged/unstaged/renamed/unmerged)
//!   * `? …`                    → an untracked path
//!   * `! …` and other `# …`    → ignored

use serde::Serialize;

/// Compact git status for one worktree. `dirty` is the convenience "worktree is
/// not clean" flag; the raw `changed`/`untracked` counts are exposed so the
/// frontend can recompute its own badge logic if it wants finer detail.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    /// `# branch.head` value (a branch name, or `(detached)` when detached HEAD).
    pub branch: String,
    /// Commits ahead of upstream (0 when no upstream / no `# branch.ab`).
    pub ahead: u32,
    /// Commits behind upstream (0 when no upstream / no `# branch.ab`).
    pub behind: u32,
    /// `changed > 0 || untracked > 0` — i.e. the worktree is not clean.
    pub dirty: bool,
    /// Count of tracked changes: `1` (ordinary), `2` (renamed/copied), `u` (unmerged).
    pub changed: u32,
    /// Count of untracked paths (`?` lines).
    pub untracked: u32,
}

/// Fold porcelain-v2 (`--branch`) output into a `GitStatus`. Total over any input:
/// unknown lines are ignored, missing branch/ab default to empty/0.
pub fn parse_porcelain_v2(out: &str) -> GitStatus {
    let mut branch = String::new();
    let mut ahead = 0u32;
    let mut behind = 0u32;
    let mut changed = 0u32;
    let mut untracked = 0u32;

    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            branch = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            // Format: "+A -B". Tolerate missing/garbled tokens (default 0).
            for tok in rest.split_whitespace() {
                if let Some(n) = tok.strip_prefix('+') {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = tok.strip_prefix('-') {
                    behind = n.parse().unwrap_or(0);
                }
            }
        } else if line.starts_with("1 ") || line.starts_with("2 ") || line.starts_with("u ") {
            changed += 1;
        } else if line.starts_with("? ") {
            untracked += 1;
        }
        // `! ` (ignored) and other `# ` header lines are intentionally skipped.
    }

    GitStatus {
        branch,
        ahead,
        behind,
        dirty: changed > 0 || untracked > 0,
        changed,
        untracked,
    }
}

/// Probe one worktree's git status. `Ok(None)` when `cwd` is not a git repo (git
/// exits non-zero — e.g. exit 128 "not a git repository", or a missing dir);
/// `Err` only when `git` itself can't be spawned (e.g. not installed). Never
/// panics. Runs `git -C <cwd> status --porcelain=v2 --branch` directly via
/// `std::process::Command` — this is a structural read, not a user command, so it
/// bypasses the shell plugin.
#[tauri::command]
pub fn git_status_snapshot(cwd: String) -> Result<Option<GitStatus>, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&cwd)
        .args(["status", "--porcelain=v2", "--branch"])
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;

    if !output.status.success() {
        // Not a repo (exit 128 / "not a git repository"), missing dir, or any
        // other git error ⇒ "no status" rather than a hard failure.
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(Some(parse_porcelain_v2(&stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_repo_is_not_dirty() {
        let out = "\
# branch.oid 1111111111111111111111111111111111111111
# branch.head main
# branch.upstream origin/main
# branch.ab +0 -0
";
        let s = parse_porcelain_v2(out);
        assert_eq!(
            s,
            GitStatus {
                branch: "main".into(),
                ahead: 0,
                behind: 0,
                dirty: false,
                changed: 0,
                untracked: 0,
            }
        );
    }

    #[test]
    fn changed_entries_count_as_dirty() {
        // One ordinary unstaged mod, one staged mod, one rename ⇒ 3 changed.
        let out = "\
# branch.oid 1111111111111111111111111111111111111111
# branch.head main
# branch.upstream origin/main
# branch.ab +0 -0
1 .M N... 100644 100644 100644 aaaa aaaa src/main.rs
1 M. N... 100644 100644 100644 bbbb cccc README.md
2 R. N... 100644 100644 100644 dddd eeee R100 new.rs\told.rs
";
        let s = parse_porcelain_v2(out);
        assert_eq!(s.branch, "main");
        assert_eq!(s.changed, 3);
        assert_eq!(s.untracked, 0);
        assert!(s.dirty);
    }

    #[test]
    fn untracked_only_is_dirty() {
        let out = "\
# branch.oid 1111111111111111111111111111111111111111
# branch.head main
# branch.ab +0 -0
? newfile.txt
? logs/another.log
";
        let s = parse_porcelain_v2(out);
        assert_eq!(s.changed, 0);
        assert_eq!(s.untracked, 2);
        assert!(s.dirty);
    }

    #[test]
    fn ahead_behind_parsed() {
        let out = "\
# branch.oid 1111111111111111111111111111111111111111
# branch.head feature
# branch.upstream origin/feature
# branch.ab +2 -3
";
        let s = parse_porcelain_v2(out);
        assert_eq!(s.branch, "feature");
        assert_eq!(s.ahead, 2);
        assert_eq!(s.behind, 3);
        assert!(!s.dirty);
    }

    #[test]
    fn detached_head_reports_detached_branch() {
        let out = "\
# branch.oid 1111111111111111111111111111111111111111
# branch.head (detached)
";
        let s = parse_porcelain_v2(out);
        assert_eq!(s.branch, "(detached)");
        assert_eq!(s.ahead, 0);
        assert_eq!(s.behind, 0);
        assert!(!s.dirty);
    }

    #[test]
    fn unmerged_lines_count_as_changed() {
        let out = "\
# branch.head main
# branch.ab +0 -0
u UU N... 100644 100644 100644 100644 aaaa bbbb cccc conflict.rs
";
        let s = parse_porcelain_v2(out);
        assert_eq!(s.changed, 1);
        assert!(s.dirty);
    }

    #[test]
    fn empty_input_is_clean_with_empty_branch() {
        let s = parse_porcelain_v2("");
        assert_eq!(s.branch, "");
        assert!(!s.dirty);
        assert_eq!(s.changed, 0);
        assert_eq!(s.untracked, 0);
    }
}

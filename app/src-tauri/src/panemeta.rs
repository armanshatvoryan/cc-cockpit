//! Free pane-name inputs: which project/branch a local pane is in, and which
//! host an `ssh` pane is connected to.
//!
//! Both are read without spawning git or touching the network:
//!   * **Repo + branch** — walk up from the pane's cwd to the nearest `.git`
//!     (a dir, or a worktree's `gitdir:` file) and read its `HEAD`.
//!   * **ssh host** — the destination argument of the pane's `ssh` process,
//!     from one `ps` listing shared by every ssh pane in a refresh.
//!
//! Never panics; anything unreadable degrades to "no info".

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The repo a cwd sits in: root folder name + checked-out branch (a short sha
/// when HEAD is detached).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoHead {
    pub project: String,
    pub branch: String,
}

/// Nearest enclosing git repo of `cwd`, or None outside any repo.
pub fn repo_head(cwd: &Path) -> Option<RepoHead> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let dotgit = d.join(".git");
        if let Some(git_dir) = resolve_git_dir(&dotgit, d) {
            let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
            let project = d.file_name()?.to_string_lossy().into_owned();
            return Some(RepoHead {
                project,
                branch: parse_head(&head)?,
            });
        }
        dir = d.parent();
    }
    None
}

/// `.git` is the git dir itself, or (worktree / submodule) a file pointing at it.
fn resolve_git_dir(dotgit: &Path, root: &Path) -> Option<PathBuf> {
    if dotgit.is_dir() {
        return Some(dotgit.to_path_buf());
    }
    let text = fs::read_to_string(dotgit).ok()?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    let p = Path::new(target);
    Some(if p.is_absolute() { p.to_path_buf() } else { root.join(p) })
}

/// `ref: refs/heads/<branch>` → branch; a bare sha (detached) → its first 7.
fn parse_head(head: &str) -> Option<String> {
    let head = head.trim();
    if let Some(r) = head.strip_prefix("ref:") {
        let r = r.trim();
        return Some(r.strip_prefix("refs/heads/").unwrap_or(r).to_string());
    }
    if head.len() >= 7 && head.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(head[..7].to_string());
    }
    None
}

/// ssh options that consume the next argument (from `man ssh`).
const SSH_ARG_OPTS: &[u8] = b"BbcDEeFIiJLlmOoPpQRSWw";

/// The host an `ssh …` command line connects to: the first non-option
/// argument, minus any `user@`, `ssh://` and `:port`. None when the line isn't
/// ssh or names no destination.
pub fn ssh_destination(args: &str) -> Option<String> {
    let mut it = args.split_whitespace();
    let prog = it.next()?;
    if prog.rsplit('/').next() != Some("ssh") {
        return None;
    }
    while let Some(a) = it.next() {
        if a == "--" {
            return it.next().and_then(clean_host);
        }
        if let Some(flags) = a.strip_prefix('-') {
            // `-p22` carries its value inline; `-p 22` takes the next word.
            if let Some(pos) = flags.bytes().position(|b| SSH_ARG_OPTS.contains(&b)) {
                if pos + 1 == flags.len() {
                    it.next();
                }
            }
            continue;
        }
        return clean_host(a);
    }
    None
}

fn clean_host(dest: &str) -> Option<String> {
    let d = dest.strip_prefix("ssh://").unwrap_or(dest);
    let d = d.rsplit('@').next().unwrap_or(d);
    let d = d.split([':', '/']).next().unwrap_or(d);
    (!d.is_empty()).then(|| d.to_string())
}

/// For each `(pane_id, pane_pid)`: the host of the ssh process that IS the
/// pane's process or is its direct child (a shell that ran `ssh win`). One
/// `ps` call covers every pane.
pub fn ssh_hosts(panes: &[(String, String)]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if panes.is_empty() {
        return out;
    }
    let Ok(ps) = Command::new("ps").args(["-A", "-o", "pid=,ppid=,args="]).output() else {
        return out;
    };
    let listing = String::from_utf8_lossy(&ps.stdout);
    let procs: Vec<(&str, &str, &str)> = listing
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (pid, rest) = l.split_once(char::is_whitespace)?;
            let rest = rest.trim_start();
            let (ppid, args) = rest.split_once(char::is_whitespace)?;
            Some((pid, ppid, args.trim()))
        })
        .collect();
    for (pane_id, pane_pid) in panes {
        let host = procs
            .iter()
            .filter(|(pid, ppid, _)| pid == pane_pid || ppid == pane_pid)
            .find_map(|(_, _, args)| ssh_destination(args));
        if let Some(h) = host {
            out.insert(pane_id.clone(), h);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_destination_finds_host_past_options() {
        assert_eq!(ssh_destination("ssh win").as_deref(), Some("win"));
        assert_eq!(ssh_destination("/usr/bin/ssh -p 2222 me@box.local").as_deref(), Some("box.local"));
        assert_eq!(ssh_destination("ssh -p2222 -A -i ~/.ssh/k host -t tmux").as_deref(), Some("host"));
        assert_eq!(ssh_destination("ssh -o ConnectTimeout=5 ssh://u@h:22").as_deref(), Some("h"));
        assert_eq!(ssh_destination("ssh -- win").as_deref(), Some("win"));
        assert_eq!(ssh_destination("ssh -v"), None);
        assert_eq!(ssh_destination("sshd: user@pts/0"), None);
        assert_eq!(ssh_destination("zsh"), None);
    }

    #[test]
    fn parse_head_branch_and_detached() {
        assert_eq!(parse_head("ref: refs/heads/fix/pane-tab-names\n").as_deref(), Some("fix/pane-tab-names"));
        assert_eq!(parse_head("c94f4e7a1b2c3d4e5f60718293a4b5c6d7e8f901").as_deref(), Some("c94f4e7"));
        assert_eq!(parse_head("garbage"), None);
    }

    #[test]
    fn repo_head_walks_up_and_follows_worktree_gitdir() {
        let base = std::env::temp_dir().join(format!("panemeta-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        // main repo with a nested cwd
        let repo = base.join("proj");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::create_dir_all(repo.join("src/deep")).unwrap();
        // worktree whose .git is a file pointing at a relative gitdir
        let wt = base.join("wt-feature");
        fs::create_dir_all(&wt).unwrap();
        fs::create_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        fs::write(repo.join(".git/worktrees/wt/HEAD"), "ref: refs/heads/feat/x\n").unwrap();
        fs::write(wt.join(".git"), "gitdir: ../proj/.git/worktrees/wt\n").unwrap();

        assert_eq!(
            repo_head(&repo.join("src/deep")),
            Some(RepoHead { project: "proj".into(), branch: "main".into() })
        );
        assert_eq!(
            repo_head(&wt),
            Some(RepoHead { project: "wt-feature".into(), branch: "feat/x".into() })
        );
        let _ = fs::remove_dir_all(&base);
    }
}

//! File-tree backend (v1.1 docked sidebar).
//!
//! The sidebar is a *navigation + path helper* for the terminals & agents — NOT
//! an editor. Two reads live here (Phase A):
//!   * `list_dir` — the IMMEDIATE children of one directory (the tree expands
//!     lazily, one call per opened folder), filtered like a sane editor:
//!     `.gitignore` is always honored and dotfiles are hidden unless asked, so
//!     `node_modules` / `target` / `.git` never reach the tree (or, later, the
//!     fs-watcher) and bury the real files.
//!   * `active_pane_cwd` — resolve a tmux pane's cwd, which the tree roots on
//!     (it follows the active pane).
//!
//! Pure reads, no writes, no fs-watch yet — live updates + create/trash land in
//! later phases. Filtering/walking uses the `ignore` crate (the ripgrep engine).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use ignore::WalkBuilder;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::tmux;

/// One immediate child of a listed directory.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// Basename shown in the tree.
    pub name: String,
    /// Absolute path (what click-actions insert / cd into).
    pub path: String,
    /// Directory? Sorts first; expandable in the tree.
    pub is_dir: bool,
}

/// Build-artifact directories the navigator ALWAYS hides, regardless of the
/// `.gitignore` toggle — browsing into them is never the point. (`.gitignore` is
/// a project-hygiene list, not a navigation filter: in a monorepo-of-repos it
/// ignores the very sub-repos you want to reach, so the tree no longer honors it
/// by default — it just drops this fixed denylist of heavy junk.)
const BUILD_JUNK: &[&str] = &[
    "node_modules", "target", "dist", "build", ".next", ".nuxt", ".svelte-kit",
    ".venv", "venv", "__pycache__", ".git", ".turbo", ".cache", ".parcel-cache",
    ".gradle", ".idea", "DerivedData", "Pods",
];

fn is_build_junk(name: &str) -> bool {
    BUILD_JUNK.contains(&name)
}

/// List the IMMEDIATE children of `dir` (one level — the tree expands lazily,
/// one `list_dir` per opened folder).
///
/// Filtering (a NAVIGATOR, not a git view):
///   * a fixed build-junk denylist (`node_modules`, `target`, `dist`, `.git`, …)
///     is ALWAYS dropped — those are never navigation targets;
///   * dotfiles are hidden unless `show_hidden` (the ⚙ toggle);
///   * `.gitignore` is honored ONLY when `hide_ignored` is set (the new toggle).
///     By default the tree SHOWS gitignored dirs, because in a monorepo-of-repos
///     `.gitignore` hides exactly the sub-repos/projects you want to open. When
///     `hide_ignored` is on, ancestor ignore files are consulted (`parents(true)`)
///     so a deep dir still respects its repo-root `.gitignore`.
///
/// Sorted dirs-first, then case-insensitively by name. Unreadable entries are
/// skipped, never fatal — a permission error on one child shouldn't blank the
/// whole panel.
pub fn list_dir(dir: &str, show_hidden: bool, hide_ignored: bool) -> Result<Vec<FileEntry>, String> {
    let root = Path::new(dir);
    if !root.is_dir() {
        return Err(format!("not a directory: {dir}"));
    }

    let mut out: Vec<FileEntry> = Vec::new();
    // max_depth(1) yields `dir` itself (depth 0) + its immediate children only.
    // Ignore sources (git + `.ignore`) are wired to `hide_ignored` so they're OFF
    // by default — the tree shows everything but the build-junk denylist.
    let walker = WalkBuilder::new(root)
        .max_depth(Some(1))
        .hidden(!show_hidden) // hidden(true) => SKIP dotfiles
        .parents(hide_ignored)
        .ignore(hide_ignored)
        .git_ignore(hide_ignored)
        .git_global(hide_ignored)
        .git_exclude(hide_ignored)
        .require_git(false)
        .build();

    for dent in walker {
        let dent = match dent {
            Ok(d) => d,
            Err(_) => continue, // unreadable entry — skip, don't fail the listing
        };
        // depth 0 is `dir` itself; we only want its children.
        if dent.depth() == 0 {
            continue;
        }
        let path = dent.path();
        let name = match path.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => continue,
        };
        let is_dir = dent.file_type().map(|t| t.is_dir()).unwrap_or(false);
        // Always drop build-junk DIRECTORIES (a file coincidentally named "build"
        // is kept — only dirs are the heavy ones).
        if is_dir && is_build_junk(&name) {
            continue;
        }
        out.push(FileEntry {
            name,
            path: path.to_string_lossy().into_owned(),
            is_dir,
        });
    }

    out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(out)
}

/// Resolve a tmux pane's current working directory — the tree roots on the
/// active pane's cwd and re-roots when it changes. A *shell* pane tracks live
/// `cd`; a *claude* pane reports its launch dir (claude isn't a shell) — both
/// acceptable. `pane_id` is validated to the tmux `%<n>` shape before it reaches
/// the tmux argv, so a crafted id can't smuggle a flag/option (boundary defense).
pub fn active_pane_cwd(pane_id: &str) -> Result<String, String> {
    if !is_pane_id(pane_id) {
        return Err(format!("bad pane id: {pane_id}"));
    }
    let out = tmux::tmux(&[
        "display-message",
        "-p",
        "-t",
        pane_id,
        "#{pane_current_path}",
    ])?;
    if !out.ok() {
        return Err(format!("pane cwd query failed: {}", out.stderr.trim()));
    }
    let cwd = out.trimmed();
    if cwd.is_empty() {
        return Err("empty pane cwd".into());
    }
    Ok(cwd)
}

/// A tmux pane id is `%` followed by one or more digits. Validate before using
/// it as a `-t` target so a crafted value can't be read as a tmux flag/option.
fn is_pane_id(s: &str) -> bool {
    let mut chars = s.chars();
    if chars.next() != Some('%') {
        return false;
    }
    let rest: String = chars.collect();
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

/// A pane's current command (`claude.exe`, `zsh`, …) — the sidebar uses it to
/// choose the insert format: a claude pane gets an `@path` mention, a shell gets
/// a raw path. `pane_id` validated to `%<n>` before reaching the tmux argv.
pub fn pane_command(pane_id: &str) -> Result<String, String> {
    if !is_pane_id(pane_id) {
        return Err(format!("bad pane id: {pane_id}"));
    }
    let out = tmux::tmux(&[
        "display-message",
        "-p",
        "-t",
        pane_id,
        "#{pane_current_command}",
    ])?;
    if !out.ok() {
        return Err(format!("pane command query failed: {}", out.stderr.trim()));
    }
    Ok(out.trimmed())
}

/// The user's home directory (`$HOME`). The sidebar's "Home" breadcrumb cd's
/// here, and the breadcrumb roots its segment labels below it. Falls back to "/"
/// if `$HOME` is somehow unset. Pure read of one env var.
pub fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".into())
}

/// One candidate in the repo-picker dropdown: a sibling project directory.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RepoEntry {
    /// Basename shown in the picker.
    pub name: String,
    /// Absolute path (what a pick cd's into).
    pub path: String,
    /// Does it contain a `.git`? (Marked in the picker; both repos and plain
    /// project dirs are listed, because a monorepo's sub-projects aren't separate
    /// git repos and would otherwise vanish from the list.)
    pub is_repo: bool,
}

/// Nearest ancestor of `start` (inclusive) that contains a `.git` entry, by a
/// pure fs probe — no `git` subprocess. `None` if `start` isn't inside a repo.
fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(p) = cur {
        if p.join(".git").exists() {
            return Some(p.to_path_buf());
        }
        cur = p.parent();
    }
    None
}

/// Sibling project directories for the repo-picker dropdown. We ANCHOR ON THE
/// WORKSPACE rather than sweeping `$HOME`: walk up from `from_dir` to the
/// enclosing git repo, then list that repo's PARENT's immediate child dirs — the
/// sibling repos/projects. Special cases:
///   * a repo that sits DIRECTLY in `$HOME` is itself a top-level project /
///     monorepo, so we list its OWN children (the sub-projects) instead of
///     `$HOME`'s Desktop/Documents/…;
///   * if `from_dir` isn't inside any repo, fall back to its own parent's dirs.
///
/// fs-reads only (a `.git` existence probe per candidate); no shell, no `git`.
/// Dotdirs are skipped; results sorted case-insensitively and capped. Recents
/// (the "jump anywhere" escape hatch) are tracked on the frontend, not here.
pub fn discover_repos(from_dir: &str) -> Result<Vec<RepoEntry>, String> {
    let start = Path::new(from_dir);
    if !start.is_dir() {
        return Err(format!("not a directory: {from_dir}"));
    }
    let home = home_dir();
    let workspace: PathBuf = match find_git_root(start) {
        Some(repo_root) => match repo_root.parent() {
            // Repo lives straight under $HOME → it's a top-level project/monorepo;
            // list its OWN sub-projects, not $HOME's dirs.
            Some(parent) if parent == Path::new(&home) => repo_root.clone(),
            Some(parent) => parent.to_path_buf(),
            None => repo_root,
        },
        // Not in a repo — show the dir's own siblings as a sane fallback.
        None => start
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| start.to_path_buf()),
    };

    let mut out: Vec<RepoEntry> = Vec::new();
    let rd = std::fs::read_dir(&workspace).map_err(|e| format!("read workspace: {e}"))?;
    for dent in rd.flatten() {
        let path = dent.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => continue,
        };
        if name.starts_with('.') {
            continue; // skip dotdirs (.git, .Trash, …)
        }
        if is_build_junk(&name) {
            continue; // never offer node_modules/target/dist/… as a repo target
        }
        let is_repo = path.join(".git").exists();
        out.push(RepoEntry {
            name,
            path: path.to_string_lossy().into_owned(),
            is_repo,
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out.truncate(80);
    Ok(out)
}

/// Reveal a path in Finder (`open -R <path>`). The path is passed as its own
/// argv element (no shell), so it can't inject; we only verify it exists first.
pub fn reveal_in_finder(path: &str) -> Result<(), String> {
    if !Path::new(path).exists() {
        return Err(format!("no such path: {path}"));
    }
    let status = Command::new("open")
        .arg("-R")
        .arg(path)
        .status()
        .map_err(|e| format!("spawn open: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("open -R failed".into())
    }
}

/// Create a file or directory `name` inside `parent`. `name` is validated to a
/// single SAFE path segment (non-empty, no separators, not `.`/`..`) so a created
/// name can't traverse out of `parent`. Returns the new absolute path; errors if
/// it already exists (never clobbers).
pub fn create_entry(parent: &str, name: &str, is_dir: bool) -> Result<String, String> {
    let pdir = Path::new(parent);
    if !pdir.is_dir() {
        return Err(format!("not a directory: {parent}"));
    }
    if !is_safe_segment(name) {
        return Err(format!("invalid name: {name}"));
    }
    let target = pdir.join(name);
    if target.exists() {
        return Err(format!("already exists: {name}"));
    }
    if is_dir {
        std::fs::create_dir(&target).map_err(|e| format!("create dir: {e}"))?;
    } else {
        std::fs::File::create(&target).map_err(|e| format!("create file: {e}"))?;
    }
    Ok(target.to_string_lossy().into_owned())
}

/// A safe single path segment: non-empty, trimmed, no `/`, no NUL, not `.`/`..`.
/// Prevents path traversal from a user-typed new-entry name.
fn is_safe_segment(name: &str) -> bool {
    !name.is_empty()
        && name == name.trim()
        && !name.contains('/')
        && !name.contains('\0')
        && name != "."
        && name != ".."
}

/// Move a path to the macOS Trash (recoverable) — NEVER an unlink. Verified to
/// exist first; uses the `trash` crate so a mis-click is reversible from Finder.
pub fn trash_path(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(format!("no such path: {path}"));
    }
    trash::delete(p).map_err(|e| format!("trash: {e}"))
}

// ── Live fs-watch (Phase D) ───────────────────────────────────────────────────
//
// The sidebar reflects files appearing/vanishing without a manual refresh (agents
// write constantly). We watch ONLY the visible dirs — the root + every expanded
// folder — each NON-recursively, so we never descend into an unopened heavy/
// ignored dir (node_modules/target) and drown in events. On any change we emit
// `filetree:changed { dir }` for the affected parent; the frontend debounces a
// reload of that one dir. The watched set is replaced wholesale by `set_watched`
// whenever the visible set changes (re-root / expand / collapse / hide).

struct WatchState {
    watcher: RecommendedWatcher,
    dirs: HashSet<PathBuf>,
}
static WATCH: Mutex<Option<WatchState>> = Mutex::new(None);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    /// The directory whose contents changed (the frontend reloads just this one).
    pub dir: String,
}

/// Replace the EXACT set of watched directories. Each is watched non-recursively.
/// Diff-based: unwatch what left the set, watch what entered it. The single
/// `notify` watcher is created lazily on first call (its callback owns an
/// `AppHandle` clone to emit changes). Passing an empty list unwatches everything
/// (e.g. when the sidebar is hidden).
pub fn set_watched(app: &AppHandle, dirs: Vec<String>) -> Result<(), String> {
    let want: HashSet<PathBuf> = dirs
        .iter()
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .collect();

    let mut guard = WATCH.lock().map_err(|_| "watch lock poisoned")?;
    if guard.is_none() {
        let app2 = app.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                // Emit one change per unique affected parent dir (a rename touches
                // two paths; coalesce so the frontend reloads each dir once).
                let mut seen = HashSet::new();
                for p in ev.paths {
                    let dir = p
                        .parent()
                        .map(|d| d.to_path_buf())
                        .unwrap_or_else(|| p.clone());
                    let s = dir.to_string_lossy().into_owned();
                    if seen.insert(s.clone()) {
                        let _ = app2.emit("filetree:changed", FileChange { dir: s });
                    }
                }
            }
        })
        .map_err(|e| format!("watcher init: {e}"))?;
        *guard = Some(WatchState {
            watcher,
            dirs: HashSet::new(),
        });
    }

    let st = guard.as_mut().unwrap();
    // Unwatch directories that left the visible set.
    for d in st.dirs.difference(&want).cloned().collect::<Vec<_>>() {
        let _ = st.watcher.unwatch(&d);
        st.dirs.remove(&d);
    }
    // Watch directories that newly entered it.
    for d in want.difference(&st.dirs).cloned().collect::<Vec<_>>() {
        if st.watcher.watch(&d, RecursiveMode::NonRecursive).is_ok() {
            st.dirs.insert(d);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Unique temp dir per test (pid + tag avoids cross-test collisions in one
    /// process; no Date/random needed). Fresh each run.
    fn temp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("cockpit-filetree-test-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn names(entries: &[FileEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn lists_filtered_and_sorted() {
        let d = temp_dir("list");
        std::fs::create_dir(d.join("src")).unwrap();
        std::fs::create_dir(d.join("node_modules")).unwrap();
        std::fs::write(d.join("README.md"), "x").unwrap();
        std::fs::write(d.join("a.txt"), "x").unwrap();
        std::fs::write(d.join(".env"), "secret").unwrap();
        std::fs::write(d.join(".gitignore"), "node_modules/\n").unwrap();

        let entries = list_dir(d.to_str().unwrap(), false, false).unwrap();
        // node_modules = build-junk denylist (gone, even with gitignore OFF);
        // .env + .gitignore = dotfiles (hidden); dirs first, then files
        // case-insensitively (a.txt < README.md).
        assert_eq!(names(&entries), vec!["src", "a.txt", "README.md"]);
        assert!(entries[0].is_dir);
        assert!(!entries[1].is_dir);
        // Absolute paths.
        assert!(entries[0].path.ends_with("/src"));
    }

    #[test]
    fn show_hidden_reveals_dotfiles_but_keeps_gitignore() {
        let d = temp_dir("hidden");
        std::fs::create_dir(d.join("node_modules")).unwrap();
        std::fs::write(d.join(".env"), "x").unwrap();
        std::fs::write(d.join("a.txt"), "x").unwrap();
        std::fs::write(d.join(".gitignore"), "node_modules/\n").unwrap();

        let entries = list_dir(d.to_str().unwrap(), true, false).unwrap();
        let shown = names(&entries);
        // Dotfiles now visible…
        assert!(shown.contains(&".env"));
        assert!(shown.contains(&".gitignore"));
        assert!(shown.contains(&"a.txt"));
        // …but build-junk stays hidden (show_hidden ≠ show node_modules).
        assert!(!shown.contains(&"node_modules"));
    }

    #[test]
    fn shows_gitignored_dirs_by_default_but_hides_build_junk() {
        // A monorepo-of-repos: `nested_repo/` is gitignored (like cc-cockpit in
        // ~/Workflows). The navigator must SHOW it by default, while the build-junk
        // denylist (node_modules) stays hidden regardless of gitignore.
        let d = temp_dir("shownested");
        std::fs::create_dir(d.join("proj")).unwrap();
        std::fs::create_dir(d.join("nested_repo")).unwrap();
        std::fs::create_dir(d.join("node_modules")).unwrap();
        std::fs::write(d.join(".gitignore"), "nested_repo/\nnode_modules/\n").unwrap();

        // Default (hide_ignored = false): gitignored project shows, junk doesn't.
        let shown = list_dir(d.to_str().unwrap(), false, false).unwrap();
        let n = names(&shown);
        assert!(n.contains(&"proj"));
        assert!(n.contains(&"nested_repo"), "gitignored project must show by default");
        assert!(!n.contains(&"node_modules"), "build junk always hidden");

        // Toggle ON (hide_ignored = true): gitignore re-applies, project hidden.
        let hidden = list_dir(d.to_str().unwrap(), false, true).unwrap();
        let h = names(&hidden);
        assert!(h.contains(&"proj"));
        assert!(!h.contains(&"nested_repo"), "hide-ignored must re-apply .gitignore");
        assert!(!h.contains(&"node_modules"));
    }

    #[test]
    fn errors_on_non_dir_and_missing() {
        let d = temp_dir("nondir");
        let f = d.join("file.txt");
        std::fs::write(&f, "x").unwrap();
        assert!(list_dir(f.to_str().unwrap(), false, false).is_err());
        assert!(list_dir(d.join("nope").to_str().unwrap(), false, false).is_err());
    }

    #[test]
    fn pane_id_validation() {
        assert!(is_pane_id("%0"));
        assert!(is_pane_id("%42"));
        assert!(!is_pane_id("%"));
        assert!(!is_pane_id("0"));
        assert!(!is_pane_id("%1 kill-server"));
        assert!(!is_pane_id("%1;x"));
        assert!(!is_pane_id("-t"));
        assert!(!is_pane_id(""));
    }

    #[test]
    fn bad_pane_id_rejected_before_tmux() {
        // A non-`%n` id must error at the boundary, never reaching the tmux argv.
        assert!(active_pane_cwd("--kill-server").is_err());
        assert!(active_pane_cwd("; rm -rf /").is_err());
        // pane_command shares the same guard (bad shape errors before tmux).
        assert!(pane_command("$(whoami)").is_err());
        assert!(pane_command("-X").is_err());
    }

    #[test]
    fn safe_segment_rejects_traversal() {
        assert!(is_safe_segment("file.txt"));
        assert!(is_safe_segment("my-dir"));
        assert!(!is_safe_segment(""));
        assert!(!is_safe_segment("."));
        assert!(!is_safe_segment(".."));
        assert!(!is_safe_segment("a/b"));
        assert!(!is_safe_segment("../escape"));
        assert!(!is_safe_segment(" leading"));
        assert!(!is_safe_segment("trailing "));
        assert!(!is_safe_segment("nul\0byte"));
    }

    #[test]
    fn create_entry_file_and_dir() {
        let d = temp_dir("create");
        let f = create_entry(d.to_str().unwrap(), "new.txt", false).unwrap();
        assert!(Path::new(&f).is_file());
        let sub = create_entry(d.to_str().unwrap(), "sub", true).unwrap();
        assert!(Path::new(&sub).is_dir());
        // No clobber: a second create at the same name errors.
        assert!(create_entry(d.to_str().unwrap(), "new.txt", false).is_err());
        // Traversal name rejected.
        assert!(create_entry(d.to_str().unwrap(), "../evil", false).is_err());
    }

    #[test]
    fn home_dir_is_absolute() {
        // Whatever $HOME is in this env, it must be an absolute path (never empty).
        let h = home_dir();
        assert!(h.starts_with('/'), "home not absolute: {h}");
    }

    #[test]
    fn find_git_root_walks_up() {
        let d = temp_dir("gitroot");
        let repo = d.join("repo");
        let deep = repo.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        // From deep inside, the nearest `.git` ancestor is `repo`.
        assert_eq!(find_git_root(&deep), Some(repo.clone()));
        assert_eq!(find_git_root(&repo), Some(repo.clone()));
        // A tree with no `.git` anywhere up to the temp root resolves to None only
        // if no ancestor has one — temp dirs under /var have none we created.
        let bare = d.join("bare");
        std::fs::create_dir(&bare).unwrap();
        // (bare's ancestors up to / have no .git we control; assert it's not `repo`.)
        assert_ne!(find_git_root(&bare), Some(repo));
    }

    #[test]
    fn discover_repos_lists_siblings_with_repo_flag() {
        // workspace/{repoA/.git, repoB/.git, plain/, .hidden/}; probe from inside
        // repoA → siblings are repoA(repo), repoB(repo), plain(not). .hidden skipped.
        let ws = temp_dir("siblings");
        for n in ["repoA", "repoB", "plain", ".hidden"] {
            std::fs::create_dir(ws.join(n)).unwrap();
        }
        std::fs::create_dir(ws.join("repoA").join(".git")).unwrap();
        std::fs::create_dir(ws.join("repoB").join(".git")).unwrap();
        let from = ws.join("repoA").join("src");
        std::fs::create_dir(&from).unwrap();

        let repos = discover_repos(from.to_str().unwrap()).unwrap();
        let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
        // Sorted case-insensitively, dotdir excluded.
        assert_eq!(names, vec!["plain", "repoA", "repoB"]);
        let by = |n: &str| repos.iter().find(|r| r.name == n).unwrap();
        assert!(by("repoA").is_repo);
        assert!(by("repoB").is_repo);
        assert!(!by("plain").is_repo);
        // Absolute paths.
        assert!(by("repoA").path.ends_with("/repoA"));
    }

    #[test]
    fn discover_repos_fallback_when_not_in_repo() {
        // No `.git` anywhere we created → fall back to the dir's OWN parent's
        // children (still lists siblings, just not anchored on a repo).
        let parent = temp_dir("norepo");
        for n in ["alpha", "beta"] {
            std::fs::create_dir(parent.join(n)).unwrap();
        }
        let from = parent.join("alpha");
        let repos = discover_repos(from.to_str().unwrap()).unwrap();
        let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
    }

    #[test]
    fn discover_repos_errors_on_non_dir() {
        let d = temp_dir("discnondir");
        let f = d.join("x.txt");
        std::fs::write(&f, "x").unwrap();
        assert!(discover_repos(f.to_str().unwrap()).is_err());
    }

    #[test]
    fn discover_repos_hides_build_junk() {
        // workspace/{realproj/.git, node_modules, target}; probe from inside
        // realproj → workspace = ws; the picker must NOT list build-junk dirs.
        let ws = temp_dir("discjunk");
        for n in ["realproj", "node_modules", "target"] {
            std::fs::create_dir(ws.join(n)).unwrap();
        }
        std::fs::create_dir(ws.join("realproj").join(".git")).unwrap();
        let from = ws.join("realproj").join("src");
        std::fs::create_dir(&from).unwrap();

        let repos = discover_repos(from.to_str().unwrap()).unwrap();
        let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"realproj"));
        assert!(!names.contains(&"node_modules"), "build junk must not appear in the repo picker");
        assert!(!names.contains(&"target"), "build junk must not appear in the repo picker");
    }

    #[test]
    fn reveal_validates_existence() {
        let d = temp_dir("reveal");
        assert!(reveal_in_finder(d.join("nope").to_str().unwrap()).is_err());
        // (We don't assert the success path — it would pop Finder in CI.)
    }

    #[test]
    fn trash_moves_existing_file() {
        // Trashing a temp file is harmless + recoverable; verifies the path is
        // gone from its origin afterward.
        let d = temp_dir("trash");
        let f = d.join("doomed.txt");
        std::fs::write(&f, "x").unwrap();
        assert!(trash_path(f.to_str().unwrap()).is_ok());
        assert!(!f.exists());
        // Missing path errors.
        assert!(trash_path(d.join("ghost").to_str().unwrap()).is_err());
    }
}

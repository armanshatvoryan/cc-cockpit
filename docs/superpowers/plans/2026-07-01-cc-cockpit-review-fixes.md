# CC Cockpit v0.1.1 Review Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the four actionable findings from the xhigh code review of branch `fix/gui-path-v0.1.1` (the file-tree cd-nav + GUI PATH-repair diff).

**Architecture:** Three self-contained fixes plus one optional cleanup. (1) The repo-picker's directory scan reuses the existing build-junk denylist. (2) `repair_path_for_gui` is hardened against a hung interactive shell (bounded wait), a fish space-joined `$PATH`, and a reversed-sentinel slice panic — by extracting a pure, unit-tested parse/validate helper and a bounded-wait spawn wrapper. (3, optional) The tree's `cd` sends its command line + Enter as one atomic backend command instead of two racy fire-and-forget key sends.

**Tech Stack:** Rust (edition 2021, Tauri 2, std-only — no new crates), SolidJS + TypeScript frontend, tmux control-mode backend.

## Global Constraints

- Rust edition **2021**; `std::env::set_var` stays valid (do not migrate).
- **No new dependencies.** The bounded-wait timeout MUST use `std::thread` + `std::sync::mpsc` only — do NOT add `wait-timeout` or any crate (repo rule: ask before adding any dependency).
- Keep the login shell **interactive** (`-ilc`, not `-lc`): many users set PATH (nvm/brew shims) only in interactive `~/.zshrc`; dropping `-i` would regress the PATH repair this function exists to provide. The fix is a *timeout*, not removing `-i`.
- macOS-only app; Claude Max (never scaffold api-key handling).
- Version stays `0.1.1` (no bump — not in scope).
- Backend tests run from `app/src-tauri` with `cargo test --lib`. Frontend has no unit runner — its gate is `npm --prefix app/frontend run typecheck` (tsc) + `cargo check`.
- Follow existing test style in `filetree.rs` (the `temp_dir(tag)` helper, sandbox-only, never touch real `~`).

---

### Task 1: Repo-picker hides build-junk dirs (finding #4)

`discover_repos` lists a workspace's child directories but skips only dotdirs — so `node_modules`, `target`, `dist`, `Pods`, `DerivedData` show up as pickable "repos". `list_dir` already drops these via `is_build_junk`; reuse it.

**Files:**
- Modify: `app/src-tauri/src/filetree.rs` (the child loop inside `discover_repos`, ~line 253; add a test in the `#[cfg(test)] mod tests` block)

**Interfaces:**
- Consumes: `is_build_junk(name: &str) -> bool` (already defined in this module).
- Produces: no signature change — `discover_repos` output just omits build-junk dirs.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `app/src-tauri/src/filetree.rs` (near `discover_repos_lists_siblings_with_repo_flag`):

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd app/src-tauri && cargo test --lib discover_repos_hides_build_junk`
Expected: FAIL — `node_modules`/`target` currently appear (assert fails).

- [ ] **Step 3: Write minimal implementation**

In `discover_repos`, inside `for dent in rd.flatten() { … }`, add the build-junk skip immediately after the existing dotdir skip:

```rust
        if name.starts_with('.') {
            continue; // skip dotdirs (.git, .Trash, …)
        }
        if is_build_junk(&name) {
            continue; // never offer node_modules/target/dist/… as a repo target
        }
        let is_repo = path.join(".git").exists();
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd app/src-tauri && cargo test --lib discover_repos`
Expected: PASS (the new test plus the existing `discover_repos_*` tests).

- [ ] **Step 5: Commit**

```bash
git add app/src-tauri/src/filetree.rs
git commit -m "fix(filetree): drop build-junk dirs from repo picker

$(printf '%s' 'discover_repos listed node_modules/target/… as pickable repos; reuse the is_build_junk denylist list_dir already applies.')
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Harden repair_path_for_gui — bounded wait + validated capture (findings #1, #2, #5)

`repair_path_for_gui` (in `app/src-tauri/src/lib.rs`) blocks on `$SHELL -ilc …` with no timeout (a hung rc bricks launch — #1), trusts a space-joined fish `$PATH` (#2), and slices `&s[a+len..b]` without asserting `a < b` (panic on reversed sentinels — #5). Split it: a **pure** `parse_path_capture` (unit-tested; fixes #2 + #5) and a **bounded-wait** `capture_login_path` (fixes #1, std-only).

**Files:**
- Modify: `app/src-tauri/src/lib.rs` (rewrite `repair_path_for_gui`, ~lines 588-617; add two helpers; add a `#[cfg(test)] mod tests` block — lib.rs currently has none)

**Interfaces:**
- Produces:
  - `fn parse_path_capture(stdout: &str) -> Option<String>` — extract the sentinel-wrapped PATH; `None` if sentinels missing, reversed, empty, or the value is not a plausible PATH (no `:` and not an existing dir).
  - `fn capture_login_path(shell: &str) -> Option<String>` — spawn the probe, read stdout on a thread, give up after 5s, kill the child; returns `parse_path_capture(stdout)`.
  - `repair_path_for_gui()` — unchanged signature/behavior on the happy path; only the capture is now bounded + validated, with the existing Homebrew-widen fallback retained.

- [ ] **Step 1: Write the failing tests**

Add at the END of `app/src-tauri/src/lib.rs` (lib.rs has no test module yet):

```rust
#[cfg(test)]
mod tests {
    use super::parse_path_capture;

    #[test]
    fn parse_extracts_between_sentinels() {
        let s = "rc-noise\n__CCPATH__/opt/homebrew/bin:/usr/bin:/bin__CCEND__";
        assert_eq!(parse_path_capture(s).as_deref(), Some("/opt/homebrew/bin:/usr/bin:/bin"));
    }

    #[test]
    fn parse_rejects_reversed_sentinels_without_panic() {
        // #5: __CCEND__ before __CCPATH__ must return None, never slice-panic.
        assert_eq!(parse_path_capture("__CCEND__junk__CCPATH__"), None);
    }

    #[test]
    fn parse_rejects_empty_capture() {
        assert_eq!(parse_path_capture("__CCPATH____CCEND__"), None);
    }

    #[test]
    fn parse_rejects_space_joined_fish_path() {
        // #2: fish quoted $PATH is space-joined (no ':', not a dir) → reject → caller falls back.
        assert_eq!(parse_path_capture("__CCPATH__/opt/homebrew/bin /usr/bin__CCEND__"), None);
    }

    #[test]
    fn parse_accepts_colonless_but_real_single_dir() {
        // A legit single-entry PATH (rare but valid) that is a real dir passes.
        let d = std::env::temp_dir();
        let d = d.to_string_lossy().into_owned();
        let s = format!("__CCPATH__{d}__CCEND__");
        assert_eq!(parse_path_capture(&s).as_deref(), Some(d.as_str()));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd app/src-tauri && cargo test --lib parse_`
Expected: FAIL to COMPILE — `parse_path_capture` does not exist yet.

- [ ] **Step 3: Write the pure helper**

In `app/src-tauri/src/lib.rs`, ABOVE `repair_path_for_gui`, add:

```rust
/// Extract the sentinel-wrapped PATH from the login-shell probe's stdout, and
/// validate it. Returns `None` (→ caller uses its fallback) when the sentinels
/// are missing, out of order (guards a reversed-range slice panic), empty, or the
/// value isn't a plausible PATH. A fish login shell renders a quoted `$PATH`
/// space-joined (no `:`), which would install one bogus dir — reject anything
/// that has no `:` and isn't itself an existing directory.
fn parse_path_capture(stdout: &str) -> Option<String> {
    const OPEN: &str = "__CCPATH__";
    let a = stdout.find(OPEN)?;
    let b = stdout.find("__CCEND__")?;
    let start = a + OPEN.len();
    if b <= start {
        return None; // missing/reversed sentinels — never slice a reversed range
    }
    let path = &stdout[start..b];
    if path.is_empty() {
        return None;
    }
    // A real PATH is colon-separated; the only colon-less value we accept is a
    // single existing directory (rules out fish's space-joined list).
    if !path.contains(':') && !std::path::Path::new(path).is_dir() {
        return None;
    }
    Some(path.to_string())
}
```

- [ ] **Step 4: Run the pure-helper tests to verify they pass**

Run: `cd app/src-tauri && cargo test --lib parse_`
Expected: PASS (all five `parse_*` tests).

- [ ] **Step 5: Write the bounded-wait capture + rewire repair_path_for_gui**

Replace the whole body of `repair_path_for_gui` and add `capture_login_path` above it (still above `parse_path_capture` is fine — order among fns doesn't matter):

```rust
/// Spawn the login-shell PATH probe and read its stdout, but GIVE UP after 5s so
/// a hung interactive rc (a `read` from the tty, a slow network mount) can never
/// brick launch. std-only: a reader thread pushes stdout to a channel; the main
/// thread waits with a timeout, then kills the child regardless. `stdin` is
/// /dev/null so the shell can't block reading from us.
fn capture_login_path(shell: &str) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    let mut child = Command::new(shell)
        // Keep -i: many users set PATH only in interactive ~/.zshrc.
        .args(["-ilc", "printf '__CCPATH__%s__CCEND__' \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let mut out = child.stdout.take()?;
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = out.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });

    let captured = rx.recv_timeout(Duration::from_secs(5)).ok();
    // Reap regardless: on timeout kill the hung shell; on success it has exited.
    let _ = child.kill();
    let _ = child.wait();

    parse_path_capture(&captured?)
}

/// Apps launched from Finder/launchd inherit a stripped PATH (e.g.
/// `/usr/local/bin:/bin:/usr/bin` — no `/opt/homebrew/bin`), so every bare
/// `Command::new("tmux"|"git"|"zsh"|"open")` spawn fails with "No such file or
/// directory (os error 2)". Pull the real PATH from the user's login shell once
/// at startup (bounded + validated) and install it so all children inherit it. A
/// terminal/dev launch already has a full PATH, so the probe just re-sets the same
/// value (harmless). Edition 2021 → `set_var` is safe; this runs before any
/// thread/child of the app proper is spawned.
fn repair_path_for_gui() {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    if let Some(path) = capture_login_path(&shell) {
        std::env::set_var("PATH", path);
        return;
    }
    // Probe failed / timed out / invalid: widen PATH with the usual Homebrew dirs
    // so spawns still resolve rather than leaving the stripped GUI PATH untouched.
    let cur = std::env::var("PATH").unwrap_or_default();
    let mut parts: Vec<String> =
        cur.split(':').filter(|s| !s.is_empty()).map(String::from).collect();
    for d in ["/opt/homebrew/bin", "/usr/local/bin"] {
        if !parts.iter().any(|p| p == d) {
            parts.push(d.to_string());
        }
    }
    std::env::set_var("PATH", parts.join(":"));
}
```

- [ ] **Step 6: Run full backend suite + check to verify it passes and compiles**

Run: `cd app/src-tauri && cargo test --lib parse_ && cargo check`
Expected: PASS (parse tests) and `cargo check` EXIT 0 (the reader thread + `recv_timeout` compile; `repair_path_for_gui` still called once in `run()`).

- [ ] **Step 7: Commit**

```bash
git add app/src-tauri/src/lib.rs
git commit -m "fix(gui): bound + validate the login-shell PATH probe

$(printf '%s' 'repair_path_for_gui could hang launch on a slow/interactive rc (no timeout), install a space-joined fish PATH, or panic on reversed sentinels. Add a 5s bounded-wait spawn and a pure, unit-tested parse/validate helper; keep -i so ~/.zshrc PATH still resolves.')
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3 (OPTIONAL cleanup): send tree `cd` as one atomic backend command (finding #3)

LOW severity — the identical `paneSendKeys` channel carries every terminal keystroke reliably, so the two-call ordering race almost certainly never fires. But folding the command line + Enter into one backend call (mirroring `run_line_in_pane`) removes the narrow window and is the correct shape. Skip if trimming scope.

**Files:**
- Modify: `app/src-tauri/src/manager.rs` (expose a public wrapper over `run_line_in_pane`, near the private fn ~line 514)
- Modify: `app/src-tauri/src/lib.rs` (add a `pane_run_line` command; register it in `generate_handler!`)
- Modify: `app/frontend/src/ipc.ts` (add `paneRunLine`)
- Modify: `app/frontend/src/store.ts` (`ftCdActivePane` ~lines 1273-1274 uses `paneRunLine`; import it)

**Interfaces:**
- Consumes: `SessionManager::run_line_in_pane(&mut self, pane_id, line)` (already exists, private — sends literal line then a hex CR under one control-client round-trip).
- Produces:
  - `SessionManager::pane_run_line(&mut self, pane_id: &str, line: &str) -> Result<(), String>`
  - Tauri command `pane_run_line(pane_id: String, line: String) -> Result<(), String>`
  - IPC `paneRunLine(paneId: string, line: string): Promise<void>`

- [ ] **Step 1: Expose the manager wrapper**

In `app/src-tauri/src/manager.rs`, next to the private `run_line_in_pane`, add:

```rust
    /// Public entry to type a full command line + Enter atomically (one control-
    /// client round-trip). Used by the file-tree `cd` so the line and its CR can't
    /// be split into two racy IPC calls.
    pub fn pane_run_line(&mut self, pane_id: &str, line: &str) -> Result<(), String> {
        self.run_line_in_pane(pane_id, line)
    }
```

- [ ] **Step 2: Add the Tauri command + register it**

In `app/src-tauri/src/lib.rs`, add the command near `pane_send_keys`:

```rust
#[tauri::command]
fn pane_run_line(state: State<'_, AppState>, pane_id: String, line: String) -> Result<(), String> {
    let mut mgr = state.mgr.lock().unwrap();
    mgr.pane_run_line(&pane_id, &line)
}
```

Then add `pane_run_line,` to the `tauri::generate_handler![ … ]` list (e.g. right after `pane_send_keys,`).

- [ ] **Step 3: Verify the backend compiles**

Run: `cd app/src-tauri && cargo check`
Expected: EXIT 0.

- [ ] **Step 4: Add the IPC wrapper**

In `app/frontend/src/ipc.ts`, after `paneSendKeys`:

```ts
/** Type a full command line + Enter into a pane atomically (one backend command,
 *  single lock) — used by the file-tree `cd` so the line and its CR can't race. */
export function paneRunLine(paneId: string, line: string): Promise<void> {
  return invoke<void>("pane_run_line", { paneId, line });
}
```

- [ ] **Step 5: Use it in ftCdActivePane**

In `app/frontend/src/store.ts`: add `paneRunLine` to the `./ipc` import block (alongside `paneSendKeys`), then replace the two send calls in `ftCdActivePane`:

```ts
  // was: paneSendKeys(pid, `cd ${shellQuoteIfNeeded(dir)}`); paneSendKeys(pid, "\r");
  void paneRunLine(pid, `cd ${shellQuoteIfNeeded(dir)}`);
  pushRecent(dir);
  ftSetRoot(dir); // snappy re-root; syncFileTreeRoot would also catch it
```

- [ ] **Step 6: Verify the frontend type-checks**

Run: `npm --prefix app/frontend run typecheck`
Expected: EXIT 0 (no unused-import / type errors; `paneSendKeys` may now be unused in store.ts — if tsc flags it, keep it only if still referenced elsewhere, else drop it from the import).

- [ ] **Step 7: Commit**

```bash
git add app/src-tauri/src/manager.rs app/src-tauri/src/lib.rs app/frontend/src/ipc.ts app/frontend/src/store.ts
git commit -m "refactor(filetree): send tree cd as one atomic pane_run_line

$(printf '%s' 'ftCdActivePane split cd and Enter into two fire-and-forget sends; fold them into one backend command (mirrors run_line_in_pane) to close the ordering window.')
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- #4 (repo picker build-junk) → Task 1 ✓
- #1 (startup hang, no timeout) → Task 2, `capture_login_path` bounded wait ✓
- #2 (fish space-joined PATH) → Task 2, `parse_path_capture` colon/is_dir guard ✓
- #5 (reversed-sentinel slice panic) → Task 2, `b <= start` guard ✓
- #3 (cd/CR race) → Task 3 (optional) ✓
- #7 (symlinked $HOME lexical compare), #9 (ftHome→"/"), #8 (hide_ignored default, deliberate) — intentionally out of scope (very-low / by-design); note here so the omission is explicit, not forgotten.

**Placeholder scan:** none — every code step shows full code; every run step shows the exact command + expected result.

**Type consistency:** `parse_path_capture(&str) -> Option<String>` and `capture_login_path(&str) -> Option<String>` used consistently; `pane_run_line` name identical across manager → command → ipc (`paneRunLine`) → store call site; `is_build_junk(&str) -> bool` matches its existing definition.

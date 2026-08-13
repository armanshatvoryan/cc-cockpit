# First-Run Onboarding Wizard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new user launching the .dmg for the first time gets a 3-step wizard (environment check with one-click installs → projects folder → shortcut card) before the cockpit boots; the wizard never shows twice and is re-runnable from Settings.

**Architecture:** New Rust module `onboarding.rs` (prereq probe + hardcoded-command install runner with streamed output events); one additive settings field `onboardingDone`; frontend boot gating in `store.ts` (`decideBoot` defers `bootCockpit` until finish/skip); new `OnboardingWizard.tsx` modeled on SettingsDialog.

**Tech Stack:** Tauri 2 (Rust backend, sync/async commands, `app.emit` events), SolidJS frontend, serde, `zsh -lc` login-shell probing.

**Spec:** `docs/superpowers/specs/2026-08-12-onboarding-wizard-design.md` (read it first).

**Working copy:** worktree `/Users/armanshatvoran/Workflows/.cc-worktrees/cc-cockpit-onboarding`, branch `feat/onboarding-wizard`. All paths below are relative to the worktree root. Do NOT touch the main checkout at `~/Workflows/cc-cockpit` (another session owns it).

## Global Constraints

- tmux minimum version: **3.3** (`ok = version >= 3.3`).
- claude CLI check is **presence-only** — no login/auth probing.
- macOS arm64 only; no Windows/Linux/Intel handling.
- **No user input ever reaches a command line.** Installable tools are a Rust enum (`Tmux | ClaudeCli`), each mapping to a hardcoded argv. Serde rejects unknown values at the IPC boundary.
- Install commands, verbatim: `zsh -lc "brew install tmux 2>&1"` and `zsh -lc "npm install -g @anthropic-ai/claude-code 2>&1"`.
- Settings schema stays **v1**; `onboardingDone` is additive (`serde(default)`, omitted when false).
- Skip persists the flag too — the wizard must never nag twice.
- A corrupt settings file is only overwritten when the user finishes **or skips** the wizard — never on load.
- Only one install may run at a time.
- Event names, verbatim: `onboarding:install-line` `{tool, line}`, `onboarding:install-done` `{tool, exitCode}`.
- Test commands: Rust `cd app/src-tauri && cargo test`; frontend `cd app/frontend && npm run typecheck && npm run build`.

---

### Task 1: `onboardingDone` settings field

**Files:**
- Modify: `app/src-tauri/src/settings.rs` (struct at :28, tests at :121)
- Modify: `app/frontend/src/ipc.ts` (`CockpitSettings` interface at :554)

**Interfaces:**
- Consumes: existing `CockpitSettings` (Rust + TS).
- Produces: Rust field `pub onboarding_done: bool` on `CockpitSettings`; TS field `onboardingDone?: boolean` on `CockpitSettings`. Later tasks read/write it through the existing `load_settings` / `save_settings` commands — no new commands here.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `app/src-tauri/src/settings.rs`:

```rust
    #[test]
    fn onboarding_flag_defaults_off_and_round_trips() {
        // Older file without the key ⇒ false ⇒ wizard shows.
        let s: CockpitSettings = serde_json::from_str("{}").unwrap();
        assert!(!s.onboarding_done);

        // false is omitted from disk; true round-trips in camelCase.
        let off = CockpitSettings {
            schema_version: 1,
            default_cwd: None,
            onboarding_done: false,
        };
        assert!(!serde_json::to_string(&off).unwrap().contains("onboardingDone"));

        let on = CockpitSettings {
            onboarding_done: true,
            ..off.clone()
        };
        let json = serde_json::to_string(&on).unwrap();
        assert!(json.contains("\"onboardingDone\":true"), "json: {json}");
        let back: CockpitSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(on, back);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd app/src-tauri && cargo test onboarding_flag`
Expected: COMPILE FAIL — `struct CockpitSettings has no field named onboarding_done`.

- [ ] **Step 3: Add the field**

In `CockpitSettings` (after `default_cwd`):

```rust
    /// `true` once the first-run wizard has been completed or skipped. Absent
    /// (older files / fresh installs) ⇒ show the wizard on next launch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub onboarding_done: bool,
```

The four existing tests use full struct literals — add `onboarding_done: false,` to each (`round_trip_with_path`, `round_trip_unset`, `serializes_camel_case_keys`; `parses_file_written_by_an_older_build` uses no literal, leave it).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd app/src-tauri && cargo test settings`
Expected: all settings tests PASS (including the new one).

- [ ] **Step 5: Mirror in TypeScript**

In `app/frontend/src/ipc.ts`, inside `CockpitSettings` (after `defaultCwd?`):

```ts
  /** True once the first-run wizard has been completed or skipped. */
  onboardingDone?: boolean;
```

Run: `cd app/frontend && npm run typecheck` — expected PASS.

- [ ] **Step 6: Commit**

```bash
git add app/src-tauri/src/settings.rs app/frontend/src/ipc.ts
git commit -m "feat(onboarding): add onboardingDone settings flag (schema stays v1)"
```

---

### Task 2: `onboarding.rs` — pure probe parsing

**Files:**
- Create: `app/src-tauri/src/onboarding.rs`
- Modify: `app/src-tauri/src/lib.rs` (module list at :15-25 — add `pub mod onboarding;`)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces (used by Tasks 3-4): `pub struct ToolStatus { found: bool, version: Option<String>, ok: bool }`, `pub struct PrereqReport { tmux: ToolStatus, claude: ToolStatus, brew: bool, npm: bool }` (both `Serialize`, camelCase), `fn parse_tmux_version(&str) -> Option<(u32, u32)>`, `fn parse_probe_output(&str) -> PrereqReport`, `const MIN_TMUX: (u32, u32)`.

- [ ] **Step 1: Create the module with failing tests**

Create `app/src-tauri/src/onboarding.rs`:

```rust
//! First-run onboarding: environment probe + one-click prereq installs.
//!
//! Security invariant: NO user input ever reaches a command line. The only
//! installable things are the variants of `PrereqTool` (Task 4), each mapping
//! to a hardcoded argv; serde rejects anything else at the IPC boundary.
//!
//! Probing uses `zsh -lc` for the same reason as the PATH capture in
//! `lib.rs`: a Finder-launched app has a stripped PATH, and the login shell
//! sources /etc/zprofile + ~/.zprofile (Homebrew shellenv) — where tmux and
//! brew actually live on a normal setup.

use serde::Serialize;

/// Minimum tmux version the cockpit supports.
const MIN_TMUX: (u32, u32) = (3, 3);

/// Presence/health of one probed tool.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub found: bool,
    /// Raw version line (e.g. `"tmux 3.4"`), `None` when not found.
    pub version: Option<String>,
    /// tmux: found AND version >= 3.3. claude: same as `found` (presence-only).
    pub ok: bool,
}

/// Everything Step 1 of the wizard renders.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrereqReport {
    pub tmux: ToolStatus,
    pub claude: ToolStatus,
    pub brew: bool,
    pub npm: bool,
}

/// `"tmux 3.4"` → `(3, 4)`; `"tmux 3.3a"` → `(3, 3)`; `"tmux next-3.6"` →
/// `(3, 6)`; garbage → `None`.
fn parse_tmux_version(s: &str) -> Option<(u32, u32)> {
    let rest = s.trim().strip_prefix("tmux ").unwrap_or_else(|| s.trim());
    let rest = rest.strip_prefix("next-").unwrap_or(rest);
    let digits = |p: &str| -> Option<u32> {
        let d: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
        d.parse().ok()
    };
    let mut parts = rest.split('.');
    let major = digits(parts.next()?)?;
    let minor = parts.next().and_then(digits).unwrap_or(0);
    Some((major, minor))
}

/// Parse the sentinel-keyed probe output (Task 3 emits `CC_TMUX=` etc.). A
/// login shell can print rc noise, so only lines starting with a known key
/// count, and the LAST occurrence wins. An empty value ⇒ tool not found.
fn parse_probe_output(out: &str) -> PrereqReport {
    let mut tmux_v = String::new();
    let mut claude_v = String::new();
    let mut brew = String::new();
    let mut npm = String::new();
    for line in out.lines() {
        if let Some(v) = line.strip_prefix("CC_TMUX=") {
            tmux_v = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("CC_CLAUDE=") {
            claude_v = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("CC_BREW=") {
            brew = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("CC_NPM=") {
            npm = v.trim().to_string();
        }
    }
    PrereqReport {
        tmux: ToolStatus {
            found: !tmux_v.is_empty(),
            ok: parse_tmux_version(&tmux_v).is_some_and(|v| v >= MIN_TMUX),
            version: (!tmux_v.is_empty()).then(|| tmux_v.clone()),
        },
        claude: ToolStatus {
            found: !claude_v.is_empty(),
            ok: !claude_v.is_empty(),
            version: (!claude_v.is_empty()).then(|| claude_v.clone()),
        },
        brew: !brew.is_empty(),
        npm: !npm.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_version_parse() {
        assert_eq!(parse_tmux_version("tmux 3.4"), Some((3, 4)));
        assert_eq!(parse_tmux_version("tmux 3.3a"), Some((3, 3)));
        assert_eq!(parse_tmux_version("tmux next-3.6"), Some((3, 6)));
        assert_eq!(parse_tmux_version("tmux 3.2"), Some((3, 2)));
        assert_eq!(parse_tmux_version(""), None);
        assert_eq!(parse_tmux_version("command not found"), None);
    }

    #[test]
    fn tmux_ok_gate_is_3_3() {
        let ok = |s: &str| parse_tmux_version(s).is_some_and(|v| v >= MIN_TMUX);
        assert!(ok("tmux 3.3"));
        assert!(ok("tmux 3.4"));
        assert!(ok("tmux 4.0"));
        assert!(!ok("tmux 3.2"));
        assert!(!ok("garbage"));
    }

    #[test]
    fn probe_output_full_and_noisy() {
        let out = "rc-noise: welcome\nCC_TMUX=tmux 3.4\nCC_CLAUDE=1.0.72 (Claude Code)\nCC_BREW=/opt/homebrew/bin/brew\nCC_NPM=/Users/u/.nvm/versions/node/v22/bin/npm\n";
        let r = parse_probe_output(out);
        assert!(r.tmux.found && r.tmux.ok);
        assert_eq!(r.tmux.version.as_deref(), Some("tmux 3.4"));
        assert!(r.claude.found && r.claude.ok);
        assert!(r.brew && r.npm);
    }

    #[test]
    fn probe_output_all_missing() {
        let r = parse_probe_output("CC_TMUX=\nCC_CLAUDE=\nCC_BREW=\nCC_NPM=\n");
        assert!(!r.tmux.found && !r.tmux.ok && r.tmux.version.is_none());
        assert!(!r.claude.found && !r.claude.ok);
        assert!(!r.brew && !r.npm);
    }

    #[test]
    fn probe_output_old_tmux_found_but_not_ok() {
        let r = parse_probe_output("CC_TMUX=tmux 3.2\n");
        assert!(r.tmux.found);
        assert!(!r.tmux.ok);
    }

    #[test]
    fn report_serializes_camel_case() {
        let r = parse_probe_output("CC_TMUX=tmux 3.4\n");
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"tmux\""), "json: {json}");
        assert!(json.contains("\"found\":true"), "json: {json}");
        // Option<String> None serializes as null (frontend types it string|null).
        assert!(json.contains("\"version\":null"), "json: {json}");
    }
}
```

- [ ] **Step 2: Register the module and run tests**

In `app/src-tauri/src/lib.rs` module list (alphabetical, after `pub mod manager;`): add

```rust
pub mod onboarding;
```

Run: `cd app/src-tauri && cargo test onboarding`
Expected: all 6 tests PASS. (Dead-code warnings for the not-yet-used structs are fine at this stage.)

- [ ] **Step 3: Commit**

```bash
git add app/src-tauri/src/onboarding.rs app/src-tauri/src/lib.rs
git commit -m "feat(onboarding): prereq probe parsing (tmux>=3.3 gate, sentinel-keyed output)"
```

---

### Task 3: `check_prereqs` command + frontend wrapper

**Files:**
- Modify: `app/src-tauri/src/onboarding.rs` (add command)
- Modify: `app/src-tauri/src/lib.rs` (`generate_handler!` list at :751-791)
- Modify: `app/frontend/src/ipc.ts` (append near the settings wrappers at :560-576)

**Interfaces:**
- Consumes: `parse_probe_output` from Task 2.
- Produces: Tauri command `check_prereqs() -> Result<PrereqReport, String>` (async); TS `checkPrereqs(): Promise<PrereqReport>` and interfaces `ToolStatus { found: boolean; version: string | null; ok: boolean }`, `PrereqReport { tmux: ToolStatus; claude: ToolStatus; brew: boolean; npm: boolean }`.

- [ ] **Step 1: Add the probe command**

In `app/src-tauri/src/onboarding.rs`, add `use std::process::Command;` to the imports, then:

```rust
/// One login-shell round trip probing all four tools. `2>/dev/null` per tool:
/// a missing binary prints an empty value instead of shell noise.
const PROBE_SCRIPT: &str = "printf 'CC_TMUX=%s\\n' \"$(tmux -V 2>/dev/null)\"; \
printf 'CC_CLAUDE=%s\\n' \"$(claude --version 2>/dev/null)\"; \
printf 'CC_BREW=%s\\n' \"$(command -v brew 2>/dev/null)\"; \
printf 'CC_NPM=%s\\n' \"$(command -v npm 2>/dev/null)\"";

/// Probe the login-shell environment for the cockpit's prerequisites. Async so
/// the (up to ~2 s — `claude --version` pays node startup) probe never runs on
/// the main thread; the wizard shows a spinner meanwhile.
#[tauri::command]
pub async fn check_prereqs() -> Result<PrereqReport, String> {
    let out = Command::new("zsh")
        .args(["-lc", PROBE_SCRIPT])
        .output()
        .map_err(|e| format!("spawn zsh probe: {e}"))?;
    Ok(parse_probe_output(&String::from_utf8_lossy(&out.stdout)))
}
```

- [ ] **Step 2: Register the handler**

In `app/src-tauri/src/lib.rs` `generate_handler!` list, after `settings::effective_default_cwd,`:

```rust
            onboarding::check_prereqs,
```

Run: `cd app/src-tauri && cargo test`
Expected: full suite PASS, no warnings about unused `PrereqReport`.

- [ ] **Step 3: Sanity-run the probe on this machine**

Run: `zsh -lc "printf 'CC_TMUX=%s\n' \"\$(tmux -V 2>/dev/null)\"; printf 'CC_CLAUDE=%s\n' \"\$(claude --version 2>/dev/null)\"; printf 'CC_BREW=%s\n' \"\$(command -v brew 2>/dev/null)\"; printf 'CC_NPM=%s\n' \"\$(command -v npm 2>/dev/null)\""`
Expected: four `CC_*=` lines with real values (this machine has all four). This closes the spec's open verification: nvm-installed `npm` resolving under `zsh -lc`. **If `CC_NPM=` comes back empty**, nvm loads only in `~/.zshrc` — change the probe args from `-lc` to `-ilc` (matching the PATH capture at `lib.rs:662`) and note it in the commit message.

- [ ] **Step 4: Add the frontend wrapper**

In `app/frontend/src/ipc.ts`, after `effectiveDefaultCwd`:

```ts
/** Presence/health of one probed tool (onboarding wizard, step 1). */
export interface ToolStatus {
  found: boolean;
  /** Raw version line (e.g. `"tmux 3.4"`), null when not found. */
  version: string | null;
  /** tmux: found AND version >= 3.3. claude: same as `found`. */
  ok: boolean;
}

/** Everything the wizard's environment step renders. */
export interface PrereqReport {
  tmux: ToolStatus;
  claude: ToolStatus;
  brew: boolean;
  npm: boolean;
}

/** Probe the login-shell environment for tmux / claude / brew / npm. */
export function checkPrereqs(): Promise<PrereqReport> {
  return invoke<PrereqReport>("check_prereqs");
}
```

Run: `cd app/frontend && npm run typecheck`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add app/src-tauri/src/onboarding.rs app/src-tauri/src/lib.rs app/frontend/src/ipc.ts
git commit -m "feat(onboarding): check_prereqs command + frontend wrapper"
```

---

### Task 4: install runner (enum whitelist, streamed output, cancel, exit-kill)

**Files:**
- Modify: `app/src-tauri/src/onboarding.rs`
- Modify: `app/src-tauri/src/lib.rs` (handler list; `.manage(...)` near :749; `.run(...)` tail at :813-814)

**Interfaces:**
- Consumes: nothing new.
- Produces: Tauri commands `install_prereq(tool: PrereqTool) -> Result<(), String>` and `cancel_install()`; managed state `pub struct InstallGuard(pub Mutex<Option<u32>>)`; `pub fn kill_running_install(&InstallGuard)`; events `onboarding:install-line` payload `{ tool: "tmux"|"claude-cli", line: string }` and `onboarding:install-done` payload `{ tool, exitCode: number }` (camelCase).

- [ ] **Step 1: Write the failing serde-whitelist tests**

Append to `mod tests` in `onboarding.rs`:

```rust
    #[test]
    fn tool_enum_rejects_everything_but_the_two_variants() {
        assert_eq!(
            serde_json::from_str::<PrereqTool>("\"tmux\"").unwrap(),
            PrereqTool::Tmux
        );
        assert_eq!(
            serde_json::from_str::<PrereqTool>("\"claude-cli\"").unwrap(),
            PrereqTool::ClaudeCli
        );
        assert!(serde_json::from_str::<PrereqTool>("\"tmux; rm -rf /\"").is_err());
        assert!(serde_json::from_str::<PrereqTool>("\"brew\"").is_err());
        assert!(serde_json::from_str::<PrereqTool>("\"\"").is_err());
    }

    #[test]
    fn install_scripts_are_exactly_the_two_known_commands() {
        assert_eq!(PrereqTool::Tmux.install_script(), "brew install tmux 2>&1");
        assert_eq!(
            PrereqTool::ClaudeCli.install_script(),
            "npm install -g @anthropic-ai/claude-code 2>&1"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd app/src-tauri && cargo test onboarding`
Expected: COMPILE FAIL — `cannot find type PrereqTool`.

- [ ] **Step 3: Implement the runner**

In `onboarding.rs`, extend imports:

```rust
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
```

Then add:

```rust
/// The only things the wizard can install. `install_prereq` takes this enum —
/// never a string — so no user-controlled text can reach a shell. Serde
/// (kebab-case) rejects unknown values at the IPC boundary.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum PrereqTool {
    Tmux,
    ClaudeCli,
}

impl PrereqTool {
    /// Hardcoded install command. `2>&1` merges stderr into the streamed log
    /// so a single reader thread sees everything in order.
    fn install_script(self) -> &'static str {
        match self {
            PrereqTool::Tmux => "brew install tmux 2>&1",
            PrereqTool::ClaudeCli => "npm install -g @anthropic-ai/claude-code 2>&1",
        }
    }
}

/// Process-group id of the running install, if any. One install at a time —
/// the wizard disables the other Install button while this is `Some`.
#[derive(Default)]
pub struct InstallGuard(pub Mutex<Option<u32>>);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallLine {
    tool: PrereqTool,
    line: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallDone {
    tool: PrereqTool,
    exit_code: i32,
}

/// Start an install. Returns immediately; output streams via
/// `onboarding:install-line` and completion via `onboarding:install-done`.
#[tauri::command]
pub fn install_prereq(
    app: AppHandle,
    guard: State<InstallGuard>,
    tool: PrereqTool,
) -> Result<(), String> {
    let mut running = guard.0.lock().unwrap();
    if running.is_some() {
        return Err("an install is already running".into());
    }

    use std::os::unix::process::CommandExt;
    let mut child = Command::new("zsh")
        .args(["-lc", tool.install_script()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null()) // merged into stdout by the script's 2>&1
        .process_group(0) // own group ⇒ cancel kills brew/npm's children too
        .spawn()
        .map_err(|e| format!("spawn installer: {e}"))?;
    // With process_group(0) the child's pid IS its pgid.
    *running = Some(child.id());
    drop(running);

    let stdout = child.stdout.take().expect("stdout was piped");
    let app2 = app.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let _ = app2.emit("onboarding:install-line", InstallLine { tool, line });
        }
        // Killed-by-signal (cancel) has no exit code — report -1, non-zero.
        let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
        *app2.state::<InstallGuard>().0.lock().unwrap() = None;
        let _ = app2.emit("onboarding:install-done", InstallDone { tool, exit_code: code });
    });
    Ok(())
}

/// Kill the whole install process group (zsh + brew/npm + their children).
/// Best-effort: the reader thread sees EOF and emits `install-done` itself.
/// `/bin/kill` with a negative pgid avoids a libc dependency.
pub fn kill_running_install(guard: &InstallGuard) {
    if let Some(pgid) = *guard.0.lock().unwrap() {
        let _ = Command::new("/bin/kill")
            .args(["-TERM", &format!("-{pgid}")])
            .status();
    }
}

/// Frontend cancel button / wizard-close hook.
#[tauri::command]
pub fn cancel_install(guard: State<InstallGuard>) {
    kill_running_install(&guard);
}
```

- [ ] **Step 4: Wire into lib.rs**

Three edits in `app/src-tauri/src/lib.rs`:

1. After `.manage(AppState::default())` (~:749) add:

```rust
        .manage(onboarding::InstallGuard::default())
```

2. In `generate_handler!`, after `onboarding::check_prereqs,`:

```rust
            onboarding::install_prereq,
            onboarding::cancel_install,
```

3. Replace the run tail

```rust
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
```

with:

```rust
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // ⌘Q with an install running: the installer sits in its own
            // process group (so Cancel can kill brew's children), which also
            // means it would survive the app — kill it on exit instead.
            if let tauri::RunEvent::Exit = event {
                let guard = app.state::<onboarding::InstallGuard>();
                onboarding::kill_running_install(&guard);
            }
        });
```

(`&guard` deref-coerces `&State<InstallGuard>` → `&InstallGuard`.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd app/src-tauri && cargo test`
Expected: full suite PASS, including the two new tests. Build must be warning-free for `onboarding.rs` (everything is now referenced).

- [ ] **Step 6: Commit**

```bash
git add app/src-tauri/src/onboarding.rs app/src-tauri/src/lib.rs
git commit -m "feat(onboarding): hardcoded-command install runner with streamed output + cancel"
```

---

### Task 5: frontend IPC wrappers, boot gating, settings preservation

**Files:**
- Modify: `app/frontend/src/ipc.ts` (append after `checkPrereqs`)
- Modify: `app/frontend/src/store.ts` (bootCockpit catch at :310-316; `setDefaultCwd` at :879-896; new onboarding section after settings section ~:906; export `reloadSettings`)
- Modify: `app/frontend/src/App.tsx` (onMount at :28-32)

**Interfaces:**
- Consumes: `checkPrereqs`, `CockpitSettings` (Tasks 1/3); Rust commands from Task 4.
- Produces (Task 6 relies on these exact names): ipc — `type PrereqTool = "tmux" | "claude-cli"`, `installPrereq(tool: PrereqTool): Promise<void>`, `cancelInstall(): Promise<void>`, `onInstallLine(h: (p: InstallLinePayload) => void): Promise<UnlistenFn>`, `onInstallDone(h: (p: InstallDonePayload) => void): Promise<UnlistenFn>`, `interface InstallLinePayload { tool: PrereqTool; line: string }`, `interface InstallDonePayload { tool: PrereqTool; exitCode: number }`. store — signals `onboardingOpen: () => boolean`, `onboardingMode: () => "first-run" | "rerun"`; functions `decideBoot(): Promise<void>`, `finishOnboarding(): Promise<void>` (finish AND skip both call this), `openOnboardingRerun(): void`, `reloadSettings(): Promise<void>` (now exported).

- [ ] **Step 1: ipc.ts additions**

After `checkPrereqs` in `app/frontend/src/ipc.ts`:

```ts
/** The only installable tools — mirrors the Rust `PrereqTool` enum (kebab-case). */
export type PrereqTool = "tmux" | "claude-cli";

/** Start a one-click install. Resolves once SPAWNED — progress arrives via
 *  `onInstallLine` / `onInstallDone`. Rejects if an install is already running. */
export function installPrereq(tool: PrereqTool): Promise<void> {
  return invoke("install_prereq", { tool });
}

/** Kill the running install's whole process group. No-op when idle. */
export function cancelInstall(): Promise<void> {
  return invoke("cancel_install");
}

export interface InstallLinePayload {
  tool: PrereqTool;
  line: string;
}
export interface InstallDonePayload {
  tool: PrereqTool;
  /** Process exit code; -1 when killed by a signal (cancel). 0 = success. */
  exitCode: number;
}

export function onInstallLine(
  handler: (p: InstallLinePayload) => void,
): Promise<UnlistenFn> {
  return listen<InstallLinePayload>("onboarding:install-line", (e) => handler(e.payload));
}
export function onInstallDone(
  handler: (p: InstallDonePayload) => void,
): Promise<UnlistenFn> {
  return listen<InstallDonePayload>("onboarding:install-done", (e) => handler(e.payload));
}
```

(`listen` and `UnlistenFn` are already imported at `ipc.ts:14`.)

- [ ] **Step 2: store.ts — onboarding section**

Add after the settings section (after `toggleSettings`, ~:906). Requires `CockpitSettings` in the ipc import list at the top of `store.ts` (add it if absent):

```ts
// ── Onboarding (first-run wizard) ───────────────────────────────────────────

const [onboardingOpen, setOnboardingOpen] = createSignal(false);
const [onboardingMode, setOnboardingMode] = createSignal<"first-run" | "rerun">(
  "first-run",
);
export { onboardingOpen, onboardingMode };

/** True when this launch showed the wizard — the boot-failure toast then adds
 *  a route-back hint (Skip with tmux still missing must stay recoverable). */
let wizardShownThisLaunch = false;

/** Boot decision, called once on mount instead of `bootCockpit()`: flag set ⇒
 *  boot exactly as before; absent or settings unreadable ⇒ wizard first, boot
 *  deferred until finish/skip. The corrupt-file case deliberately does NOT
 *  rewrite the file here — only a finish/skip does. */
export async function decideBoot(): Promise<void> {
  let done = false;
  try {
    done = (await loadSettings()).onboardingDone === true;
  } catch {
    done = false; // corrupt settings file ⇒ treat as first run
  }
  if (done) {
    void bootCockpit();
  } else {
    wizardShownThisLaunch = true;
    setOnboardingMode("first-run");
    setOnboardingOpen(true);
  }
}

/** Finish OR skip (both persist the flag — the wizard must never nag twice),
 *  close the wizard, and in first-run mode start the deferred boot. Re-run
 *  mode (opened from Settings over a running app) never re-boots. */
export async function finishOnboarding(): Promise<void> {
  try {
    let current: CockpitSettings = { schemaVersion: 1 };
    try {
      current = await loadSettings();
    } catch {
      /* corrupt file: the user chose finish/skip — overwriting now is the
         spec'd behavior */
    }
    await saveSettings({ ...current, schemaVersion: 1, onboardingDone: true });
  } catch (e) {
    setStore("error", `Could not save onboarding state: ${e}`);
  }
  setOnboardingOpen(false);
  if (onboardingMode() === "first-run") void bootCockpit();
}

/** Settings → "Show welcome guide": open over the running app. */
export function openOnboardingRerun(): void {
  setOnboardingMode("rerun");
  setOnboardingOpen(true);
}
```

- [ ] **Step 3: store.ts — boot-failure hint**

In `bootCockpit`'s catch (store.ts:310-316), replace:

```ts
    setStore("error", `cockpit_init failed: ${String(e)}`);
```

with:

```ts
    const hint = wizardShownThisLaunch
      ? " — open Settings (⌘,) → Show welcome guide to re-check prerequisites"
      : "";
    setStore("error", `cockpit_init failed: ${String(e)}${hint}`);
```

- [ ] **Step 4: store.ts — `setDefaultCwd` must preserve `onboardingDone`**

The current body builds `{ schemaVersion: 1 }` from scratch — after Task 1 that would silently reset `onboardingDone` to false on every folder change, re-arming the wizard. Replace the `try` block of `setDefaultCwd` (store.ts:879-896) with:

```ts
  try {
    // Start from the file's current contents so unrelated fields
    // (onboardingDone) survive a folder change.
    let current: CockpitSettings = { schemaVersion: 1 };
    try {
      current = await loadSettings();
    } catch {
      /* corrupt file: best-effort — this save rewrites it cleanly */
    }
    const next: CockpitSettings = { ...current, schemaVersion: 1 };
    delete next.defaultCwd;
    if (dir.trim()) next.defaultCwd = dir.trim();
    const effective = await saveSettings(next);
    setSettings("defaultCwd", next.defaultCwd ?? "");
    setSettings("effectiveCwd", effective);
  } catch (e) {
```

Also change `async function reloadSettings` to `export async function reloadSettings` (the wizard's folder step pre-fills through it).

- [ ] **Step 5: App.tsx — defer boot**

In `app/frontend/src/App.tsx`, change the store import to pull `decideBoot` (and drop `bootCockpit` if now unused), and replace in `onMount`:

```ts
    void bootCockpit();
```

with:

```ts
    void decideBoot();
```

`ftInitHome()` and `installKeyboard()` stay — both are tmux-independent (`home_dir` is a plain fs call; keyboard handlers only act on store state).

- [ ] **Step 6: Typecheck**

Run: `cd app/frontend && npm run typecheck`
Expected: PASS. (`OnboardingWizard` isn't rendered yet — that's Task 6; `onboardingOpen` may be flagged unused only if the linter runs, tsc won't complain about unused exports.)

- [ ] **Step 7: Commit**

```bash
git add app/frontend/src/ipc.ts app/frontend/src/store.ts app/frontend/src/App.tsx
git commit -m "feat(onboarding): boot gating + install IPC + settings-preserving saves"
```

---

### Task 6: `OnboardingWizard` component + rendering + CSS

**Files:**
- Create: `app/frontend/src/components/OnboardingWizard.tsx`
- Modify: `app/frontend/src/App.tsx` (render wizard; adjust boot fallback)
- Modify: `app/frontend/src/styles.css` (append; existing modal styles at :743+)

**Interfaces:**
- Consumes: everything Task 5 produced; `settings`, `setDefaultCwd`, `setSettingsError`, `reloadSettings` from store; `effectiveDefaultCwd` not needed directly (settings store holds it).
- Produces: `export const OnboardingWizard: Component`.

- [ ] **Step 1: Write the component**

Create `app/frontend/src/components/OnboardingWizard.tsx`:

```tsx
// OnboardingWizard — first-run setup (and Settings → "Show welcome guide").
//
// Three steps: environment check → projects folder → shortcuts. Step 1 blocks
// Continue on tmux only (the app cannot function without it); claude is a
// warning. One-click installs go through the hardcoded-command runner in
// src-tauri/src/onboarding.rs. Skip is always available and persists the
// done-flag too — the wizard must never nag twice. Re-run mode (from Settings)
// overlays the running app and never re-triggers boot.

import {
  createSignal,
  onCleanup,
  onMount,
  Show,
  type Component,
} from "solid-js";
import { open } from "@tauri-apps/plugin-dialog";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  checkPrereqs,
  installPrereq,
  cancelInstall,
  onInstallLine,
  onInstallDone,
  type PrereqReport,
  type PrereqTool,
} from "../ipc";
import {
  settings,
  setDefaultCwd,
  setSettingsError,
  reloadSettings,
  finishOnboarding,
  onboardingMode,
} from "../store";

const MANUAL_CMD: Record<PrereqTool, string> = {
  tmux: "brew install tmux",
  "claude-cli": "npm install -g @anthropic-ai/claude-code",
};

type StepId = 1 | 2 | 3;

export const OnboardingWizard: Component = () => {
  const [step, setStep] = createSignal<StepId>(1);
  const [report, setReport] = createSignal<PrereqReport | null>(null);
  const [checking, setChecking] = createSignal(false);
  const [checkError, setCheckError] = createSignal<string | null>(null);
  const [installing, setInstalling] = createSignal<PrereqTool | null>(null);
  const [log, setLog] = createSignal<string[]>([]);
  const [failedTool, setFailedTool] = createSignal<PrereqTool | null>(null);

  let unlisteners: UnlistenFn[] = [];

  async function runCheck() {
    setChecking(true);
    setCheckError(null);
    try {
      setReport(await checkPrereqs());
    } catch (e) {
      // Probe itself failed (zsh missing — effectively impossible): tools
      // unknown, manual commands shown, Skip still available.
      setReport(null);
      setCheckError(String(e));
    } finally {
      setChecking(false);
    }
  }

  onMount(() => {
    void runCheck();
    void reloadSettings(); // pre-fill step 2's folder row
    void (async () => {
      unlisteners = [
        await onInstallLine((p) => setLog((l) => [...l, p.line])),
        await onInstallDone((p) => {
          setInstalling(null);
          if (p.exitCode === 0) {
            setFailedTool(null);
            void runCheck(); // re-probe so the row flips to ✓ by itself
          } else {
            setFailedTool(p.tool); // log stays expanded; manual fallback shows
          }
        }),
      ];
    })();
  });
  onCleanup(() => {
    // Wizard closing mid-install: kill the child. No-op when idle.
    if (installing()) void cancelInstall();
    for (const un of unlisteners) un();
  });

  async function startInstall(tool: PrereqTool) {
    setLog([]);
    setFailedTool(null);
    setInstalling(tool);
    try {
      await installPrereq(tool);
    } catch (e) {
      setInstalling(null);
      setFailedTool(tool);
      setLog((l) => [...l, String(e)]);
    }
  }

  async function chooseFolder() {
    // Same guard as SettingsDialog: the native dialog can fail for OS-level
    // reasons; without a catch the button would just look inert.
    try {
      const picked = await open({
        directory: true,
        multiple: false,
        title: "Choose the folder new tabs open in",
        defaultPath: settings.effectiveCwd || undefined,
      });
      if (typeof picked === "string") await setDefaultCwd(picked);
    } catch (e) {
      setSettingsError(`Could not open the folder picker: ${e}`);
    }
  }

  const tmuxOk = () => report()?.tmux.ok ?? false;
  const claudeOk = () => report()?.claude.ok ?? false;

  return (
    <div class="modal-overlay onboarding-overlay">
      <div class="modal onboarding" onClick={(e) => e.stopPropagation()}>
        <div class="modal-header">
          <span class="modal-title">Welcome to CC Cockpit</span>
          <span class="onb-stepmark">step {step()} / 3</span>
        </div>

        {/* ── Step 1: environment ── */}
        <Show when={step() === 1}>
          <p class="field-hint">
            The cockpit drives a tmux session and runs Claude Code inside it.
            Two tools to check:
          </p>

          <Show when={checkError()}>
            <div class="field-error">
              Could not probe the environment: {checkError()} — run the
              commands below manually, or skip.
            </div>
          </Show>

          <div class="onb-row">
            <span class="onb-badge" classList={{ ok: tmuxOk() }}>
              {tmuxOk() ? "✓" : "✗"}
            </span>
            <div class="onb-row-main">
              <span class="onb-row-title">tmux — required</span>
              <span class="field-hint">
                {checking()
                  ? "checking…"
                  : report()?.tmux.found
                    ? tmuxOk()
                      ? report()?.tmux.version
                      : `${report()?.tmux.version} — 3.3 or newer required`
                    : "not found"}
              </span>
            </div>
            <Show when={!checking() && !tmuxOk()}>
              <Show
                when={report()?.brew}
                fallback={<code class="onb-cmd">{MANUAL_CMD["tmux"]}</code>}
              >
                <button
                  type="button"
                  class="btn btn-primary"
                  disabled={installing() !== null}
                  onClick={() => void startInstall("tmux")}
                >
                  {installing() === "tmux" ? "Installing…" : "Install"}
                </button>
              </Show>
            </Show>
          </div>

          <div class="onb-row">
            <span class="onb-badge" classList={{ ok: claudeOk() }}>
              {claudeOk() ? "✓" : "!"}
            </span>
            <div class="onb-row-main">
              <span class="onb-row-title">claude CLI — recommended</span>
              <span class="field-hint">
                {checking()
                  ? "checking…"
                  : claudeOk()
                    ? report()?.claude.version
                    : "not found — plain shell panes still work"}
              </span>
            </div>
            <Show when={!checking() && !claudeOk()}>
              <Show
                when={report()?.npm}
                fallback={<code class="onb-cmd">{MANUAL_CMD["claude-cli"]}</code>}
              >
                <button
                  type="button"
                  class="btn"
                  disabled={installing() !== null}
                  onClick={() => void startInstall("claude-cli")}
                >
                  {installing() === "claude-cli" ? "Installing…" : "Install"}
                </button>
              </Show>
            </Show>
          </div>

          <Show when={report() && !report()!.brew && !tmuxOk()}>
            <div class="field-hint">
              No Homebrew found — install it from{" "}
              <code class="onb-cmd">https://brew.sh</code> first, then Re-check.
            </div>
          </Show>

          <div class="onb-row-actions">
            <button
              type="button"
              class="btn"
              disabled={checking() || installing() !== null}
              onClick={() => void runCheck()}
            >
              Re-check
            </button>
            <Show when={installing() !== null}>
              <button
                type="button"
                class="btn btn-ghost"
                onClick={() => void cancelInstall()}
              >
                Cancel install
              </button>
            </Show>
          </div>

          <Show when={log().length > 0}>
            <pre class="onb-log">{log().join("\n")}</pre>
          </Show>
          <Show when={failedTool()}>
            <div class="field-error">
              Install failed. Run it manually in a terminal:{" "}
              <code class="onb-cmd">{MANUAL_CMD[failedTool()!]}</code>
            </div>
          </Show>
        </Show>

        {/* ── Step 2: projects folder ── */}
        <Show when={step() === 2}>
          <div class="field">
            <span class="field-label">
              Projects folder{" "}
              <span class="field-hint">where new tabs and panes start</span>
            </span>
            <div class="settings-row">
              <code class="settings-path" title={settings.defaultCwd || undefined}>
                {settings.loading
                  ? "Loading…"
                  : settings.defaultCwd ||
                    "Not set — using the built-in default"}
              </code>
              <button
                type="button"
                class="btn btn-primary"
                disabled={settings.loading || settings.saving}
                onClick={() => void chooseFolder()}
              >
                Choose…
              </button>
            </div>
            <Show when={!settings.loading && settings.effectiveCwd}>
              <div class="field-hint">
                New tabs open in {settings.effectiveCwd}
              </div>
            </Show>
            <Show when={settings.error}>
              <div class="field-error">{settings.error}</div>
            </Show>
          </div>
        </Show>

        {/* ── Step 3: shortcuts ── */}
        <Show when={step() === 3}>
          <div class="onb-shortcuts">
            <div class="onb-key"><kbd>⌘T</kbd> new tab</div>
            <div class="onb-key"><kbd>⌘D</kbd> split pane</div>
            <div class="onb-key"><kbd>⌘1–9</kbd> switch tabs</div>
            <div class="onb-key"><kbd>⌘B</kbd> file tree</div>
            <div class="onb-key"><kbd>⌘I</kbd> inventory</div>
            <div class="onb-key"><kbd>⌘⇧T</kbd> team board</div>
            <div class="onb-key"><kbd>⌘,</kbd> settings</div>
          </div>
          <p class="field-hint">
            Every pane shows a status badge — Working / Needs input / Idle /
            Dead — so you can jump straight to the pane that needs you.
          </p>
        </Show>

        <div class="modal-actions">
          <button
            type="button"
            class="btn btn-ghost"
            onClick={() => void finishOnboarding()}
          >
            {onboardingMode() === "rerun" ? "Close" : "Skip setup"}
          </button>
          <span class="footer-spacer" />
          <Show when={step() > 1}>
            <button
              type="button"
              class="btn"
              onClick={() => setStep((s) => (s - 1) as StepId)}
            >
              Back
            </button>
          </Show>
          <Show
            when={step() < 3}
            fallback={
              <button
                type="button"
                class="btn btn-primary"
                onClick={() => void finishOnboarding()}
              >
                Start
              </button>
            }
          >
            <button
              type="button"
              class="btn btn-primary"
              disabled={step() === 1 && !tmuxOk()}
              onClick={() => setStep((s) => (s + 1) as StepId)}
            >
              Continue
            </button>
          </Show>
        </div>
      </div>
    </div>
  );
};
```

Notes baked into the design (do not "fix" these):
- The overlay has NO click-to-dismiss — a first-run wizard dismissed by a stray click would just re-arm confusion. Skip is the explicit way out.
- In re-run mode Step 1's Continue gate still applies, but Close is always available; `finishOnboarding` in re-run mode only re-persists the (already-true) flag and closes — it never calls `bootCockpit`.
- `defaultCwd` is only written when the user actually picks a folder (`chooseFolder`) — re-run mode therefore never clobbers it, per spec.

- [ ] **Step 2: Render it in App.tsx**

In `app/frontend/src/App.tsx`:

1. Import: `import { OnboardingWizard } from "./components/OnboardingWizard";` and add `onboardingOpen` to the store import.
2. Inside `<div class="app">`, BEFORE the `<Show when={store.ready}>` block, add:

```tsx
      <Show when={onboardingOpen()}>
        <OnboardingWizard />
      </Show>
```

3. The boot fallback must not show behind the wizard (nothing is booting while deferred). Replace:

```tsx
        fallback={<div class="boot">cockpit booting…</div>}
```

with:

```tsx
        fallback={
          <Show when={!onboardingOpen()}>
            <div class="boot">cockpit booting…</div>
          </Show>
        }
```

- [ ] **Step 3: CSS**

Append to `app/frontend/src/styles.css`:

```css
/* ── Onboarding wizard ─────────────────────────────────────────────────── */

.onboarding {
  width: 560px;
  max-width: 90vw;
}
.onb-stepmark {
  margin-left: auto;
  opacity: 0.6;
  font-size: 12px;
}
.onb-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 0;
}
.onb-row-main {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-width: 0;
}
.onb-row-title {
  font-weight: 600;
}
.onb-badge {
  width: 20px;
  text-align: center;
  font-weight: 700;
  color: #e5534b;
}
.onb-badge.ok {
  color: #3fb950;
}
.onb-cmd {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
  padding: 2px 6px;
  border-radius: 4px;
  background: rgba(128, 128, 128, 0.15);
  user-select: all; /* one click selects the whole command for copying */
}
.onb-row-actions {
  display: flex;
  gap: 8px;
  margin-top: 6px;
}
.onb-log {
  max-height: 160px;
  overflow: auto;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px;
  line-height: 1.4;
  background: rgba(0, 0, 0, 0.25);
  border-radius: 6px;
  padding: 8px;
  margin: 8px 0 0;
  white-space: pre-wrap;
}
.onb-shortcuts {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 6px 16px;
  margin: 10px 0;
}
.onb-key kbd {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
  background: rgba(128, 128, 128, 0.15);
  border-radius: 4px;
  padding: 1px 6px;
  margin-right: 6px;
}
```

(If `styles.css` defines a monospace CSS variable — check the `.mono` class near the top — use it instead of the literal font stack, matching repo idiom.)

- [ ] **Step 4: Typecheck + build**

Run: `cd app/frontend && npm run typecheck && npm run build`
Expected: both PASS.

- [ ] **Step 5: Commit**

```bash
git add app/frontend/src/components/OnboardingWizard.tsx app/frontend/src/App.tsx app/frontend/src/styles.css
git commit -m "feat(onboarding): 3-step wizard component (env check, folder, shortcuts)"
```

---

### Task 7: Settings re-run button

**Files:**
- Modify: `app/frontend/src/components/SettingsDialog.tsx` (before `modal-actions` at :142)

**Interfaces:**
- Consumes: `openOnboardingRerun`, `closeSettings` from store.

- [ ] **Step 1: Add the button**

In `SettingsDialog.tsx`, add `openOnboardingRerun` to the store import, then insert before the `<Show when={settings.error}>` block (:138):

```tsx
        <div class="field">
          <span class="field-label">Welcome guide</span>
          <div class="settings-row">
            <span class="field-hint">
              Re-run the first-launch setup: environment check, folder,
              shortcuts.
            </span>
            <button
              type="button"
              class="btn"
              onClick={() => {
                closeSettings();
                openOnboardingRerun();
              }}
            >
              Show welcome guide
            </button>
          </div>
        </div>
```

- [ ] **Step 2: Typecheck**

Run: `cd app/frontend && npm run typecheck`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add app/frontend/src/components/SettingsDialog.tsx
git commit -m "feat(onboarding): Show welcome guide button in Settings"
```

---

### Task 8: Full verification + live smoke

**Files:** none new — verification only.

- [ ] **Step 1: Full automated suite**

Run: `cd app/src-tauri && cargo test` — expected: all green (339+ tests plus the new onboarding/settings ones).
Run: `cd app/frontend && npm run typecheck && npm run build` — expected: both green.

- [ ] **Step 2: Dev-build live smoke — normal path**

Per repo gotchas: quit the running CC Cockpit app first; `tauri dev` shares the tmux `cockpit-main` session.

Run `npm run tauri dev` from `app/`. Settings file lives at `~/Library/Application Support/com.cc-cockpit.app/cockpit/settings.json` (verify the exact bundle id dir by `ls ~/Library/Application\ Support/ | grep -i cockpit`). Back it up, then delete the `onboardingDone` key (or the file, noting it also holds `defaultCwd`).

1. Launch → wizard appears, cockpit does NOT boot behind it; all rows green on this machine → Continue enabled.
2. Step 2 shows the effective folder; Step 3 shows shortcuts; **Start** → cockpit boots, tabs work.
3. Relaunch → no wizard (flag persisted).
4. Settings (⌘,) → "Show welcome guide" → wizard overlays the running app; **Close** → app untouched, no re-boot.
5. Change the default folder in Settings afterwards → relaunch → still no wizard (`onboardingDone` survived `setDefaultCwd`).

- [ ] **Step 3: Live smoke — tmux-missing path (release gate)**

1. `sudo mv /opt/homebrew/bin/tmux /opt/homebrew/bin/tmux.bak`, delete the flag from settings.json, relaunch.
2. Wizard: tmux row ✗, Continue disabled, Install button visible (brew present).
3. Click **Install** → log streams brew output live → on success row flips to ✓ automatically, Continue enables. (Alternative if you don't want a real install: restore `tmux.bak` and click **Re-check** instead — row flips to ✓.)
4. While an install runs: second Install button disabled; **Cancel install** kills it and `onboarding:install-done` arrives with a non-zero code, manual-command fallback appears.
5. Skip with tmux still missing → boot-failure toast includes "Settings (⌘,) → Show welcome guide".
6. Restore: `sudo mv /opt/homebrew/bin/tmux.bak /opt/homebrew/bin/tmux`.

- [ ] **Step 4: Record results + commit any smoke fixes**

Update the plan checkboxes; if smoke surfaced fixes, commit them with `fix(onboarding): <what the smoke caught>`. Do NOT merge to main without the live smoke — per build rules, unsmoked = YELLOW.

---

## Self-review notes (already applied)

- Spec's "one-install-at-a-time guard in the manager" is implemented as `InstallGuard` managed state, not in `manager.rs` — `manager.rs` is the tmux session manager and has no business owning installer state. Same guarantee, better home.
- Spec's open verification #1 (bootCockpit deferral vs `ftInitHome`/keyboard): resolved — both are tmux-independent, they stay unconditional (Task 5 Step 5).
- Spec's open verification #2 (nvm npm under `zsh -lc`): probed live in Task 3 Step 3, with the `-ilc` fallback documented inline.
- `setDefaultCwd` clobbering `onboardingDone` was found during planning and fixed in Task 5 Step 4 — without it the wizard would re-arm after any folder change.
- App-quit mid-install: handled via `RunEvent::Exit` (Task 4 Step 4), required because `process_group(0)` detaches the child from the app's group.

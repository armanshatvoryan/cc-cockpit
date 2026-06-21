# Terax → CC Cockpit — Backend / Architecture / Data-Model Findings (DEV)

**Provenance:** DEV (Phase-2 analysis, TaskList task #2). Throwaway spike, no shipped code.
**Axis:** backend / architecture / data-model only. Frontend (theming, editor, chat UX) is flagged and deferred to the frontend findings.
**Method:** Terax is closed-source — evidence is *inferred* from config JSON, `Terax.log`, and `strings` over the Mach-O arm64 binary (`/Applications/Terax.app/Contents/MacOS/terax`). Cockpit gaps are confirmed against the **real** source at `file:line`.

---

## 1. Terax config schema map

Persistence dir: `~/Library/Application Support/app.crynta.terax/`. Each file is a `tauri-plugin-store` domain store (see §3). Types below are inferred from observed values + serde signatures in the binary; out-of-scope fields are tagged inline.

### `terax-spaces.json` — workspace registry + per-space restorable UI tree  *(headline data model)*
```jsonc
{
  "activeId": "default",                    // string — id of focused space
  "spaces": [                               // Space[]
    {
      "id": "default",                      // string (stable key)
      "name": "Default",                    // string (display)
      "root": "/Users/armanshatvoran",      // string (absolute path; space's base dir)
      "env": { "kind": "local" },           // WorkspaceEnv (internally-tagged enum, see below)
      "createdAt": 1781559366658,           // number (epoch ms)
      "updatedAt": 1781559366658            // number (epoch ms)
    }
  ],
  "state:default": {                        // key = "state:<spaceId>", one per space
    "activeTabIndex": 4,                    // number (0-based index into tabs[])
    "tabs": [                               // Tab[]
      {
        "customTitle": "Dev-team",          // string OPTIONAL (absent ⇒ derive label from cwd)
        "kind": "terminal",                 // string (tab kind discriminator)
        "tree": { "active": true, "cwd": "/Users/.../Workflows", "kind": "leaf" }
      }
      // ... tabs without customTitle observed (index 2-4) ⇒ field is genuinely optional
    ]
  }
}
```

**`WorkspaceEnv`** is an *internally-tagged* enum keyed on `kind` (binary: `"internally tagged enum WorkspaceEnv"`, `"WorkspaceEnvLocal"`, `"struct variant WorkspaceEnv::Wsl with 1 element"` → `distro`). Observed: `{kind:"local"}`. Inferred variants: `{kind:"wsl", distro:"<name>"}` and a remote/SSH variant (binary carries `host key verification failed`, `SSH_ASKPASS`, `t/SSH`, `wsl:` — exact remote shape **unconfirmed**).

**Pane tree (`tree`) — task #5.** Observed leaf node, **verbatim**:
```json
{ "active": true, "cwd": "/Users/armanshatvoran/Workflows", "kind": "leaf" }
```
The `kind` discriminator implies a recursive node type: a **leaf** carries `{active, cwd}`; a split is **inferred** to be a branch node `{kind:"branch"|"split", …, children:[TreeNode]}`. **The branch node was NOT observed** (the user's spaces hold only single-pane leaves; binary strings for split variant names were dictionary-polluted and unverifiable). Treat branch as a reasonable inference, not fact. Net: the model is a **recursive node tree with a `kind` discriminator** (leaf vs branch), *not* a flat list with parent pointers.

### `terax-settings.json` — flat user prefs (key→scalar)
| key | type | in/out of scope |
|---|---|---|
| `autostart` | bool | in (QoL) |
| `vimMode` | bool | frontend-axis |
| `showHidden` | bool | frontend-axis |
| `zoomLevel` | number (0.9) | frontend-axis |
| `theme` / `themeId` / `editorTheme` | string (`dark`/`claude`/`copilot`) | **frontend-axis (theming)** |
| `autocompleteEnabled` | bool | frontend-axis |
| `defaultModelId`, `favoriteModelIds[]`, `recentModelIds[]` | string / string[] | **OUT OF SCOPE (model picker/favorites)** |
| `openaiCompatibleBaseURL`, `openaiCompatibleModelId` | string | **OUT OF SCOPE (OpenAI-compat API infra)** |
| `lmstudioModelId` | string | **OUT OF SCOPE (LM Studio / local model)** |

### Other domains
- `terax-ai-sessions.json` — `{ sessions:[{id,title,createdAt,updatedAt}], "messages:<id>":[], activeId }` → **OUT OF SCOPE (chat history, multi-provider)**.
- `terax-ai-agents.json` — `{ activeAgentId:"builtin:designer" }` → **OUT OF SCOPE (agent runtime)**.
- `terax-ai-snippets.json` — `{}` (empty; frontend `snippetsStore`) → frontend-axis.
- `terax-custom-themes.json` — `{}` (empty; frontend) → **frontend-axis (theming)**.
- `.window-state.json` — `tauri-plugin-window-state`; per-window geometry for **two** windows (`main`, `settings`): `{width,height,x,y,prev_x,prev_y,maximized,visible,decorated,fullscreen}`.

---

## 2. Module / architecture inventory (task #2)

**Backend Rust modules** — from `Terax.log` (`terax_lib::modules::*`, INFO-level) + binary string module paths + the `tauri::generate_handler!` command blob recovered from the binary:

| Module | Sub-paths (binary) | Tauri commands (recovered) | Role |
|---|---|---|---|
| **pty** | `pty::session`, `pty::shell_init::unix` | `pty_open`, `pty_close`, `pty_close_all`, `pty_resize`, `pty_write`, `pty_has_foreground_job`, `pty_has_foreground_process` | Interactive PTY lifecycle. **Only module logging at INFO** (423 lines: `pty opened/closed id= cols= rows=`, `pty cwd:`). |
| **fs** | `fs::filepath`, `fs::grep`, `fs::tree`/gitignore, `fs::watch`, `fs::mutate` | `fs_read_dir/read_file/write_file/stat/canonicalize/create_file/create_dir/rename/delete/copy`, `fs_watch_add/watch_remove`, `fs_search/list_files/glob`, `fs_grep/grep_interactive` | Filesystem: read/mutate, recursive gitignore-aware tree, ripgrep-style search, file watching. |
| **shell** | `shell`, `shellno` (non-unix stub) | `shell_run_command`, `shell_session_open/session_run/session_close`, `shell_bg_spawn/bg_logs/bg_kill/bg_list` | Non-interactive exec: one-shot, **persistent shell session**, and a **background-job manager** (spawn detached → poll logs → list/kill). |
| **workspace** | `workspace` | `workspace_authorize`, `workspace_current_dir`, `get_launch_dir`, `open_settings_window`, `reveal_item_in_dir`, `list_subdirs` | Space registry + **path-scope authorization** (binary: `workspace registry poisoned`, `canonical cache poisoned`, `pty_open: cwd rejected:`). |
| **secrets** | `secrets` | (`set_password`/`set_username` via keychain) | **Git/SSH credential storage** (`GIT_ASKPASS`, `SSH_ASKPASS`, `credential helper`) — **not** model API keys. Low fit for cockpit. |
| **history** | `history` | — | Persistent command history (`HISTFILE`; frontend `GitHistoryStack`/`history-*.js` chunks). Low fit. |
| *(git)* | not a logged module | `git_panel_snapshot`, `git_status`, `git_stage`, `git_unstage`, `git_discard`, `git_commit`, `git_fetch`, `git_log`, `git_show_commit`, `git_commit_file_diff`, `git_remote_url` | Integrated source control (frontend `SourceControlPanel`/`GitDiffStack`/`GitHistoryStack`). Clear command cluster; likely an internal git module under `shell`/`fs`. |

**No backend module for themes / snippets / ai-agents** — those are frontend stores (`agentsStore`, `managedAgentsStore`, `snippetsStore`, `bgImageStore`, `themes`, `planStore` — from `/assets/*.js` chunk names) persisted opaquely via the store plugin. **Architectural shape: backend = OS-integration only (pty/fs/shell/git/workspace/secrets/history); all AI/theming/snippet state is frontend + keyed-JSON persistence.**

**Other arch signals**
- Tauri command convention = `<module>_<verb>` snake_case (vs cockpit's mixed `create_tab` / `launch_cc`).
- **Multi-window**: `main` + `settings` (`open_settings_window`; both in `.window-state.json`).
- **Shell-integration injection**: writes `terax.fish` + zsh hooks into `~/.config/conf.d`, `.zshrc/.zprofile/.zlogin` ("`# terax-shell-integration`") to track cwd + command boundaries via OSC. (Cockpit instead reads cwd from tmux `#{pane_current_path}` — no shell mutation.)
- Plugins in use: `tauri-plugin-store`, `tauri-plugin-window-state`, `tauri-plugin-autostart`.

---

## 3. Persistence model (task #4)

- **Mechanism:** `tauri-plugin-store` (binary: `get_storeplugin`, `set_temp_dir_pathplugin`). **One JSON file per *domain*** (spaces, settings, ai-sessions, …) — **NOT** one-file-per-space.
- **Namespacing:** keys are namespaced *within* a domain file. The spaces file holds `activeId`, `spaces`, and one `state:<spaceId>` key per space; ai-sessions holds `sessions`, `activeId`, and one `messages:<id>` per chat.
- **Write granularity:** whole-file rewrite of the domain store when any of its keys change (plugin batches keys → file). Atomic **temp-write + rename** inferred from `set_temp_dir_path`.
- **Schema versioning:** **ABSENT.** Grep found **no** `schemaVersion`/`version` field in any Terax config file. Versioning is implicit; any migration would be field-presence-based. *(If cockpit copies this model, add an explicit `schemaVersion` — this is a gap to improve on, not replicate.)*
- **Timestamps:** `createdAt`/`updatedAt` epoch-ms on spaces and sessions. Window geometry is separate (`.window-state.json`).

---

## 4. Ranked steal-candidate table

Rubric: **Gate A** = Claude-Max compatible (single provider, no model/API/cost infra). **Gate B** = not already in cockpit (cite `file:line`). Survivors: **Value × Fit × Effort** (each 1-5; Effort inverse: 5 = smallest lift), max **125**. Tier: **STEAL ≥60 / MAYBE 30-59 / SKIP <30**. Fit is anchored to cockpit's actual mission: **Claude-Code teams over tmux git-worktrees**.

| # | Candidate (backend/data-model) | Gate A | Gate B (cockpit gap) | V | F | E | Score | Tier | t-shirt |
|---|---|---|---|---|---|---|---|---|---|
| 1 | **Disk-persisted cockpit layout** (tab list + custom names + per-tab cwd + activeTab) to survive tmux-session-loss | PASS | PASS — `store.ts:68-69` + `:380` (renames in-memory only); no `app_config_dir`/`tauri-plugin-store` write in `src-tauri` (negative grep); not in `generate_handler!` `lib.rs:472-494` | 5 | 5 | 4 | **100** | **STEAL** | S–M |
| 2 | **Per-worktree git-status snapshot** (branch + dirty count + ahead/behind per tab cwd) — the *snapshot* slice of `git_panel_snapshot` | PASS | PASS — no `git_*` command anywhere; `generate_handler!` `lib.rs:472-494` has none | 4 | 5 | 4 | **80** | **STEAL** | S–M |
| 3 | **Workspace path-scope authorization** (canonicalize + prefix-check launch cwd/flags against an allowed root before `send-keys`) | PASS | PASS — cockpit shell-quotes (`tmux.rs:161 shq`) + validates agent name (`lib.rs:202-211`) but does **not** scope paths to a canonical root | 3 | 3 | 4 | **36** | MAYBE | S |
| 4 | **`shell_bg` background-job manager** (spawn detached → capture logs → list/kill) | PASS | PASS — no background-job API; cockpit runs everything in tmux panes | 3 | 2 | 3 | **18** | SKIP | M |
| 5 | **Named multi-space grouping** (project-scoped tab groups w/ root; *incremental* value beyond persistence in #1) | PASS | PASS — single flat tab strip; no space concept (`manager.rs` `TabInfo`/`CockpitState`, no grouping) | 3 | 2 | 2 | **12** | SKIP | M–L |
| 6 | **`fs_watch` backend file-change events** (notify-based, debounced emit) | PASS | PASS — no fs watcher in `src-tauri` | 2 | 2 | 3 | **12** | SKIP | M |
| — | **pty foreground-job detection** (`pty_has_foreground_job`/`_process`) | PASS | **SKIP — already covered** by a *richer*, CC-aware classifier: `status.rs classify` + poller `lib.rs:451` (`IDLE/WORKING/NEEDS_INPUT/DEAD/UNKNOWN`). Cockpit's is strictly better for CC panes. | — | — | — | — | SKIP | — |

### Candidate rationales (evidence + effort)

**#1 — Disk-persisted cockpit layout  (STEAL, score 100).**
*Source evidence:* `terax-spaces.json` `state:<spaceId>` = `{activeTabIndex, tabs:[{customTitle, kind, tree}]}`, persisted via `tauri-plugin-store` (§3).
*Cockpit gap — precise framing:* Cockpit is **not** un-persisted — it persists via the **tmux session** (`cockpit-main`), which survives an app restart because `cockpit_init` re-attaches (`lib.rs:98`). What it lacks is **disk** persistence: state dies on reboot, `kill-server`, or the self-heal `reset_server` (`tmux.rs:121`) that wipes the socket; and custom tab names never reach tmux at all — they live only in the SolidJS store (`store.ts:68-69` *"v1 rename is client-side only"*; `store.ts:380 renameTabLocal`). So a single poisoned-socket reset silently discards every custom tab name and the user's intended tab order. The steal: mirror `{tabs:[{customTitle,cwd}], activeTab}` to a JSON file in `app_config_dir` on tab create/close/rename, and reconcile it against the live (or freshly-created) tmux session on `cockpit_init`.
*Effort (S–M):* new ~120-line `persist.rs` (serde_json, atomic temp-write+rename — copy Terax's mechanism but **add an explicit `schemaVersion`**, the one thing Terax omits); one `save_layout`/load hook wired into the existing `create_tab`/`close_tab`/`renameTabLocal` paths + `cockpit_init`. Reuses the existing `CockpitState` DTO. Only fiddly part is reconcile vs a fresh tmux session. *Bonus:* once the keyed-store exists, persisting user settings + command snippets is nearly free as extra store domains.

**#2 — Per-worktree git-status snapshot  (STEAL, score 80).**
*Source evidence:* `git_panel_snapshot`, `git_status`, `git_remote_url`, `git_log` (binary command blob); frontend `SourceControlPanel`/`GitDiffStack`.
*Cockpit gap:* zero git integration — no `git_*` command (`generate_handler!` `lib.rs:472-494`).
*Why high fit:* cockpit's mission is literally Claude teams over **git worktrees**; surfacing per-tab branch + dirty-file count + ahead/behind tells the operator at a glance which agent's worktree has uncommitted/divergent work — today invisible.
*Effort (S–M):* one new Tauri command `git_status_snapshot(cwd)` shelling `git -C <cwd> status --porcelain=v2 --branch` (+ `rev-list --count`), parsed in a small Rust helper, returned as `{branch,ahead,behind,dirty}`; polled on a slow tick or on tab focus. No new deps. **Scope discipline:** steal only the read-only *snapshot* — the full stage/unstage/commit/discard/diff panel (`git_stage`/`git_commit`/`git_commit_file_diff`…) is **XL** and **not** recommended for v1.

**#3 — Workspace path-scope authorization  (MAYBE, score 36).**
*Source evidence:* `workspace_authorize`, `canonical cache poisoned`, `pty_open: cwd rejected:` — Terax canonicalizes + authorizes a path against the space root before opening a PTY there.
*Cockpit gap:* cockpit shell-quotes interpolated values (`tmux.rs:161 shq`) and validates the agent name (`lib.rs:202-211`), but never canonicalizes/prefix-checks the launch `cwd`/`flags` against an allowed root.
*Note on urgency:* lower than Terax's — in cockpit the dangerous inputs reaching `send-keys` are the agent name (already validated) and user-chosen cwd, not free LLM strings. Still a cheap defense-in-depth hardening of the tool-dispatch boundary (aligns with the repo's "LLM-supplied string at a privileged path" rule).
*Effort (S):* a `canonicalize + starts_with(root)` guard helper invoked in `launch_cc`/`launch_shell`/`launch_agent`.

**#4 — `shell_bg` background-job manager (SKIP, 18).** Useful pattern (detached run + log capture + list/kill) but **redundant with tmux**: cockpit can already open a pane and `capture-pane`. Low incremental fit.

**#5 — Named multi-space grouping (SKIP, 12).** Persistence value is already counted in #1; the *incremental* grouping value (per-project tab sets + switcher) is real QoL but low-fit for a single-session, single-strip cockpit, and the `remote`/`ssh`/`wsl` `env.kind` half is **out of fit** (cockpit is local-tmux only). Over-engineered for v1.

**#6 — `fs_watch` file-change events (SKIP, 12).** No standalone value (cockpit has no file browser); only an *enabler* for live git-status refresh (#2), which a slow poll already covers. Revisit only if #2 needs push-based refresh.

---

## 5. Out-of-scope — seen & skipped (no rows)

Per PO scope (user is Claude-Max, single provider). All observed in Terax, all skipped, **no candidate rows**:

- **Model picker / favorites** — `defaultModelId`, `favoriteModelIds[]`, `recentModelIds[]` (`terax-settings.json`).
- **OpenAI-compatible API infra** — `openaiCompatibleBaseURL`, `openaiCompatibleModelId`.
- **LM Studio / local model** — `lmstudioModelId`.
- **Multi-provider AI chat + agent runtime** — `terax-ai-sessions.json` (`sessions`/`messages:<id>`), `terax-ai-agents.json` (`activeAgentId`), frontend `AgentRunBridge`/`AgentSwitcher`/`chatRuntime`/`managedAgentsStore`/`planStore`.
- **Token/cost tracking** — none observed in Terax (noted for completeness; nothing to skip).

## 5b. Frontend-axis — deferred to frontend findings (not this backend slice)

Real Terax features, but no backend module — frontend stores persisted via the same keyed-JSON mechanism. Flagged for the frontend teammate, not scored here:
- **Theming** — `theme`/`themeId`/`editorTheme`, `terax-custom-themes.json`, `colorSwatches`, `bgImageStore` (no `theme` backend module in the binary).
- **Command/prompt snippets** — `terax-ai-snippets.json` / `snippetsStore` (would ride on candidate #1's persistence if pursued).
- **Editor / UX prefs** — `vimMode`, `autocompleteEnabled`, `showHidden`, `zoomLevel`, `editorTheme` (codemirror editor).
- **Low-priority off-the-shelf plugins** — `tauri-plugin-autostart` (`autostart`), `tauri-plugin-window-state` (`.window-state.json`), separate settings window (`open_settings_window`): each is "add a plugin," not a pattern to engineer; note only.
```

# Terax → CC Cockpit — Backend / Architecture / Data-Model Findings (DEV)

**Provenance:** DEV. Throwaway spike. **Axis:** backend/architecture/data-model only; frontend (theming/editor/chat) flagged & deferred. **Method:** Terax is closed-source — evidence *inferred* from config JSON, `Terax.log`, and `strings` over the Mach-O arm64 binary. Cockpit gaps confirmed against real source at `file:line`.

## 1. Terax config schema map

Dir: `~/Library/Application Support/app.crynta.terax/`. Each file = a `tauri-plugin-store` domain store (§3).

**`terax-spaces.json`** — workspace registry + per-space restorable UI tree (headline data model):
- `activeId: string` (focused space)
- `spaces: Space[]` — `Space = {id:string, name:string, root:string(abs path), env:WorkspaceEnv, createdAt:number(epoch ms), updatedAt:number}`
- `state:<spaceId>` (one key per space) = `{activeTabIndex:number, tabs:Tab[]}`
- `Tab = {customTitle?:string (OPTIONAL — absent ⇒ derive from cwd), kind:"terminal", tree:TreeNode}`

`WorkspaceEnv` = **internally-tagged enum on `kind`** (binary: `"internally tagged enum WorkspaceEnv"`, `"WorkspaceEnvLocal"`, `"WorkspaceEnv::Wsl with 1 element"`→`distro`). Observed `{kind:"local"}`; inferred `{kind:"wsl",distro}` + a remote/SSH variant (binary has `host key verification failed`, `SSH_ASKPASS` — shape unconfirmed).

**Pane tree (task #5)** — observed leaf node **verbatim**:
```json
{ "active": true, "cwd": "/Users/user/Workflows", "kind": "leaf" }
```
The `kind` discriminator implies a recursive node type: leaf = `{active,cwd}`; a split is **inferred** branch `{kind:"branch"|"split", children:[TreeNode]}` — **branch was NOT observed** (only single-pane leaves present). Model = **recursive node tree with a `kind` discriminator**, not a flat list w/ parent pointers.

**`terax-settings.json`** (flat key→scalar): in/near-scope — `autostart:bool`, `vimMode/showHidden/autocompleteEnabled:bool`, `zoomLevel:number`, `theme/themeId/editorTheme:string` (theming, frontend-axis). **OUT OF SCOPE:** `defaultModelId`, `favoriteModelIds[]`, `recentModelIds[]`, `openaiCompatibleBaseURL`, `openaiCompatibleModelId`, `lmstudioModelId`.

**Other domains:** `terax-ai-sessions.json` `{sessions[{id,title,createdAt,updatedAt}], "messages:<id>":[], activeId}` → OUT OF SCOPE (chat). `terax-ai-agents.json` `{activeAgentId}` → OUT OF SCOPE. `terax-ai-snippets.json` `{}` (frontend snippetsStore). `terax-custom-themes.json` `{}` (frontend, theming). `.window-state.json` → `tauri-plugin-window-state`, per-window geometry for **two** windows (`main`,`settings`).

## 2. Module / architecture inventory

| Module | Tauri commands | Role |
|---|---|---|
| **pty** | `pty_open/close/close_all/resize/write/has_foreground_job/has_foreground_process` | Interactive PTY lifecycle. Only module logging at INFO. |
| **fs** | `fs_read_dir/read_file/write_file/stat/canonicalize/create_file/create_dir/rename/delete/copy`, `fs_watch_add/remove`, `fs_search/list_files/glob`, `fs_grep/grep_interactive` | FS read/mutate, gitignore-aware tree, ripgrep search, file watch. |
| **shell** | `shell_run_command`, `shell_session_open/run/close`, `shell_bg_spawn/logs/kill/list` | One-shot exec + persistent shell session + background-job manager. |
| **workspace** | `workspace_authorize`, `workspace_current_dir`, `get_launch_dir`, `open_settings_window`, `reveal_item_in_dir`, `list_subdirs` | Space registry + path-scope authorization (`pty_open: cwd rejected:`). |
| **secrets** | keychain set_password/username | Git/SSH credential storage (`GIT_ASKPASS`/`SSH_ASKPASS`) — NOT model keys. |
| **history** | — | Persistent command history. |
| *(git cluster)* | `git_panel_snapshot/status/stage/unstage/discard/commit/fetch/log/show_commit/commit_file_diff/remote_url` | Integrated source control. |

**No backend module for themes/snippets/ai-agents** — frontend stores persisted opaquely via store plugin. **Shape: backend = OS-integration only; all AI/theming/snippet state = frontend + keyed-JSON persistence.** Conventions: `<module>_<verb>` commands; multi-window (main+settings); shell-integration injection (OSC cwd tracking — cockpit instead reads tmux `#{pane_current_path}`); plugins `store`/`window-state`/`autostart`.

## 3. Persistence model

- **Mechanism:** `tauri-plugin-store`. **One JSON file per domain**, NOT one-file-per-space.
- **Namespacing:** keys namespaced within a file (spaces file holds `activeId`, `spaces`, one `state:<spaceId>` per space).
- **Write granularity:** whole-file rewrite per key change; atomic temp-write+rename.
- **Schema versioning: ABSENT** — no `schemaVersion`/`version` field anywhere. *(Cockpit should ADD an explicit `schemaVersion` if copying — a gap to improve, not replicate.)*
- Timestamps `createdAt`/`updatedAt` epoch-ms on spaces+sessions.

## 4. Ranked steal-candidate table

Rubric: Gate A (Claude-Max compatible) · Gate B (not in cockpit, file:line) · **V×F×E** (Effort inverse) max 125 · STEAL≥60 / MAYBE 30-59 / SKIP<30.

| # | Candidate | A | B (cockpit gap) | V | F | E | Score | Tier | Size |
|---|---|---|---|---|---|---|---|---|---|
| 1 | **Disk-persisted cockpit layout** (tabs+names+cwd+activeTab) to survive tmux-session-loss | ✅ | ✅ `store.ts:68-69`,`:380` renames in-memory; no store write in src-tauri; not in `lib.rs:472-494` | 5 | 5 | 4 | **100** | **STEAL** | S–M |
| 2 | **Per-worktree git-status snapshot** (branch+dirty+ahead/behind) | ✅ | ✅ no `git_*` cmd; `lib.rs:472-494` | 4 | 5 | 4 | **80** | **STEAL** | S–M |
| 3 | **Workspace path-scope authorization** (canonicalize+prefix-check cwd vs root) | ✅ | ✅ shq only `tmux.rs:161`, name validate `lib.rs:202-211`; no root scoping | 3 | 3 | 4 | **36** | MAYBE | S |
| 4 | **`shell_bg` background-job manager** | ✅ | ✅ none | 3 | 2 | 3 | **18** | SKIP | M |
| 5 | **Named multi-space grouping** | ✅ | ✅ flat strip, no space concept | 3 | 2 | 2 | **12** | SKIP | M–L |
| 6 | **`fs_watch` file-change events** | ✅ | ✅ none | 2 | 2 | 3 | **12** | SKIP | M |
| — | pty foreground-job detection | ✅ | **SKIP — already covered**, richer: `status.rs classify` + poller `lib.rs:451` | — | — | — | — | SKIP | — |

**#1 (STEAL 100):** Cockpit persists via tmux session (survives restart, `lib.rs:98`) but lacks **disk** persistence: state dies on reboot / `kill-server` / self-heal `reset_server` (`tmux.rs:121`); custom tab names never reach tmux, live only in SolidJS (`store.ts:68-69` "v1 rename is client-side only", `:380`). Steal: mirror `{tabs:[{customTitle,cwd}],activeTab}` to `app_config_dir` JSON on create/close/rename; reconcile vs tmux on init. ~120-line `persist.rs` (serde_json, atomic temp+rename — ADD the `schemaVersion` Terax omits). Bonus: settings/snippets persistence then nearly free.

**#2 (STEAL 80):** Mission = teams over **worktrees**; per-tab branch+dirty+ahead/behind shows which agent's worktree has uncommitted/divergent work (today invisible). One `git_status_snapshot(cwd)` shelling `git status --porcelain=v2 --branch`, polled on focus. **Read-only snapshot only** — full panel is XL.

**#3 (MAYBE 36):** canonicalize+`starts_with(root)` guard. Lower urgency (cockpit inputs are validated agent name + user cwd, not free LLM strings) but aligns with repo's LLM-string-at-privileged-path rule.

## 5. Out-of-scope — seen & skipped
Model picker/favorites; OpenAI-compat API; LM Studio/local; multi-provider chat+agent runtime; token/cost tracking (none observed).

## 5b. Frontend-axis — deferred to frontend slice
Theming; snippets (would ride on #1's persistence); editor/UX prefs; off-the-shelf plugins (`autostart`, `window-state`).

**Open:** branch-node pane shape and remote `WorkspaceEnv` variant are inference, not observed.

# Reverse-engineer: Terax — clone-and-improve build spec

**Date:** 2026-07-07 · **Target:** Terax.app v0.8.2 (`app.crynta.terax`, closed-source Tauri AI terminal, macOS 13+)
**Inputs:** live logged-in app run (driven via Accessibility + screen capture) · config JSON stores · binary `strings` (June pass) · June teardown docs (`docs/terax-steal/`)
**Access depth:** paid/full — user's own installed, configured instance. **Live teardown: YES** (screens in `docs/terax-steal/live-2026-07-07/`).
**Baseline for the delta:** CC Cockpit (`app/` in this repo).

---

## 1. Snapshot

Terax is a **multi-model AI terminal** for developers: a Tauri (Rust + webview) terminal emulator with tabs/splits/Spaces, an inline AI agent composer wired to *any* provider (local LM Studio, DeepSeek, OpenAI-compatible, Whisper voice input), agent personas, a built-in CodeMirror editor, git integration, and a full theme system. Positioning: "your terminal + your models" — BYO keys, keys live in OS keychain, local-first. Distribution: direct download; no pricing surface observed anywhere in-app (free beta or license-elsewhere; version 0.8.2 suggests pre-1.0).

Same species as CC Cockpit (pane/tab Tauri terminal shell) but **generalist**: Terax optimizes single-user + any-model chat-in-terminal; Cockpit optimizes many-Claude-Code-agents orchestration over tmux + worktrees.

## 2. Feature & screen map [OBSERVED]

**Main window** (`05-main.png`):
- Top bar: sidebar toggle · **command palette icon (⌘)** · **Space switcher** ("Default >") · tab strip with per-tab icons + custom titles + close ⓧ + "+" · **Search (⌘F)** · notification bell · settings gear.
- Center: terminal pane (xterm-class, WebGL renderer toggleable). Renders TUI apps cleanly.
- Bottom bar: **path breadcrumb** ("Home > Workflows > cc-cockpit ˅") · "Open AI agent ⌘I" button.
- Per-pane **AI composer** (`19-ai-panel.png`): "Ask Terax anything" input docked under terminal with context chips — **cwd · git branch · shell** — `#` inserts snippets/commands, `@` inserts files, mic button (voice), model selector (LM Studio ˅), agent selector (Coder ˅), attach "+".

**Command palette** (`18-palette.png`, ⌘P):
- Prefix modes: plain = commands · `>` = shell history · `#` = find in files.
- Sections observed: General (Open settings, Change theme…, Keyboard shortcuts) · Spaces (Overview, New Space, Switch to <space> w/ "Current space" badge) · Tabs (New terminal ⌘T, New block terminal, New private terminal ⌘R).

**Settings window** (separate native window, ⌘,; tabs: General / Themes / Shortcuts / Models / Agents / About):
- **General** (`11-settings.png`): Appearance System/Light/Dark · UI zoom slider (90%) · Editor: Vim mode, Word wrap, Auto save · Explorer: Show hidden files, **Git decorations** ("tint changed files and dim gitignored entries in the file explorer") · Terminal: **Use WebGL renderer** toggle with corruption-fallback hint.
- **Shortcuts** (`14-shortcuts.png`): searchable, per-action rebind, Reset All. Table: palette ⌘P · find-in-files ⇧⌘P · settings ⌘, · new tab ⌘T · **new Blocks terminal ⇧⌘T** · **new private terminal ⌘R** · **new web preview ⇧⌘O** · **new editor tab ⌘E** · close tab-or-pane ⌘W · next/prev tab ⌃Tab/⌃⇧Tab · split pane right ⌘D (+ more below fold).
- **Models**: default chat model picker (LM Studio · Local) · autocomplete model (GPT-OSS 120B "Ultra-fast", off) · **Voice input: OpenAI Whisper** · Providers list with per-provider connection state, masked keys ("keys live in your OS keychain"), Add provider; LM Studio entry = Base URL (`http://localhost:1234/v1`) + Model ID + Test button.
- **Agents** (`15-agents.png`): global **Custom instructions** textarea · built-in personas: **Coder (active), Architect, Code Reviewer, Security, Designer** with one-line role blurbs + "Use agent" · "+ New agent" · **Snippets**: "reusable instructions you drop into any prompt with `#handle`".
- **Themes** (`17-themes` captured, excluded from repo — personal notification in frame): 15 built-ins — Terax Default, **Claude ("warm clay accent on paper")**, Kanagawa, Kanagawa Dragon, Tokyo Night (active), Catppuccin, Rosé Pine, Everforest, Nord, Gruvbox, Dracula, Solarized, Tide, Sage, Caffeine · "+ Create" · **Import `.terax-theme`** (portable theme file format) · separate **editor syntax theme** ("Copilot", "auto follows app theme") · **background image** (drop/pick, stored locally).

**Tab kinds** (shortcuts table + palette): terminal · **Blocks terminal** (Warp-style block-structured shell) · **private terminal** (no history — incognito) · **web preview** (embedded browser pane) · **editor tab** (CodeMirror + markdown view toggle, vim mode, own theme).

**Menu bar:** stock Tauri defaults only — every product shortcut lives in webview JS (same architecture as Cockpit).

## 3. Key user flows [OBSERVED]

- **AI ask:** focus pane → composer at bottom always present → type ("Ask Terax anything"), `@file` / `#snippet` enrich → pick model + agent inline → response streams in a panel; session history persisted (`terax-ai-sessions.json` sessions + `messages:<id>`).
- **Command palette loop:** ⌘P → fuzzy command / `>` history / `#` file grep → run. Discoverability backbone.
- **Spaces:** switcher top-left → each Space = named workspace `{name, root, env{kind:local|wsl|ssh}}` owning its own tab set + active tab; "Spaces: Overview" + "New Space" in palette; per-space state persisted and restored across relaunch.
- **Tabs/splits:** ⌘T/⌘D etc.; split tree persisted as recursive nodes; custom tab titles survive restart.
- **Onboarding equivalent:** BYO provider — Settings → Models → Add provider → key into keychain → pick default. No account, no cloud login observed.

## 4. Copy & positioning [OBSERVED]

- Terse utility voice, no marketing inside app. Security-forward microcopy: "Keys live in your OS keychain and are used only by Terax."
- Feature naming plain: "Blocks terminal", "private terminal", "web preview".
- Personas sold in 5-word job descriptions ("Design and tradeoffs. Plans before code.").
- Placeholder copy teaches syntax in-place ("Type a command, > for history, # to find in files"; "# for snippets and commands, @ for files") — zero-doc discoverability.

## 5. Inferred data model [INFERRED — config JSON verbatim + shapes]

One `tauri-plugin-store` JSON per domain in `~/Library/Application Support/app.crynta.terax/`, atomic temp+rename, **no schemaVersion**:

- `terax-spaces.json`: `{activeId, spaces: [{id, name, root, env{kind}, createdAt, updatedAt}], "state:<spaceId>": {activeTabIndex, tabs: [{customTitle?, kind:"terminal", tree}]}}`.
  **Pane tree (split node now OBSERVED live, closes June's open item):**
  `{kind:"split", dir:"row", children:[{kind:"leaf", cwd}, {kind:"leaf", cwd, active:true}]}` — recursive, `dir` row/col, **no ratio stored** (even splits).
- `terax-settings.json`: flat scalars — theme/themeId/editorTheme, zoomLevel, vimMode, autostart, showHidden, autocompleteEnabled, defaultModelId, favoriteModelIds[], recentModelIds[], lmstudioModelId, openaiCompatible{BaseURL,ModelId}.
- `terax-ai-sessions.json`: `{activeId, sessions[{id,title,createdAt,updatedAt}], "messages:<id>":[…]}`.
- `terax-ai-agents.json`: `{activeAgentId:"builtin:coder"}` (custom agents likely same store).
- `terax-ai-snippets.json`, `terax-custom-themes.json`: `{}` keyed stores.
- `.window-state.json`: per-window geometry for `main` + `settings` (tauri-plugin-window-state).
- Secrets: provider keys in **macOS keychain**, not JSON (git/SSH creds too — `GIT_ASKPASS`/`SSH_ASKPASS` in binary).

Entities: **Space 1—N Tab 1—1 PaneTree(recursive) · Provider 1—N Model · Agent(persona) · Snippet · AiSession 1—N Message · Theme**.

## 6. Clone build spec (parity with observed core)

Stack proven cheap for this shape: **Tauri v2 + Rust backend (pty/fs/shell/git modules, `<module>_<verb>` commands) + webview SPA + xterm.js(WebGL) + CodeMirror + tauri-plugin-store/window-state/autostart**.

Build order:
1. **Shell**: window w/ custom top bar (space switcher · tab strip · search · bell · gear), bottom bar (breadcrumb · AI button). Tab kinds: terminal, editor, web preview (iframe/child-webview), private terminal (skip history), blocks terminal (parse shell prompt boundaries → block list UI).
2. **Panes**: recursive split tree `{kind:leaf|split, dir, children}`; ⌘D split, ⌘W close-pane-then-tab; persist per Space.
3. **Spaces**: registry + per-space tab-state keys; palette verbs (new/switch/overview); env.kind local now, ssh/wsl later.
4. **Persistence**: one JSON store per domain, atomic write, restore on boot; **add `schemaVersion`** (Terax omits it — improve, don't copy).
5. **Palette** ⌘P: command registry + `>` shell-history source + `#` ripgrep file search; sectioned results, inline shortcut hints.
6. **Settings window** (2nd native window): General/Themes/Shortcuts/Models/Agents tabs as observed; shortcuts = searchable rebindable registry (single source of truth shared with palette).
7. **Themes**: `themeId → CSS-var token map`; ~10 stock palettes; portable `.theme` import/export; separate editor syntax theme; optional background image; System/Light/Dark mode.
8. **AI layer**: docked composer per pane with context chips (cwd, branch, shell) auto-attached; `@file` picker, `#snippet` expansion; provider abstraction (OpenAI-compat base-URL client covers LM Studio/DeepSeek/etc.); keys → OS keychain; personas = system-prompt presets (5 stock); sessions store; voice via Whisper API; optional inline autocomplete model.
9. **Git**: status snapshot per pane cwd (branch chip in composer), file-explorer decorations; full panel (stage/commit/diff/log) later — binary shows `git_panel_snapshot/stage/commit/…` cluster.
10. **OS integration**: dock badge + `request_user_attention`, autostart toggle, zoom (⌘±/0 + slider, `set_webview_zoom`, persisted).

## 7. Where we beat them — delta for CC Cockpit [RANKED]

Cockpit already shipped from the June list: whole-UI zoom, disk-persisted layout, git-status snapshot. Updated ranking of what Terax has that Cockpit still lacks — impact-to-effort for the *cockpit mission* (orchestrating Claude Code agents):

1. **OS dock badge + attention bounce** — S. `needs_input` state already computed; few capability lines + one Tauri call. Terax bundles the same APIs. *(Tier-1 leftover, do first.)*
2. **Command palette ⌘P with prefix modes** — M. Steal the **prefix pattern** (`>` = send-to-focused-agent history, `#` = find across worktrees) — fixes Cockpit's 7-shortcut discoverability wall; overlay precedent exists (`InventoryPanel`).
3. **Composer context chips (cwd · branch · shell) + `#snippet` / `@file`** — M. Cockpit operators retype spin-up prompts constantly; snippets store + chips over the existing xterm write path. Terax's `#handle` insertion UX is the model.
4. **Searchable, rebindable Shortcuts settings tab** — S/M. Registry-driven; unlocks palette for free (same registry). Terax proves menu-bar-free + JS-shortcut approach scales with a settings surface.
5. **Theme system + `.theme` import** — M. Cockpit tokens already CSS vars; add `themeId→map` + 2-3 palettes (steal "Claude: warm clay on paper" concept). Portable theme file = community juice.
6. **Private pane (no-history)** — S. Trivial flag on pane spawn; useful for credentials work during agent runs.
7. **Zoom-to-pane maximize** (Cockpit's cheaper answer to Terax's draggable splits) — M. `focusedPaneId` + tmux zoom + ⌘⏎.

**Where Cockpit already wins Terax (defend, don't dilute):** tmux-native panes survive app death; team/worktree orchestration (Team Board, spin-up, per-agent status classify) has no Terax equivalent; single-provider Claude Max = zero key-management surface. **Do NOT import:** multi-provider model stack, built-in editor (breaks tmux pane model), Spaces (single-session cockpit), blocks terminal (tmux + Claude Code TUI already structure output).

## 8. Assumptions & guesses [SPECULATIVE — quarantined]

- Split ratios: none in store → assumed even splits or in-memory only (med confidence).
- Remote Space env (`ssh` arm): binary strings only; never observed live (low).
- AI response rendering: June screenshot showed right-docked panel titled by live agent state; today only the composer was exercised (didn't submit a prompt — avoided burning user's local model/API quota). Panel behavior on submit = from June evidence (med).
- Blocks terminal / web preview / editor tab UIs: named in shortcuts+palette, not opened live (med — existence certain, UX inferred).
- "Claude Code hook integration" (June: `.claude/settings.json`, `UserPromptSubmit` strings in binary) — mechanism unverified (low).
- Update channel: "Relaunch to update" pattern not observed in Terax (that was another app's UI); updater unknown (low).

## 9. Sources

- Live captures 2026-07-07: `docs/terax-steal/live-2026-07-07/{05-main,11-settings,14-shortcuts,15-agents,18-palette,19-ai-panel}.png` (+ Models/Themes tabs captured, excluded from repo — personal notification in frame; facts transcribed in §2).
- Config stores: `~/Library/Application Support/app.crynta.terax/*.json` (spaces incl. observed split node, settings, ai-sessions/agents/snippets, window-state).
- Menu dump via AX (stock Tauri menus only).
- June static teardown: `docs/terax-steal/{dev-findings,frontend-findings,qa-verification,RECOMMENDATION}.md` (binary strings, command inventory, plugin allow-lists).

## 10. Done-bar self-check

☑ build-agent-ready (§6 ordered, stack + data model + flows specified) ☑ core coverage (main shell, palette, all 6 settings tabs, composer, persistence) ☑ concrete ranked delta (§7, sized) ☑ sourced claims (§9; guesses quarantined in §8).
**Scope-cap leftovers:** AI panel post-submit UX, git panel UI, editor/blocks/web-preview tab internals, Spaces overview screen — all flagged in §8, none blocks the delta list.

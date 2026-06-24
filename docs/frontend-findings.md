# Phase 2 — Frontend Findings: Terax → CC Cockpit (UI/UX/Layout/Interaction/Theming)

**Provenance:** FRONTEND · **Axis:** UI / UX / layout / interaction / theming only
**Baseline:** `/Users/armanshatvoran/Workflows/cc-cockpit/app/frontend/src` (SolidJS)
**Subject:** Terax `1.x` — closed-source Tauri AI terminal (`app.crynta.terax`), inspected via binary `strings`, config JSON, and one live screenshot.

---

## 0. Evidence sources & access notes (deliverable item 3)

| Source | Status | What it gave |
|---|---|---|
| `strings -n 6 .../MacOS/terax` (8,972 lines) | ✅ | Tauri **injected** JS (zoom handler), lazy-chunk filenames (component map), backend command names, Tauri plugin allow-lists |
| `~/Library/Application Support/app.crynta.terax/*.json` | ✅ | settings, spaces (space→tab→pane-tree model), ai-sessions, ai-snippets schema, custom-themes schema |
| Live screenshot `/tmp/terax2.png` (`screencapture -x`) | ✅ | **Three-column layout** observed: left rail + tabbed terminal center + right-docked AI agent panel; "claude" theme colors; per-pane status line |
| `osascript … menu items` (native menu / shortcut enumeration) | ❌ **blocked** | Accessibility not granted to `osascript` (err `-1719`). Full app-command accelerator list **not** obtainable. |
| App's own Vite chunk **contents** (theme palette hexes, app keybinding table) | ⚠️ **limited** | Chunks (`themes-*.js`, `EditorStack-*.js`) are referenced by name but minified/compressed inside the binary — `strings` surfaces only filenames, not the palette/keymap bodies. Preset-theme grep (`dracula\|nord\|…`) returned nothing; `"claude"` appears only as the settings value. |

> **Net:** "claude" theme is confirmed to *exist & be default*, and observed *visually*; its exact token hexes were not extractable. The full keyboard table beyond zoom was not extractable (compressed chunk + blocked menu dump). Everything else below is hard-sourced.

---

## 1. Terax UI/UX surface map (evidence-cited)

### 1.1 Spaces (workspace) model — PO axis #1
`terax-spaces.json`:
```json
{ "activeId": "default",
  "spaces": [ { "id":"default","name":"Default","root":"/Users/armanshatvoran",
               "env": { "kind": "local" }, "createdAt":…,"updatedAt":… } ],
  "state:default": { "activeTabIndex": 4, "tabs": [ … ] } }
```
- A **Space** = `{name, root dir, env}` and owns its own tab set + active-tab index (`state:<spaceId>`).
- `env.kind: "local"` is a discriminated union; `strings` corroborates the remote arm — `ssh`, `remote`, `environments` tokens present. So a Space visibly carries a **local-vs-remote (SSH) badge**.
- Switcher UX: screenshot shows a single active-space view; spaces are an outer grouping above tabs (top-left region).

### 1.2 Pane layout — PO axis #2
Per-tab `tree` is a **recursive split tree**, not an even tiling:
```json
"tree": { "kind": "leaf", "cwd": "/…/Workflows", "active": true }
```
- The data observed shows only `kind: "leaf"` nodes (each carrying its own `cwd` + `active`); the `leaf`/`split` discriminator (`strings`: `split always has at least 1 item`) **implies** interior `split` nodes once a tab is divided.
- A recursive `leaf`/`split` tree (vs cockpit's count-based tiling) **implies user-driven splits and draggable dividers**. Note: no width/ratio geometry fields appear in the captured config (the single-leaf default tabs have none), so persisted divider ratios are inferred, not observed.

### 1.3 Theming — PO axis #3
- `terax-settings.json`: `"themeId": "claude"`, `"editorTheme": "copilot"`, `"theme": "dark"`.
- Lazy chunk `/assets/themes-BX2zvCeh.js` = theme registry; `terax-custom-themes.json` (`{}`) = **user-definable theme schema** (empty = none authored yet).
- **"claude" theme, observed** (screenshot): near-black base (~`#1a1c1c`), neutral dark-grey chrome bars, **green** for diff-add / approved lines, **amber/orange** for "auto mode" + warnings, off-white foreground. Reads as a warm-neutral dark — a usable on-brand design asset.

### 1.4 Keyboard — PO axis #4
- **Extractable (injected JS):** whole-UI zoom — `⌘ + / ⌘ - / ⌘ 0` and `Ctrl+scroll` (see 1.5).
- **Search/command entry:** screenshot shows a top-left **`Search (⌘?)`** field beside a settings gear.
- App-command accelerators (new tab/split/etc.) **not extractable** (compressed chunk + blocked menu dump) — see §0.

### 1.5 Zoom — PO axis #5 (richest hard evidence)
Tauri-injected `keydown`/`mousewheel` handler in the binary:
```js
let zoomLevel = 1; const MAX_ZOOM_LEVEL = 10, MIN_ZOOM_LEVEL = 0.2
if (OS_NAME==='macos' ? event.metaKey : event.ctrlKey) {
  if (event.key==='-') zoomLevel-=0.2
  else if (event.key==='='||event.key==='+') zoomLevel+=0.2
  else if (event.key==='0') zoomLevel=1 … }
window.__TAURI_INTERNALS__.invoke('plugin:webview|set_webview_zoom',{ value: zoomLevel })
```
Also `Ctrl+mousewheel` (±0.2). Persisted: `terax-settings.json "zoomLevel": 0.9`. This is **whole-UI webview zoom** (chrome + terminals together), not font-only.

### 1.6 Snippets — PO axis #6
- Chunk `/assets/snippetsStore-DcMviJq7.js`; store `terax-ai-snippets.json` (`{}` = schema, none saved). `strings`: a `/snippet` trigger token.
- Pattern: a snippet store + a slash-trigger (`/…`) that expands into an input (terminal or AI chat).

### 1.7 AI chat session panel — PO axis #7 (PATTERN ONLY)
- **Observed (screenshot):** a **right-docked split panel** (a persistent right rail that splits the window — *not* a full-screen overlay) titled by live agent state ("**Working — frontend-agent**"), showing chat history + an "auto mode on (shift+tab)" footer.
- Persistence: `terax-ai-sessions.json` = `{ sessions:[{id,title:"New chat",…}], "messages:<id>":[], activeId }` — multi-session with per-session message history.
- Chunks: `AgentSwitcher`, `managedAgentsStore`, `agentsStore`, `AgentRunBridge`; default agent `builtin:designer` (`terax-ai-agents.json`); IPC `terax:agent-signal`. Claude-Code hook integration present: `.claude/settings.json`, `UserPromptSubmit`, `notify;Terax;codex`.

### 1.8 Built-in editor — PO axis #8
- Chunks `/assets/EditorStack-BATUG4IM.js`, `/assets/codemirror-BLovk85f.js`, `/assets/MarkdownViewToggle-BD_rsMft.js`; settings `editorTheme:"copilot"`, `vimMode:true`.
- Pattern: a **CodeMirror-backed editor surface** (its own stack, with a markdown view/preview toggle) — appears as a non-terminal surface, themed independently of the terminal.

### 1.9 Status / attention signals — PO axis #9
- **In-app (screenshot):** per-pane status line = cwd + token count (`121.7k`) + "auto mode on (shift+tab)"; right panel header = live agent state ("Working").
- **OS-level (strings):** Tauri window plugin allow-list bundled — `request_user_attention`, `set_badge_count`, `set_badge_label` ⇒ dock badge / window attention-bounce when an agent needs input while backgrounded.

---

## 2. Cockpit baseline (gap anchors)

| Cockpit fact | File:line |
|---|---|
| Keyboard = **7 combos total** (⌘T, ⌘1-9, ⌘D, ⌘⇧D, ⌘I, ⌘⇧T, Esc); no zoom | `keyboard.ts:1-77` |
| Panes = **count-based auto-tiling** (`columnsFor`, cap 3 wide); *"deliberately does NOT parse tmux layout strings"*; no dividers/tree/geometry | `PaneGrid.tsx:13-18, 35-42` |
| Theming = **single hardcoded `:root` palette**, `color-scheme: dark`; no switcher/tokens-per-theme | `styles.css:6-33` |
| Tabs = **flat list**, no Spaces grouping; rename is local-only | `TabBar.tsx:117-128`, comment `:6` |
| Shell = TabBar + PaneGrid + footer + overlay panels (Inventory/TeamBoard/Spinup); **no AI chat panel, no editor, no command palette** | `App.tsx:29-75` |
| Store = `{tabs, panes}` flat model; **no theme/zoom/vim/snippet/space** keys | `store.ts:48` (CockpitStore) |
| Attention = **in-app only** — per-tab dot + per-pane StatusBadge | `TabBar.tsx:60-66`, `StatusBadge.tsx:1-27` |
| No OS dock badge / `request_user_attention` anywhere | grep `badge\|user_attention` → only in-app StatusBadge |

---

## 3. Ranked steal-candidate table

**Rubric:** Gate A = Claude-Max compatible (single provider, no model/API/cost UI) · Gate B = not already in cockpit (file:line) · **Steal = Value(1-5) × Fit(1-5) × Effort(1-5, inverse: 5 = least effort)**, max 125 · STEAL ≥60 / MAYBE 30-59 / SKIP <30 · Effort t-shirt tied to cockpit store/components/CSS.

| # | Candidate | Gate A | Gate B (cockpit gap) | V | F | E | Score | Tier | T-shirt |
|---|---|---|---|---|---|---|---|---|---|
| **C1** | **Whole-UI zoom** (⌘+/⌘−/⌘0 + Ctrl-scroll, persisted) | PASS | PASS — `keyboard.ts:1-77` no zoom | 4 | 5 | 5 | **100** | **STEAL** | **S** |
| **C2** | **OS attention** — dock badge + window bounce on `needs_input` | PASS | PASS — in-app only `TabBar.tsx:60-66`, `StatusBadge.tsx` | 4 | 4 | 4 | **64** | **STEAL** | **S/M** |
| **C3** | **Theme system + "claude" theme** (switcher + CSS-var token sets) | PASS (theme≠model) | PASS — `styles.css:6-33` single `:root` | 4 | 4 | 3 | **48** | MAYBE | M |
| **C4** | **Command / search palette** (`⌘?` overlay, command registry) | PASS | PASS — `App.tsx:67-69` footer hint only | 4 | 4 | 3 | **48** | MAYBE | M |
| **C5** | **Snippets** (store + slash-trigger, insert into focused xterm/AI input) | PASS | PASS — no snippet in store/keyboard | 4 | 3 | 3 | **36** | MAYBE | M |
| **C6** | **AI-chat docked side-panel layout** (persistent right rail that *splits*, not overlays) — PATTERN ONLY | PASS (no model UI) | PASS — `App.tsx:29-75` overlay panels only | 3 | 4 | 3 | **36** | MAYBE | M |
| **C7** | **Spaces (workspace) model** w/ local·remote env switcher — PO #1 | PASS | PASS — `TabBar.tsx` flat tabs, `store.ts:48` no space layer | 4 | 2 | 1 | **8** | SKIP | XL |
| **C8** | **Built-in editor pane-type** (CodeMirror + md-preview) — PO #8 | PASS | PASS — `Pane.tsx`→xterm only, no pane-kind | 3 | 2 | 2 | **12** | SKIP | L/XL |
| **C9** | **Split-tree pane layout** w/ draggable dividers — PO #2 | PASS | PASS — `PaneGrid.tsx:13-18` auto-tile, no dividers | 3 | 2 | 1 | **6** | SKIP | XL |

### Per-candidate notes

- **C1 — Whole-UI zoom · STEAL · S.** *Evidence:* injected JS (`MAX_ZOOM_LEVEL=10`, `set_webview_zoom`) + `"zoomLevel":0.9`. *Build:* ~30 lines in `keyboard.ts` (⌘±/0 + Ctrl-wheel) → one `set_webview_zoom` invoke; persist `zoomLevel` in `store.ts`. Highest legibility-per-effort win on a dense terminal grid; demos instantly. **Top pick.**
- **C2 — OS attention · STEAL · S/M.** *Evidence:* `request_user_attention` / `set_badge_count` in bundled Tauri allow-list; agent terminal whose job is signaling. *Fit is high because the state already exists* — cockpit computes `needs_input`/`working` per pane/tab (`tabAttention`, `attn-input`). *Build:* in the existing attention reducer, call the Tauri window API (request-attention + badge count of waiting agents). Pure UX win for a "watch many agents" cockpit. **Second pick.**
- **C3 — Theme system · MAYBE · M.** *Evidence:* `themeId:"claude"`, `themes-*.js`, custom-theme schema. *Fit:* cockpit **already** uses CSS custom-property tokens (`--bg-0`, `--accent`, `--focus`…) — so the lift is refactoring `:root` into `themeId → token map`, adding `store.themeId`, and a switcher. The "claude" palette (§1.3) is a free on-brand design asset to ship as the second theme.
- **C4 — Command palette · MAYBE · M.** *Evidence:* `Search (⌘?)` field + `openSettings`. *Fit:* cockpit's overlay precedent (`InventoryPanel`, `SpinupDialog`) makes an overlay+registry cheap; directly serves the keyboard-first ethos and fixes thin discoverability (7 combos + footer hint).
- **C5 — Snippets · MAYBE · M.** *Evidence:* `snippetsStore-*.js`, `/snippet` trigger, `ai-snippets.json` schema. *Fit:* high-value for an orchestration cockpit where operators retype spin-up/launch prompts; insert path = existing xterm write. *Build:* new store + picker overlay + trigger.
- **C6 — AI-chat docked side-panel layout · MAYBE · M.** **PATTERN ONLY — model/provider UI excluded.** *Evidence:* screenshot right-dock + `ai-sessions.json` (sessions+history). *Steal = the layout lesson:* a persistent right rail that **splits** the window (coexists with the grid) instead of cockpit's full-screen overlays — pure flex/CSS shell change in `App.tsx`. Backend agent-stream is out of frontend lane (flag to lead).
- **C7 — Spaces · SKIP (frontend slice) · XL.** PO axis #1, scored honestly. Local-only switcher (group tabs under named workspaces) is the cheap slice; the **remote/SSH** `env.kind` arm is a large backend dependency → **flag to lead** (cross-lane). Frontend slice alone: store schema (`spaces[]`, per-space tab sets) + a top-left switcher.
- **C8 — Editor pane-type · SKIP · L/XL.** PO axis #8. **Friction flag:** cockpit panes are tmux PTYs (`Pane.tsx`→`XtermHost`); a non-terminal pane-kind breaks the tmux-backed model and needs a CodeMirror dep + store pane-kind + `PaneGrid` render branch. Terminal `$EDITOR` already covers the need. *(Vim mode — `vimMode:true`, NORMAL/INSERT — folds in here; standalone value ~12, SKIP, since terminal vim already works via the shell.)*
- **C9 — Split-tree layout · SKIP · XL.** PO axis #2. *Evidence is the `leaf`/`split` tree model (§1.2); divider ratios are inferred, not in the captured config.* Replacing count-tiling with a drag-resizable tree + tmux `resize-pane` wiring is XL. **Recommended cheaper slice → "zoom-to-pane" (maximize focused pane):** `focusedPaneId` already in store; add a `maximized` flag + `PaneGrid` full-bleed branch + `⌘⏎` keybind, backed by tmux zoom. That slice scores ~**V3×F4×E3 = 36 (MAYBE, M)** and is the recommendation here.

---

## 4. Out-of-scope — flagged & skipped (no rows)

Per PO out-of-scope (multi-provider / model-picker / API-key / cost UI):

| Seen in Terax | Source | Why skipped |
|---|---|---|
| `favoriteModelIds`, `recentModelIds`, `defaultModelId` (model favorites/recents switcher) | `terax-settings.json` | Multi-model picker UI — out of scope (Claude-Max = single provider) |
| `lmstudioModelId:"qwen3.5-9b"` (LM Studio UI) | settings | Local-model provider UI — out |
| `openaiCompatibleBaseURL` / `openaiCompatibleModelId` | settings | OpenAI-compatible / API-key fields — out |
| `autocompleteEnabled` (model autocomplete) | settings | Model-completion UI — out |
| **Git panel** (`git_panel_snapshot`, `git_status`, `git_stage`, `git_commit`, `git_diff_content`) | strings | *Seen & scoped out:* valid UI but Value3×Fit1×Effort1≈3 (SKIP) — large surface, heavy backend; not a UI/UX steal. |

---

## 5. Recommendation to lead

- **Ship now (STEAL):** **C1 whole-UI zoom (S)** and **C2 OS dock-badge/attention (S/M)** — both small, both high-fit (zoom = Tauri webview already; attention = state already computed), both demo-grade.
- **Next polish (MAYBE):** **C3 theme system + claude theme**, **C4 command palette**, **C5 snippets**, **C6 AI-panel dock layout** — all M, all leverage existing CSS-token / overlay / xterm-write infrastructure.
- **Defer / cross-lane (SKIP):** C7 Spaces (remote=backend, **flag**), C8 editor (breaks tmux pane model), C9 split-tree (XL). For pane layout, prefer the **zoom-to-pane maximize** slice (M) over full draggable tree.
- **Out of lane to confirm:** C2 and C6 touch the Tauri/IPC boundary — surface to lead so backend-agent wires the invoke/stream; frontend owns the trigger + render.

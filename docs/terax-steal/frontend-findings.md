# Phase 2 — Frontend Findings: Terax → CC Cockpit (UI/UX/Layout/Interaction/Theming)

**Provenance:** FRONTEND · **Axis:** UI / UX / layout / interaction / theming only
**Baseline:** `app/frontend/src` (SolidJS) · **Subject:** Terax (closed-source Tauri AI terminal), inspected via binary `strings`, config JSON, and one live screenshot.

## 0. Evidence sources & access notes

| Source | Status | Gave |
|---|---|---|
| `strings -n 6 .../MacOS/terax` (8,972 lines) | ✅ | Tauri **injected** JS (zoom handler), lazy-chunk filenames (component map), backend command names, plugin allow-lists |
| config JSON | ✅ | settings, spaces (space→tab→pane-tree), ai-sessions, ai-snippets schema, custom-themes schema |
| Live screenshot (`screencapture -x`) | ✅ | **Three-column layout**: left rail + tabbed terminal center + right-docked AI agent panel; "claude" theme colors; per-pane status line |
| `osascript` menu/shortcut dump | ❌ blocked | Accessibility not granted (`-1719`). Full accelerator list **not** obtainable. |
| Vite chunk **contents** (theme hexes, keymap) | ⚠️ limited | Chunks minified inside binary — `strings` surfaces filenames, not bodies. |

> **Net:** "claude" theme confirmed exists/default + observed visually; exact token hexes not extractable. Full keyboard table beyond zoom not extractable. Everything else hard-sourced.

## 1. Terax UI/UX surface map

- **1.1 Spaces:** Space = `{name, root, env}` owns its tab set + active-tab index (`state:<spaceId>`). `env.kind:"local"` discriminated union; `strings` corroborates remote/SSH arm → Space carries local-vs-remote badge. Outer grouping above tabs.
- **1.2 Pane layout:** per-tab `tree` is a recursive `leaf`/`split` tree (only `leaf` observed; `split always has at least 1 item`). Implies user splits + draggable dividers. No width/ratio geometry in captured config (inferred).
- **1.3 Theming:** `themeId:"claude"`, `editorTheme:"copilot"`, `theme:"dark"`. `themes-*.js` registry + custom-themes schema. **"claude" theme observed:** near-black base (~`#1a1c1c`), neutral dark-grey chrome, **green** diff-add/approved, **amber/orange** auto-mode/warnings, off-white fg. Warm-neutral dark, on-brand.
- **1.4 Keyboard:** extractable = whole-UI zoom (⌘+/⌘−/⌘0, Ctrl+scroll); top-left `Search (⌘?)` field + gear. App accelerators not extractable.
- **1.5 Zoom (richest evidence):** injected handler — `MAX_ZOOM_LEVEL=10, MIN=0.2`; ⌘±/0 + Ctrl-wheel → `plugin:webview|set_webview_zoom`. Persisted `zoomLevel:0.9`. **Whole-UI webview zoom**, not font-only.
- **1.6 Snippets:** `snippetsStore-*.js` + `ai-snippets.json` schema + `/snippet` trigger. Slash-trigger expands into input.
- **1.7 AI chat panel (PATTERN ONLY):** observed **right-docked split panel** (persistent rail that splits window, not overlay) titled by live agent state ("Working — frontend-agent"), chat history + "auto mode (shift+tab)" footer. `ai-sessions.json` = multi-session + per-session history. Chunks: `AgentSwitcher`, `managedAgentsStore`, `AgentRunBridge`. Claude-Code hook integration present (`.claude/settings.json`, `UserPromptSubmit`).
- **1.8 Built-in editor:** `EditorStack-*.js`, `codemirror-*.js`, `MarkdownViewToggle-*.js`; `editorTheme:"copilot"`, `vimMode:true`. CodeMirror surface w/ md preview toggle, themed independently.
- **1.9 Status/attention:** in-app per-pane status line (cwd + token count + auto-mode); right header live agent state. OS-level: bundled allow-list has `request_user_attention`, `set_badge_count`, `set_badge_label` → dock badge / attention-bounce when backgrounded.

## 2. Cockpit baseline (gap anchors)

| Cockpit fact | File:line |
|---|---|
| Keyboard = 7 combos; no zoom | `keyboard.ts:1-77` |
| Panes = count-based auto-tiling, no dividers/tree | `PaneGrid.tsx:13-18,35-42` |
| Theming = single hardcoded `:root`; no switcher | `styles.css:6-33` |
| Tabs = flat list, no Spaces; rename local-only | `TabBar.tsx:117-128` |
| Shell = TabBar+PaneGrid+footer+overlays; no AI panel/editor/palette | `App.tsx:29-75` |
| Store = `{tabs,panes}` flat; no theme/zoom/vim/snippet/space keys | `store.ts:48` |
| Attention = in-app only (per-tab dot + StatusBadge) | `TabBar.tsx:60-66`, `StatusBadge.tsx` |
| No OS dock badge / `request_user_attention` | grep → in-app only |

## 3. Ranked steal-candidate table

**V×F×E** (Effort inverse) max 125 · STEAL≥60 / MAYBE 30-59 / SKIP<30.

| # | Candidate | A | B (cockpit gap) | V | F | E | Score | Tier | Size |
|---|---|---|---|---|---|---|---|---|---|
| **C1** | **Whole-UI zoom** (⌘+/⌘−/⌘0 + Ctrl-scroll, persisted) | ✅ | ✅ `keyboard.ts:1-77` | 4 | 5 | 5 | **100** | **STEAL** | **S** |
| **C2** | **OS attention** — dock badge + window bounce on `needs_input` | ✅ | ✅ in-app only `TabBar.tsx:60-66` | 4 | 4 | 4 | **64** | **STEAL** | **S/M** |
| **C3** | Theme system + "claude" theme (switcher + CSS-var token sets) | ✅ | ✅ `styles.css:6-33` | 4 | 4 | 3 | **48** | MAYBE | M |
| **C4** | Command / search palette (`⌘?` overlay + registry) | ✅ | ✅ `App.tsx:67-69` hint only | 4 | 4 | 3 | **48** | MAYBE | M |
| **C5** | Snippets (store + slash-trigger → focused xterm/AI input) | ✅ | ✅ none | 4 | 3 | 3 | **36** | MAYBE | M |
| **C6** | AI-chat docked side-panel **layout** (split rail, not overlay) — PATTERN ONLY | ✅ | ✅ `App.tsx:29-75` overlays only | 3 | 4 | 3 | **36** | MAYBE | M |
| **C7** | Spaces (workspace) model w/ local·remote env | ✅ | ✅ `store.ts:48` no space layer | 4 | 2 | 1 | **8** | SKIP | XL |
| **C8** | Built-in editor pane-type (CodeMirror+md) | ✅ | ✅ `Pane.tsx`→xterm only | 3 | 2 | 2 | **12** | SKIP | L/XL |
| **C9** | Split-tree pane layout w/ draggable dividers | ✅ | ✅ `PaneGrid.tsx:13-18` | 3 | 2 | 1 | **6** | SKIP | XL |

**Notes:**
- **C1 STEAL S** — ~30 lines in `keyboard.ts` (⌘±/0 + Ctrl-wheel) → `set_webview_zoom`; persist `zoomLevel`. Top legibility-per-effort win. **Top pick.**
- **C2 STEAL S/M** — state already computed (`tabAttention`, `attn-input`); call Tauri window API (request-attention + badge count of waiting agents). **Second pick.**
- **C3 MAYBE M** — cockpit already uses CSS custom-prop tokens (`--bg-0`,`--accent`,`--focus`); lift = refactor `:root` into `themeId→token map` + switcher. "claude" palette = free 2nd theme asset.
- **C4 MAYBE M** — overlay precedent (`InventoryPanel`,`SpinupDialog`) makes overlay+registry cheap; fixes thin discoverability.
- **C5 MAYBE M** — high value for operators retyping spin-up/launch prompts; insert via existing xterm write.
- **C6 MAYBE M** — steal = the **layout lesson** (persistent right rail that splits vs full-screen overlays); pure flex/CSS change in `App.tsx`. Backend stream out of frontend lane.
- **C7/C8/C9 SKIP** — Spaces remote=backend (flag cross-lane); editor breaks tmux pane model; split-tree XL. For pane layout prefer cheaper **zoom-to-pane maximize** slice (`focusedPaneId` exists + `maximized` flag + `⌘⏎` + tmux zoom) ≈ V3×F4×E3 = **36 MAYBE M**.

## 4. Out-of-scope — flagged & skipped
Model favorites/recents/default; LM Studio; OpenAI-compat URL/key; model autocomplete; Git panel (Value3×Fit1×Effort1≈3 SKIP — heavy backend, not a UI steal).

## 5. Recommendation to lead
- **Ship now (STEAL):** C1 whole-UI zoom (S), C2 OS dock-badge/attention (S/M).
- **Next (MAYBE):** C3 theme + claude theme, C4 command palette, C5 snippets, C6 AI-panel dock layout.
- **Defer (SKIP):** C7 Spaces (remote=backend, flag), C8 editor (breaks tmux model), C9 split-tree (prefer zoom-to-pane maximize).
- **Cross-lane:** C2/C6/C7 touch Tauri/IPC — backend wires invoke/stream, frontend owns trigger+render.

**Open:** menu/shortcut dump blocked (Accessibility); "claude" exact hexes not extractable; C2/C6/C7 cross frontend↔backend boundary.

# Spec — Terax Tier-1 Steal-List (CC Cockpit)

**Date:** 2026-06-21 · **Branch:** `feat/terax-tier1-steal` · **Source:** `docs/terax-steal/RECOMMENDATION.md`
**Build model:** sequential team on main checkout — dev (backend) → frontend (FE) → lead (smoke). Tauri v2, SolidJS, tmux.

## Scope (exactly 4)
C1 whole-UI zoom · dev#1 disk-persisted layout · dev#2 per-worktree git-status · C2 OS attention.
**Out:** Spaces, editor, themes, palette, snippets, anything multi-provider. Single-provider untouched.

## IPC contract (Tauri v2, camelCase JS args → snake_case Rust)

### `persist.rs` (dev#1)
```
LayoutSnapshot { schemaVersion: u32 (=1), activeTabId: Option<String>,
                 tabs: Vec<TabLayout> }
TabLayout { index: u32, cwd: String, customTitle: Option<String> }

#[command] save_layout(snapshot: LayoutSnapshot) -> Result<(), String>
#[command] load_layout() -> Result<Option<LayoutSnapshot>, String>
```
- File: `<app_config_dir>/cockpit/layout.json` (via `app.path().app_config_dir()`). Write = serialize to `layout.json.tmp` then atomic rename. `load_layout` returns `Ok(None)` when file absent (never error on missing). Include `schemaVersion` (Terax omits it — we don't).
- Unit test: serde round-trip (snapshot → JSON → snapshot equal).

### `gitstatus.rs` (dev#2)
```
GitStatus { branch: String, ahead: u32, behind: u32,
            dirty: bool, changed: u32, untracked: u32 }

#[command] git_status_snapshot(cwd: String) -> Result<Option<GitStatus>, String>
```
- Runs `git -C <cwd> status --porcelain=v2 --branch` via `std::process::Command` (NOT shell plugin). Parse `# branch.head`, `# branch.ab +A -B`, count `1`/`2`/`u`/`?` lines. `Ok(None)` if not a repo (exit≠0 or "not a git repository"). Never panic.
- **TDD:** pure parser fn `parse_porcelain_v2(&str) -> GitStatus` with unit tests (clean / dirty / ahead-behind / detached-head / untracked fixtures) BEFORE wiring the command.

### `capabilities/default.json` (+ permissions)
Add: `core:webview:allow-set-webview-zoom` (C1); `core:window:allow-request-user-attention` + the app-badge-count permission (C2). **Verify exact identifiers against `gen/schemas/capabilities.json` — no guessing constants.**

### `lib.rs`
Register `save_layout, load_layout, git_status_snapshot` in `generate_handler!`. New `pub mod persist; pub mod gitstatus;`.

## Frontend

### ipc.ts
Typed wrappers + types mirroring the Rust DTOs: `saveLayout(snapshot)`, `loadLayout()`, `gitStatusSnapshot(cwd)`.

### C1 zoom — `keyboard.ts` + boot
- ⌘= / ⌘+ → +0.1 · ⌘- → −0.1 · ⌘0 → reset 1.0 · Ctrl+wheel → ±0.1. Clamp [0.3, 3.0].
- Apply `getCurrentWebview().setZoom(z)` (`@tauri-apps/api/webview`). Persist `localStorage["cockpit.zoom"]`; restore + apply on boot.
- No conflict with existing ⌘ combos (=,+,-,0 unused).

### C2 attention — `store.ts`
- Derive needs-input count from `store.panes` (status === "NEEDS_INPUT").
- In `onPaneStatus` handler (or effect): if `!document.hasFocus()` && count>0 → `getCurrentWindow().requestUserAttention(UserAttentionType.Critical)` + `setBadgeCount(count)`.
- `window.addEventListener("focus", …)` → clear attention + `setBadgeCount(0/null)`.

### dev#1 wiring — `store.ts`
- Build snapshot from `store.tabs` (index), each tab's first-pane cwd, `tabNameOverrides`, `activeTabId()`.
- Debounced (~300ms) `saveLayout` after newTab / requestCloseTab / renameTabLocal / switchTab.
- On boot after `reconcile(state)`: `loadLayout()` → for each persisted tab, match the live tab at same index whose first-pane cwd equals → `renameTabLocal(liveTabId, customTitle)`. Restore activeTabId if present. Best-effort; never block / throw into boot.

### dev#2 badge — `store.ts` + `TabBar.tsx`
- `gitStatus` store: `Record<tabId, GitStatus|null>`. Refresh active tab's first-pane cwd on `switchTab` + `setInterval(8000)` for active tab only (cheap; no all-tabs hammer).
- TabChip: small `git-badge` next to name → `branch ● (dirty) ↑ahead ↓behind`. Hidden when null.

## Build order (sequential, on main)
1. **dev:** gitstatus parser (TDD) → gitstatus cmd → persist.rs → capabilities (+verify ids) → register in lib.rs. `cargo build` + `cargo test` green.
2. **frontend:** ipc wrappers → C1 → C2 → dev#1 wiring → dev#2 badge. `tsc --noEmit` + `vite build` green.
3. **lead:** `tauri dev`, observe: zoom ⌘+/−/0 works; rename a tab → restart → name restored; git badge shows branch; no crash.

## Done = observed
Not "code written." Lead must watch `tauri dev` run with the 4 behaviors before declaring GREEN (global build-gate rule).

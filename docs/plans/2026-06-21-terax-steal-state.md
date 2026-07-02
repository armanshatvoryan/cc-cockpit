# Terax → CC Cockpit "what to steal" — orchestration state

**Date:** 2026-06-21
**Lead:** main (team lead, dev-team)
**ANALYSIS COMPLETE** → docs/terax-steal/RECOMMENDATION.md. **NOW BUILDING Tier-1** on branch `feat/terax-tier1-steal` (spec: docs/superpowers/specs/2026-06-21-terax-tier1-design.md).
BUILD LOG:
- Backend (task#6) DONE + verified green: gitstatus.rs (7 tests), persist.rs (5 tests), lib.rs registered, capabilities/default.json +3 (set-webview-zoom, request-user-attention, set-badge-count — badge is core:window:). Committed eb77fed (dev committed unprompted — feature branch only; flag to user). Pre-existing failure templates::shipped_example_templates_parse_clean reproduces pre-edit (NOT ours).
- FE contract: git_status_snapshot({cwd})→GitStatus|null; save_layout({snapshot})→void; load_layout()→LayoutSnapshot|null. setZoom via getCurrentWebview(); requestUserAttention+setBadgeCount via getCurrentWindow().
- Frontend (task#7) DONE + verified: tsc exit 0, vite build exit 0; 5 files +329 (ipc/keyboard/store/TabBar/styles), NOT committed (working tree). Contract mismatch handled: setBadgeCount(undefined) clears (null is tsc error). Known-limit: launchTeam/launchFromInventory/focusTeamMemberPane bypass switchTab → skip persist/git-refresh (self-heals via 8s interval).
- SMOKE (task#8) — app already live under user's `tauri dev` (vite HMR + rust-watch). Evidence: (1) screenshot — renders multi-pane grid, NO crash on new FE; (2) `strings target/debug/cc-cockpit` → git_status_snapshot/save_layout/load_layout + allow-set-webview-zoom/allow-request-user-attention/allow-set-badge-count ALL compiled into running binary; (3) tab-chip crop — git badge LIVE showing `feat/terax-tie…` + dirty dot (dev#2 end-to-end + IPC bridge proven at runtime). (4) save_layout PROVEN end-to-end: ~/Library/Application Support/studio.arag.cc-cockpit/cockpit/layout.json written at runtime with schemaVersion:1 + tabs[{index,cwd}] (serde camelCase rename works live). PROVEN: render, git badge (dev#2), save_layout (dev#1 write). WIRED+COMPILED, NOT keystroke-observed (live team sessions in panes → won't risk zoom-garble): ⌘zoom visual scale + grid-garble, load_layout restore-on-restart, C2 dock badge — pending user keystroke.
- BUILD COMPLETE + SHIPPED. Branch feat/terax-tier1-steal: eb77fed (backend) + 160eb09 (FE) + 444a9af (docs). User chose commit FE + PR. Created PRIVATE GitHub remote github.com/armanshatvoryan/cc-cockpit (repo had NO remote before — first publish; no .gitignore exists, only 129 source files tracked, no secrets). Pushed main + branch. **PR #1: https://github.com/armanshatvoryan/cc-cockpit/pull/1**. Team still parked (user did not dismiss). Residual manual checks for user: ⌘zoom (on throwaway tab), restart→name restore, C2 dock badge. FOLLOW-UP: add .gitignore.
**Task:** Explore Terax app, compare to CC Cockpit, decide what to steal from Terax.
**Deliverable:** recommendation doc → `cc-cockpit/docs/` (final, phase 4).

## Team roster
- product-owner = product-owner-agent (read-only) — scope
- dev = dev-agent (claude-opus-4-8, own worktree) — backend/arch findings
- frontend = frontend-agent (own worktree) — UI/UX findings
- qa = qa-agent (headless, read+run) — verify findings vs real cockpit

## Phases & gates
1. scope — PO  → **STOP, user approval gate**
2. build — dev + frontend (parallel) — ANALYSIS not implementation; findings docs only
3. qa — qa  → **STOP, user approval gate**
4. integrate — lead (solo, no gate)

## Hard constraints (bake into scope)
- User on Claude Max: NO multi-provider / paid-API / model-cost infra. Steal-worthy = single-provider compatible.
- Phase 2 = analysis; any spike is throwaway, no shipped features.
- Terax backend is compiled (not source). dev INFERS arch from config schemas + Terax.log + `strings` on binary. No "read Terax source".

## Inspectable Terax surface
- Configs: `~/Library/Application Support/app.crynta.terax/*.json` (settings, spaces, ai-agents, ai-sessions, ai-snippets, custom-themes)
- Log: `~/Library/Logs/app.crynta.terax/Terax.log` (module names: terax_lib::modules::pty …)
- Binary: `/Applications/Terax.app/Contents/MacOS/terax` (Mach-O; `strings` for cmds/labels/themes)
- Installed v0.8.0 (DMG in Downloads = 0.7.3)

## Terax feature surface (initial recon)
- Spaces (named workspace, env local/remote, root) → tabs → split-pane terminal trees (leaf/tree)
- Multi-model AI (deepseek/gpt/lmstudio/openai-compatible; favorites/recents/default) — CONSTRAINT-FLAGGED
- AI agents (builtin:designer), AI chat sessions, AI snippets
- Themes (themeId "claude", editorTheme "copilot", custom themes), dark/light, zoom
- Vim mode, autocomplete, autostart, showHidden, built-in editor

## CC Cockpit surface
- Frontend (SolidJS): App, XtermHost, Pane/PaneGrid, TabBar, TeamBoardPanel, SpinupDialog, InventoryPanel, LaunchDialog, StatusBadge
- Backend (Rust): inventory, manager, status, teamruns, templates, tmux, smoke

## STATUS LOG
- 2026-06-21: orientation done; team tooling confirmed (TaskList). State doc created. Spun up PO.
- 2026-06-21: USER APPROVED at gate 2. Phase 4 integrate DONE → docs/terax-steal/RECOMMENDATION.md. Task#5 done. ALL PHASES COMPLETE. Teammates (product-owner/dev/frontend/qa) idle/available.
- 2026-06-21: Phase 3 QA DONE → docs/terax-steal/qa-verification.md. Verdict APPROVE, 0 rejected, baseline green (tsc exit 0). Corrections: (1) C1/C2 need core capability-permission lines (no new crate); (2) dev#1 dep is SOFT for C1/C3/C5 (localStorage covers FE persistence); (3) C4 citation loose (App.tsx:67-69 = footer hints not ⌘? field, gap still real). Task#4 done. NEXT: **USER GATE 2** — approve shortlist before phase-4 integrate.
- 2026-06-21: Phase 2 DONE. Findings persisted to docs/terax-steal/{dev,frontend}-findings.md. DEV top: #1 disk-persist layout (STEAL 100), #2 per-worktree git-status snapshot (STEAL 80), #3 path-scope auth (MAYBE 36). FRONTEND top: C1 whole-UI zoom (STEAL 100), C2 OS dock-badge attention (STEAL 64); MAYBEs C3 theme, C4 cmd palette, C5 snippets, C6 AI-panel dock. No hard conflicts. Tasks #2/#3 done. Phase 3 QA launched (task#4). NEXT: collect QA verdict → **USER GATE 2**.
- 2026-06-21: USER APPROVED scope at gate 1. Phase 2 launched: dev (opus, worktree) + frontend (worktree) in parallel. Tasks #2/#3 in_progress. Awaiting both findings. NEXT: collect dev+frontend findings → qa (phase 3) → USER GATE 2.
- 2026-06-21: PO delivered phase-1 scope (7 dims, 2 hard-gates + V×F×E rubric, dev/frontend split, accept criteria, out-of-scope multi-provider). Task#1 done. MECHANICS: read-only subagents canNOT SendMessage — deliverables retrieved from agent transcript JSONL (last assistant text). dev/frontend will Write findings doc + return full text. NEXT: **USER GATE 1** — awaiting scope approval before phase 2.

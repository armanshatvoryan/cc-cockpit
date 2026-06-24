# What to Steal from Terax — CC Cockpit Recommendation

**Date:** 2026-06-21 · **Author:** dev-team (lead-integrated) · **Status:** final
**Process:** product-owner scoped → dev + frontend investigated in parallel → qa verified vs real source → lead integrated.
**Evidence:** `docs/terax-steal/{dev-findings,frontend-findings,qa-verification}.md`

> **Verification scope:** QA verified the **cockpit side** — each gap is grep-confirmed absent, each `file:line` citation opened and checked, each Tauri API checked feasible against capabilities. The **Terax side** is *inferred* from a single `strings` pass over the binary + config JSON + one live screenshot, not behaviorally re-verified (the app is closed-source). The recommendation deliberately rests on **cockpit-need + feasibility** ("cockpit lacks X, X is cheap, X is on-mission"), which holds regardless of exactly how Terax implements X. The weakest Terax-side inference is C2 (the API is *permitted* in Terax's allow-list; that it fires dock-badge-on-needs-input is assumed).

---

## TL;DR

Terax (`app.crynta.terax`, closed-source Tauri AI terminal) and CC Cockpit are the same species — pane/tab terminal shells. Terax is a *general* multi-model AI terminal; Cockpit is *specialized* for orchestrating Claude Code teams over tmux + git worktrees. **The single biggest thing Terax does that Cockpit doesn't — multi-provider model infrastructure — is exactly the thing we must NOT steal** (Claude Max = single provider). What's left and worth taking is a tight set of **persistence, OS-integration, and UX-polish** patterns.

**Steal now (4, all STEAL-tier):** whole-UI zoom · disk-persisted layout · per-worktree git-status · OS dock-badge attention.
**Steal next (4 MAYBE):** theme system · command palette · snippets · AI-panel dock layout.
**Don't steal:** Spaces (XL), built-in editor (breaks tmux pane model), draggable split-tree (prefer a cheap zoom-to-pane), the entire multi-provider/model/API/cost stack (out of scope by constraint).

Gate A (Claude-Max compatible — no model-picker / API-key / cost infra) holds across the entire recommended list. QA rejected nothing; baseline `tsc --noEmit` green.

---

## Tier 1 — STEAL NOW

### 1. Whole-UI zoom  ·  `C1` · STEAL 100 · effort **S**
⌘+ / ⌘− / ⌘0 and Ctrl+scroll → `set_webview_zoom`, persisted as `zoomLevel`.
- **Why:** highest legibility-per-effort win on a dense terminal grid; demos instantly.
- **Evidence:** Terax injected JS handler (`MAX_ZOOM_LEVEL=10`, `plugin:webview|set_webview_zoom`) + `"zoomLevel":0.9`. Cockpit gap: `keyboard.ts:1-77` has no zoom.
- **Build:** ~30 lines in `keyboard.ts` (⌘±/0 + Ctrl-wheel) → one webview-zoom invoke; persist in store.
- **⚠ QA correction:** add capability `core:webview:allow-set-webview-zoom` (not in webview default). Core API — **no new crate/plugin.**

### 2. Disk-persisted cockpit layout  ·  `dev#1` · STEAL 100 · effort **S–M**
Mirror `{tabs:[{customTitle,cwd}], activeTab}` to `app_config_dir` JSON; reconcile vs tmux on init.
- **Why:** today, custom tab names live only in SolidJS (`store.ts:68-69` "v1 rename is client-side only", `:380`) and the whole layout dies on reboot / `kill-server` / self-heal `reset_server` (`tmux.rs:121`). One poisoned-socket reset silently discards every custom name + order. tmux survives app-quit (`lib.rs:98`) but not these.
- **Evidence:** Terax `tauri-plugin-store`, one JSON per domain, atomic temp+rename. Cockpit gap: no persist code in `src-tauri` (grep clean); not in `lib.rs:472-494`.
- **Build:** ~120-line `persist.rs` (serde_json, atomic temp+rename), hook into existing tab create/close/rename paths. **Add an explicit `schemaVersion` field — Terax omits it; we shouldn't.**
- **Note:** foundation for *backend-owned* state. (FE-only persistence for C3/C5 can ride localStorage instead — see QA correction below.)

### 3. Per-worktree git-status snapshot  ·  `dev#2` · STEAL 80 · effort **S–M**
Read-only per-tab badge: branch + dirty + ahead/behind.
- **Why:** the mission is literally Claude-Code teams over **git worktrees** — yet which agent's worktree has uncommitted or divergent work is currently invisible.
- **Evidence:** Terax has a full `git_*` command cluster (we take only the snapshot). Cockpit gap: zero git anywhere in Rust (grep clean).
- **Build:** one `git_status_snapshot(cwd)` shelling `git status --porcelain=v2 --branch`, polled on focus; small unscoped frontend badge to surface it. **Snapshot only** — full stage/commit/diff panel is XL, explicitly out.

### 4. OS attention — dock badge + window bounce  ·  `C2` · STEAL 64 · effort **S/M**
Fire `request_user_attention` + `set_badge_count(<waiting agents>)` when a backgrounded agent needs input.
- **Why:** a "watch many agents" cockpit should signal the OS when one blocks; today attention is in-app only (`TabBar.tsx:60-66`, `StatusBadge.tsx`).
- **Evidence:** Terax bundles these in its Tauri window allow-list. The cockpit *state already exists* — `needs_input`/`working` is computed per pane/tab (`store.ts:100`, `status.rs:217 classify`).
- **Build:** in the existing attention reducer, call the Tauri window API.
- **⚠ QA correction:** add capabilities `allow-request-user-attention` + `allow-set-badge-count` (not in window default). Core API — **no new crate/plugin.**

---

## Tier 2 — STEAL NEXT (MAYBE)

| # | Candidate | Score · effort | Why it's a fit | Note |
|---|---|---|---|---|
| C3 | **Theme system + "claude" theme** | 48 · M | Cockpit already uses CSS custom-prop tokens (`--bg-0`,`--accent`,`--focus`); lift = refactor `styles.css:6-33` `:root` into `themeId→token map` + switcher. The "claude" palette is a free 2nd theme. | Exact hexes approximate (correctly flagged). FE persist via localStorage. |
| C4 | **Command / search palette** (`⌘?` overlay + registry) | 48 · M | Overlay precedent (`InventoryPanel`, `SpinupDialog`) makes it cheap; fixes thin discoverability (7 combos + footer hint). | Citation loose — `App.tsx:67-69` is footer key-hints, not a real field; **gap still real.** |
| C5 | **Snippets** (store + slash-trigger → focused xterm / AI input) | 36 · M | High value for operators retyping spin-up / launch prompts; insert via existing xterm write. | FE persist via localStorage (soft dep on dev#1). |
| C6 | **AI-panel dock layout** (persistent right rail that *splits*, not a full-screen overlay) | 36 · M | Pure flex/CSS shell change in `App.tsx`; better than cockpit's overlay panels for always-visible context. | **PATTERN ONLY** — no AI-stream backend exists; layout shell only. |

---

## Do NOT steal

| Candidate | Tier | Reason |
|---|---|---|
| **Spaces** (named workspaces, local/remote env) | SKIP · XL | Over-engineered for a single-tmux-session cockpit; the remote/SSH `env.kind` arm is a large backend dependency. |
| **Built-in editor** (CodeMirror pane-type) | SKIP · L/XL | Breaks the tmux-PTY pane model (`Pane.tsx`→xterm); terminal `$EDITOR` already covers it. Vim mode folds in here — terminal vim already works. |
| **Draggable split-tree layout** | SKIP · XL | Replacing count-tiling + wiring tmux `resize-pane` is XL. **Cheaper slice instead → "zoom-to-pane" maximize** (`focusedPaneId` exists + `maximized` flag + `⌘⏎` + tmux zoom) ≈ MAYBE 36, M. |
| **Multi-provider / model-picker / OpenAI-compat / LM Studio / model favorites / API-key / token-cost UI** | OUT OF SCOPE | Constraint: user on Claude Max — single provider. Verified absent from the recommendation; **do not introduce.** |
| Full git panel (stage/commit/diff) | SKIP | Heavy backend surface; only the read-only snapshot (dev#2) is worth it. |
| `shell_bg` job manager, `fs_watch`, persistent shell sessions | SKIP | Redundant with tmux. |

---

## Recommended build order (QA-validated)

1. **C1 zoom + C2 attention** — cheapest, highest-visibility; bundle the 3–4 capability-permission lines.
2. **dev#1 disk-persist** — backend foundation for tab/layout/name survival.
3. **C3 theme + C5 snippets + dev#2 git-snapshot(+badge)** — leverage existing tokens / xterm-write / worktree model.
4. **C4 command palette** — anytime.
5. **C6 AI-panel dock** — last (layout shell; real value waits on any future agent-stream).

## Two corrections folded in (from QA)
1. **Capability perms:** C1 and C2 each need 1–3 lines in `app/src-tauri` capabilities — they are **core Tauri APIs, no new crate.** Add to their effort.
2. **dev#1 dependency is soft:** WKWebView localStorage (default-persistent) covers C1/C3/C5 frontend persistence. dev#1 is hard-required only for *backend* tab/layout/name survival, not for the FE-state items.

## Open items (inference, not blockers)
- Terax split/branch pane-node shape unconfirmed (only single-leaf observed) → feeds only the SKIP'd C9.
- Remote/SSH `WorkspaceEnv` variant unconfirmed → feeds only the SKIP'd C7.
- Exact "claude" theme hexes not extractable (compressed chunk) → C3 cosmetic, observed-approximate.
- Full app keyboard table not extractable (Accessibility-blocked menu dump) → doesn't affect the shortlist.

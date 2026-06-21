# QA Verification — Terax→Cockpit steal-findings (Phase 3)

**Verifier:** qa-agent. **Method:** every STEAL/MAYBE candidate checked against REAL source
(`app/src-tauri/src/*.rs`, `app/frontend/src/**`, `capabilities/default.json`, `Cargo.toml`,
`tauri.conf.json`, `node_modules/@tauri-apps/api`). Citations opened at `file:line`; gaps
grep-confirmed; Tauri APIs checked against permission schema + JS bindings.

## Verdict table

| candidate | verdict | citation check | gap confirmed? | Gate A | feasibility flag | note |
|---|---|---|---|---|---|---|
| **dev #1** disk-persist layout (STEAL 100) | **VERIFIED** | ✅ store.ts:69 `tabNameOverrides "v1 rename client-side only"`, :380 `renameTabLocal` (no backend call), lib.rs:472-494 no persist cmd, lib.rs:98/500 tmux re-attach, tmux.rs:121 `reset_server`→kill-server | ✅ grep: zero `app_config_dir`/`to_writer`/`tauri-plugin-store` (only a test fn name) | ✅ | OK — serde_json already in Cargo; hand-rolled persist.rs, no new dep | FOUNDATION for **backend-owned** state (tab/cwd/layout + tab-name survival across kill-server/reboot). Reconcile-vs-tmux is the real cost; ~120 lines mildly optimistic, S–M ok |
| **dev #2** per-worktree git snapshot (STEAL 80) | **VERIFIED** | ✅ lib.rs:472-494 no `git_*`; grep: ZERO git refs/subprocess in all rust | ✅ fully absent | ✅ | OK — `Command::new("git")` backend (unrestricted) or `shell:allow-execute` (present); needs git on PATH | Backend-only. Surfacing needs a small **frontend per-tab branch/dirty badge that the frontend lane did NOT scope** (they SKIP'd the full git panel). Consistent (snapshot ≠ panel) — flag the cross-lane UI gap |
| **dev #3** workspace path-scope auth (MAYBE 36) | **VERIFIED (partial-cover)** | ✅ tmux.rs:161 `shq`, lib.rs:202-211 `launch_agent` name-validate | ✅ no canonicalize+`starts_with(root)`; BUT partial mitigation exists (name-validate + shq) | ✅ | trivial S | Gap real but partially mitigated; MAYBE fair. Current inputs = validated name + user cwd, not free LLM strings — lower urgency, finding says so |
| **C1** whole-UI zoom (STEAL 100) | **VERIFIED** | ✅ keyboard.ts:1-77 = 7 combos, zero zoom | ✅ grep: no `setZoom`/`set_webview_zoom` in frontend | ✅ | **FLAG**: `set_webview_zoom` is CORE (JS `webview.setZoom` present; perm `core:webview:allow-set-webview-zoom` exists) but **NOT in core:webview default** (default = get-all-webviews/position/size/toggle-devtools). Must add 1 perm to capabilities/default.json. No plugin/crate | Interaction ~30 lines (S, correct). "Persisted zoomLevel" via **localStorage** (WKWebView default-persistent) — **ships standalone, no dev #1** |
| **C2** OS attention dock-badge/bounce (STEAL 64) | **VERIFIED** | ✅ TabBar.tsx:60-66 in-app dot; store.ts:100 `tabAttention` computed; grep no `requestUserAttention`/`setBadge` in frontend | ✅ OS-level absent | ✅ | **FLAG**: `request_user_attention`+`set_badge_count` are CORE window (JS bindings present; perms exist) but **NOT in core:window default**. Must add `allow-request-user-attention`+`allow-set-badge-count`(+`-set-badge-label`) to capabilities. No plugin/crate | State already computed; S/M fair. macOS badge/bounce supported. Findings cited Terax's allow-list, omitted cockpit's capability adds |
| **C3** theme system + claude theme (MAYBE 48) | **VERIFIED** | ✅ styles.css:6-33 single `:root`, custom-prop tokens (`--bg-0/--accent/--focus`) present as claimed, no switcher; grep no `themeId` | ✅ | ✅ | OK — refactor `:root`→`themeId` token map + switcher | Persists via **localStorage (no dev #1)**. Exact "claude" hexes NOT extractable (correctly flagged) → palette approximate = design choice, not blocker |
| **C4** command/search palette (MAYBE 48) | **VERIFIED (citation imprecise)** | ⚠️ App.tsx:67-69 = **footer KEY HINTS** (`⌘T tab · ⌘1-9…`), NOT a `⌘?` search field. Gap real (grep: no palette anywhere) but cited line is the static hint, not a `⌘?` anchor | ✅ | ✅ | OK — overlay precedent (InventoryPanel/Spinup/TeamBoard, App.tsx:72-74) | Mislabel only; conclusion stands |
| **C5** snippets (MAYBE 36) | **VERIFIED** | ✅ "none" — grep: no snippet code, no backend | ✅ | ✅ | OK — insert via existing `paneSendKeys`/xterm write | dev-findings said "rides on #1's persistence" → **DOWNGRADE that dep to SOFT**: localStorage suffices, no hard dev #1 dep |
| **C6** AI-chat docked panel LAYOUT (MAYBE 36) | **VERIFIED (pattern-only)** | ✅ App.tsx:29-75 overlays only, no split rail | ✅ | ✅ | OK as pure flex/CSS | **No AI-chat stream backend exists** → steals the layout SHELL with no content yet. Frontend scoped it correctly ("pattern only"). Lowest-value MAYBE until a stream exists |

## (a) Rejections
**None.** Every STEAL/MAYBE gap is genuinely absent against a baseline read directly. The
self-downgraded "pty foreground-job detection → SKIP" is justified: richer coverage already
exists (`status.rs:217 classify` IDLE/WORKING/NEEDS_INPUT/DEAD/UNKNOWN + poller `lib.rs:451`).

## (b) Inference-flagged items — all correctly marked uncertain
- **Terax split-pane branch node** — both docs flag "branch NOT observed / inferred." Feeds C9 (SKIP) → no bite.
- **Remote `WorkspaceEnv` variant** — "shape unconfirmed," strings-corroborated only. Feeds C7 Spaces (SKIP) → no bite.
- **Exact "claude" theme hexes** — "not extractable / ~#1a1c1c approximate." Feeds C3 (cosmetic) → no bite.

## (c) Confidence
**High.** All 9 candidates' gaps confirmed absent against real source. **Gate A holds across
the whole shortlist** — none pull in model-picker/API-key/multi-provider/token-cost infra
(those were listed OUT OF SCOPE and verified absent). Only two corrections, neither a blocker:
(1) C1/C2 each need 1–3 **capability permission** lines added (core APIs, no new crate/plugin —
findings omitted this); (2) the dev#1 persistence dependency is **softer than written** —
localStorage (WKWebView default-persistent; no incognito/dataDirectory override) covers
C1/C3/C5; dev#1 is hard-required only for backend tab/layout/name survival.

## (d) Dependencies / ordering
- **dev #1 = foundation** for BACKEND-owned persistence only (tab/cwd/layout + custom tab
  names surviving kill-server/reboot; today names live only in in-memory `tabNameOverrides`).
- **C1 / C3 / C5** → persist via **localStorage** → soft/zero dep on dev #1; ship independently.
- **C2** → pure runtime, no persistence; independent. Needs capability adds.
- **dev #2** → backend-only; needs a small unscoped frontend badge to be user-visible.
- **C6** → needs an AI stream backend that doesn't exist; layout-only until then.
- **Suggested order:** C1 zoom + C2 attention (cheap, independent, +capability perms) →
  dev #1 foundation → C3/C5 + dev #2(+its UI badge) → C4 palette (anytime) → C6 last.

## Ship verdict
This is a **pre-implementation research deliverable** (no diff) → no regression suite applies;
"ship" = **APPROVE the shortlist to proceed to build**. Verdict: **SHIP / APPROVE**, with the
two capability-wiring notes folded into C1/C2 effort and the dev#1 dependency reframed (soft for
C1/C3/C5). Nothing rejected. Baseline not built by me; findings verified by direct source read.

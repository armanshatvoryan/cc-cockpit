# Team Board — filter + cleanup + row actions (state doc)

Date: 2026-07-05 · Branch: (feature branch off main) · Owner: Arman

## Goal
Kill the Team Board graveyard (32+ dead `~/.claude/teams/session-*` stubs) and make
the panel useful: default filter, one-click cleanup of dead runs, clickable rows.

## Approved design (3 features)
1. **Default filter** — show a run only if real team (≥2 members) AND created ≤7d ago.
   Hide: lead-only stubs (any age) + real teams >7d. Missing `createdAt` → old → hidden.
   Header toggle `[ show all N ]` ⇄ `[ filtered ]`. Const `STALE_DAYS = 7`.
   Today: shows a221ed65(6), 4991c242(38), acba674f(2), b4a54b03(3); hides 29.
2. **Cleanup** — header button `🗑 clean up N dead runs`. Deletable = hidden runs MINUS
   (a) any run with a member pane live-tracked in this cockpit, (b) any run whose
   `config.json` mtime < 10 min (active session still writing — protects THIS session).
   Confirm dialog with count. Deletes `~/.claude/teams/session-<id>/` + `~/.claude/tasks/session-<id>/`.
   New Tauri command `cleanup_team_runs(session_ids)` — re-validates server-side:
   name matches `session-*`, path under teams dir, mtime guard re-checked. Rejects others.
3. **Row click** — whole member row is the target. Live `%N` pane → jump (▶, existing
   `focusTeamMemberPane`). Else has cwd → new pane cd'd there via createTab+launchShell (↗).
   Else no cwd → dimmed/inert.

## Files
- `app/src-tauri/src/teamruns.rs` — `cleanup_team_runs_at(home, ids)` core + tests
- `app/src-tauri/src/lib.rs` — `#[tauri::command] cleanup_team_runs` + register in handler
- `app/frontend/src/ipc.ts` — `cleanupTeamRuns(sessionIds)` wrapper
- `app/frontend/src/store.ts` — `teamBoardShowAll` signal, derived shown/hidden/deletable,
  `cleanupDeadRuns()`, `openMemberCwd(m)`; STALE_DAYS/FRESH_MIN consts
- `app/frontend/src/components/TeamBoardPanel.tsx` — toggle, cleanup btn, confirm, row wiring
- (CSS as needed in panel stylesheet)

## Safety (deletion)
- Backend re-validates every id; never trusts frontend list blindly.
- 10-min mtime guard on config.json protects any actively-writing session (incl. current).
- Never deletes outside `~/.claude/teams` / `~/.claude/tasks`.

## Stashed → RESTORED
Unrelated window-sizing debug (S3940) was stashed to keep the build clean, then
restored to the working tree (pop/apply glitched — reapplied both hunks by hand:
engine `write_cmd` + lib.rs `set_grid` /tmp/cockpit-dbg.log tracers). Stash dropped.
NOTE: the installed /Applications build is the CLEAN feature build (no debug tracers);
the debug tracers live only in the source tree for the user's S3940 continuation.

## Status — SHIPPED (merged to main, PR #3, feature 0bcc96d, merge e5457c6)
- [x] backend cleanup core + tests (TDD) — 5 new tests green
- [x] register tauri command `cleanup_team_runs`
- [x] ipc wrapper `cleanupTeamRuns` + `modifiedAt` field
- [x] store: filter state + `cleanupDeadRuns` + `openMemberCwd` + consts
- [x] panel UI: toggle + cleanup btn + confirm + row wiring + CSS
- [x] typecheck clean, cargo check clean, release build + DMG bundle OK
- [x] data layer verified on real corpus (35 dirs, filter→4 visible, no panic)
- [x] installed to /Applications + launched (pid alive, 78MB, no crash) — GUI
      RENDER itself unverified (headless: cannot see panel/click a delete)
- [x] committed 0bcc96d (feature only; S3940 debug tracers kept OUT via edit-out→commit→edit-in)
- [x] pushed + PR #3 → merged to main (merge e5457c6); remote branch still up (delete blocked)

## Follow-ups (not blockers)
- Eyeball the panel in the GUI (⌘⇧T) — render never verified headless.
- Aggressive default hides/deletes OLD REAL teams too (intended). One-line tweak
  to `runPassesDefaultFilter` if age-cap on real teams is unwanted.
- Remote branch `feat/team-board-filter-cleanup` still up (delete was blocked).

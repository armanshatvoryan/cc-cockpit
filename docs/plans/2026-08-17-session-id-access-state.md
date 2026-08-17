# Session-id access — state doc

**Started** 2026-08-17 · **Repo** cc-cockpit (main @ `bec4f46`, v0.1.5) · **Owner ask**: "access to id of every session"

## Goal (owner-stated, all three)

1. **Copy current id fast** — grab this pane's Claude session UUID mid-session.
2. **Find + resume old sessions** — browse past sessions across projects, jump back in.
3. **Script/pipe it** — machine-readable list for devlog / claude-mem / brain sync.

## Owner decisions (locked)

- **Surface** = cc-cockpit (option 3), not statusline.
- **Placement** = pane toolbar, in the slot next to `Launch CC`.
- **Interrupt button = DELETED.** Chip takes its place permanently. Ctrl+C stays available by typing into the focused pane (passes through xterm → tmux → claude). No keyboard fallback exists in `keyboard.ts` — accepted by owner.
- Chip shows short id; **click = copy full UUID**.

## Verified facts (do not re-derive)

| Fact | Evidence |
|---|---|
| Hook stdin JSON carries `session_id` | `security-guidance/hooks/security_reminder_hook.py:2177` — `input_data.get("session_id")` |
| Hooks inherit `$TMUX_PANE` | this session: `TMUX_PANE=%56`, `TMUX=/private/tmp/tmux-501/cockpit,1497,0` |
| `$TMUX_PANE` format == cockpit's pane id | `tmux.rs:186` reads `-F "#{pane_id}"` → `%N`; `PaneInfo.pane_id` doc says "e.g. `%3`" |
| Transcripts live at `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl` | 1584 files across 37 project dirs |
| Clipboard is already wired | `lib.rs:874` plugin init · `capabilities/default.json:16` allow-write-text · `package.json:16` JS pkg |
| Cockpit "SessionsPanel" is a DIFFERENT concept | it lists parked `_sb:` tmux windows; `teamruns.rs` uses `session-<hash>` — third namespace. Do not conflate. |
| A SessionStart hook already exists | `~/bin/brain-session-start.sh` — does NOT read session_id; add a second hook, don't edit it |

## Design

### Layer 1 — capture (hook)

New `~/.claude/hooks/cockpit-session-map.sh`, registered on `SessionStart` **and** `SessionEnd`.

- Reads stdin JSON → `session_id`, `cwd`, `transcript_path`.
- Reads `$TMUX_PANE`, `$TMUX` from env. **No-op if `$TMUX_PANE` unset.**
- Writes `~/.claude/cockpit-sessions/<pane-without-%>.json`:
  `{session_id, cwd, transcript_path, tmux_pane, tmux_server_pid, started_at}`
- `SessionEnd` → delete the file.

**Why `tmux_server_pid`**: tmux pane ids are monotonic per server but reset when the server restarts. Storing the pid (field 2 of `$TMUX`) lets the backend discard stale entries from a dead server instead of showing a wrong UUID.

**`/clear` and `--resume` both re-fire SessionStart** (`source` = clear / resume), so the file self-corrects. `compact` keeps the same id — harmless overwrite.

### Layer 2 — backend (Rust), new `app/src-tauri/src/sessions.rs`

- `read_pane_session_map() -> HashMap<String, PaneSession>` — reads the map dir, drops entries whose `tmux_server_pid` ≠ live server.
- `list_claude_sessions(limit, project_filter) -> Vec<ClaudeSessionRow>` — scans `~/.claude/projects/*/*.jsonl`: uuid (filename stem), project dir, decoded cwd, mtime, size, first user prompt, line count. Cached; only runs on panel open. Capped/paginated (1584 files today).
- `PaneInfo` gains `session_id: Option<String>` — joined during the existing `list_state` reconcile, so the chip rides the poller already in place. **No new frontend poll.**

Security: `list_claude_sessions` takes no path from the frontend; project filter is matched against the enumerated dir list, never joined into a path.

### Layer 3 — frontend

- `Pane.tsx`: **delete the Interrupt button** (and the now-unused `interruptPane` import). Insert chip in its slot:
  `[Launch CC][⌄] [⧉ 95876de8] [⤴][⇥][✕]`
  Click → `writeText(fullUuid)` → flash "copied" ~1s. Hidden when the pane has no session (shell pane).
- Keep the `interrupt_pane` Tauri command + `ipc.ts` binding (backend stays capable; only the button dies).
- `styles.css`: `.tb-session` next to the existing `.pane-id` rule (`styles.css:538`).

## Phases

- [ ] **P1 — copy current id** (hook + map reader + `PaneInfo.session_id` + chip + Interrupt removal). Covers goal 1.
- [ ] **P2 — CLI** `~/bin/cc-sessions` (list/filter/JSON/TSV over `~/.claude/projects/`). Independent of P1, cheap. Covers goal 3.
- [ ] **P3 — history browser panel** (`list_claude_sessions` + new panel + resume-into-pane). Biggest. Covers goal 2.

## Gates

- Branch off `main`, worktree under `.cc-worktrees/`. Not committed to main directly.
- **Smoke launch required before "done"** — build + run the real app, park a pane, confirm the chip shows THIS session's id and the copy lands on the clipboard. Per build-gate rule: code-written ≠ done.
- PR per phase, matching the wave/PR convention (#10–#20 all went through PRs).
- Version bump only at the end, if owner wants a release.

## Status

- 2026-08-17: investigation done, all seams verified, plan written. Owner: **P1+P2 together**, Interrupt **deleted**.
- 2026-08-17: **P1 + P2 code complete on `feat/session-id-access`.** 4 commits. Installed and live.

### Proven

| Claim | How |
|---|---|
| lib logic correct | 12 Rust unit tests, written first, observed failing |
| hook correct | 11 shell tests, written first, observed failing |
| CLI correct | 10 shell tests, incl. a cross-language E2E (real hook writes → real Rust reader reads) |
| no regression | `cargo test -p cc-cockpit` 169 passed / 0 failed |
| frontend sound | `tsc --noEmit` clean, `vite build` clean |
| CLI works on real data | 1584 transcripts indexed in 66ms; `cc-sessions` live on PATH |
| **hook fires on real sessions** | watched `~/.claude/cockpit-sessions/` during a real `claude -p` run: entry appeared on SessionStart, vanished on SessionEnd. Also captured live cockpit panes `1497-0`, `1497-54`. |

### Smoke: PASSED 2026-08-17

Built the COMBINED branch (`integration/tabname-panes` + this one) so the parallel session's PR #21
tab-name fix and PR #22 two-panes fix were not clobbered by the install. Clean merge; cargo 170/0,
tsc + vite clean.

Installed via the app-bundle flow (`--bundles app`, never dmg -- a dmg run clobbers /Applications):
quit -> ditto -> re-sign -> bust WebKit cache -> open by explicit path. Asset-hash fingerprint
confirmed the installed binary each time.

**Owner screenshot confirmed the chip rendering** on pane `%54` next to `Launch CC`, with Interrupt
absent -- the last unproven piece. Panes survived both reinstalls (a 42-minute MRA run included),
re-confirming `teardown()` is smoke-binary-only.

Owner feedback on seeing it: the button should say what it DOES, not show a uuid prefix you cannot
act on. Label is now `Copy id` -> `copied`; the full id lives in the tooltip and on the clipboard.

🔑 **I could not screenshot it myself** -- `screencapture` returns the desktop wallpaper with no
window contents (missing Screen Recording permission for the terminal process), and `-l <windowid>`
fails with "could not create image from window". CGWindowList still reports true window geometry, so
that is the reliable way to check whether the app has real windows. GUI scripting on this app also
flaked mid-run (`-1728`), matching the existing note that it is unreliable here.

### Known limitation (by design)

A session that started BEFORE the hook was installed has no entry, so its pane shows no chip until
that session restarts. This session (`95876de8…`, pane `%56`) is in exactly that state.

### Left for P3 (not started, owner leaning skip)

Owner's use-case review: the browse/search half duplicates claude-mem + devlog. The one thing only
P3 can do is **one-click resume into a cockpit pane**. Decide after living with the chip.

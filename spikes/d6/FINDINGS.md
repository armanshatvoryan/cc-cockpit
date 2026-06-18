# D6 feasibility spike — FINDINGS

**Date:** 2026-06-18 · **Box:** macOS, tmux 3.6b (`/opt/homebrew/bin/tmux`), no API key
**Question:** Can we reliably read live per-pane Claude Code state (IDLE / WORKING /
NEEDS_INPUT / DEAD)? Two-source design = hooks (Source A, push) + capture-pane
heuristic (Source B, pull).
**Scope:** FIRST CUT — not the 50-rep statistical run. Establishes the real
marker vocabulary, a working parser, and that the hook shim + R1 mechanism hold.

---

## VERDICT: GREEN (with one corpus caveat on NEEDS_INPUT wording)

- Capture-pane heuristic classifies all four states correctly on REAL CC output
  (4/4 live panes) and on the harness (4/4 phases incl. real `pane_dead=1`).
- Hook shim fires on every event and **`COCKPIT_PANE_ID` survives launch-env ->
  hook-env (R1 PASS)** — the riskiest mapping assumption holds on this box.
- One yellow stripe inside the green: the NEEDS_INPUT permission-box *wording*
  was reproduced from the documented CC shape, not observed live (no pane was
  on a permission prompt at capture time). The full run must confirm the exact
  prose/option strings real CC emits on THIS build. Parser keys on structure
  (question phrase + numbered options + `> N.`), which is robust to wording, but
  the phrase deny-list needs one live confirmation before the badge ships.

Net: ship the **full 4-state badge** path, not the degraded fallback — pending
the 50-rep confusion matrix and the one live NEEDS_INPUT capture.

---

## 1. Real marker strings (verbatim) — full table in REAL-MARKERS.md

Captured READ-ONLY from 4 live `claude.exe` panes on the DEFAULT socket
(`%0 %1 %2 %3`) via `tmux capture-pane -p -e -S -120`. No `send-keys` to live panes.
Raw dumps: `raw-capture-pane-%N.txt`.

**WORKING** (observed live, pane %2 mid-tool-run):
```
✽ Transmuting… (30m 50s · ⎈ 137.1k tokens)
     Running…
     ⎿ +2 tool uses
     (ctrl+b ctrl+b (twice) to run in background)
```
- `✽` = U+273D (`e2 9c bd`), the ACTIVE spinner frame.
- The decisive, verb-independent signal is the **elapsed-timer-in-parens**:
  `(([0-9]+m )?[0-9]+s · …tokens)`. The verb (`Transmuting`/`Running`/…) rotates.
- **CORRECTION to backend §3:** this CC build does **NOT** print `"esc to
  interrupt"`. The doc's assumed WORKING marker is wrong for this build; the
  spinner-glyph + live-timer is the real one. Parser still matches `esc to
  interrupt` as a defensive fallback for other builds.

**IDLE** (observed live, panes %0/%1/%3 — settled at the prompt):
```
❯␣                                       <- U+276F + NBSP, empty input box
⏵⏵ auto mode on (shift+tab to cycle) · ⏐ for agents
```
- A settled `✻ Baked for 3s` (U+273B, `e2 9c bb`, past-tense, NO live timer) can
  sit just above and is STILL idle. Discriminator: `✻` settled vs `✽` active.

**NEEDS_INPUT** (documented shape — NOT live in snapshot, flagged):
```
│ Do you want to <action>? │
│ ❯ 1. Yes │
│   2. Yes, and don't ask again │
│   3. No, and tell Claude what to do differently │
```

**DEAD:** not text — `tmux list-panes -F '#{pane_dead}'` == `1` (needs
`remain-on-exit on` set per-pane at create). Verified live on the harness.

---

## 2. Parser results

`parse_state.sh` (pure bash + grep, no deps) — rules from backend §3, keyed on
the real markers. Bottom-anchored-TUI model: when several affordances are in the
snapshot, the **lowest one on screen wins** (kills "stale box above fresh prompt").

**On the 4 REAL live captures (ground truth from eyeballing each pane):**
| pane | hand-label | parser | |
|---|---|---|---|
| %0 | IDLE (empty prompt + agents picker) | IDLE | PASS |
| %1 | IDLE (empty prompt + footer) | IDLE | PASS |
| %2 | WORKING (spinner + live timer + tool run) | WORKING | PASS |
| %3 | IDLE (settled glyph + empty prompt) | IDLE | PASS |

4/4. Notably distinguishes the settled `✻ Baked for 3s` IDLE pane (%3) from a
live `✽ …(timer)` WORKING pane — the trickiest real case.

**Unit cases (synthetic edge cases):** all pass —
stale-perm-box-above-fresh-prompt -> IDLE; live-perm-box-below-old-working ->
NEEDS_INPUT; timer-only (no glyph) -> WORKING; idle-prompt + activity<idle_secs +
working-marker -> WORKING (debounce); unrecognizable -> `?`; `--dead` -> DEAD.

---

## 3. Harness results — `run_spike.sh` (private `tmux -L cockpit-d6` socket)

`fake-claude.sh` emits the real markers through WORKING -> NEEDS_INPUT -> IDLE ->
exit, firing the shim at each boundary as real CC's hooks would.

```
[WORKING]      parser=WORKING      -> PASS
[NEEDS_INPUT]  parser=NEEDS_INPUT  -> PASS
[IDLE]         parser=IDLE         -> PASS
[DEAD]         pane_dead=1 parser=DEAD -> PASS   (real list-panes signal)
R1 (COCKPIT_PANE_ID survival): PASS
```
Isolation verified: DEFAULT socket panes (`%0..%3 claude.exe`) byte-identical
before/after; `-L cockpit-d6` server fully torn down (`kill-server` on the
private socket only); native `-L cockpit` never touched.

---

## 4. Hook shim + R1 (COCKPIT_PANE_ID survival)

`cockpit-hook-shim.sh` appends `{ts,session,pane,event}` NDJSON to
`events/<sessionId>.ndjson`, reading `$COCKPIT_PANE_ID` from the hook env.

**R1 PASS.** Launched exactly as the cockpit launches real claude
(`cd <dir> && COCKPIT_PANE_ID=%0 … <cmd>`); all 6 hook invocations wrote
`"pane":"%0"` — the env var set in the pane's shell survives end-to-end into the
hook process. The cwd-fallback in R1 is therefore not needed on this box.

Events captured in order: `UserPromptSubmit -> PreToolUse -> PostToolUse ->
PermissionRequest -> Stop -> SessionEnd` — exactly the sequence the state machine
consumes (Stop->IDLE, PermissionRequest->NEEDS_INPUT, PreTool/PostTool->WORKING,
SessionEnd->DEAD).

**Shim is jq-free** (jq absent on this box): extracts `session_id` /
`hook_event_name` from the JSON payload via grep/sed fallback. Verified.

### Which settings.json entries invoke the shim
`sample-hooks-settings.json` shows the `hooks` sub-tree the cockpit MERGES
(via §5 safe-write + confirm-diff) — replace `COCKPIT_SHIM` with the shim abs path.

**Confirmed the QA corpus correction on disk (read-only):** this box wires only
`PermissionRequest`, `PreToolUse(AskUserQuestion)`, `SessionStart`. **`Stop` and
`Notification` are NOT wired.** -> The cockpit MUST install its own `Stop`
(->IDLE) and `Notification` (->NEEDS_INPUT) shims. Backend §3's "settings.json
already wires Stop/Notification" is wrong for this box; QA §B / R4 are right.

---

## 5. What the full statistical run still needs

1. **Real-CC corpus, no fake-claude.** Drive a real `claude` (the user must
   start it — no API key here for an automated headless model) through the 5
   transitions x10 reps on `-L cockpit-d6`. fake-claude proved the plumbing; the
   matrix needs real frames.
2. **One live NEEDS_INPUT capture** to confirm this build's exact permission /
   AskUserQuestion wording + option strings (the single yellow caveat above).
3. **Latency p50/p95 per source + fused** (D6-1): capture-pane @1Hz timestamped
   vs hook-event ts vs hand-labeled transition. Need <=2s p95 all 4 states.
4. **NEEDS_INPUT false-positive rate over 50 reps** (D6-2): must be <=2%. The
   bottom-most-affordance rule should keep this low; measure it.
5. **Confusion matrix** (the gating deliverable) — per-source and fused.
6. **`idle_secs` tuning:** spike used 3s (backend default). Sweep 2–5s against
   real quiet-vs-working frames; pin the value that minimizes IDLE/WORKING
   flap without lagging real transitions.
7. **Source-fusion conflict logging** (hooks vs capture-pane disagree -> emit `?`
   + log): exercise with a hook-regression pane (shim removed) to confirm the
   capture-pane fallback alone still yields IDLE without the Stop hook (D6-5).

---

## Files (all under spikes/d6/)
- `REAL-MARKERS.md` — verbatim marker strings + glyph byte table
- `parse_state.sh` — Source-B state heuristic (the parser)
- `cockpit-hook-shim.sh` — Source-A hook shim (NDJSON writer, jq-free)
- `sample-hooks-settings.json` — settings.json hook entries that invoke the shim
- `fake-claude.sh` — emits real markers + fires shim (no-API-key stand-in)
- `run_spike.sh` — self-contained harness on private `-L cockpit-d6` socket
- `raw-capture-pane-%N.txt` — the 4 real live captures (evidence)
- `events/*.ndjson`, `spike-run.log` — generated run output (gitignored)

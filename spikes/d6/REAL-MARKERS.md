# Real Claude Code terminal markers (verbatim, captured 2026-06-18)

Source: `tmux capture-pane -p -e -S -120 -t %{0,1,2,3}` on the DEFAULT socket
(4 live `claude.exe` panes — user work, READ-ONLY, no send-keys). Raw dumps in
`raw-capture-pane-%N.txt`. This is the THIS-BUILD vocabulary; CC redraws the
status region every frame, so these are the literal strings the heuristic keys on.

NOTE: this CC build does **NOT** print the `"esc to interrupt"` string the
backend §3 doc assumed. The real WORKING marker is a spinner glyph + an
**elapsed-timer-in-parens**. Recorded below.

## Glyph table (the load-bearing bytes)

| Glyph | Unicode | UTF-8 bytes | Where it appears | Meaning |
|---|---|---|---|---|
| `✽` | U+273D | `e2 9c bd` | leading the live status line | ACTIVE spinner (one frame of the animation) → WORKING |
| `✻` | U+273B | `e2 9c bb` | leading a settled `✻ Baked for 3s` line | completed/settled spinner frame → NOT working by itself |
| `…` | U+2026 | `e2 80 a6` | `Running…`, `Transmuting…` | trailing the in-progress verb |
| `❯` | U+276F | `e2 9d af` (+ `c2 a0` NBSP) | lone line inside the input box | EMPTY INPUT PROMPT → IDLE affordance |
| `⏺` | U+23FA | `e2 8f ba` | leading a result/done bullet (`⏺ Standing down.`) | a completed turn's output bullet |
| `⏵⏵` | U+23F5 ×2 | `e2 8f b5 e2 8f b5` | footer `⏵⏵ auto mode on (shift+tab to cycle)` | the persistent input-box footer (present in IDLE) |
| `─ │ ╭ ╰` | box-draw | — | the input box / dialog borders | structural, not state |

## WORKING — verbatim (pane %2, live during a long tool run)

The decisive line (ANSI stripped):
```
✽ Transmuting… (30m 50s · ⎈ 137.1k tokens)
```
Supporting lines that co-occur while WORKING:
```
     Running…
     ⎿ +2 tool uses
     (ctrl+b ctrl+b (twice) to run in background)
```
- The spinner **verb** rotates (`Transmuting`, `Running`, `Initializing`, `Baking`, …) — do NOT key on the verb word.
- The reliable signals are: leading `✽` glyph AND/OR an **elapsed-timer-in-parens** matching `([0-9]+m )?[0-9]+s · ` followed by `tokens)`.
- `(ctrl+b ctrl+b (twice) to run in background)` appears ONLY while a tool runs → strong WORKING corroborator.

## IDLE — verbatim (panes %0, %1, %3 — settled, waiting at the prompt)

The input box renders with an empty prompt glyph line:
```
❯␣            <- U+276F + NBSP, nothing typed
⏵⏵ auto mode on (shift+tab to cycle) · ⏐ for agents
```
A settled completion line may sit just above and is STILL idle:
```
⏺ Standing down.
✻ Baked for 3s          <- past-tense, NO live (elapsed · tokens) parens
```
Discriminator vs WORKING: IDLE has the empty `❯` prompt + `auto mode on` footer
and **no** `✽ …(elapsed · tokens)` line; WORKING has the live `✽`+timer line and
usually no free `❯` prompt (input is suppressed while the turn runs).

## NEEDS_INPUT — canonical shape (NOT live in this snapshot)

No pane was sitting on a permission prompt at capture time, so this is the
documented CC permission/AskUserQuestion box shape (boxed, numbered options):
```
╭─ ... ─╮
│ Do you want to <action>?            │
│                                     │
│ ❯ 1. Yes                            │
│   2. Yes, and don't ask again       │
│   3. No, and tell Claude what to do │
╰─ ... ─╯
```
Markers keyed on: a boxed prompt containing `Do you want` / `Would you like`
AND a numbered selectable list `❯ 1.` / `2.` / `3.`. The `fake-claude.sh`
harness reproduces this exact shape so the parser path is exercised end-to-end.
**This needs a real-CC corpus confirmation in the full run** (the box's CC build
may word it differently); flagged in FINDINGS.

## DEAD

Not text — `tmux list-panes -F '#{pane_dead}'` == `1` (with `remain-on-exit on`).
Cross-check `#{pane_pid}` gone. No capture-pane string needed.

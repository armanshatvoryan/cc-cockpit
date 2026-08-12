# First-run onboarding wizard — design

Date: 2026-08-12
Status: approved (brainstorm 2026-08-12)

## Goal

A new user who downloads the .dmg gets from first launch to a working cockpit
without reading the README: missing prereqs are detected and installable from
inside the app, the default projects folder is set, and the core shortcuts are
shown once.

## Non-goals

- Gatekeeper/quarantine handling (happens before the app can run; stays in README).
- Interactive coach-marks tour over the live UI.
- `claude` CLI login/auth checks — presence only.
- Windows/Linux/Intel builds.

## Trigger & boot gating

- New optional settings field `onboardingDone: bool` in `CockpitSettings`
  (`settings.rs`, additive — serde `default`, schema stays v1).
- `App.tsx` onMount today calls `bootCockpit()` unconditionally. Change: first
  `load_settings`; if `onboardingDone` is not `true`, render `OnboardingWizard`
  full-screen and **do not** call `bootCockpit()` yet. Wizard finish/skip →
  persist flag → then `bootCockpit()`. This keeps `ensure_healthy_session` from
  running while tmux may be absent.
- Flag already `true` → boot exactly as today (no extra latency beyond the
  `load_settings` call, which is a local file read).
- Re-run: "Show welcome guide" button in `SettingsDialog` opens the wizard over
  the running app. In re-run mode it never defers boot and never overwrites
  `defaultCwd` unless the user changes it.
- Skip is always available (link-style button, all steps). Skip persists the
  flag too — the wizard must never nag twice. If tmux is still missing after
  skip, the existing `cockpit_init` error path surfaces it (unchanged).

## Steps

### Step 1 — Environment check

Rust command `check_prereqs` → probes via login shell (`zsh -lc`, same PATH
rationale as the existing PATH-capture logic in `lib.rs`):

```
{ tmux:   { found: bool, version: string|null, ok: bool },  // ok = version >= 3.3
  claude: { found: bool, version: string|null },
  brew:   bool,
  npm:    bool }
```

UI: one row per tool, ✓/✗ badge.

- tmux missing/too old → **blocking**: "Continue" disabled (the app cannot
  function). Row shows an **Install** button when `brew` is present, else a
  copy-paste block (`brew install tmux` + Homebrew install URL).
- claude missing → **warning only**: plain-shell panes still work. Install
  button when `npm` present, else copy-paste (`npm install -g @anthropic-ai/claude-code`).
- **Re-check** button re-runs `check_prereqs`.

### Step 2 — Projects folder

Same logic as SettingsDialog's folder picker: show effective default
(`effective_default_cwd`), native dir picker to change, saved via existing
`save_settings`. Pre-filled; "Next" without touching it is valid.

### Step 3 — Shortcut card

Static content, then "Start".

- ⌘T new tab · ⌘D split · ⌘1-9 switch tabs · ⌘B file tree · ⌘I inventory ·
  ⌘⇧T team board · ⌘, settings
- One line on status badges: Working / Needs input / Idle / Dead — jump to the
  pane that needs you.

## One-click install runner

New module `src-tauri/src/onboarding.rs`.

- `install_prereq(tool)` where `tool` is a **Rust enum** (`Tmux | ClaudeCli`),
  not a string command. Each variant maps to a hardcoded argv:
  - `Tmux` → `zsh -lc "brew install tmux"`
  - `ClaudeCli` → `zsh -lc "npm install -g @anthropic-ai/claude-code"`
- No user input ever reaches the command line. Serde deserializes the enum;
  unknown values are rejected at the IPC boundary.
- Output streamed line-by-line to the frontend via event
  `onboarding:install-line { tool, line }`; terminal event
  `onboarding:install-done { tool, exit_code }`.
- Only one install may run at a time (guard in the manager); the wizard UI
  disables the other Install button meanwhile.
- A **Cancel** button is shown while an install runs — kills the child
  (same process-group kill as wizard-close) and re-enables the row.
- Child process is killed if the wizard/app closes mid-install (process-group
  kill, same pattern as other spawned helpers).
- Failure (non-zero exit) → log area auto-expands + copy-paste fallback shown.
  Failure never blocks Skip.

## Component

`frontend/src/components/OnboardingWizard.tsx`, modeled on `SettingsDialog`
(modal overlay, tokenized palette, dark + light). Local step state
(1→2→3), no router. Collapsible `<pre>` log area for install output.

## Error handling

- `check_prereqs` itself failing (zsh missing — effectively impossible) →
  treat all tools as unknown, show copy-paste blocks, allow Skip.
- Corrupt settings file: existing `read_settings` error path unchanged; wizard
  treats it as first run but must not clobber the corrupt file until the user
  finishes **or skips** (same best-effort stance as `apply_at_startup`).
- Post-skip with tmux still missing: the `cockpit_init` failure toast should
  mention Settings → "Show welcome guide" so the route back is discoverable.
- Re-entry safety: wizard finish writes settings via existing atomic
  tmp+rename path.

## Testing

- Rust unit: version parse (`tmux 3.4` → ok, `3.2` → not ok, garbage → not
  found), enum whitelist (serde rejects unknown tool), probe-output parsing.
- Frontend: `tsc` + `vite build` green.
- Live smoke (release gate, per build rules):
  1. Move `tmux` out of PATH → launch → wizard appears, Continue blocked,
     Install visible.
  2. Restore tmux → Re-check → green → finish → cockpit boots.
  3. Relaunch → no wizard.
  4. Settings → "Show welcome guide" → wizard opens over running app.

## Open verification during implementation

- Confirm `bootCockpit()` deferral doesn't break `ftInitHome`/keyboard install
  ordering in `App.tsx` (both look independent of tmux; verify).
- Confirm nvm-installed `npm` resolves under `zsh -lc` in a GUI-launched app
  (PATH-capture precedent in `lib.rs` suggests yes).

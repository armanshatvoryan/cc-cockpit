// OnboardingWizard — first-run setup (and Settings → "Show welcome guide").
//
// Three steps: environment check → projects folder → shortcuts. Step 1 blocks
// Continue on tmux only (the app cannot function without it); claude is a
// warning. One-click installs go through the hardcoded-command runner in
// src-tauri/src/onboarding.rs. Skip is always available and persists the
// done-flag too — the wizard must never nag twice. Re-run mode (from Settings)
// overlays the running app and never re-triggers boot.

import {
  createSignal,
  onCleanup,
  onMount,
  Show,
  type Component,
} from "solid-js";
import { open } from "@tauri-apps/plugin-dialog";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  checkPrereqs,
  installPrereq,
  cancelInstall,
  onInstallLine,
  onInstallDone,
  type PrereqReport,
  type PrereqTool,
} from "../ipc";
import {
  settings,
  setDefaultCwd,
  setSettingsError,
  reloadSettings,
  finishOnboarding,
  onboardingMode,
} from "../store";

const MANUAL_CMD: Record<PrereqTool, string> = {
  tmux: "brew install tmux",
  "claude-cli": "npm install -g @anthropic-ai/claude-code",
};

type StepId = 1 | 2 | 3;

export const OnboardingWizard: Component = () => {
  const [step, setStep] = createSignal<StepId>(1);
  const [report, setReport] = createSignal<PrereqReport | null>(null);
  const [checking, setChecking] = createSignal(false);
  const [checkError, setCheckError] = createSignal<string | null>(null);
  const [installing, setInstalling] = createSignal<PrereqTool | null>(null);
  const [log, setLog] = createSignal<string[]>([]);
  const [failedTool, setFailedTool] = createSignal<PrereqTool | null>(null);

  let disposed = false;
  let unlisteners: UnlistenFn[] = [];

  async function runCheck() {
    setChecking(true);
    setCheckError(null);
    try {
      setReport(await checkPrereqs());
    } catch (e) {
      // Probe itself failed (zsh missing — effectively impossible): tools
      // unknown, manual commands shown, Skip still available.
      setReport(null);
      setCheckError(String(e));
    } finally {
      setChecking(false);
    }
  }

  onMount(() => {
    void runCheck();
    void reloadSettings(); // pre-fill step 2's folder row
    void (async () => {
      const registered = [
        await onInstallLine((p) => setLog((l) => [...l, p.line])),
        await onInstallDone((p) => {
          setInstalling(null);
          if (p.exitCode === 0) {
            setFailedTool(null);
            void runCheck(); // re-probe so the row flips to ✓ by itself
          } else if (p.exitCode === -1) {
            setFailedTool(null); // signal-kill = user cancel, not a failure
          } else {
            setFailedTool(p.tool); // log stays expanded; manual fallback shows
          }
        }),
      ];
      // Unmount can race the awaits above: if cleanup already ran, tear the
      // fresh registrations down immediately instead of leaking them.
      if (disposed) {
        for (const un of registered) un();
      } else {
        unlisteners = registered;
      }
    })();
  });
  onCleanup(() => {
    disposed = true;
    // Wizard closing mid-install: kill the child. No-op when idle.
    if (installing()) void cancelInstall();
    for (const un of unlisteners) un();
  });

  async function startInstall(tool: PrereqTool) {
    setLog([]);
    setFailedTool(null);
    setInstalling(tool);
    try {
      await installPrereq(tool);
    } catch (e) {
      setInstalling(null);
      setFailedTool(tool);
      setLog((l) => [...l, String(e)]);
    }
  }

  async function chooseFolder() {
    // Same guard as SettingsDialog: the native dialog can fail for OS-level
    // reasons; without a catch the button would just look inert.
    try {
      const picked = await open({
        directory: true,
        multiple: false,
        title: "Choose the folder new tabs open in",
        defaultPath: settings.effectiveCwd || undefined,
      });
      if (typeof picked === "string") await setDefaultCwd(picked);
    } catch (e) {
      setSettingsError(`Could not open the folder picker: ${e}`);
    }
  }

  const tmuxOk = () => report()?.tmux.ok ?? false;
  const claudeOk = () => report()?.claude.ok ?? false;

  return (
    <div class="modal-overlay onboarding-overlay">
      <div class="modal onboarding" onClick={(e) => e.stopPropagation()}>
        <div class="modal-header">
          <span class="modal-title">Welcome to CC Cockpit</span>
          <span class="onb-stepmark">step {step()} / 3</span>
        </div>

        {/* ── Step 1: environment ── */}
        <Show when={step() === 1}>
          <p class="field-hint">
            The cockpit drives a tmux session and runs Claude Code inside it.
            Two tools to check:
          </p>

          <Show when={checkError()}>
            <div class="field-error">
              Could not probe the environment: {checkError()} — run the
              commands below manually, or skip.
            </div>
          </Show>

          <div class="onb-row">
            <span class="onb-badge" classList={{ ok: tmuxOk() }}>
              {tmuxOk() ? "✓" : "✗"}
            </span>
            <div class="onb-row-main">
              <span class="onb-row-title">tmux — required</span>
              <span class="field-hint">
                {checking()
                  ? "checking…"
                  : report()?.tmux.found
                    ? tmuxOk()
                      ? report()?.tmux.version
                      : `${report()?.tmux.version} — 3.3 or newer required`
                    : "not found"}
              </span>
            </div>
            <Show when={!checking() && !tmuxOk()}>
              <Show
                when={report()?.brew}
                fallback={<code class="onb-cmd">{MANUAL_CMD["tmux"]}</code>}
              >
                <button
                  type="button"
                  class="btn btn-primary"
                  disabled={installing() !== null}
                  onClick={() => void startInstall("tmux")}
                >
                  {installing() === "tmux" ? "Installing…" : "Install"}
                </button>
              </Show>
            </Show>
          </div>

          <div class="onb-row">
            <span class="onb-badge" classList={{ ok: claudeOk() }}>
              {claudeOk() ? "✓" : "!"}
            </span>
            <div class="onb-row-main">
              <span class="onb-row-title">claude CLI — recommended</span>
              <span class="field-hint">
                {checking()
                  ? "checking…"
                  : claudeOk()
                    ? report()?.claude.version
                    : "not found — plain shell panes still work"}
              </span>
            </div>
            <Show when={!checking() && !claudeOk()}>
              <Show
                when={report()?.npm}
                fallback={<code class="onb-cmd">{MANUAL_CMD["claude-cli"]}</code>}
              >
                <button
                  type="button"
                  class="btn"
                  disabled={installing() !== null}
                  onClick={() => void startInstall("claude-cli")}
                >
                  {installing() === "claude-cli" ? "Installing…" : "Install"}
                </button>
              </Show>
            </Show>
          </div>

          <Show when={report() && !report()!.brew && !tmuxOk()}>
            <div class="field-hint">
              No Homebrew found — install it from{" "}
              <code class="onb-cmd">https://brew.sh</code> first, then Re-check.
            </div>
          </Show>

          <div class="onb-row-actions">
            <button
              type="button"
              class="btn"
              disabled={checking() || installing() !== null}
              onClick={() => void runCheck()}
            >
              Re-check
            </button>
            <Show when={installing() !== null}>
              <button
                type="button"
                class="btn btn-ghost"
                onClick={() => void cancelInstall()}
              >
                Cancel install
              </button>
            </Show>
          </div>

          <Show when={log().length > 0}>
            <pre class="onb-log">{log().join("\n")}</pre>
          </Show>
          <Show when={failedTool()}>
            <div class="field-error">
              Install failed. Run it manually in a terminal:{" "}
              <code class="onb-cmd">{MANUAL_CMD[failedTool()!]}</code>
            </div>
          </Show>
        </Show>

        {/* ── Step 2: projects folder ── */}
        <Show when={step() === 2}>
          <div class="field">
            <span class="field-label">
              Projects folder{" "}
              <span class="field-hint">where new tabs and panes start</span>
            </span>
            <div class="settings-row">
              <code class="settings-path" title={settings.defaultCwd || undefined}>
                {settings.loading
                  ? "Loading…"
                  : settings.defaultCwd ||
                    "Not set — using the built-in default"}
              </code>
              <button
                type="button"
                class="btn btn-primary"
                disabled={settings.loading || settings.saving}
                onClick={() => void chooseFolder()}
              >
                Choose…
              </button>
            </div>
            <Show when={!settings.loading && settings.effectiveCwd}>
              <div class="field-hint">
                New tabs open in {settings.effectiveCwd}
              </div>
            </Show>
            <Show when={settings.error}>
              <div class="field-error">{settings.error}</div>
            </Show>
          </div>
        </Show>

        {/* ── Step 3: shortcuts ── */}
        <Show when={step() === 3}>
          <div class="onb-shortcuts">
            <div class="onb-key"><kbd>⌘T</kbd> new tab</div>
            <div class="onb-key"><kbd>⌘D</kbd> split pane</div>
            <div class="onb-key"><kbd>⌘1–9</kbd> switch tabs</div>
            <div class="onb-key"><kbd>⌘B</kbd> file tree</div>
            <div class="onb-key"><kbd>⌘I</kbd> inventory</div>
            <div class="onb-key"><kbd>⌘⇧T</kbd> team board</div>
            <div class="onb-key"><kbd>⌘,</kbd> settings</div>
          </div>
          <p class="field-hint">
            Every pane shows a status badge — Working / Needs input / Idle /
            Dead — so you can jump straight to the pane that needs you.
          </p>
        </Show>

        <div class="modal-actions">
          <button
            type="button"
            class="btn btn-ghost"
            onClick={() => void finishOnboarding()}
          >
            {onboardingMode() === "rerun" ? "Close" : "Skip setup"}
          </button>
          <span class="footer-spacer" />
          <Show when={step() > 1}>
            <button
              type="button"
              class="btn"
              onClick={() => setStep((s) => (s - 1) as StepId)}
            >
              Back
            </button>
          </Show>
          <Show
            when={step() < 3}
            fallback={
              <button
                type="button"
                class="btn btn-primary"
                onClick={() => void finishOnboarding()}
              >
                Start
              </button>
            }
          >
            <button
              type="button"
              class="btn btn-primary"
              disabled={step() === 1 && !tmuxOk()}
              onClick={() => setStep((s) => (s + 1) as StepId)}
            >
              Continue
            </button>
          </Show>
        </div>
      </div>
    </div>
  );
};

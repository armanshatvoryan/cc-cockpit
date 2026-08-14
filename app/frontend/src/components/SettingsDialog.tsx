// SettingsDialog — cockpit preferences (⌘,).
//
// Two preferences: the appearance (dark/light) and the directory new tabs open
// in. Appearance is applied the instant it is picked — a theme you have to
// confirm is a theme you cannot judge — and is stored in localStorage by
// `theme.ts` rather than in the backend settings, so it needs no save cycle and
// no `busy()` gating.
//
// The directory preference: the cockpit's built-in
// default is `~/Workflows` (the author's layout) falling back to `$HOME`, which
// is fine but arbitrary on someone else's machine — this makes it a choice.
//
// Two values are shown deliberately: the folder the user PICKED, and the folder
// new tabs will ACTUALLY open in. They diverge when nothing is configured, or
// when a configured folder has since been deleted or renamed — the backend
// silently falls back so tmux never gets a `-c` into a missing directory, and
// silent is exactly what a settings screen should refuse to be.

import { For, Show, type Component } from "solid-js";
import { open } from "@tauri-apps/plugin-dialog";
import {
  settings,
  setDefaultCwd,
  closeSettings,
  setSettingsError,
  openOnboardingRerun,
} from "../store";
import { theme, setTheme, type ThemeName } from "../theme";

const THEMES: { value: ThemeName; label: string }[] = [
  { value: "dark", label: "Dark" },
  { value: "light", label: "Light" },
];

export const SettingsDialog: Component = () => {
  async function chooseFolder() {
    // The native dialog is the one call here that can fail for reasons outside
    // this app (plugin/permission wiring, OS-level refusal). `setDefaultCwd`
    // has its own catch; this doesn't, so without a guard a failure would be a
    // silent unhandled rejection and the button would just look inert.
    try {
      const picked = await open({
        directory: true,
        multiple: false,
        title: "Choose the folder new tabs open in",
        defaultPath: settings.effectiveCwd || undefined,
      });
      // `open` resolves to null when the user cancels — leave the setting alone.
      if (typeof picked === "string") await setDefaultCwd(picked);
    } catch (e) {
      setSettingsError(`Could not open the folder picker: ${e}`);
    }
  }

  const busy = () => settings.loading || settings.saving;
  /** True when the picked folder isn't the one actually in use — i.e. it's gone. */
  const stale = () =>
    !!settings.defaultCwd && settings.defaultCwd !== settings.effectiveCwd;

  return (
    <div class="modal-overlay" onClick={closeSettings}>
      <div class="modal settings-dialog" onClick={(e) => e.stopPropagation()}>
        <div class="modal-header">
          <span class="modal-title">Settings</span>
          <button
            type="button"
            class="modal-x"
            onClick={closeSettings}
            aria-label="Close"
          >
            ×
          </button>
        </div>

        <div class="field">
          <span class="field-label">
            Appearance{" "}
            <span class="field-hint">applies immediately, remembered</span>
          </span>

          <div class="theme-seg" role="group" aria-label="Appearance">
            <For each={THEMES}>
              {(t) => (
                <button
                  type="button"
                  class="theme-seg-btn"
                  classList={{ active: theme() === t.value }}
                  aria-pressed={theme() === t.value}
                  onClick={() => setTheme(t.value)}
                >
                  {t.label}
                </button>
              )}
            </For>
          </div>
        </div>

        <div class="field">
          <span class="field-label">
            Default folder{" "}
            <span class="field-hint">where new tabs and panes start</span>
          </span>

          <div class="settings-row">
            <code class="settings-path" title={settings.defaultCwd || undefined}>
              {settings.loading
                ? "Loading…"
                : settings.defaultCwd || "Not set — using the built-in default"}
            </code>
            <button
              type="button"
              class="btn btn-primary"
              disabled={busy()}
              onClick={() => void chooseFolder()}
            >
              Choose…
            </button>
            <Show when={settings.defaultCwd}>
              <button
                type="button"
                class="btn btn-ghost"
                disabled={busy()}
                onClick={() => void setDefaultCwd("")}
              >
                Reset
              </button>
            </Show>
          </div>

          <Show when={!settings.loading && settings.effectiveCwd}>
            <div class={stale() ? "field-error" : "field-hint"}>
              {stale()
                ? `That folder no longer exists — new tabs open in ${settings.effectiveCwd}`
                : `New tabs open in ${settings.effectiveCwd}`}
            </div>
          </Show>
        </div>

        <div class="field">
          <span class="field-label">Welcome guide</span>
          <div class="settings-row">
            <span class="field-hint">
              Re-run the first-launch setup: environment check, folder,
              shortcuts.
            </span>
            <button
              type="button"
              class="btn"
              onClick={() => {
                closeSettings();
                openOnboardingRerun();
              }}
            >
              Show welcome guide
            </button>
          </div>
        </div>

        <Show when={settings.error}>
          <div class="field-error">{settings.error}</div>
        </Show>

        <div class="modal-actions">
          <span class="field-hint">
            Applies to the next tab you open. Existing panes keep their own
            directory.
          </span>
          <button type="button" class="btn btn-primary" onClick={closeSettings}>
            Done
          </button>
        </div>
      </div>
    </div>
  );
};

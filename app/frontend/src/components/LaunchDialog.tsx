// LaunchDialog — modal to launch Claude Code or a plain shell into a pane.
//
// Fields: working directory (defaults to the pane's cwd or ~), model (optional),
// flags (optional). NO API-KEY FIELD, EVER — the user is on Claude Max.
//
// "Launch Claude" -> launchCc, "Just a shell" -> launchShell.

import { createSignal, Show, type Component } from "solid-js";
import { launchCc, launchShell } from "../ipc";

export interface LaunchDialogProps {
  paneId: string;
  /** Initial working directory (pane cwd, falls back to ~ in the field). */
  defaultCwd: string;
  onClose: () => void;
}

export const LaunchDialog: Component<LaunchDialogProps> = (props) => {
  const [cwd, setCwd] = createSignal(props.defaultCwd || "~");
  const [model, setModel] = createSignal("");
  const [flags, setFlags] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);

  async function launchClaude(e: Event) {
    e.preventDefault();
    setBusy(true);
    setErr(null);
    try {
      await launchCc(props.paneId, cwd().trim() || "~", model(), flags());
      props.onClose();
    } catch (x) {
      setErr(String(x));
      setBusy(false);
    }
  }

  async function launchPlainShell() {
    setBusy(true);
    setErr(null);
    try {
      await launchShell(props.paneId, cwd().trim() || "~");
      props.onClose();
    } catch (x) {
      setErr(String(x));
      setBusy(false);
    }
  }

  return (
    <div class="modal-overlay" onClick={props.onClose}>
      <form
        class="modal launch-dialog"
        onClick={(e) => e.stopPropagation()}
        onSubmit={launchClaude}
      >
        <div class="modal-header">
          <span class="modal-title">Launch in pane {props.paneId}</span>
          <button
            type="button"
            class="modal-x"
            onClick={props.onClose}
            aria-label="Close"
          >
            ×
          </button>
        </div>

        <label class="field">
          <span class="field-label">Working directory</span>
          <input
            class="field-input"
            value={cwd()}
            onInput={(e) => setCwd(e.currentTarget.value)}
            spellcheck={false}
            autocomplete="off"
            autofocus
          />
        </label>

        <label class="field">
          <span class="field-label">
            Model <span class="field-hint">optional</span>
          </span>
          <input
            class="field-input"
            value={model()}
            onInput={(e) => setModel(e.currentTarget.value)}
            placeholder="e.g. opus, sonnet"
            spellcheck={false}
            autocomplete="off"
          />
        </label>

        <label class="field">
          <span class="field-label">
            Flags <span class="field-hint">optional</span>
          </span>
          <input
            class="field-input"
            value={flags()}
            onInput={(e) => setFlags(e.currentTarget.value)}
            placeholder="extra claude flags"
            spellcheck={false}
            autocomplete="off"
          />
        </label>

        <Show when={err()}>
          <div class="field-error">{err()}</div>
        </Show>

        <div class="modal-actions">
          <button
            type="button"
            class="btn btn-ghost"
            disabled={busy()}
            onClick={launchPlainShell}
          >
            Just a shell
          </button>
          <button type="submit" class="btn btn-primary" disabled={busy()}>
            {busy() ? "Launching…" : "Launch Claude"}
          </button>
        </div>
      </form>
    </div>
  );
};

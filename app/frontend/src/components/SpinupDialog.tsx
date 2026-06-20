// SpinupDialog (P3 step 2) — the one-click "run a team": pick a saved roster +
// workflow + type a task, review the generated lead prompt, and launch. On
// launch the cockpit opens a tab, boots `claude`, and (once it's ready) sends
// the prompt — the lead then spins up the live teammates, which show on the
// team board (⌘⇧T). Native owns the runtime; this just composes + kicks it off.
//
// Launch is BLOCKED while the roster doesn't cover every role the workflow names
// (the backend returns coverageProblems) — so a team never starts half-staffed.

import { For, Show, type Component } from "solid-js";
import {
  templates,
  spinupPrev,
  spinupOpen,
  spinupRosterId,
  spinupWorkflowId,
  spinupTask,
  setSpinupRoster,
  setSpinupWorkflow,
  setSpinupTask,
  closeSpinupDialog,
  canLaunchTeam,
  launchTeam,
} from "../store";

export const SpinupDialog: Component = () => {
  return (
    <Show when={spinupOpen()}>
      <div class="modal-overlay" onClick={() => closeSpinupDialog()}>
        <div class="modal modal-wide" onClick={(e) => e.stopPropagation()}>
          <div class="modal-header">
            <span class="modal-title">Spin up a team</span>
            <button class="modal-x" onClick={() => closeSpinupDialog()} aria-label="Cancel">
              ×
            </button>
          </div>

          <div class="su-body">
            <label class="su-field">
              <span class="su-label">Roster — who</span>
              <select
                class="su-select"
                value={spinupRosterId() ?? ""}
                onChange={(e) => setSpinupRoster(e.currentTarget.value)}
              >
                <option value="" disabled>
                  {templates.teams.length ? "select a roster…" : "no rosters saved"}
                </option>
                <For each={templates.teams}>
                  {(r) => (
                    <option value={r.id}>
                      {r.name} · {r.scope} ({r.roles.length} roles)
                    </option>
                  )}
                </For>
              </select>
            </label>

            <label class="su-field">
              <span class="su-label">Workflow — how</span>
              <select
                class="su-select"
                value={spinupWorkflowId() ?? ""}
                onChange={(e) => setSpinupWorkflow(e.currentTarget.value)}
              >
                <option value="" disabled>
                  {templates.workflows.length ? "select a workflow…" : "no workflows saved"}
                </option>
                <For each={templates.workflows}>
                  {(w) => (
                    <option value={w.id}>
                      {w.name} · {w.scope} ({w.phases.length} phases)
                    </option>
                  )}
                </For>
              </select>
            </label>

            <label class="su-field">
              <span class="su-label">Task — the goal for this run</span>
              <textarea
                class="su-task"
                rows={2}
                placeholder="e.g. add the export-to-CSV feature and ship it"
                value={spinupTask()}
                onInput={(e) => setSpinupTask(e.currentTarget.value)}
              />
            </label>

            <Show when={templates.error}>
              <div class="su-problem">templates: {templates.error}</div>
            </Show>

            <Show when={spinupRosterId() && spinupWorkflowId()}>
              <div class="su-preview">
                <div class="su-preview-head">generated lead prompt</div>
                <Show
                  when={!spinupPrev.loading}
                  fallback={<div class="su-hint">composing…</div>}
                >
                  <Show
                    when={spinupPrev.data}
                    fallback={<div class="su-problem">{spinupPrev.error}</div>}
                  >
                    <Show when={spinupPrev.data!.coverageProblems.length > 0}>
                      <div class="su-coverage">
                        <div class="su-coverage-head">
                          ✗ roster doesn't cover this workflow — fix before launch:
                        </div>
                        <For each={spinupPrev.data!.coverageProblems}>
                          {(p) => <div class="su-problem">· {p}</div>}
                        </For>
                      </div>
                    </Show>
                    <pre class="confirm-cmd su-prompt">{spinupPrev.data!.prompt}</pre>
                  </Show>
                </Show>
              </div>
            </Show>
          </div>

          <div class="modal-actions">
            <button class="btn btn-ghost" onClick={() => closeSpinupDialog()}>
              Cancel
            </button>
            <button
              class="btn btn-primary"
              disabled={!canLaunchTeam()}
              title={canLaunchTeam() ? "Open a tab, boot the lead, send the prompt" : "pick roster + workflow + task; roster must cover the workflow"}
              onClick={() => void launchTeam()}
            >
              Launch team ▶
            </button>
          </div>
        </div>
      </div>
    </Show>
  );
};

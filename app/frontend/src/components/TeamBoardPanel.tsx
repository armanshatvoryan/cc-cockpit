// TeamBoardPanel (P3 step 3) — the live team board: a read-only, newest-first
// view of native Agent Teams sessions on disk (`~/.claude/teams/session-*/`).
//
// Native owns the team runtime (lead + teammate panes + file mailbox + tasks);
// nothing in native shows your teams at a glance, across runs, with a link from
// a role straight to its pane. That's this panel. The socket spike (2026-06-21)
// confirmed teammate panes land on the cockpit's own `-L cockpit` socket, so a
// live member's `%N` is an ordinary tracked pane — click it to jump there.
//
// Read-only this slice: no spin-up (step 2), no interrupt/detach yet. Slide-in
// from the right (⌘⇧T), same shell as the inventory panel.

import { For, Show, type Component } from "solid-js";
import type { TeamMember, TeamRun } from "../ipc";
import {
  teamBoard,
  teamBoardOpen,
  closeTeamBoard,
  loadTeamRunsNow,
  focusTeamMemberPane,
  memberPaneIsLive,
  openSpinupDialog,
} from "../store";

/** Short session id: drop the `session-` prefix for display. */
function shortSession(id: string): string {
  return id.startsWith("session-") ? id.slice("session-".length) : id;
}

const MemberRow: Component<{ member: TeamMember }> = (props) => {
  const m = props.member;
  const live = () => memberPaneIsLive(m.tmuxPaneId);
  return (
    <div class="tb-member" classList={{ "tb-member-lead": m.isLead }}>
      <span
        class="tb-dot"
        style={{ background: m.color ? colorOf(m.color) : "var(--tb-dot, #64748b)" }}
        title={m.color ?? ""}
      />
      <div class="tb-member-main">
        <div class="tb-member-top">
          <span class="tb-member-name">{m.name || m.agentId}</span>
          <Show when={m.isLead}>
            <span class="tb-tag tb-tag-lead">LEAD</span>
          </Show>
          <span class="tb-agent">{m.agentType}</span>
          <Show when={m.model}>
            <span class="tb-model">{m.model}</span>
          </Show>
        </div>
        <Show when={m.cwd}>
          <div class="tb-cwd">{m.cwd}</div>
        </Show>
      </div>
      <span
        class="inv-pill"
        classList={{ "inv-pill-on": m.mode === "live", "inv-pill-file": m.mode !== "live" }}
        title={`backend: ${m.backendType}`}
      >
        {m.mode}
      </span>
      <Show
        when={live()}
        fallback={
          <span class="tb-pane tb-pane-dim" title="not a pane in this cockpit">
            {m.tmuxPaneId ?? "—"}
          </span>
        }
      >
        <button
          class="tb-pane tb-pane-link"
          title={`Focus pane ${m.tmuxPaneId}`}
          onClick={() => focusTeamMemberPane(m.tmuxPaneId)}
        >
          {m.tmuxPaneId} ▶
        </button>
      </Show>
    </div>
  );
};

const TeamRunCard: Component<{ run: TeamRun }> = (props) => {
  const r = props.run;
  return (
    <div class="tb-run" classList={{ "tb-run-err": !!r.parseError }}>
      <div class="tb-run-head">
        <span class="tb-run-name">{r.name}</span>
        <span class="tb-run-id">{shortSession(r.sessionId)}</span>
        <span class="tb-spacer" />
        <Show when={!r.parseError}>
          <span class="tb-badge" title="members">
            {r.members.length}⬡
          </span>
          <Show when={r.inboxDepth > 0}>
            <span class="tb-badge tb-badge-warn" title="undelivered mailbox messages">
              {r.inboxDepth}✉
            </span>
          </Show>
          <Show when={r.taskCount > 0}>
            <span class="tb-badge" title="tasks">
              {r.taskCount}☑
            </span>
          </Show>
        </Show>
      </div>
      <Show
        when={!r.parseError}
        fallback={<div class="tb-run-error">!PARSE · {r.parseError}</div>}
      >
        <div class="tb-members">
          <For each={r.members}>{(m) => <MemberRow member={m} />}</For>
        </div>
      </Show>
    </div>
  );
};

export const TeamBoardPanel: Component = () => {
  return (
    <Show when={teamBoardOpen()}>
      <aside class="inv-panel" role="dialog" aria-label="Team board">
        <header class="inv-header">
          <span class="inv-title">TEAM BOARD</span>
          <Show when={!teamBoard.loading}>
            <span class="inv-count">{teamBoard.runs.length}</span>
          </Show>
          <span class="inv-spacer" />
          <button
            class="tb-new-btn"
            title="Spin up a team from a saved roster + workflow"
            onClick={() => openSpinupDialog()}
          >
            + team
          </button>
          <button
            class="inv-icon-btn"
            title="Reload"
            onClick={() => void loadTeamRunsNow()}
            aria-label="Reload"
          >
            ⟳
          </button>
          <button
            class="inv-icon-btn"
            title="Close (Esc)"
            onClick={() => closeTeamBoard()}
            aria-label="Close team board"
          >
            ×
          </button>
        </header>

        <div class="inv-body">
          <Show when={!teamBoard.loading} fallback={<div class="inv-empty">loading…</div>}>
            <Show
              when={!teamBoard.error}
              fallback={<div class="inv-empty inv-error">{teamBoard.error}</div>}
            >
              <Show
                when={teamBoard.runs.length > 0}
                fallback={
                  <div class="inv-empty">
                    no team sessions yet — start one with the Agent Teams feature
                    (ask a Claude lead to spin up a teammate)
                  </div>
                }
              >
                <For each={teamBoard.runs}>{(run) => <TeamRunCard run={run} />}</For>
              </Show>
            </Show>
          </Show>
        </div>

        <footer class="inv-footer">
          live from ~/.claude/teams · read-only · ⌘⇧T to close
        </footer>
      </aside>
    </Show>
  );
};

/** Map a native color name to a CSS color; fall back to the raw string. */
function colorOf(name: string): string {
  const map: Record<string, string> = {
    blue: "#38bdf8",
    green: "#34d399",
    red: "#f87171",
    yellow: "#fbbf24",
    purple: "#a78bfa",
    orange: "#fb923c",
    cyan: "#22d3ee",
    pink: "#f472b6",
  };
  return map[name] ?? name;
}

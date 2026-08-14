// TeamBoardPanel (P3 step 3) — the live team board: a mostly-read-only,
// newest-first view of native Agent Teams sessions on disk (`~/.claude/teams/
// session-*/`).
//
// Native owns the team runtime (lead + teammate panes + file mailbox + tasks);
// nothing in native shows your teams at a glance, across runs, with a link from
// a role straight to its pane. That's this panel. The socket spike (2026-06-21)
// confirmed teammate panes land on the cockpit's own `-L cockpit` socket, so a
// live member's `%N` is an ordinary tracked pane — click it to jump there.
//
// step 3.1: the board accretes one dir per Claude session, most of them lead-only
// stubs that never spawned a team. So the default view HIDES that graveyard
// (show only a real team ≥2 members created ≤7d ago; toggle reveals all), a
// cleanup button DELETES the dead runs (guarding anything live/fresh), and every
// member row is clickable — jump to a live pane, else open its cwd in a new pane.
// Slide-in from the right (⌘⇧T), same shell as the inventory panel.

import { createSignal, For, Show, type Component } from "solid-js";
import type { TeamMember, TeamRun } from "../ipc";
import {
  teamBoard,
  teamBoardOpen,
  closeTeamBoard,
  loadTeamRunsNow,
  focusTeamMemberPane,
  memberPaneIsLive,
  openSpinupDialog,
  teamBoardShowAll,
  toggleTeamBoardShowAll,
  visibleTeamRuns,
  deletableTeamRuns,
  cleanupDeadRuns,
  openMemberCwd,
} from "../store";

/** Short session id: drop the `session-` prefix for display. */
function shortSession(id: string): string {
  return id.startsWith("session-") ? id.slice("session-".length) : id;
}

const MemberRow: Component<{ member: TeamMember }> = (props) => {
  const m = props.member;
  const live = () => memberPaneIsLive(m.tmuxPaneId);
  const openable = () => !live() && !!m.cwd;
  const clickable = () => live() || openable();
  const act = () => {
    if (live()) focusTeamMemberPane(m.tmuxPaneId);
    else if (openable()) void openMemberCwd(m.cwd);
  };
  return (
    <div
      class="tb-member"
      classList={{
        "tb-member-lead": m.isLead,
        "tb-member-click": clickable(),
        "tb-member-dim": !clickable(),
      }}
      role={clickable() ? "button" : undefined}
      tabindex={clickable() ? 0 : undefined}
      title={
        live()
          ? `Focus pane ${m.tmuxPaneId}`
          : openable()
            ? `Open ${m.cwd} in a new pane`
            : undefined
      }
      onClick={clickable() ? act : undefined}
      onKeyDown={
        clickable()
          ? (e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                act();
              }
            }
          : undefined
      }
    >
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
          {/* A2 — "general-purpose" is noise (every generic teammate has it);
              swap it for the task summary derived from the spin-up prompt
              when one's available, keeping agentType as the hover title. */}
          <span class="tb-agent" title={m.agentType}>
            {m.agentType === "general-purpose" && m.taskSummary
              ? m.taskSummary
              : m.agentType}
          </span>
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
      <span
        class="tb-pane"
        classList={{
          "tb-pane-link": live(),
          "tb-pane-open": openable(),
          "tb-pane-dim": !clickable(),
        }}
      >
        {live() ? `${m.tmuxPaneId} ▶` : openable() ? "↗ cwd" : (m.tmuxPaneId ?? "—")}
      </span>
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
  // Two-step confirm for the (destructive, irreversible) cleanup.
  const [confirming, setConfirming] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const deadCount = () => deletableTeamRuns().length;

  const runCleanup = async () => {
    setBusy(true);
    try {
      await cleanupDeadRuns();
    } finally {
      setBusy(false);
      setConfirming(false);
    }
  };

  const closeAll = () => {
    setConfirming(false);
    closeTeamBoard();
  };

  return (
    <Show when={teamBoardOpen()}>
      <aside class="inv-panel" role="dialog" aria-label="Team board">
        <header class="inv-header">
          <span class="inv-title">TEAM BOARD</span>
          <Show when={!teamBoard.loading}>
            <span class="inv-count">{visibleTeamRuns().length}</span>
            <Show when={teamBoard.runs.length !== visibleTeamRuns().length || teamBoardShowAll()}>
              <button
                class="tb-toggle"
                title={teamBoardShowAll() ? "Show only recent real teams" : "Show every session on disk"}
                onClick={() => toggleTeamBoardShowAll()}
              >
                {teamBoardShowAll() ? "filtered" : `show all ${teamBoard.runs.length}`}
              </button>
            </Show>
          </Show>
          <span class="inv-spacer" />
          <Show when={!teamBoard.loading && deadCount() > 0}>
            <button
              class="tb-cleanup-btn"
              title={`Delete ${deadCount()} dead run${deadCount() === 1 ? "" : "s"} from disk`}
              onClick={() => setConfirming(true)}
            >
              🗑 {deadCount()}
            </button>
          </Show>
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
            onClick={closeAll}
            aria-label="Close team board"
          >
            ×
          </button>
        </header>

        <Show when={confirming()}>
          <div class="tb-confirm" role="alertdialog" aria-label="Confirm cleanup">
            <div class="tb-confirm-text">
              Delete {deadCount()} dead team run{deadCount() === 1 ? "" : "s"}? Removes each
              session's <code>teams/</code> + <code>tasks/</code> dir. Live and just-written
              sessions are kept.
            </div>
            <div class="tb-confirm-actions">
              <button class="tb-btn" disabled={busy()} onClick={() => setConfirming(false)}>
                cancel
              </button>
              <button class="tb-btn tb-btn-danger" disabled={busy()} onClick={() => void runCleanup()}>
                {busy() ? "deleting…" : `delete ${deadCount()}`}
              </button>
            </div>
          </div>
        </Show>

        <div class="inv-body">
          <Show when={!teamBoard.loading} fallback={<div class="inv-empty">loading…</div>}>
            <Show
              when={!teamBoard.error}
              fallback={<div class="inv-empty inv-error">{teamBoard.error}</div>}
            >
              <Show
                when={visibleTeamRuns().length > 0}
                fallback={
                  <div class="inv-empty">
                    <Show
                      when={teamBoard.runs.length > 0}
                      fallback={
                        <>
                          no team sessions yet — start one with the Agent Teams feature
                          (ask a Claude lead to spin up a teammate)
                        </>
                      }
                    >
                      no recent teams — {teamBoard.runs.length} older/stub session
                      {teamBoard.runs.length === 1 ? "" : "s"} hidden.{" "}
                      <button class="tb-inline-link" onClick={() => toggleTeamBoardShowAll()}>
                        show all
                      </button>
                    </Show>
                  </div>
                }
              >
                <For each={visibleTeamRuns()}>{(run) => <TeamRunCard run={run} />}</For>
              </Show>
            </Show>
          </Show>
        </div>

        <footer class="inv-footer">
          live from ~/.claude/teams · ⌘⇧T to close
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

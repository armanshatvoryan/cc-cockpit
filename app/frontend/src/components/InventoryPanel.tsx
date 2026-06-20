// InventoryPanel (P2-F1) — the unified, read-only browser of the CC toolkit:
// skills, subagents, plugins, MCP servers, across the global `~/.claude` scope
// and the active tab's per-project `.claude/` scope.
//
// Native CC manages each of these one domain at a time, current-dir only; this
// is the one panel that shows all four together, both scopes, with a live count.
// Slide-in from the right (⌘I), overlaying the pane grid. Read-only this slice —
// toggles (P2-F2) land next, delegated to `claude plugin/mcp` subcommands.

import { For, Show, type Component } from "solid-js";
import type { AuditCellState, InventoryItem, InventoryType } from "../ipc";
import {
  inventory,
  inventoryOpen,
  invTypes,
  invScope,
  invQuery,
  invView,
  audit,
  closeInventory,
  reloadInventoryView,
  setInventoryView,
  toggleInvType,
  setInvScopeFilter,
  setInvQueryFilter,
  filteredInventory,
  inventoryCounts,
  requestTogglePlugin,
  togglingId,
  pendingToggle,
  confirmToggle,
  cancelToggle,
} from "../store";

const TYPE_META: Record<InventoryType, { label: string; glyph: string; color: string }> = {
  skill: { label: "Skill", glyph: "✦", color: "#a78bfa" },
  subagent: { label: "Agent", glyph: "⬡", color: "#38bdf8" },
  plugin: { label: "Plugin", glyph: "▣", color: "#34d399" },
  mcp: { label: "MCP", glyph: "⊶", color: "#fbbf24" },
};

const TYPES: InventoryType[] = ["skill", "subagent", "plugin", "mcp"];
const SCOPES: Array<"all" | "global" | "project"> = ["all", "global", "project"];

/** The state control on the right of a row — mirrors backend truth exactly.
 *  Plugins get a clickable toggle (confirm-first); everything else is a static
 *  pill. MCP shows on/off but has no in-app toggle yet (no safe native verb). */
const StatePill: Component<{ item: InventoryItem }> = (props) => {
  const i = props.item;
  const busy = () => togglingId() === i.id;

  if (i.parseError) {
    return (
      <span class="inv-pill inv-pill-err" title={i.parseError}>
        !PARSE
      </span>
    );
  }
  if (!i.toggleable) {
    // Skills/subagents + shareable .mcp.json: file-driven, no toggle.
    return (
      <span class="inv-pill inv-pill-file" title="File-driven — remove the file to disable">
        file
      </span>
    );
  }
  if (i.type === "plugin") {
    return (
      <button
        class="inv-pill inv-pill-btn"
        classList={{ "inv-pill-on": i.enabled, "inv-pill-off": !i.enabled }}
        disabled={busy()}
        title={busy() ? "working…" : i.enabled ? "Click to disable" : "Click to enable"}
        onClick={() => void requestTogglePlugin(i)}
      >
        {busy() ? "…" : i.enabled ? "ON" : "OFF"}
      </button>
    );
  }
  // MCP: real state, but no in-app toggle this slice.
  return (
    <span
      class="inv-pill"
      classList={{ "inv-pill-on": i.enabled, "inv-pill-off": !i.enabled }}
      title="MCP toggle not available yet — use `claude mcp`"
    >
      {i.enabled ? "ON" : "OFF"}
    </span>
  );
};

/** Confirm-first modal for a plugin toggle — shows the exact native command. */
const ConfirmToggle: Component = () => {
  return (
    <Show when={pendingToggle()}>
      {(pt) => (
        <div class="modal-overlay" onClick={() => cancelToggle()}>
          <div class="modal" onClick={(e) => e.stopPropagation()}>
            <div class="modal-header">
              <span class="modal-title">
                {pt().enable ? "Enable" : "Disable"} plugin · {pt().item.name}
              </span>
              <button class="modal-x" onClick={() => cancelToggle()} aria-label="Cancel">
                ×
              </button>
            </div>
            <p class="confirm-body">
              Runs this native command ({pt().item.scope} scope) — the cockpit
              never edits config directly:
            </p>
            <pre class="confirm-cmd">{pt().preview}</pre>
            <div class="modal-actions">
              <button class="btn btn-ghost" onClick={() => cancelToggle()}>
                Cancel
              </button>
              <button
                class="btn btn-primary"
                onClick={() => void confirmToggle()}
              >
                {pt().enable ? "Enable" : "Disable"}
              </button>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
};

const InventoryRow: Component<{ item: InventoryItem }> = (props) => {
  const i = props.item;
  const meta = () => TYPE_META[i.type];
  return (
    <div class="inv-row" classList={{ "inv-row-dim": i.toggleable && !i.enabled }}>
      <span class="inv-type" style={{ color: meta().color }} title={meta().label}>
        {meta().glyph}
      </span>
      <div class="inv-main">
        <div class="inv-row-top">
          <span class="inv-name">{i.name}</span>
          <span class="inv-scope" classList={{ "inv-scope-project": i.scope === "project" }}>
            {i.scope}
          </span>
        </div>
        <Show when={i.desc}>
          <div class="inv-desc">{i.desc}</div>
        </Show>
        <Show when={i.detail}>
          <div class="inv-detail">{i.detail}</div>
        </Show>
      </div>
      <StatePill item={i} />
    </div>
  );
};

const CELL_META: Record<AuditCellState, { glyph: string; cls: string; title: string }> = {
  on: { glyph: "●", cls: "mx-on", title: "enabled" },
  off: { glyph: "○", cls: "mx-off", title: "disabled" },
  absent: { glyph: "·", cls: "mx-absent", title: "not present" },
  error: { glyph: "!", cls: "mx-error", title: "parse error" },
};

/** Cross-project audit matrix (P2-F5): rows = plugins/MCP, cols = open projects. */
const AuditMatrixView: Component = () => {
  return (
    <Show
      when={!audit.loading}
      fallback={<div class="inv-empty">loading matrix…</div>}
    >
      <Show
        when={audit.error}
        fallback={
          <Show
            when={audit.data && audit.data.columns.length > 0}
            fallback={<div class="inv-empty">no open project tabs to audit</div>}
          >
            <div class="inv-matrix-wrap">
              <table class="inv-matrix">
                <thead>
                  <tr>
                    <th class="mx-corner">item</th>
                    <For each={audit.data!.columns}>
                      {(c) => (
                        <th class="mx-col" title={c.projectPath}>
                          {c.label}
                        </th>
                      )}
                    </For>
                  </tr>
                </thead>
                <tbody>
                  <For each={audit.data!.rows}>
                    {(row) => (
                      <tr>
                        <td class="mx-rowhead" title={row.detail ?? row.name}>
                          <span
                            class="mx-rowtype"
                            style={{ color: TYPE_META[row.type].color }}
                          >
                            {TYPE_META[row.type].glyph}
                          </span>
                          <span class="mx-rowname">{row.name}</span>
                        </td>
                        <For each={row.cells}>
                          {(state) => (
                            <td
                              class={`mx-cell ${CELL_META[state].cls}`}
                              title={CELL_META[state].title}
                            >
                              {CELL_META[state].glyph}
                            </td>
                          )}
                        </For>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
          </Show>
        }
      >
        <div class="inv-empty inv-error">{audit.error}</div>
      </Show>
    </Show>
  );
};

export const InventoryPanel: Component = () => {
  const counts = inventoryCounts;
  const shown = filteredInventory;
  return (
    <Show when={inventoryOpen()}>
      <aside class="inv-panel" role="dialog" aria-label="Inventory">
        <header class="inv-header">
          <span class="inv-title">INVENTORY</span>
          <div class="inv-view-seg">
            <button
              class="inv-seg-btn"
              classList={{ active: invView() === "browse" }}
              onClick={() => setInventoryView("browse")}
            >
              Browse
            </button>
            <button
              class="inv-seg-btn"
              classList={{ active: invView() === "audit" }}
              onClick={() => setInventoryView("audit")}
            >
              Audit
            </button>
          </div>
          <Show when={invView() === "browse"}>
            <span class="inv-count">
              {shown().length}
              <Show when={shown().length !== inventory.items.length}>
                <span class="inv-count-total"> / {inventory.items.length}</span>
              </Show>
            </span>
          </Show>
          <span class="inv-spacer" />
          <button
            class="inv-icon-btn"
            title="Reload"
            onClick={() => reloadInventoryView()}
            aria-label="Reload"
          >
            ⟳
          </button>
          <button
            class="inv-icon-btn"
            title="Close (Esc)"
            onClick={() => closeInventory()}
            aria-label="Close inventory"
          >
            ×
          </button>
        </header>

        <Show when={invView() === "browse"}>
        <div class="inv-filters">
          <div class="inv-type-chips">
            <For each={TYPES}>
              {(t) => (
                <button
                  class="inv-chip"
                  classList={{ active: invTypes().has(t) }}
                  onClick={() => toggleInvType(t)}
                  style={{
                    "--chip-color": TYPE_META[t].color,
                  }}
                >
                  <span class="inv-chip-glyph">{TYPE_META[t].glyph}</span>
                  {TYPE_META[t].label}
                  <span class="inv-chip-count">{counts()[t]}</span>
                </button>
              )}
            </For>
          </div>
          <div class="inv-scope-seg">
            <For each={SCOPES}>
              {(s) => (
                <button
                  class="inv-seg-btn"
                  classList={{ active: invScope() === s }}
                  onClick={() => setInvScopeFilter(s)}
                >
                  {s}
                </button>
              )}
            </For>
          </div>
          <input
            class="inv-search"
            type="text"
            placeholder="filter…"
            value={invQuery()}
            onInput={(e) => setInvQueryFilter(e.currentTarget.value)}
          />
        </div>
        </Show>

        <div class="inv-body">
          <Show
            when={invView() === "browse"}
            fallback={<AuditMatrixView />}
          >
            <Show
              when={!inventory.loading}
              fallback={<div class="inv-empty">loading…</div>}
            >
              <Show
                when={inventory.error}
                fallback={
                  <Show
                    when={shown().length > 0}
                    fallback={<div class="inv-empty">no items match</div>}
                  >
                    <For each={shown()}>{(item) => <InventoryRow item={item} />}</For>
                  </Show>
                }
              >
                <div class="inv-empty inv-error">{inventory.error}</div>
              </Show>
            </Show>
          </Show>
        </div>

        <footer class="inv-footer">
          <Show
            when={invView() === "browse"}
            fallback={<>● on · ○ off · · absent · ! error — effective per project</>}
          >
            plugins toggle via native claude · ⌘I to close
          </Show>
        </footer>
      </aside>
      <ConfirmToggle />
    </Show>
  );
};

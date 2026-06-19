// StatusBadge — reflects the backend pane status EXACTLY (never invent a state).
//
// Design colors:
//   IDLE        #6b7280  gray dot
//   WORKING     #3b82f6  blue, pulsing
//   NEEDS_INPUT #ef4444  red dot
//   DEAD        #991b1b  dark red dot
//   UNKNOWN     #9ca3af  gray "?"

import { type Component } from "solid-js";
import type { PaneStatus } from "../ipc";

const META: Record<
  PaneStatus,
  { color: string; label: string; pulse: boolean; glyph: string }
> = {
  IDLE: { color: "#6b7280", label: "Idle", pulse: false, glyph: "" },
  WORKING: { color: "#3b82f6", label: "Working", pulse: true, glyph: "" },
  NEEDS_INPUT: { color: "#ef4444", label: "Needs input", pulse: false, glyph: "" },
  DEAD: { color: "#991b1b", label: "Dead", pulse: false, glyph: "" },
  UNKNOWN: { color: "#9ca3af", label: "Unknown", pulse: false, glyph: "?" },
};

export const StatusBadge: Component<{ status: PaneStatus }> = (props) => {
  const meta = () => META[props.status] ?? META.UNKNOWN;
  return (
    <span class="status-badge" title={meta().label}>
      <span
        class="status-dot"
        classList={{ pulse: meta().pulse }}
        style={{ background: meta().color }}
      >
        {meta().glyph}
      </span>
      <span class="status-label" style={{ color: meta().color }}>
        {meta().label}
      </span>
    </span>
  );
};

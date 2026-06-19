/* CC Cockpit frontend — PLACEHOLDER.
 *
 * The frontend-agent replaces this with the real tab/pane/xterm UI, building
 * against the backend IPC contract (commands: cockpit_init, create_tab, …;
 * events: pane:data, pane:topology, pane:status). For now this just renders a
 * boot message so `tauri dev` / `tauri build` succeed before the real UI lands.
 */
import { render } from "solid-js/web";

function Booting() {
  return (
    <div
      style={{
        display: "flex",
        "align-items": "center",
        "justify-content": "center",
        height: "100vh",
        "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace",
        "font-size": "14px",
        color: "#cbd5e1",
        background: "#0b0f17",
      }}
    >
      cockpit booting…
    </div>
  );
}

const root = document.getElementById("root");
if (root) render(() => <Booting />, root);

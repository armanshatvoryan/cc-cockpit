/* CC Cockpit frontend — entry point.
 *
 * Mounts the real cockpit shell (tabs / panes / live xterm) against the backend
 * IPC contract (commands: cockpit_init, create_tab, …; events: pane:data,
 * pane:topology, pane:status).
 */
import { render } from "solid-js/web";
import { App } from "./App";
import "./styles.css";
// Side-effect import: stamps `data-theme` on <html> at module load, so a light
// user's first painted frame is already light. Listed here rather than left to
// the component graph so the ordering is explicit and survives a refactor.
import "./theme";

const root = document.getElementById("root");
if (root) render(() => <App />, root);

/* CC Cockpit frontend — entry point.
 *
 * Mounts the real cockpit shell (tabs / panes / live xterm) against the backend
 * IPC contract (commands: cockpit_init, create_tab, …; events: pane:data,
 * pane:topology, pane:status).
 */
import { render } from "solid-js/web";
import { App } from "./App";
import "./styles.css";

const root = document.getElementById("root");
if (root) render(() => <App />, root);

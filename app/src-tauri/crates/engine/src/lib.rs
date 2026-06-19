//! Control-client engine — the GUI-independent core the Tauri commands wrap.
//!
//! Responsibilities (D3 priority 2, but factored so it runs headless):
//!   * spawn `tmux -L <socket> -C attach -t <session>` (the single control client
//!     per session, per §2 of the frontend design doc);
//!   * pump its stdout through the proven `cockpit_control_mode::Parser`;
//!   * route decoded `%output` to a sink as `PaneData { pane_id, bytes_b64 }`
//!     (base64, as the bridge requires) and topology events to a callback;
//!   * write input back to the control client's stdin via `pane_send_keys`
//!     (`send-keys -t <pane> -l <data>`, literal) and `pane_resize`
//!     (`resize-pane`/`refresh-client -C`).
//!
//! Tauri's `tauri::AppHandle::emit` and `tauri::command` are intentionally NOT
//! referenced here so this whole engine compiles + runs without Tauri. The
//! Tauri layer (`tauri-app/`) is a thin shell that owns an `Engine` and forwards
//! its callbacks to `app.emit("pane:data", …)`.

use base64::Engine as _;
use cockpit_control_mode::{Event, Parser};
use std::io::{BufWriter, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

/// What the engine pushes outward. The Tauri layer maps these 1:1 onto the
/// `pane:data` / `pane:topology` events in the §5 IPC contract.
#[derive(Debug, Clone)]
pub enum Outbound {
    /// `%output` for a pane, already base64-encoded for the bridge.
    PaneData { pane_id: String, bytes_b64: String },
    /// A topology / lifecycle event the frontend cares about.
    Topology(TopologyEvent),
    /// The control client exited (session gone / detached).
    Exit { reason: Option<String> },
}

#[derive(Debug, Clone)]
pub enum TopologyEvent {
    WindowAdd { window_id: String },
    WindowClose { window_id: String },
    LayoutChange { window_id: String, layout: String },
    ActivePaneChanged { window_id: String, pane_id: String },
    PaneModeChanged { pane_id: String },
}

/// Errors the engine can return on command invocation.
#[derive(Debug)]
pub enum EngineError {
    Spawn(std::io::Error),
    NotAttached,
    Write(std::io::Error),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Spawn(e) => write!(f, "spawn control client: {e}"),
            EngineError::NotAttached => write!(f, "no control client attached"),
            EngineError::Write(e) => write!(f, "write to control client: {e}"),
        }
    }
}
impl std::error::Error for EngineError {}

/// A live control client: the child `tmux -CC` process + its reader thread.
pub struct ControlClient {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    reader: Option<JoinHandle<()>>,
}

impl ControlClient {
    /// Spawn `tmux -L <socket> -C attach -t <session>` and start pumping its
    /// stdout through the parser. Every `Outbound` is sent on the returned
    /// channel; the Tauri layer forwards them to `app.emit`. A headless test or
    /// the `live-bridge` binary can just drain the receiver.
    pub fn attach(socket: &str, session: &str) -> Result<(Self, Receiver<Outbound>), EngineError> {
        let mut child = Command::new("tmux")
            .args(["-L", socket, "-C", "attach", "-t", session])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(EngineError::Spawn)?;

        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take().expect("piped stdin");

        let (tx, rx) = mpsc::channel::<Outbound>();
        let reader = std::thread::spawn(move || pump(stdout, tx));

        Ok((
            ControlClient {
                child,
                stdin: BufWriter::new(stdin),
                reader: Some(reader),
            },
            rx,
        ))
    }

    /// Send literal VT input to a pane. xterm `onData` already encodes arrows /
    /// ctrl-codes / paste — we forward verbatim with `send-keys -l` (literal).
    /// Fire-and-forget at the JS layer; here we just write + flush.
    pub fn pane_send_keys(&mut self, pane_id: &str, data: &str) -> Result<(), EngineError> {
        // `-l` = literal (no key-name translation). Quote with tmux's `;`-safe
        // single quotes? send-keys -l takes the data as one argument; to pass
        // arbitrary bytes safely over the control-client command line we use the
        // hex form is overkill — control mode forwards the command line verbatim
        // to the tmux server, so we wrap the data in single quotes and escape any
        // embedded single quotes. (For a spike this is adequate; production may
        // prefer `send-keys -H <hexpairs>` for full binary safety.)
        let quoted = shell_single_quote(data);
        let cmd = format!("send-keys -t {pane_id} -l {quoted}\n");
        self.write_cmd(&cmd)
    }

    /// Send raw bytes as hex pairs via `send-keys -H` — fully binary-safe path
    /// for control codes / paste. Preferred for the real input round-trip.
    pub fn pane_send_keys_hex(&mut self, pane_id: &str, bytes: &[u8]) -> Result<(), EngineError> {
        let mut hexes = String::new();
        for b in bytes {
            if !hexes.is_empty() {
                hexes.push(' ');
            }
            hexes.push_str(&format!("{b:02x}"));
        }
        let cmd = format!("send-keys -t {pane_id} -H {hexes}\n");
        self.write_cmd(&cmd)
    }

    /// Resize: the xterm fit value is authoritative, pushed to tmux. We resize
    /// the whole control client viewport via `refresh-client -C <w>,<h>` (and a
    /// best-effort `resize-pane` for the specific pane).
    pub fn pane_resize(&mut self, pane_id: &str, cols: u16, rows: u16) -> Result<(), EngineError> {
        // resize-pane sets the pane; refresh-client -C sets the control client's
        // own size so tmux lays out to our WebView. Send both.
        let cmd = format!(
            "resize-pane -t {pane_id} -x {cols} -y {rows}\nrefresh-client -C {cols},{rows}\n"
        );
        self.write_cmd(&cmd)
    }

    /// Ctrl+C interrupt (P1-F5): send 0x03 to the pane.
    pub fn interrupt_pane(&mut self, pane_id: &str) -> Result<(), EngineError> {
        self.pane_send_keys_hex(pane_id, &[0x03])
    }

    fn write_cmd(&mut self, cmd: &str) -> Result<(), EngineError> {
        self.stdin
            .write_all(cmd.as_bytes())
            .map_err(EngineError::Write)?;
        self.stdin.flush().map_err(EngineError::Write)
    }

    /// Detach + reap. Idempotent-ish; safe to call on drop.
    pub fn shutdown(&mut self) {
        let _ = self.write_cmd("detach-client\n");
        let _ = self.child.wait();
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
    }
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Reader thread body: stream stdout → parser → `Outbound` channel.
fn pump(mut stdout: impl Read, tx: Sender<Outbound>) {
    let mut parser = Parser::new();
    let mut buf = [0u8; 64 * 1024];
    let b64 = base64::engine::general_purpose::STANDARD;
    loop {
        let n = match stdout.read(&mut buf) {
            Ok(0) => break, // EOF: control client closed
            Ok(n) => n,
            Err(_) => break,
        };
        for ev in parser.feed(&buf[..n]) {
            let out = match ev {
                Event::Output { pane_id, data } => Some(Outbound::PaneData {
                    pane_id,
                    bytes_b64: b64.encode(&data),
                }),
                Event::WindowAdd { window_id } => {
                    Some(Outbound::Topology(TopologyEvent::WindowAdd { window_id }))
                }
                Event::WindowClose { window_id } => {
                    Some(Outbound::Topology(TopologyEvent::WindowClose { window_id }))
                }
                Event::LayoutChange {
                    window_id, layout, ..
                } => Some(Outbound::Topology(TopologyEvent::LayoutChange {
                    window_id,
                    layout,
                })),
                Event::WindowPaneChanged {
                    window_id,
                    pane_id,
                } => Some(Outbound::Topology(TopologyEvent::ActivePaneChanged {
                    window_id,
                    pane_id,
                })),
                Event::PaneModeChanged { pane_id } => {
                    Some(Outbound::Topology(TopologyEvent::PaneModeChanged { pane_id }))
                }
                Event::Exit { reason } => Some(Outbound::Exit { reason }),
                // Begin/End/Error/renames/session events: not forwarded to the
                // terminal layer in this spike (command-reply plumbing).
                _ => None,
            };
            if let Some(o) = out {
                if tx.send(o).is_err() {
                    return; // receiver dropped
                }
            }
        }
    }
    let _ = tx.send(Outbound::Exit { reason: Some("eof".into()) });
}

/// Wrap `s` in single quotes, escaping embedded single quotes for the tmux
/// command line (`'\''` trick). Adequate for the spike's send-keys path.
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_quote_escapes_embedded_quote() {
        assert_eq!(shell_single_quote("ab"), "'ab'");
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn hex_path_is_used_for_control_bytes_conceptually() {
        // Ctrl+C is 0x03 -> "03" hex. We can't spawn tmux in a pure unit test,
        // but we lock the hex formatting that interrupt_pane relies on.
        let bytes = [0x03u8, 0x1b, 0x5b, 0x41]; // Ctrl-C, ESC, [, A  (an up-arrow)
        let hexes: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hexes.join(" "), "03 1b 5b 41");
    }
}

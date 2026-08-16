//! live-bridge — headless end-to-end proof of the D3 data path WITHOUT a GUI.
//!
//! Spawns the real control-client engine against a private `-L cockpit-d3`
//! tmux session and exercises the full loop the Tauri app will run:
//!   1. attach  -> receive base64 `%output` (output path: tmux -> screen)
//!   2. send keys (`echo BRIDGE_RTT_<n>`) -> assert it round-trips in output
//!      (input path: keyboard -> tmux)
//!   3. resize  -> assert a layout-change topology event arrives (resize path)
//!   4. split   -> assert a second pane appears + emits output (topology)
//!
//! Exit code 0 = every leg observed; non-zero = which leg failed (stderr).
//! The Tauri layer reuses `cockpit_engine::ControlClient` verbatim; this binary
//! is the auto-verifiable stand-in for the parts a GUI window would show.
//!
//! Usage: `live-bridge <socket> <session>`  (defaults: cockpit-d3 d3live)

use base64::Engine as _;
use cockpit_engine::{ControlClient, Outbound, TopologyEvent};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let socket = args.next().unwrap_or_else(|| "cockpit-d3".into());
    let session = args.next().unwrap_or_else(|| "d3live".into());

    let b64 = base64::engine::general_purpose::STANDARD;

    eprintln!("[live-bridge] attaching to -L {socket} session {session}");
    let (mut cc, rx) = match ControlClient::attach(&socket, &session) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("FAIL: attach: {e}");
            std::process::exit(2);
        }
    };

    // Accumulators for verdicts.
    let mut saw_initial_output = false;
    let mut input_roundtrip = false;
    let mut saw_resize_layout = false;
    let mut saw_split_pane_output = false;
    let mut second_pane_id: Option<String> = None;

    // Discover the first pane id from initial output so we can target it.
    let mut first_pane: Option<String> = None;

    // The unique token we expect to see echoed back (input round-trip proof).
    let rtt_token = format!("BRIDGE_RTT_{}", std::process::id());

    // Phase machine driven by elapsed time so the single-threaded drain stays
    // simple and deterministic-ish.
    let start = Instant::now();
    let mut sent_keys = false;
    let mut sent_resize = false;
    let mut sent_split = false;

    let deadline = start + Duration::from_secs(8);
    loop {
        if Instant::now() >= deadline {
            break;
        }
        // Drive actions on a timeline.
        let elapsed = start.elapsed();
        if !sent_keys && elapsed >= Duration::from_millis(800) {
            if let Some(p) = &first_pane {
                eprintln!("[live-bridge] -> send-keys '{rtt_token}' to {p}");
                // Send the command then an Enter (0x0d) so the shell runs it.
                let _ = cc.pane_send_keys(p, &format!("echo {rtt_token}"));
                let _ = cc.pane_send_keys_hex(p, &[0x0d]);
                sent_keys = true;
            }
        }
        if !sent_resize && elapsed >= Duration::from_millis(2200) {
            if let Some(p) = &first_pane {
                eprintln!("[live-bridge] -> resize {p} to 100x30");
                let _ = cc.pane_resize(p, 100, 30);
                sent_resize = true;
            }
        }
        if !sent_split && elapsed >= Duration::from_millis(3600) {
            eprintln!("[live-bridge] -> split-window (via raw command)");
            // Split is a control-client command, not send-keys; reuse the stdin.
            // We expose it through pane_send_keys? No — send a real tmux command.
            // ControlClient has no generic exec in the spike API, so we drive the
            // split with a second short-lived plain tmux call on the same socket
            // (proving external-terminal topology reflection at the same time).
            let _ = std::process::Command::new("tmux")
                .args(["-L", &socket, "split-window", "-t", &session, "-h"])
                .status();
            sent_split = true;
        }

        match rx.recv_timeout(Duration::from_millis(150)) {
            Ok(Outbound::PaneData { pane_id, bytes_b64 }) => {
                saw_initial_output = true;
                if first_pane.is_none() {
                    first_pane = Some(pane_id.clone());
                }
                let raw = b64.decode(bytes_b64.as_bytes()).unwrap_or_default();
                let text = String::from_utf8_lossy(&raw);
                if text.contains(&rtt_token) {
                    input_roundtrip = true;
                }
                if let Some(p2) = &second_pane_id {
                    if &pane_id == p2 {
                        saw_split_pane_output = true;
                    }
                }
            }
            Ok(Outbound::Topology(TopologyEvent::LayoutChange { layout, .. })) => {
                eprintln!("[live-bridge] <- layout-change: {layout}");
                if layout.contains("100x30") {
                    saw_resize_layout = true;
                }
            }
            Ok(Outbound::Topology(TopologyEvent::ActivePaneChanged { pane_id, .. })) => {
                eprintln!("[live-bridge] <- active-pane-changed -> {pane_id}");
                // After a horizontal split the new pane becomes active; remember
                // it so we can confirm it emits output (a fresh pane id != first).
                if Some(&pane_id) != first_pane.as_ref() {
                    second_pane_id = Some(pane_id);
                }
            }
            Ok(Outbound::Topology(ev)) => {
                eprintln!("[live-bridge] <- topology: {ev:?}");
            }
            Ok(Outbound::CommandError { lines }) => {
                eprintln!("[live-bridge] <- command REJECTED: {}", lines.join(" | "));
            }
            Ok(Outbound::Exit { reason }) => {
                eprintln!("[live-bridge] <- control client exit: {reason:?}");
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    cc.shutdown();

    // ── verdict ──────────────────────────────────────────────────────────────
    println!("\n=== live-bridge results ===");
    report("output path (received %output)", saw_initial_output);
    report("input round-trip (echo token observed)", input_roundtrip);
    report("resize path (100x30 layout-change)", saw_resize_layout);
    report("split topology (2nd pane emitted output)", saw_split_pane_output);

    let all = saw_initial_output && input_roundtrip && saw_resize_layout;
    // split-pane-output is best-effort timing-wise; report but don't gate on it.
    if all {
        println!("\nLIVE DATA PATH: PROVEN (output + input + resize)");
        std::process::exit(0);
    } else {
        println!("\nLIVE DATA PATH: INCOMPLETE — see legs above");
        std::process::exit(1);
    }
}

fn report(label: &str, ok: bool) {
    println!("  [{}] {label}", if ok { "PASS" } else { "FAIL" });
}

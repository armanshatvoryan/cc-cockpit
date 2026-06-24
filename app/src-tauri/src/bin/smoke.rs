//! Headless smoke — drives the SessionManager end-to-end WITHOUT the GUI.
//!
//! Proves the v1 backbone works against the PRIVATE `-L cockpit` socket:
//!   1. init      -> ensure socket + cockpit-main session, attach control client
//!   2. create_tab -> a second tab (tmux window)
//!   3. split_pane -> a second pane in that tab
//!   4. launch_shell -> run `echo COCKPIT_SMOKE_OK` + `sleep 1` in a pane (NOT
//!      real claude — no API key), exercising the send-keys + control-client path
//!   5. list_state -> capture tabs/panes/cwd
//!   6. poll_statuses -> classify each live pane (status heuristic)
//!   7. teardown  -> kill the cockpit session on the PRIVATE socket only
//!
//! Exit 0 = every leg observed. The default tmux socket is NEVER touched; the
//! caller (smoke script) verifies the default socket's sessions are unchanged.

use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use base64::Engine as _;
use cc_cockpit_lib::manager::SessionManager;

fn main() {
    let mut fail = 0;
    let mut mgr = SessionManager::new();

    // 1. init
    let rx = match mgr.init() {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!("FAIL init: {e}");
            std::process::exit(2);
        }
    };
    println!("[smoke] init OK — attached to -L cockpit / cockpit-main");

    // Drain the engine channel on a thread; collect a flag if our token echoes.
    let b64 = base64::engine::general_purpose::STANDARD;
    let token = "COCKPIT_SMOKE_OK";
    let (saw_tx, saw_rx) = std::sync::mpsc::channel::<bool>();
    std::thread::spawn(move || {
        let mut saw = false;
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(cockpit_engine::Outbound::PaneData { bytes_b64, .. }) => {
                    let raw = b64.decode(bytes_b64.as_bytes()).unwrap_or_default();
                    let text = String::from_utf8_lossy(&raw);
                    if text.contains("COCKPIT_SMOKE_OK") && !saw {
                        saw = true;
                        let _ = saw_tx.send(true);
                    }
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {
                    // keep waiting until the channel closes
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    // 2. create_tab
    let tab = match mgr.create_tab(Some("smoke-tab")) {
        Ok(t) => {
            println!(
                "[smoke] create_tab OK — tabId={} window={} pane={}",
                t.tab_id, t.tmux_window_id, t.pane_id
            );
            t
        }
        Err(e) => {
            eprintln!("FAIL create_tab: {e}");
            mgr.teardown();
            std::process::exit(3);
        }
    };

    // 3. split_pane (split the new tab's pane horizontally)
    let split = match mgr.split_pane(&tab.pane_id, "h") {
        Ok(s) => {
            println!(
                "[smoke] split_pane OK — newPane={} layout={}",
                s.pane_id, s.layout
            );
            s
        }
        Err(e) => {
            eprintln!("FAIL split_pane: {e}");
            mgr.teardown();
            std::process::exit(4);
        }
    };

    // 4. launch_shell + send a marker command (echo + sleep), NOT real claude.
    if let Err(e) = mgr.launch_shell(&tab.pane_id, "/tmp") {
        eprintln!("FAIL launch_shell: {e}");
        fail += 1;
    } else {
        println!("[smoke] launch_shell OK (cd /tmp in {})", tab.pane_id);
    }
    // Drive an echo marker + a brief sleep through the control-client path so we
    // exercise the same send-keys round-trip the frontend will use.
    if let Err(e) = mgr.pane_send_keys(&tab.pane_id, "echo COCKPIT_SMOKE_OK && sleep 1") {
        eprintln!("FAIL pane_send_keys: {e}");
        fail += 1;
    }
    // Enter (CR).
    let _ = mgr.pane_send_keys(&tab.pane_id, "\r");

    // Give the echo time to round-trip back through %output.
    let echoed = saw_rx.recv_timeout(Duration::from_secs(3)).unwrap_or(false);
    if echoed {
        println!("[smoke] echo round-trip OK — saw '{token}' in %output");
    } else {
        eprintln!("[smoke] WARN echo round-trip not observed within 3s");
        fail += 1;
    }

    // 5. list_state
    match mgr.list_state() {
        Ok(st) => {
            println!(
                "[smoke] list_state OK — {} tab(s), {} pane(s)",
                st.tabs.len(),
                st.panes.len()
            );
            for t in &st.tabs {
                println!(
                    "         tab {} (win {} idx {}) name='{}' panes={:?}",
                    t.tab_id, t.tmux_window_id, t.index, t.name, t.pane_ids
                );
            }
            for p in &st.panes {
                println!(
                    "         pane {} tab={} cwd='{}' dead={} status={}",
                    p.pane_id, p.tab_id, p.cwd, p.dead, p.status
                );
            }
            // Expect at least 2 tabs (bootstrap + smoke-tab) and the split pane.
            if st.tabs.len() < 2 {
                eprintln!("FAIL expected >=2 tabs, got {}", st.tabs.len());
                fail += 1;
            }
            if !st.panes.iter().any(|p| p.pane_id == split.pane_id) {
                eprintln!("FAIL split pane {} not in state", split.pane_id);
                fail += 1;
            }
        }
        Err(e) => {
            eprintln!("FAIL list_state: {e}");
            fail += 1;
        }
    }

    // 6. poll_statuses — classify each live pane.
    let statuses = mgr.poll_statuses();
    println!("[smoke] poll_statuses OK — {} status change(s):", statuses.len());
    for s in &statuses {
        println!(
            "         {} -> {} (ambiguous={}, recencyMs={})",
            s.pane_id, s.status, s.ambiguous, s.recency_ms
        );
    }

    // 6b. DEAD detection — kill the shell process in pane %2 (remain-on-exit
    // keeps the dead pane around) and confirm the heuristic reports DEAD.
    if let Err(e) = mgr.pane_send_keys(&split.pane_id, "exit\r") {
        eprintln!("[smoke] WARN could not send exit to {}: {e}", split.pane_id);
    }
    std::thread::sleep(Duration::from_millis(800));
    let dead_changes = mgr.poll_statuses();
    let saw_dead = dead_changes
        .iter()
        .any(|s| s.pane_id == split.pane_id && s.status == "DEAD");
    if saw_dead {
        println!("[smoke] DEAD detection OK — {} reported DEAD", split.pane_id);
    } else {
        // Some shells/configs close the pane outright instead of leaving it dead;
        // verify via list_state that the pane is gone OR flagged dead.
        let gone_or_dead = match mgr.list_state() {
            Ok(st) => st
                .panes
                .iter()
                .find(|p| p.pane_id == split.pane_id)
                .map(|p| p.dead)
                .unwrap_or(true),
            Err(_) => false,
        };
        if gone_or_dead {
            println!(
                "[smoke] DEAD detection OK — {} is gone/dead in state",
                split.pane_id
            );
        } else {
            eprintln!("[smoke] WARN DEAD not observed for {}", split.pane_id);
            fail += 1;
        }
    }

    // 6c. RUNTIME RECONNECT — simulate the server vanishing mid-run (force-quit /
    // external kill), then drive a structural op through `with_reattach` and confirm
    // it self-heals: the op succeeds AND a fresh Outbound receiver is returned (so
    // the GUI would rebind its forwarder), and the healed server has the new tab.
    {
        let _ = std::process::Command::new("tmux")
            .args(["-L", "cockpit", "kill-server"])
            .status();
        std::thread::sleep(Duration::from_millis(150));
        match mgr.with_reattach(|m| m.create_tab(Some("after-reconnect"))) {
            Ok((t, new_rx)) => {
                if new_rx.is_some() {
                    println!(
                        "[smoke] RECONNECT OK — server vanished; re-healed + re-attached; new tab {} ({})",
                        t.tab_id, t.pane_id
                    );
                } else {
                    eprintln!("[smoke] FAIL reconnect: op succeeded but no re-attach (server not seen as gone)");
                    fail += 1;
                }
                match mgr.list_state() {
                    Ok(st) if st.tabs.iter().any(|x| x.tab_id == t.tab_id) => {
                        println!("[smoke] RECONNECT state OK — {} tab(s) after heal", st.tabs.len());
                    }
                    Ok(_) => {
                        eprintln!("[smoke] FAIL reconnect: new tab missing after heal");
                        fail += 1;
                    }
                    Err(e) => {
                        eprintln!("[smoke] FAIL reconnect list_state: {e}");
                        fail += 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("[smoke] FAIL reconnect: {e}");
                fail += 1;
            }
        }
    }

    // 6d. CLOSE IDEMPOTENCY — closing a window that's already gone must be a no-op
    // success, never a hard error. Direct regression guard for the "close_tab
    // failed: can't find window: N" toast: tabs are closed by stable window id
    // (@n), and a vanished window already IS closed.
    match mgr.close_tab("@99999", true) {
        Ok(r) if r.ok => println!("[smoke] close_tab(@99999 absent) idempotent OK"),
        Ok(r) => {
            eprintln!("FAIL close_tab(absent) returned ok=false: {:?}", r.live_panes);
            fail += 1;
        }
        Err(e) => {
            eprintln!("FAIL close_tab(absent) errored (should be idempotent): {e}");
            fail += 1;
        }
    }
    // A mutable index-style target (the old bug's shape) must be REJECTED before it
    // ever reaches tmux — close addresses the stable @n only.
    match mgr.close_tab("cockpit-main:1", true) {
        Err(_) => println!("[smoke] close_tab rejects index-style target OK"),
        Ok(_) => {
            eprintln!("FAIL close_tab accepted a non-@ target 'cockpit-main:1'");
            fail += 1;
        }
    }

    // 6e. EMPTY-STATE RE-CREATE — closing every tab destroys the tmux session (it
    // can't hold 0 windows); the next create must re-heal + ADOPT the lone
    // bootstrap window, yielding EXACTLY ONE tab, not two. Direct regression guard
    // for "close last tab → ⌘T opens 2 tabs". Closing by stable @n, idempotent.
    let windows: Vec<String> = mgr
        .list_state()
        .map(|s| s.tabs.iter().map(|t| t.tmux_window_id.clone()).collect())
        .unwrap_or_default();
    for w in &windows {
        let _ = mgr.close_tab(w, true);
    }
    match mgr.create_tab_healing(None) {
        Ok((tab, _rx)) => {
            let n = mgr.list_state().map(|s| s.tabs.len()).unwrap_or(0);
            if n == 1 {
                println!(
                    "[smoke] empty→create = exactly 1 tab OK (adopted {})",
                    tab.tmux_window_id
                );
            } else {
                eprintln!("FAIL empty→create yielded {n} tab(s), want exactly 1");
                fail += 1;
            }
        }
        Err(e) => {
            eprintln!("FAIL create_tab_healing after empty: {e}");
            fail += 1;
        }
    }

    // 7. teardown — kill the cockpit session on the PRIVATE socket only.
    mgr.teardown();
    println!("[smoke] teardown OK — cockpit session killed on -L cockpit");

    if fail == 0 {
        println!("\nSMOKE: PASS");
        std::process::exit(0);
    } else {
        println!("\nSMOKE: {fail} leg(s) FAILED");
        std::process::exit(1);
    }
}

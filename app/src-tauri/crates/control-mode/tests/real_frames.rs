//! Real-frame integration test — THE D3 verdict gate.
//!
//! Feeds bytes captured verbatim from a live `tmux -CC attach` session
//! (`capture-frames*.sh` → `tests/fixtures/*.raw`) through the parser and
//! asserts it extracts pane output + topology events correctly. If this passes,
//! the load-bearing control-mode unknown is proven against reality, not a mock.
//!
//! To prove the test really runs over real bytes (not a hand-typed mock), we
//! also feed the stream **byte-at-a-time** to exercise the partial-line buffer
//! the way a chunked socket read would, and assert identical results.

use cockpit_control_mode::{decode_octal, Event, Parser};

const FIXTURE_OUTPUT_SPLIT: &[u8] =
    include_bytes!("fixtures/session-output-split-resize-kill.raw");
const FIXTURE_WINDOW_ADD: &[u8] = include_bytes!("fixtures/window-add-error-panemode.raw");

/// Parse a whole fixture in one feed.
fn parse_all(bytes: &[u8]) -> Vec<Event> {
    let mut p = Parser::new();
    p.feed(bytes)
}

/// Parse the same bytes one byte at a time (simulates worst-case chunking).
fn parse_byte_by_byte(bytes: &[u8]) -> Vec<Event> {
    let mut p = Parser::new();
    let mut all = Vec::new();
    for &b in bytes {
        all.extend(p.feed(&[b]));
    }
    all
}

#[test]
fn real_frames_chunking_is_invariant() {
    // The exact same event sequence must come out regardless of how the stream
    // is chunked. This is the property the Tauri reader loop relies on.
    for fixture in [FIXTURE_OUTPUT_SPLIT, FIXTURE_WINDOW_ADD] {
        let whole = parse_all(fixture);
        let chunked = parse_byte_by_byte(fixture);
        assert_eq!(
            whole, chunked,
            "byte-by-byte parse diverged from whole-buffer parse"
        );
        assert!(!whole.is_empty(), "fixture produced no events");
    }
}

#[test]
fn real_frames_extract_pane_output_decoded_to_raw_vt() {
    let evs = parse_all(FIXTURE_OUTPUT_SPLIT);

    // Collect decoded output per pane.
    let mut pane0 = Vec::new();
    let mut pane1 = Vec::new();
    for ev in &evs {
        if let Event::Output { pane_id, data } = ev {
            match pane_id.as_str() {
                "%0" => pane0.extend_from_slice(data),
                "%1" => pane1.extend_from_slice(data),
                _ => {}
            }
        }
    }

    assert!(!pane0.is_empty(), "expected output for pane %0");
    assert!(!pane1.is_empty(), "expected output for pane %1 (the split pane)");

    // The octal escapes must be decoded to REAL VT bytes, not left as text.
    // ESC (0x1B) must appear in the decoded pane0 stream (colors / OSC seqs).
    assert!(
        pane0.contains(&0x1b),
        "decoded pane %0 output must contain raw ESC (0x1B) — octal \\033 decoded"
    );
    // Our printf wrote "Hello\tD3" — the echoed command + result should surface
    // the literal text "Hello" somewhere in the decoded pane0 byte stream.
    assert!(
        contains_subslice(&pane0, b"Hello"),
        "decoded pane %0 output should contain the echoed 'Hello' text"
    );
    // The ANSI red "RED box" line: after decode, the SGR red is ESC[31m and the
    // literal word RED must be present.
    assert!(
        contains_subslice(&pane0, b"RED"),
        "decoded pane %0 output should contain 'RED'"
    );
    assert!(
        contains_subslice(&pane0, b"\x1b[31m"),
        "decoded pane %0 output should contain the raw SGR red sequence ESC[31m"
    );
    // The second pane ran `echo second-pane`.
    assert!(
        contains_subslice(&pane1, b"second-pane"),
        "decoded pane %1 output should contain 'second-pane'"
    );

    // CRITICAL fidelity guard. We must NOT ship the octal *escape token* `\134`
    // (the on-wire encoding of a literal backslash) un-decoded to xterm — every
    // `\134` on the wire must have become a single 0x5C byte. Note: the decoded
    // stream legitimately MAY contain the two literal bytes `\` + `0` (e.g. the
    // shell echoing a user who typed `printf '\033…'`); that is real screen
    // content, not a parser miss. So we assert on the encoding token, not on the
    // decoded text: the 4-char sequence `\134` must be absent post-decode.
    assert!(
        !contains_subslice(&pane0, b"\\134"),
        "decoded output must NOT contain the octal escape token '\\134' — it should be a 0x5C byte"
    );
    // And the many real VT escapes on the wire (\\033 ESC) must have become 0x1B:
    // count ESC bytes and require a healthy number (the prompt/SGR/OSC sequences).
    let esc_count = pane0.iter().filter(|&&b| b == 0x1b).count();
    assert!(
        esc_count > 50,
        "expected the real \\033 escapes to decode to many ESC (0x1B) bytes, got {esc_count}"
    );
}

#[test]
fn real_frames_extract_topology_events() {
    let evs = parse_all(FIXTURE_OUTPUT_SPLIT);

    // The capture: split (-> a layout-change to 2 panes), resize the client
    // (-> layout-change to 100x30), kill-pane (-> layout-change back to 1 pane),
    // plus window-pane-changed and a final %exit.
    let layout_changes: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            Event::LayoutChange { layout, .. } => Some(layout.clone()),
            _ => None,
        })
        .collect();
    assert!(
        layout_changes.len() >= 3,
        "expected >=3 layout-change events (split, resize, kill), got {}: {:?}",
        layout_changes.len(),
        layout_changes
    );

    // First layout-change after the split must describe a TWO-pane window
    // (a `{...}` split container). The kill must collapse back to a single pane
    // (no brace container in the final layout).
    let split_layout = &layout_changes[0];
    assert!(
        split_layout.contains('{'),
        "split layout should be a split container: {split_layout}"
    );
    let final_layout = layout_changes.last().unwrap();
    assert!(
        !final_layout.contains('{'),
        "post-kill layout should collapse to a single pane: {final_layout}"
    );

    // Resize is visible as a layout-change whose dimensions changed to 100x30.
    assert!(
        layout_changes.iter().any(|l| l.contains("100x30")),
        "expected a layout-change reflecting the 100x30 resize: {layout_changes:?}"
    );

    // The active pane changed (window-pane-changed) at least once.
    assert!(
        evs.iter().any(|e| matches!(e, Event::WindowPaneChanged { .. })),
        "expected at least one window-pane-changed event"
    );

    // The control client exited cleanly at the end.
    assert!(
        matches!(evs.last(), Some(Event::Exit { .. })),
        "stream should end with %exit, got {:?}",
        evs.last()
    );

    // %begin/%end command-reply blocks must pair up (no dangling block leaks
    // into output). Count them: equal begins and ends (errors handled separately).
    let begins = evs.iter().filter(|e| matches!(e, Event::Begin { .. })).count();
    let ends = evs
        .iter()
        .filter(|e| matches!(e, Event::End { .. } | Event::Error { .. }))
        .count();
    assert_eq!(begins, ends, "every %begin must be terminated by %end/%error");
}

#[test]
fn real_frames_extract_window_add_and_error_block() {
    let evs = parse_all(FIXTURE_WINDOW_ADD);

    // new-window -> a %window-add with a window id.
    let win_add = evs.iter().find_map(|e| match e {
        Event::WindowAdd { window_id } => Some(window_id.clone()),
        _ => None,
    });
    assert_eq!(
        win_add.as_deref(),
        Some("@1"),
        "new-window should emit %window-add @1"
    );

    // The deliberately-invalid command -> an %error block carrying tmux's
    // "unknown command" message between %begin and %error.
    let err = evs.iter().find_map(|e| match e {
        Event::Error { lines, .. } => Some(lines.clone()),
        _ => None,
    });
    let err_lines = err.expect("expected an %error block from the invalid command");
    assert!(
        err_lines.iter().any(|l| l.contains("unknown command")),
        "error block should carry the 'unknown command' message: {err_lines:?}"
    );

    // copy-mode -> %pane-mode-changed for the pane.
    assert!(
        evs.iter().any(|e| matches!(e, Event::PaneModeChanged { .. })),
        "copy-mode should emit %pane-mode-changed"
    );

    // window-renamed events surfaced (zsh / [tmux] etc.).
    assert!(
        evs.iter().any(|e| matches!(e, Event::WindowRenamed { .. })),
        "expected at least one %window-renamed event"
    );
}

#[test]
fn no_event_is_silently_dropped_as_unknown_garbage() {
    // Every line in the real stream should map to a modelled event OR a clearly
    // labelled Unknown (with a non-empty name). A stray empty-name Unknown means
    // we mis-split a line — fail loudly so the parser stays honest.
    for fixture in [FIXTURE_OUTPUT_SPLIT, FIXTURE_WINDOW_ADD] {
        let evs = parse_all(fixture);
        for ev in &evs {
            if let Event::Unknown { name, rest } = ev {
                assert!(
                    !name.is_empty(),
                    "empty-name Unknown means a line was mis-parsed: rest={rest:?}"
                );
                // For our fixtures we expect to model everything; log which
                // notifications are unmodelled so the verdict can note them.
                eprintln!("note: unmodelled control-mode notification %{name} {rest}");
            }
        }
    }
}

#[test]
fn decode_octal_roundtrips_a_known_escaped_payload_from_the_capture() {
    // A literal payload lifted from the real capture (the colored line), to lock
    // the decoder against an exact real-world byte string.
    let raw = b"\\033[31mRED\\033[0m box\\015\\015\\012";
    let decoded = decode_octal(raw);
    let mut expected = Vec::new();
    expected.extend_from_slice(b"\x1b[31mRED\x1b[0m box");
    expected.extend_from_slice(&[0x0d, 0x0d, 0x0a]); // CR CR LF
    assert_eq!(decoded, expected);
}

/// True if `needle` appears contiguously in `haystack`.
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return needle.is_empty();
    }
    haystack
        .windows(needle.len())
        .any(|w| w == needle)
}

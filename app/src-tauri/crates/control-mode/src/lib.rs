//! tmux control-mode (`tmux -CC`) protocol parser — D3 spike, load-bearing unknown.
//!
//! tmux's control mode is a **line-oriented** protocol: every notification is a
//! single `\n`-terminated line beginning with `%`. Command output is wrapped in
//! `%begin … %end` (or `%begin … %error`) blocks. The bytes parsed here were
//! captured from a *real* `tmux -CC attach` session (see `tests/fixtures/*.raw`,
//! produced by `capture-frames.sh`) — this parser is verified against them.
//!
//! Wire facts confirmed from the capture:
//!   * `%output %<pane> <data>` — `<data>` is octal-escaped: a backslash followed
//!     by exactly three octal digits is one byte (`\033`→0x1B, `\015`→0x0D,
//!     `\012`→0x0A, `\134`→0x5C). Any other byte is literal. We decode to raw
//!     bytes so xterm.js receives verbatim VT (colors / box-drawing / alt-screen).
//!   * `%begin <ts> <num> <flags>` … `%end <ts> <num> <flags>` wrap a reply.
//!   * On error the text sits *between* `%begin` and `%error <ts> <num> <flags>`.
//!   * Topology: `%window-add @<win>`, `%window-close @<win>`, `%layout-change
//!     @<win> <layout> <visible> <flags>`, `%window-pane-changed @<win> %<pane>`,
//!     `%window-renamed @<win> <name>`, `%session-changed $<s> <name>`,
//!     `%session-window-changed $<s> @<win>`, `%pane-mode-changed %<pane>`,
//!     `%unlinked-window-add @<win>`, `%exit [reason]`.
//!
//! The parser is a streaming state machine: `feed(&[u8]) -> Vec<Event>`. It
//! buffers a partial trailing line, because real pipe/socket reads split mid-line.
//! This is exactly the shape the Tauri reader loop drives.

use std::collections::VecDeque;

/// A parsed control-mode event. `%output` payloads are already octal-decoded to
/// raw bytes, ready to base64 + emit over the Tauri bridge to xterm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `%output %<pane> <data>` — decoded raw bytes for this pane.
    Output { pane_id: String, data: Vec<u8> },
    /// `%begin <ts> <num> <flags>` — start of a command reply block.
    Begin { ts: u64, num: u64, flags: u64 },
    /// `%end <ts> <num> <flags>` — successful reply end. `lines` = reply body.
    End { ts: u64, num: u64, flags: u64, lines: Vec<String> },
    /// `%error <ts> <num> <flags>` — failed reply end. `lines` = error body.
    Error { ts: u64, num: u64, flags: u64, lines: Vec<String> },
    /// `%window-add @<win>`
    WindowAdd { window_id: String },
    /// `%unlinked-window-add @<win>`
    UnlinkedWindowAdd { window_id: String },
    /// `%window-close @<win>`
    WindowClose { window_id: String },
    /// `%window-renamed @<win> <name>`
    WindowRenamed { window_id: String, name: String },
    /// `%layout-change @<win> <layout> <visible-layout> <flags>`
    LayoutChange {
        window_id: String,
        layout: String,
        visible_layout: String,
        flags: String,
    },
    /// `%window-pane-changed @<win> %<pane>` — active pane in a window changed.
    WindowPaneChanged { window_id: String, pane_id: String },
    /// `%pane-mode-changed %<pane>` — pane entered/left copy/view mode.
    PaneModeChanged { pane_id: String },
    /// `%session-changed $<s> <name>`
    SessionChanged { session_id: String, name: String },
    /// `%session-window-changed $<s> @<win>`
    SessionWindowChanged { session_id: String, window_id: String },
    /// `%session-renamed <name>`
    SessionRenamed { name: String },
    /// `%client-detached <client>` / `%client-session-changed …` (carried raw).
    /// `%exit [reason]` — control client is terminating.
    Exit { reason: Option<String> },
    /// Any `%…` notification we don't model yet — kept so nothing is silently
    /// dropped (the Tauri layer can log/inspect). `name` excludes the leading %.
    Unknown { name: String, rest: String },
}

/// Streaming control-mode parser. Drive it with `feed(bytes)`; it returns every
/// fully-formed event since the last call, buffering any partial trailing line.
#[derive(Debug, Default)]
pub struct Parser {
    /// Bytes of an incomplete line not yet terminated by `\n`.
    line_buf: Vec<u8>,
    /// When inside a `%begin … %end/%error` block we accumulate reply lines here.
    block: Option<Block>,
    /// Emitted events not yet drained (used by the iterator-style API).
    pending: VecDeque<Event>,
}

#[derive(Debug)]
struct Block {
    // ts/num/flags carried for completeness/diagnostics; the terminating
    // %end/%error line re-states them, so the parser reads those, not these.
    #[allow(dead_code)]
    ts: u64,
    #[allow(dead_code)]
    num: u64,
    #[allow(dead_code)]
    flags: u64,
    lines: Vec<String>,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of raw protocol bytes; returns all events newly completed.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        for &b in bytes {
            if b == b'\n' {
                // Take the completed line (strip a trailing \r if present).
                let mut line = std::mem::take(&mut self.line_buf);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.handle_line(line);
            } else {
                self.line_buf.push(b);
            }
        }
        self.pending.drain(..).collect()
    }

    fn emit(&mut self, ev: Event) {
        self.pending.push_back(ev);
    }

    fn handle_line(&mut self, raw: Vec<u8>) {
        // `%output` FIRST, at the byte level. tmux octal-escapes only C0
        // controls and backslash in the payload — high bytes pass through RAW,
        // so a UTF-8 char split across pty reads arrives as invalid raw bytes.
        // A lossy String round-trip would replace those with U+FFFD before
        // `decode_octal` ever ran (rendered as "��"), so the payload must never
        // touch a String. Dispatched before the block check to match the old
        // routing (a `%`-line mid-block was dispatched, not body text).
        if let Some(rest) = raw.strip_prefix(b"%output ") {
            let (pane, data) = match rest.iter().position(|&b| b == b' ') {
                Some(i) => (&rest[..i], &rest[i + 1..]),
                None => (rest, &rest[rest.len()..]),
            };
            let ev = Event::Output {
                pane_id: String::from_utf8_lossy(pane).into_owned(),
                data: decode_octal(data),
            };
            self.emit(ev);
            return;
        }

        // Every other notification line is ASCII up to its payload, so lossy
        // UTF-8 for routing is safe.
        let line = String::from_utf8_lossy(&raw).into_owned();

        // Inside a reply block, non-`%` lines are body text (e.g. the error
        // message, or `list-panes` output). `%`-lines still terminate the block.
        let is_notification = line.starts_with('%');

        if self.block.is_some() && !is_notification {
            if let Some(b) = self.block.as_mut() {
                b.lines.push(line);
            }
            return;
        }

        if !is_notification {
            // Stray non-% line outside a block — shouldn't happen in well-formed
            // streams, but never panic; surface it as Unknown for diagnostics.
            self.emit(Event::Unknown {
                name: String::new(),
                rest: line,
            });
            return;
        }

        // Split "%name rest…" — note %output's data may itself contain spaces, so
        // we only split off the command word here and parse rest per-command.
        let body = &line[1..]; // drop leading %
        let (name, rest) = match body.find(' ') {
            Some(i) => (&body[..i], &body[i + 1..]),
            None => (body, ""),
        };

        match name {
            "output" => self.handle_output(rest),
            "begin" => self.handle_begin(rest),
            "end" => self.handle_end(rest, /*is_error=*/ false),
            "error" => self.handle_end(rest, /*is_error=*/ true),
            "window-add" => self.emit_win("window-add", rest),
            "unlinked-window-add" => {
                self.emit(Event::UnlinkedWindowAdd {
                    window_id: first_token(rest).to_string(),
                });
            }
            "window-close" => self.emit_win("window-close", rest),
            "window-renamed" => {
                let (w, n) = split_first(rest);
                self.emit(Event::WindowRenamed {
                    window_id: w.to_string(),
                    name: n.to_string(),
                });
            }
            "layout-change" => self.handle_layout(rest),
            "window-pane-changed" => {
                let (w, p) = split_first(rest);
                self.emit(Event::WindowPaneChanged {
                    window_id: w.to_string(),
                    pane_id: p.to_string(),
                });
            }
            "pane-mode-changed" => {
                self.emit(Event::PaneModeChanged {
                    pane_id: first_token(rest).to_string(),
                });
            }
            "session-changed" => {
                let (s, n) = split_first(rest);
                self.emit(Event::SessionChanged {
                    session_id: s.to_string(),
                    name: n.to_string(),
                });
            }
            "session-window-changed" => {
                let (s, w) = split_first(rest);
                self.emit(Event::SessionWindowChanged {
                    session_id: s.to_string(),
                    window_id: w.to_string(),
                });
            }
            "session-renamed" => {
                self.emit(Event::SessionRenamed {
                    name: rest.to_string(),
                });
            }
            "exit" => {
                let reason = if rest.is_empty() {
                    None
                } else {
                    Some(rest.to_string())
                };
                self.emit(Event::Exit { reason });
            }
            other => {
                self.emit(Event::Unknown {
                    name: other.to_string(),
                    rest: rest.to_string(),
                });
            }
        }
    }

    fn emit_win(&mut self, kind: &str, rest: &str) {
        let window_id = first_token(rest).to_string();
        let ev = match kind {
            "window-add" => Event::WindowAdd { window_id },
            "window-close" => Event::WindowClose { window_id },
            _ => unreachable!(),
        };
        self.emit(ev);
    }

    fn handle_output(&mut self, rest: &str) {
        // rest = "%<pane> <octal-escaped-data...>"  (data may be empty)
        let (pane, data) = split_first(rest);
        let decoded = decode_octal(data.as_bytes());
        self.emit(Event::Output {
            pane_id: pane.to_string(),
            data: decoded,
        });
    }

    fn handle_begin(&mut self, rest: &str) {
        let (ts, num, flags) = parse_three_u64(rest);
        // A new %begin while a block is open shouldn't occur, but be defensive:
        // flush the stale one as an End-with-no-terminator would be wrong, so we
        // just drop it and start fresh (matches tmux's strict pairing).
        self.block = Some(Block {
            ts,
            num,
            flags,
            lines: Vec::new(),
        });
        self.emit(Event::Begin { ts, num, flags });
    }

    fn handle_end(&mut self, rest: &str, is_error: bool) {
        let (ts, num, flags) = parse_three_u64(rest);
        let lines = self.block.take().map(|b| b.lines).unwrap_or_default();
        let ev = if is_error {
            Event::Error {
                ts,
                num,
                flags,
                lines,
            }
        } else {
            Event::End {
                ts,
                num,
                flags,
                lines,
            }
        };
        self.emit(ev);
    }

    fn handle_layout(&mut self, rest: &str) {
        // "@<win> <layout> <visible-layout> <flags>"
        let mut it = rest.splitn(4, ' ');
        let window_id = it.next().unwrap_or("").to_string();
        let layout = it.next().unwrap_or("").to_string();
        let visible_layout = it.next().unwrap_or("").to_string();
        let flags = it.next().unwrap_or("").to_string();
        self.emit(Event::LayoutChange {
            window_id,
            layout,
            visible_layout,
            flags,
        });
    }
}

/// Decode tmux's octal escaping in `%output` data to raw bytes.
///
/// Rule (verified against the capture): a backslash followed by exactly three
/// octal digits (0-7) is a single byte with that octal value. Any other
/// backslash, or any non-backslash byte, is taken literally. tmux emits literal
/// bytes for everything it can and `\NNN` only for control / non-printable / the
/// backslash itself (`\134`).
pub fn decode_octal(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if b == b'\\' && i + 3 < input.len() + 1 && i + 4 <= input.len() {
            let d0 = input[i + 1];
            let d1 = input[i + 2];
            let d2 = input[i + 3];
            if is_octal(d0) && is_octal(d1) && is_octal(d2) {
                let val = ((d0 - b'0') as u16) * 64
                    + ((d1 - b'0') as u16) * 8
                    + (d2 - b'0') as u16;
                out.push(val as u8); // tmux escapes are always <= \377 = 255
                i += 4;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    out
}

#[inline]
fn is_octal(b: u8) -> bool {
    (b'0'..=b'7').contains(&b)
}

/// First whitespace-delimited token of `s`.
fn first_token(s: &str) -> &str {
    s.split(' ').next().unwrap_or("")
}

/// Split `s` into (first token, remainder after the first space).
fn split_first(s: &str) -> (&str, &str) {
    match s.find(' ') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    }
}

/// Parse "<u64> <u64> <u64>" leniently (missing fields → 0).
fn parse_three_u64(s: &str) -> (u64, u64, u64) {
    let mut it = s.split(' ');
    let a = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let b = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let c = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    (a, b, c)
}

// ───────────────────────────── unit tests ──────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_passes_raw_invalid_utf8_bytes_untouched() {
        // tmux 3.6b octal-escapes only C0 controls and backslash in %output —
        // high bytes pass through RAW (verified live). A UTF-8 char split
        // across pty reads therefore arrives as invalid raw bytes, one half
        // per %output line. The parser must hand those bytes through untouched;
        // a lossy String round-trip replaces them with U+FFFD (EF BF BD) and
        // the terminal renders "��" (the emoji/em-dash garble).
        let mut p = Parser::new();
        // 🚨 = F0 9F 9A A8, split 2+2; second read also carries escaped CR LF.
        let mut evs = p.feed(b"%output %0 \xF0\x9F\n");
        evs.extend(p.feed(b"%output %0 \x9A\xA8\\015\\012\n"));
        let datas: Vec<Vec<u8>> = evs
            .iter()
            .filter_map(|e| match e {
                Event::Output { pane_id, data } if pane_id == "%0" => Some(data.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            datas,
            vec![vec![0xF0, 0x9F], vec![0x9A, 0xA8, 0x0D, 0x0A]],
            "raw high bytes must survive the parser byte-exact"
        );
    }

    #[test]
    fn octal_decode_core_controls() {
        // \033 ESC, \015 CR, \012 LF, \134 backslash — the four that appear most.
        assert_eq!(decode_octal(b"\\033"), vec![0x1b]);
        assert_eq!(decode_octal(b"\\015"), vec![0x0d]);
        assert_eq!(decode_octal(b"\\012"), vec![0x0a]);
        assert_eq!(decode_octal(b"\\134"), vec![b'\\']);
    }

    #[test]
    fn octal_decode_mixed_literal_and_escaped() {
        // "RED" literal, ESC[31m escaped, trailing literal text.
        let input = b"\\033[31mRED\\033[0m box\\r"; // note \r here is literal backslash-r (not octal)
        let out = decode_octal(input);
        // \033 -> ESC, "[31mRED", \033 -> ESC, "[0m box", then literal "\r" stays.
        let mut expect = Vec::new();
        expect.push(0x1b);
        expect.extend_from_slice(b"[31mRED");
        expect.push(0x1b);
        expect.extend_from_slice(b"[0m box\\r"); // \r is NOT 3 octal digits -> literal
        assert_eq!(out, expect);
    }

    #[test]
    fn octal_decode_trailing_backslash_is_literal() {
        // A lone trailing backslash with <3 following digits stays literal.
        assert_eq!(decode_octal(b"ab\\1"), b"ab\\1".to_vec());
        assert_eq!(decode_octal(b"ab\\"), b"ab\\".to_vec());
    }

    #[test]
    fn parses_a_single_output_line() {
        let mut p = Parser::new();
        let evs = p.feed(b"%output %0 Hello\\033[0m\n");
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            Event::Output { pane_id, data } => {
                assert_eq!(pane_id, "%0");
                let mut want = b"Hello".to_vec();
                want.push(0x1b);
                want.extend_from_slice(b"[0m");
                assert_eq!(data, &want);
            }
            other => panic!("expected Output, got {other:?}"),
        }
    }

    #[test]
    fn buffers_partial_lines_across_feeds() {
        // A read that splits mid-line must not lose or mis-parse data.
        let mut p = Parser::new();
        let e1 = p.feed(b"%output %0 par");
        assert!(e1.is_empty(), "no complete line yet");
        let e2 = p.feed(b"tial\n");
        assert_eq!(e2.len(), 1);
        match &e2[0] {
            Event::Output { pane_id, data } => {
                assert_eq!(pane_id, "%0");
                assert_eq!(data, b"partial");
            }
            other => panic!("expected Output, got {other:?}"),
        }
    }

    #[test]
    fn error_block_captures_message_between_begin_and_error() {
        let mut p = Parser::new();
        let stream = b"%begin 100 7 1\nparse error: unknown command: foo\n%error 100 7 1\n";
        let evs = p.feed(stream);
        // Begin, then Error carrying the body line.
        assert!(matches!(evs[0], Event::Begin { num: 7, .. }));
        match &evs[1] {
            Event::Error { num, lines, .. } => {
                assert_eq!(*num, 7);
                assert_eq!(lines, &vec!["parse error: unknown command: foo".to_string()]);
            }
            other => panic!("expected Error, got {other:?}"),
        }
        assert_eq!(evs.len(), 2);
    }

    #[test]
    fn parses_topology_lines() {
        let mut p = Parser::new();
        let stream = b"%window-add @1\n%window-pane-changed @0 %1\n%layout-change @0 8205,80x24,0,0{40x24,0,0,0,39x24,41,0,1} 8205,80x24,0,0{40x24,0,0,0,39x24,41,0,1} *\n%pane-mode-changed %1\n%exit\n";
        let evs = p.feed(stream);
        assert_eq!(evs[0], Event::WindowAdd { window_id: "@1".into() });
        assert_eq!(
            evs[1],
            Event::WindowPaneChanged {
                window_id: "@0".into(),
                pane_id: "%1".into()
            }
        );
        match &evs[2] {
            Event::LayoutChange {
                window_id,
                layout,
                visible_layout,
                flags,
            } => {
                assert_eq!(window_id, "@0");
                assert_eq!(layout, "8205,80x24,0,0{40x24,0,0,0,39x24,41,0,1}");
                assert_eq!(visible_layout, "8205,80x24,0,0{40x24,0,0,0,39x24,41,0,1}");
                assert_eq!(flags, "*");
            }
            other => panic!("expected LayoutChange, got {other:?}"),
        }
        assert_eq!(evs[3], Event::PaneModeChanged { pane_id: "%1".into() });
        assert_eq!(evs[4], Event::Exit { reason: None });
    }
}

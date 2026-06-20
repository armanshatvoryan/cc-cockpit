//! Pane live-state heuristic — Rust port of the validated D6 `parse_state.sh`.
//!
//! Classifies a single Claude Code pane snapshot into one of:
//!   IDLE | WORKING | NEEDS_INPUT | DEAD | UNKNOWN
//! using the REAL marker strings captured 2026-06-18 (see
//! `spikes/d6/REAL-MARKERS.md`). D6 verdict on this logic was GREEN (4/4).
//!
//! Inputs the SessionManager provides per poll:
//!   * `dead`            — `#{pane_dead}` == 1 (from list-panes).
//!   * `text`            — `capture-pane -p -e -S -120` output for the pane.
//!   * `last_activity_age` — seconds since `#{pane_activity}` advanced (optional).
//!
//! Algorithm (D6 §3, priority order):
//!   1. dead                                          -> DEAD
//!   2. NEEDS_INPUT box (question + numbered options) -> NEEDS_INPUT
//!   3. WORKING marker (spinner glyph / live timer)   -> WORKING
//!   4. empty input prompt / idle footer              -> IDLE
//!   6. nothing matched / signals disagree            -> UNKNOWN (the `?` state)
//!
//! Like the shell version, competing affordances are resolved by which one is
//! drawn LOWEST on screen (CC is a bottom-anchored TUI), with WORKING winning
//! ties against IDLE.

use serde::Serialize;

/// The five v1 statuses. `Unknown` is the explicit ambiguous (`?`) verdict —
/// we never emit a confident-wrong guess; ambiguity surfaces as Unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Status {
    Idle,
    Working,
    NeedsInput,
    Dead,
    Unknown,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Idle => "IDLE",
            Status::Working => "WORKING",
            Status::NeedsInput => "NEEDS_INPUT",
            Status::Dead => "DEAD",
            Status::Unknown => "UNKNOWN",
        }
    }
}

/// Default idle-debounce window (seconds) — matches D6 `IDLE_SECS=3`.
pub const IDLE_SECS: u64 = 3;

/// Strip ANSI SGR (and common CSI) escapes so patterns match visible text.
/// Mirrors the `sed 's/\x1b\[[0-9;?]*[A-Za-z]//g'` in parse_state.sh.
pub(crate) fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Skip CSI: ESC [ params(0-9;?) final(A-Za-z)
            let mut j = i + 2;
            while j < bytes.len() {
                let c = bytes[j];
                if c.is_ascii_alphabetic() {
                    j += 1;
                    break;
                }
                if c.is_ascii_digit() || c == b';' || c == b'?' {
                    j += 1;
                } else {
                    break;
                }
            }
            i = j;
        } else {
            // Push this char (handle multi-byte UTF-8 by copying the whole char).
            let ch_len = utf8_len(bytes[i]);
            let end = (i + ch_len).min(bytes.len());
            out.push_str(&s[i..end]);
            i = end;
        }
    }
    out
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

// ── Marker tests (no regex crate — hand-rolled scanners over the line) ───────

/// WORKING: live elapsed timer `… <N>s · … tokens …`. Mirrors the validated D6
/// shell regex `([0-9]+m )?[0-9]+s · .*tokens` — a "<digits>s · " followed
/// somewhere later by "tokens" on the same line. The enclosing parens are NOT
/// required (this CC build renders `25s · ↓ 31.7k tokens` without them).
fn has_working_timer(line: &str) -> bool {
    let b = line.as_bytes();
    let mut k = 1;
    while k < b.len() {
        // Find an 's' immediately preceded by a digit, then require " · " after.
        if b[k] == b's' && b[k - 1].is_ascii_digit() {
            let rest = line[k + 1..].trim_start();
            if rest.starts_with('\u{00b7}') {
                // " · " seen after the seconds count — now look for "tokens" later
                // on the line (the validated shell pattern's `.*tokens`).
                if rest.contains("tokens") {
                    return true;
                }
            }
        }
        k += 1;
    }
    false
}

/// The animated working spinner: a dingbat-star glyph followed by a present-
/// participle verb ending in an ellipsis — "✽ Cooking…", "✻ Coalescing…",
/// "✺ Evaporating…". The glyph FRAME animates (CC cycles ✶✷✸✹✺✻✼✽✾…) and the verb
/// list is open-ended, so we match the invariant SHAPE — a leading star glyph plus
/// a trailing "…" — NOT a specific frame or word. A SETTLED turn reads
/// "✻ Cooked for 7s" (past tense, no ellipsis) and does NOT match; this is the
/// reliable active-turn discriminator. (The old code keyed on the single `✽` frame
/// + a hardcoded verb whitelist and missed most of CC's spinner states.)
fn has_active_spinner(line: &str) -> bool {
    let t = line.trim();
    t.ends_with('…') && matches!(t.chars().next(), Some(c) if is_spinner_glyph(c))
}

/// A dingbat star/asterisk/florette glyph (U+2720–U+2746) — the range CC cycles
/// the spinner through. Only meaningful paired with the trailing-ellipsis shape.
fn is_spinner_glyph(c: char) -> bool {
    ('\u{2720}'..='\u{2746}').contains(&c)
}

/// Non-spinner WORKING corroborators — present ONLY during an active turn.
fn has_working_other(line: &str) -> bool {
    line.contains("to run in background") || line.contains("esc to interrupt")
}

/// A line that means claude is actively mid-turn. Every component is active-only
/// (a live `Ns · tokens` timer, the star+ellipsis spinner, or an interrupt hint) —
/// each is REPLACED by the settled "✻ …ed for Ns" summary when the turn ends, so
/// none linger in scrollback to cause a false positive.
fn is_working_line(line: &str) -> bool {
    has_working_timer(line) || has_active_spinner(line) || has_working_other(line)
}

/// NEEDS_INPUT question half.
fn is_needs_question(line: &str) -> bool {
    line.contains("Do you want")
        || line.contains("Would you like")
        || line.contains("Do you trust")
        || line.contains("wants to")
        || (line.contains("Allow ") && line.contains(" to"))
}

/// NEEDS_INPUT numbered-option half (`❯ 1.` / `1. Yes` / `Yes, and` / …).
fn is_needs_option(line: &str) -> bool {
    let t = line.trim_start();
    // `❯ 1.` selectable, or a plain `1. Yes/No/Allow/Deny`, or canned phrasings.
    if t.contains("Yes, and") || t.contains("No, and tell") {
        return true;
    }
    // `❯` then a digit-dot, or a leading "<digit>. <Yes|No|Allow|Deny>".
    let after_caret = t.strip_prefix('\u{276f}').map(|s| s.trim_start()).unwrap_or(t);
    let bytes = after_caret.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_digit() && bytes[1] == b'.' {
        return true;
    }
    false
}

/// Settled OS shell prompt (zsh/bash/fish): the last non-blank line ends with a
/// conventional prompt sigil. Used ONLY as the final fallback before Unknown so
/// the default + `Launch shell` panes (plain zsh) read IDLE instead of `?`.
/// `line` is already ANSI-stripped by `classify`.
fn is_shell_prompt(line: &str) -> bool {
    let t = line.trim_end();
    // Trailing-sigil prompts only ($ zsh/bash, % zsh, # root, ❯ pure/starship).
    // Leading-arrow themes (oh-my-zsh `➜`) end on the command region, not a
    // sigil, so they fall through to Unknown rather than risk a false IDLE.
    !t.is_empty() && matches!(t.chars().last(), Some('$') | Some('%') | Some('#') | Some('❯'))
}

/// IDLE footer/prompt: `⏵⏵ auto mode on` / `shift+tab to cycle`.
fn is_idle_marker(line: &str) -> bool {
    line.contains("auto mode on")
        || line.contains("shift+tab to cycle")
        || line.contains("\u{23f5}\u{23f5}") // ⏵⏵
}

/// 1-based line index (within the scanned tail) of the LAST line for which
/// `pred` is true, or 0 if none.
fn last_line_matching(lines: &[&str], pred: impl Fn(&str) -> bool) -> usize {
    let mut last = 0;
    for (i, l) in lines.iter().enumerate() {
        if pred(l) {
            last = i + 1;
        }
    }
    last
}

/// Classify a capture-pane snapshot. `last_activity_age` is seconds since
/// `#{pane_activity}` advanced (None if unknown).
pub fn classify(dead: bool, raw: &str, _last_activity_age: Option<u64>) -> Status {
    // Rule 1: dead short-circuits.
    if dead {
        return Status::Dead;
    }

    let text = strip_ansi(raw);
    // Look only at the last ~25 lines — CC's live status region is the tail.
    let all_lines: Vec<&str> = text.lines().collect();
    let start = all_lines.len().saturating_sub(25);
    let tail: Vec<&str> = all_lines[start..].to_vec();

    // WORKING: line of the last working signal.
    let w = last_line_matching(&tail, |l| is_working_line(l));

    // NEEDS_INPUT: require BOTH a question AND an option line; score by the lower
    // (later-drawn) so a half-scrolled box can't win against a fresh prompt.
    let nq = last_line_matching(&tail, |l| is_needs_question(l));
    let no = last_line_matching(&tail, |l| is_needs_option(l));
    let n = if nq > 0 && no > 0 { nq.max(no) } else { 0 };

    // IDLE prompt/footer.
    let idle = last_line_matching(&tail, |l| is_idle_marker(l));

    // ACTIVE working override. Every is_working_line signal is active-turn only
    // (see its doc), so a working line present means claude is mid-turn. CC's
    // "⏵⏵ auto mode on" footer is PERMANENT and bottom-most, so the position-based
    // "lowest marker wins" tiebreak below would pin IDLE during work; the old
    // activity-age debounce can't rescue it because tmux 3.6b reports empty
    // #{pane_activity} (verified live). So a working line forces WORKING — unless a
    // NEEDS_INPUT modal is up, which means claude has paused for the user.
    if n == 0 && w > 0 {
        return Status::Working;
    }

    // No Claude marker matched. Before giving up to Unknown, check whether the
    // last non-blank line is a settled OS shell prompt — those panes are
    // first-class in the cockpit and should read IDLE, not an alarming `?`.
    if w == 0 && n == 0 && idle == 0 {
        if tail
            .iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map_or(false, |l| is_shell_prompt(l))
        {
            return Status::Idle;
        }
        return Status::Unknown;
    }

    // Decide by which authoritative marker sits lowest; WORKING wins IDLE ties.
    let mut best = Status::Unknown;
    let mut best_line: i64 = -1;
    if (n as i64) > best_line {
        best = Status::NeedsInput;
        best_line = n as i64;
    }
    if (w as i64) > best_line {
        best = Status::Working;
        best_line = w as i64;
    }
    if (idle as i64) > best_line {
        best = Status::Idle;
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_short_circuits() {
        assert_eq!(classify(true, "anything", None), Status::Dead);
    }

    #[test]
    fn working_live_timer() {
        let snap = "✽ Transmuting… (30m 50s · ⎈ 137.1k tokens)\n   Running…\n";
        assert_eq!(classify(false, snap, None), Status::Working);
    }

    #[test]
    fn working_glyph_alone() {
        let snap = "some output\n✽ Cooking…\n";
        assert_eq!(classify(false, snap, None), Status::Working);
    }

    #[test]
    fn working_timer_without_parens() {
        // Real-corpus form (agent-list build): "25s · ↓ 31.7k tokens" — no parens.
        let snap = "  ◯ dev-agent  Run D6 live-state spike     25s · ↓ 31.7k tokens\n";
        assert_eq!(classify(false, snap, None), Status::Working);
    }

    #[test]
    fn idle_prompt_footer() {
        let snap = "⏺ Standing down.\n✻ Baked for 3s\n❯ \n⏵⏵ auto mode on (shift+tab to cycle)\n";
        assert_eq!(classify(false, snap, None), Status::Idle);
    }

    #[test]
    fn shell_prompt_is_idle_not_unknown() {
        // The default + `Launch shell` panes are plain zsh — no Claude markers.
        let snap = "Last login: …\narmanshatvoran@MacBook-Air-Arman src-tauri % \n";
        assert_eq!(classify(false, snap, None), Status::Idle);
    }

    #[test]
    fn shell_prompt_variants() {
        assert_eq!(classify(false, "user@host ~ $ \n", None), Status::Idle);
        assert_eq!(classify(false, "root@box:/etc# \n", None), Status::Idle);
        assert_eq!(classify(false, "~/repo ❯ \n", None), Status::Idle);
    }

    #[test]
    fn active_spinner_beats_persistent_footer() {
        // Real CC mid-turn on tmux 3.6b (empty pane_activity): the ✽ spinner sits
        // ABOVE the always-present, bottom-most "auto mode on" footer. The old
        // position tiebreak picked IDLE here; an active spinner must read WORKING.
        let snap = "⏺ working...\n✽ Evaporating…\n────\n❯ \n────\n  ⏵⏵ auto mode on (shift+tab to cycle)\n";
        assert_eq!(classify(false, snap, None), Status::Working);
    }

    #[test]
    fn animated_spinner_frame_is_working() {
        // The spinner glyph cycles through frames; a non-✽ frame (✻) mid-turn (verb
        // + ellipsis) must still read WORKING — this is the exact live capture that
        // the old single-glyph + verb-whitelist logic misclassified as IDLE.
        let snap = "⏺ ...\n✻ Coalescing…\n────\n❯ \n  ⏵⏵ auto mode on (shift+tab to cycle)\n";
        assert_eq!(classify(false, snap, None), Status::Working);
    }

    #[test]
    fn settled_summary_is_idle_not_working() {
        // Past-tense settled summary ("✻ Cooked for 7s", no ellipsis) + prompt +
        // footer -> IDLE. The spinner shape requires a trailing ellipsis.
        let snap = "⏺ done.\n✻ Cooked for 7s\n────\n❯ \n  ⏵⏵ auto mode on (shift+tab to cycle)\n";
        assert_eq!(classify(false, snap, None), Status::Idle);
    }

    #[test]
    fn needs_input_beats_active_spinner_override() {
        // A NEEDS_INPUT modal means claude paused for the user — not working — even
        // if a stale spinner lingers above. NEEDS_INPUT must still win.
        let snap = "✽ Working…\n╭ Do you want to proceed?\n❯ 1. Yes\n  2. No\n";
        assert_eq!(classify(false, snap, None), Status::NeedsInput);
    }

    #[test]
    fn truly_unknown_stays_unknown() {
        // No Claude markers, no shell sigil on the last non-blank line.
        let snap = "building target/release\ncompiling foo v0.1.0\n";
        assert_eq!(classify(false, snap, None), Status::Unknown);
    }

    #[test]
    fn settled_spinner_is_not_working() {
        // ✻ (settled) must NOT read as WORKING; only the idle footer matches.
        let snap = "✻ Baked for 3s\n❯ \n⏵⏵ auto mode on (shift+tab to cycle)\n";
        assert_eq!(classify(false, snap, None), Status::Idle);
    }

    #[test]
    fn needs_input_box() {
        let snap = "╭─────────╮\n│ Do you want to run this tool? │\n│ ❯ 1. Yes │\n│   2. Yes, and don't ask again │\n│   3. No, and tell Claude what to do │\n╰─────────╯\n";
        assert_eq!(classify(false, snap, None), Status::NeedsInput);
    }

    #[test]
    fn empty_snapshot_is_unknown() {
        assert_eq!(classify(false, "\n\n   \n", None), Status::Unknown);
    }

    #[test]
    fn stale_box_above_fresh_prompt_is_idle() {
        // A needs-input box scrolled up, with a fresh empty prompt below -> IDLE.
        let snap = "│ Do you want to X? │\n│ ❯ 1. Yes │\n│   2. No │\n(turn finished)\n❯ \n⏵⏵ auto mode on (shift+tab to cycle)\n";
        assert_eq!(classify(false, snap, None), Status::Idle);
    }

    #[test]
    fn live_timer_is_working_regardless_of_age() {
        // A live "Ns · tokens" timer only appears mid-turn, so it reads WORKING even
        // sitting above the permanent "auto mode on" footer — and independent of the
        // activity age (which is empty on tmux 3.6b anyway). Replaces the old
        // activity-debounce path, now obsolete: working signals are active-only.
        let snap = "  ◯ agent  building  5s · 1k tokens\n❯ \n⏵⏵ auto mode on (shift+tab to cycle)\n";
        assert_eq!(classify(false, snap, Some(10)), Status::Working);
        assert_eq!(classify(false, snap, None), Status::Working);
    }

    #[test]
    fn ansi_is_stripped_before_matching() {
        let snap = "\x1b[1;32m✽ Baking…\x1b[0m (5s · 1k tokens)\n";
        assert_eq!(classify(false, snap, None), Status::Working);
    }
}

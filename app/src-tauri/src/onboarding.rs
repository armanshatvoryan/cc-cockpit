//! First-run onboarding: environment probe + one-click prereq installs.
//!
//! Security invariant: NO user input ever reaches a command line. The only
//! installable things are the variants of `PrereqTool` (Task 4), each mapping
//! to a hardcoded argv; serde rejects anything else at the IPC boundary.
//!
//! Probing uses `zsh -lc` for the same reason as the PATH capture in
//! `lib.rs`: a Finder-launched app has a stripped PATH, and the login shell
//! sources /etc/zprofile + ~/.zprofile (Homebrew shellenv) — where tmux and
//! brew actually live on a normal setup.

use serde::Serialize;

/// Minimum tmux version the cockpit supports.
const MIN_TMUX: (u32, u32) = (3, 3);

/// Presence/health of one probed tool.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub found: bool,
    /// Raw version line (e.g. `"tmux 3.4"`), `None` when not found.
    pub version: Option<String>,
    /// tmux: found AND version >= 3.3. claude: same as `found` (presence-only).
    pub ok: bool,
}

/// Everything Step 1 of the wizard renders.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrereqReport {
    pub tmux: ToolStatus,
    pub claude: ToolStatus,
    pub brew: bool,
    pub npm: bool,
}

/// `"tmux 3.4"` → `(3, 4)`; `"tmux 3.3a"` → `(3, 3)`; `"tmux next-3.6"` →
/// `(3, 6)`; garbage → `None`.
fn parse_tmux_version(s: &str) -> Option<(u32, u32)> {
    let rest = s.trim().strip_prefix("tmux ").unwrap_or_else(|| s.trim());
    let rest = rest.strip_prefix("next-").unwrap_or(rest);
    let digits = |p: &str| -> Option<u32> {
        let d: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
        d.parse().ok()
    };
    let mut parts = rest.split('.');
    let major = digits(parts.next()?)?;
    let minor = parts.next().and_then(digits).unwrap_or(0);
    Some((major, minor))
}

/// Parse the sentinel-keyed probe output (Task 3 emits `CC_TMUX=` etc.). A
/// login shell can print rc noise, so only lines starting with a known key
/// count, and the LAST occurrence wins. An empty value ⇒ tool not found.
fn parse_probe_output(out: &str) -> PrereqReport {
    let mut tmux_v = String::new();
    let mut claude_v = String::new();
    let mut brew = String::new();
    let mut npm = String::new();
    for line in out.lines() {
        if let Some(v) = line.strip_prefix("CC_TMUX=") {
            tmux_v = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("CC_CLAUDE=") {
            claude_v = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("CC_BREW=") {
            brew = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("CC_NPM=") {
            npm = v.trim().to_string();
        }
    }
    PrereqReport {
        tmux: ToolStatus {
            found: !tmux_v.is_empty(),
            ok: parse_tmux_version(&tmux_v).is_some_and(|v| v >= MIN_TMUX),
            version: (!tmux_v.is_empty()).then(|| tmux_v.clone()),
        },
        claude: ToolStatus {
            found: !claude_v.is_empty(),
            ok: !claude_v.is_empty(),
            version: (!claude_v.is_empty()).then(|| claude_v.clone()),
        },
        brew: !brew.is_empty(),
        npm: !npm.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_version_parse() {
        assert_eq!(parse_tmux_version("tmux 3.4"), Some((3, 4)));
        assert_eq!(parse_tmux_version("tmux 3.3a"), Some((3, 3)));
        assert_eq!(parse_tmux_version("tmux next-3.6"), Some((3, 6)));
        assert_eq!(parse_tmux_version("tmux 3.2"), Some((3, 2)));
        assert_eq!(parse_tmux_version(""), None);
        assert_eq!(parse_tmux_version("command not found"), None);
    }

    #[test]
    fn tmux_ok_gate_is_3_3() {
        let ok = |s: &str| parse_tmux_version(s).is_some_and(|v| v >= MIN_TMUX);
        assert!(ok("tmux 3.3"));
        assert!(ok("tmux 3.4"));
        assert!(ok("tmux 4.0"));
        assert!(!ok("tmux 3.2"));
        assert!(!ok("garbage"));
    }

    #[test]
    fn probe_output_full_and_noisy() {
        let out = "rc-noise: welcome\nCC_TMUX=tmux 3.4\nCC_CLAUDE=1.0.72 (Claude Code)\nCC_BREW=/opt/homebrew/bin/brew\nCC_NPM=/Users/u/.nvm/versions/node/v22/bin/npm\n";
        let r = parse_probe_output(out);
        assert!(r.tmux.found && r.tmux.ok);
        assert_eq!(r.tmux.version.as_deref(), Some("tmux 3.4"));
        assert!(r.claude.found && r.claude.ok);
        assert!(r.brew && r.npm);
    }

    #[test]
    fn probe_output_all_missing() {
        let r = parse_probe_output("CC_TMUX=\nCC_CLAUDE=\nCC_BREW=\nCC_NPM=\n");
        assert!(!r.tmux.found && !r.tmux.ok && r.tmux.version.is_none());
        assert!(!r.claude.found && !r.claude.ok);
        assert!(!r.brew && !r.npm);
    }

    #[test]
    fn probe_output_old_tmux_found_but_not_ok() {
        let r = parse_probe_output("CC_TMUX=tmux 3.2\n");
        assert!(r.tmux.found);
        assert!(!r.tmux.ok);
    }

    #[test]
    fn report_serializes_camel_case() {
        let r = parse_probe_output("CC_TMUX=tmux 3.4\n");
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"tmux\""), "json: {json}");
        assert!(json.contains("\"found\":true"), "json: {json}");
        // Option<String> None serializes as null (frontend types it string|null).
        assert!(json.contains("\"version\":null"), "json: {json}");
    }
}

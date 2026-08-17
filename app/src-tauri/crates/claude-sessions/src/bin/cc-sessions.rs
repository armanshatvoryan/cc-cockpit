//! `cc-sessions` — Claude Code session ids on the command line.
//!
//! Shares the reader in `claude_sessions` with the cockpit's Tauri commands, so
//! the CLI and the toolbar chip can never disagree about what is running where.
//!
//! Tests: app/scripts/test-cc-sessions.sh

use std::io::Write;

use claude_sessions::{
    current_session, index_sessions, pane_map_dir, projects_root, read_pane_map, tmux_server_pid,
    SessionRow,
};

const USAGE: &str = "\
cc-sessions — list Claude Code sessions

USAGE:
    cc-sessions [OPTIONS]

OPTIONS:
    --current        print the session id running in THIS tmux pane, then exit
                     (exit 1 when the pane has no published session)
    --panes          list the live tmux-pane -> session mapping
    --json           emit JSON
    --tsv            emit tab-separated fields
    --limit <N>      how many sessions to list (default 20)
    -h, --help       show this help

ENVIRONMENT:
    CLAUDE_CONFIG_DIR         defaults to ~/.claude
    COCKPIT_SESSION_MAP_DIR   defaults to <config>/cockpit-sessions
";

#[derive(PartialEq)]
enum Format {
    Table,
    Json,
    Tsv,
}

fn main() {
    let mut format = Format::Table;
    let mut limit: usize = 20;
    let mut current = false;
    let mut panes = false;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => format = Format::Json,
            "--tsv" => format = Format::Tsv,
            "--current" => current = true,
            "--panes" => panes = true,
            "--limit" => {
                i += 1;
                match args.get(i).and_then(|v| v.parse::<usize>().ok()) {
                    Some(n) => limit = n,
                    None => fail("--limit needs a number"),
                }
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            other => fail(&format!("unknown option: {other}")),
        }
        i += 1;
    }

    if current {
        // Deliberately bare on stdout so `$(cc-sessions --current)` is the id
        // and nothing else.
        match current_session() {
            Some(session) => println!("{}", session.session_id),
            None => {
                eprintln!("cc-sessions: no session published for this pane");
                std::process::exit(1);
            }
        }
        return;
    }

    if panes {
        print_panes(&format);
        return;
    }

    let rows = index_sessions(&projects_root(), limit);
    match format {
        Format::Json => print_json(&rows),
        Format::Tsv => {
            for row in &rows {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    row.session_id, row.modified_epoch, row.cwd, row.project_dir, row.first_prompt
                );
            }
        }
        Format::Table => {
            for row in &rows {
                println!(
                    "{}  {:>8}  {}  {}",
                    row.session_id,
                    age(row.modified_epoch),
                    row.cwd,
                    row.first_prompt
                );
            }
        }
    }
}

fn print_panes(format: &Format) {
    let Some(pid) = tmux_server_pid() else {
        eprintln!("cc-sessions: not inside tmux");
        std::process::exit(1);
    };
    let map = read_pane_map(&pane_map_dir(), &pid);
    let mut entries: Vec<_> = map.into_values().collect();
    // Numeric pane order, so %2 precedes %10.
    entries.sort_by_key(|e| {
        e.tmux_pane
            .trim_start_matches('%')
            .parse::<u64>()
            .unwrap_or(u64::MAX)
    });

    if *format == Format::Json {
        let json = serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".into());
        println!("{json}");
        return;
    }
    for entry in entries {
        match format {
            Format::Tsv => println!(
                "{}\t{}\t{}",
                entry.tmux_pane, entry.session_id, entry.cwd
            ),
            _ => println!("{:>5}  {}  {}", entry.tmux_pane, entry.session_id, entry.cwd),
        }
    }
}

fn print_json(rows: &[SessionRow]) {
    match serde_json::to_string_pretty(rows) {
        Ok(json) => println!("{json}"),
        Err(err) => fail(&format!("could not serialise rows: {err}")),
    }
}

/// Coarse "how long ago", so the listing needs no date-formatting dependency.
fn age(epoch: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs = now.saturating_sub(epoch);
    match secs {
        s if s < 60 => "just now".to_string(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

fn fail(message: &str) -> ! {
    let _ = writeln!(std::io::stderr(), "cc-sessions: {message}");
    std::process::exit(2);
}

//! Cockpit team templates (P3 step 1) — the reusable, saveable **roster** (WHO)
//! and **workflow** (HOW) artifacts that native Agent Teams lacks.
//!
//! Native owns the live team RUNTIME (lead + teammate panes + file mailbox +
//! tasks under `~/.claude/teams/` & `~/.claude/tasks/`), driven by natural
//! language to the lead. What native has no concept of is a *saved, reusable*
//! team definition — you re-describe the team in prose every run. The cockpit
//! fills exactly that gap with two flat YAML artifacts, scoped like skills:
//!
//! * `~/.claude/cockpit/teams/<name>.yaml`      — ROSTER: roles → agent + mode + worktree
//! * `~/.claude/cockpit/workflows/<name>.yaml`  — WORKFLOW: declarative phases/gates the
//!                                                 LIVE lead reads and executes in its words
//! both mirrored at `<project>/.claude/cockpit/{teams,workflows}/*.yaml`.
//! (NOT `~/.claude/workflows/` — that is reserved for native JS workflow scripts.)
//!
//! A run = pair a roster with a workflow + a per-run task; the cockpit generates
//! the NL spin-up prompt from all three (step 2). This module is step 1: read,
//! parse, and validate those YAML files into typed rows for the frontend.
//!
//! ## Why a hand-rolled parser (no YAML dep)
//! The rest of the cockpit carries zero YAML dependency (SKILL.md frontmatter is
//! hand-parsed too). These files are cockpit-OWNED, so we define a tight, simple
//! block-YAML grammar and parse exactly that — no arbitrary-YAML surface. The
//! grammar (documented on `parse_yaml`): 2-space indent, no tabs; `key: value`
//! scalars (`true`/`false` → bool); nested block maps; `- ` block sequences of
//! maps; inline flow sequences `[a, b]`; folded `>` / literal `|` block scalars.
//!
//! ## Safety / robustness boundary (matches inventory.rs)
//! * Every file is read fault-tolerantly: a malformed file becomes one row with a
//!   `parse_error`, never a hard failure that blanks the whole panel.
//! * The reader core takes an INJECTABLE home/project root so unit tests run
//!   entirely against `$TMPDIR` fixtures — a test that touches the live
//!   `~/.claude` is a failing test by definition.
//! * Validation is NON-fatal: structural issues (missing agent, bad mode,
//!   unknown agent ref) land in a `problems[]` list, so a slightly-off template
//!   still lists (with a warning) instead of vanishing.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

// ── Public row types (serialized camelCase for the frontend) ──────────────────

/// One role line inside a roster: a name → which agent runs it, in which mode.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoleSpec {
    /// The role name (the map key, e.g. `dev`, `product-owner`). Workflows refer
    /// to roles by this name; spin-up validates the roster covers them.
    pub role: String,
    /// Which agent fills the role — a `~/.claude/agents/<agent>.md` name or a
    /// built-in agent type (e.g. `dev-agent`).
    pub agent: String,
    /// Optional per-role model override (e.g. `claude-opus-4-8`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Run this role's agent in its own git worktree (native `isolation: worktree`).
    pub worktree: bool,
    /// `"live"` (watchable tmux pane, default) or `"headless"` (in-process subagent).
    pub mode: String,
}

/// A reusable team roster (the WHO).
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Roster {
    /// Stable id: `"team:<scope>:<name>"`.
    pub id: String,
    /// `"global" | "project"`.
    pub scope: String,
    pub name: String,
    pub description: String,
    /// Absolute source path.
    pub path: String,
    pub roles: Vec<RoleSpec>,
    /// Optional default cwd for the team (`.` = the active tab's project).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_cwd: Option<String>,
    /// Non-fatal validation warnings (missing agent, bad mode, unknown agent ref).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub problems: Vec<String>,
    /// Set when the YAML itself couldn't be parsed — surfaced as a `!PARSE` badge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
}

/// One phase of a workflow: which role(s) act, whether they run in parallel, and
/// whether the lead must stop at a user gate after it.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PhaseSpec {
    pub id: String,
    /// Roles acting in this phase. A single `role: x` and a `roles: [a, b]` are
    /// both unified into this list. `lead` is allowed (the lead itself acts).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub roles: Vec<String>,
    /// Run the phase's roles concurrently.
    pub parallel: bool,
    /// `"user"` → the lead pauses and asks before continuing. `None` → no gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
}

/// A reusable declarative workflow (the HOW) — read and executed by the LIVE lead.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    /// Stable id: `"workflow:<scope>:<name>"`.
    pub id: String,
    pub scope: String,
    pub name: String,
    pub description: String,
    pub path: String,
    /// Free-prose orientation prepended to the spin-up prompt for the lead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lead_hint: Option<String>,
    pub phases: Vec<PhaseSpec>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub problems: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
}

/// Both template kinds, both scopes, in one payload for the panel.
#[derive(Clone, Debug, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CockpitTemplates {
    pub teams: Vec<Roster>,
    pub workflows: Vec<Workflow>,
}

// ── Entry points ──────────────────────────────────────────────────────────────

/// Resolve `$HOME` + the active project root, then load both template kinds.
pub fn load_cockpit_templates(project_path: Option<&str>) -> Result<CockpitTemplates, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME not set".to_string())?;
    let project = project_path.map(|p| resolve_project_root(Path::new(p)));
    Ok(load_templates_at(&home, project.as_deref()))
}

/// Walk up from `start` to the nearest ancestor that looks like a project root
/// (holds `.claude/`, `.git`, or `.mcp.json`). Falls back to `start`. Bounded to
/// 8 levels. (Mirrors `inventory::resolve_project_root`.)
fn resolve_project_root(start: &Path) -> PathBuf {
    let mut cur = start;
    for _ in 0..8 {
        if cur.join(".claude").is_dir() || cur.join(".git").exists() || cur.join(".mcp.json").is_file()
        {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(p) if p != cur => cur = p,
            _ => break,
        }
    }
    start.to_path_buf()
}

/// Testable core: read every `*.yaml` under the cockpit template dirs for the
/// injectable `home` (the `~`) + optional `project` root. Never fails as a whole.
pub fn load_templates_at(home: &Path, project: Option<&Path>) -> CockpitTemplates {
    let known = known_agents(home, project);
    let mut out = CockpitTemplates::default();

    let global = home.join(".claude").join("cockpit");
    read_rosters(&global.join("teams"), "global", &known, &mut out.teams);
    read_workflows(&global.join("workflows"), "global", &mut out.workflows);
    if let Some(p) = project {
        let proj = p.join(".claude").join("cockpit");
        read_rosters(&proj.join("teams"), "project", &known, &mut out.teams);
        read_workflows(&proj.join("workflows"), "project", &mut out.workflows);
    }
    out
}

/// Set of agent names that resolve to a real `<scope>/.claude/agents/<name>.md`
/// file (global + project). Used to flag a roster role whose agent is neither a
/// known file nor (assumed) a built-in — a soft warning, never a block.
fn known_agents(home: &Path, project: Option<&Path>) -> HashSet<String> {
    let mut set = HashSet::new();
    collect_agent_stems(&home.join(".claude").join("agents"), &mut set);
    if let Some(p) = project {
        collect_agent_stems(&p.join(".claude").join("agents"), &mut set);
    }
    set
}

fn collect_agent_stems(dir: &Path, set: &mut HashSet<String>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                set.insert(stem.to_string());
            }
        }
    }
}

/// `*.yaml`/`*.yml` files in `dir`, sorted by file name for stable ordering.
fn yaml_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("yaml") | Some("yml")
            )
        })
        .collect();
    files.sort();
    files
}

// ── Roster reader ─────────────────────────────────────────────────────────────

fn read_rosters(dir: &Path, scope: &str, known: &HashSet<String>, out: &mut Vec<Roster>) {
    for path in yaml_files(dir) {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let path_s = path.to_string_lossy().to_string();
        let mut r = Roster {
            id: format!("team:{scope}:{stem}"),
            scope: scope.to_string(),
            name: stem.clone(),
            description: String::new(),
            path: path_s,
            roles: Vec::new(),
            default_cwd: None,
            problems: Vec::new(),
            parse_error: None,
        };
        match fs::read_to_string(&path) {
            Err(e) => r.parse_error = Some(format!("read failed: {e}")),
            Ok(text) => match parse_yaml(&text) {
                Err(e) => r.parse_error = Some(format!("YAML parse error: {e}")),
                Ok(node) => build_roster(&node, scope, &stem, known, &mut r),
            },
        }
        out.push(r);
    }
}

fn build_roster(node: &Yaml, scope: &str, stem: &str, known: &HashSet<String>, r: &mut Roster) {
    let Some(map) = node.as_map() else {
        r.parse_error = Some("top level is not a mapping".into());
        return;
    };
    if let Some(name) = map_str(map, "name") {
        r.name = name;
        r.id = format!("team:{scope}:{}", r.name);
    }
    let _ = stem; // id falls back to file stem when `name:` is absent
    r.description = map_str(map, "description").unwrap_or_default();
    r.default_cwd = map_str(map, "default_cwd");

    match map.get("roles") {
        None => r.problems.push("no `roles:` block".into()),
        Some(roles_node) => match roles_node.as_map() {
            None => r.problems.push("`roles:` is not a mapping".into()),
            Some(roles) => {
                if roles.is_empty() {
                    r.problems.push("`roles:` is empty".into());
                }
                for (role_name, role_node) in roles {
                    let Some(rmap) = role_node.as_map() else {
                        r.problems.push(format!("role `{role_name}` is not a mapping"));
                        continue;
                    };
                    let agent = map_str(rmap, "agent").unwrap_or_default();
                    if agent.is_empty() {
                        r.problems.push(format!("role `{role_name}` has no `agent`"));
                    } else if !known.contains(&agent) {
                        r.problems.push(format!(
                            "role `{role_name}`: agent `{agent}` not found in .claude/agents (built-in?)"
                        ));
                    }
                    let mode = map_str(rmap, "mode").unwrap_or_else(|| "live".into());
                    if mode != "live" && mode != "headless" {
                        r.problems
                            .push(format!("role `{role_name}`: mode `{mode}` not live|headless"));
                    }
                    r.roles.push(RoleSpec {
                        role: role_name.clone(),
                        agent,
                        model: map_str(rmap, "model"),
                        worktree: map_bool(rmap, "worktree"),
                        mode,
                    });
                }
            }
        },
    }
}

// ── Workflow reader ───────────────────────────────────────────────────────────

fn read_workflows(dir: &Path, scope: &str, out: &mut Vec<Workflow>) {
    for path in yaml_files(dir) {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let path_s = path.to_string_lossy().to_string();
        let mut w = Workflow {
            id: format!("workflow:{scope}:{stem}"),
            scope: scope.to_string(),
            name: stem.clone(),
            description: String::new(),
            path: path_s,
            lead_hint: None,
            phases: Vec::new(),
            problems: Vec::new(),
            parse_error: None,
        };
        match fs::read_to_string(&path) {
            Err(e) => w.parse_error = Some(format!("read failed: {e}")),
            Ok(text) => match parse_yaml(&text) {
                Err(e) => w.parse_error = Some(format!("YAML parse error: {e}")),
                Ok(node) => build_workflow(&node, scope, &mut w),
            },
        }
        out.push(w);
    }
}

fn build_workflow(node: &Yaml, scope: &str, w: &mut Workflow) {
    let Some(map) = node.as_map() else {
        w.parse_error = Some("top level is not a mapping".into());
        return;
    };
    if let Some(name) = map_str(map, "name") {
        w.name = name;
        w.id = format!("workflow:{scope}:{}", w.name);
    }
    w.description = map_str(map, "description").unwrap_or_default();
    w.lead_hint = map_str(map, "lead_hint");

    match map.get("phases") {
        None => w.problems.push("no `phases:` block".into()),
        Some(phases_node) => match phases_node.as_seq() {
            None => w.problems.push("`phases:` is not a sequence".into()),
            Some(seq) => {
                if seq.is_empty() {
                    w.problems.push("`phases:` is empty".into());
                }
                for (i, item) in seq.iter().enumerate() {
                    let Some(pmap) = item.as_map() else {
                        w.problems.push(format!("phase #{} is not a mapping", i + 1));
                        continue;
                    };
                    let id = map_str(pmap, "id").unwrap_or_default();
                    if id.is_empty() {
                        w.problems.push(format!("phase #{} has no `id`", i + 1));
                    }
                    // Accept either `role: x` or `roles: [a, b]`; unify.
                    let mut roles = Vec::new();
                    if let Some(single) = map_str(pmap, "role") {
                        roles.push(single);
                    }
                    if let Some(list_node) = pmap.get("roles") {
                        match list_node.as_seq() {
                            Some(items) => {
                                for it in items {
                                    if let Some(s) = it.as_str() {
                                        roles.push(s.to_string());
                                    }
                                }
                            }
                            None => {
                                if let Some(s) = list_node.as_str() {
                                    roles.push(s.to_string());
                                }
                            }
                        }
                    }
                    if roles.is_empty() {
                        w.problems
                            .push(format!("phase `{}` names no role(s)", if id.is_empty() { "?" } else { &id }));
                    }
                    let gate = map_str(pmap, "gate");
                    if let Some(g) = &gate {
                        if g != "user" {
                            w.problems
                                .push(format!("phase `{id}`: gate `{g}` not `user`"));
                        }
                    }
                    w.phases.push(PhaseSpec {
                        id,
                        roles,
                        parallel: map_bool(pmap, "parallel"),
                        gate,
                    });
                }
            }
        },
    }
}

// ── Spin-up (P3 step 2): pair a roster + workflow + task → a lead prompt ──────

/// What the spin-up review dialog shows before launching: the generated lead
/// prompt + any role-coverage problems (block launch when non-empty).
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpinupPreview {
    pub roster_name: String,
    pub workflow_name: String,
    pub prompt: String,
    /// Roster does not cover these workflow roles. Non-empty ⇒ launch blocked.
    pub coverage_problems: Vec<String>,
}

/// Distinct roles a workflow names across its phases, excluding the implicit
/// `lead` (the lead is the orchestrator, not a roster role).
pub fn workflow_roles(w: &Workflow) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for ph in &w.phases {
        for role in &ph.roles {
            if role == "lead" {
                continue;
            }
            if !seen.iter().any(|s| s == role) {
                seen.push(role.clone());
            }
        }
    }
    seen
}

/// Every workflow role must map to a roster role. Returns a problem per missing
/// role (empty ⇒ the pairing is launchable).
pub fn validate_roster_covers_workflow(r: &Roster, w: &Workflow) -> Vec<String> {
    let have: HashSet<&str> = r.roles.iter().map(|x| x.role.as_str()).collect();
    workflow_roles(w)
        .into_iter()
        .filter(|role| !have.contains(role.as_str()))
        .map(|role| format!("workflow role `{role}` is not in roster `{}`", r.name))
        .collect()
}

/// Compose the single-line natural-language spin-up prompt the cockpit sends to
/// a freshly-launched `claude` lead. SINGLE LINE on purpose: `pane_send_keys`
/// delivers raw bytes, so an embedded newline would submit the input early —
/// every newline in `lead_hint`/`task` is flattened to a space.
pub fn generate_spinup_prompt(r: &Roster, w: &Workflow, task: &str) -> String {
    let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s = String::new();
    s.push_str(&format!("You are the team lead for the \"{}\" team. ", r.name));

    if let Some(h) = &w.lead_hint {
        let h = flat(h);
        if !h.is_empty() {
            s.push_str(&h);
            if !h.ends_with(['.', '!', '?']) {
                s.push('.');
            }
            s.push(' ');
        }
    }

    s.push_str("Team roster: ");
    let roles: Vec<String> = r
        .roles
        .iter()
        .map(|role| {
            let mut p = format!("{} = {}", role.role, role.agent);
            if let Some(m) = &role.model {
                p.push_str(&format!(" (model {m})"));
            }
            if role.worktree {
                p.push_str(" [own git worktree]");
            }
            if role.mode == "headless" {
                p.push_str(" [headless]");
            }
            p
        })
        .collect();
    s.push_str(&roles.join("; "));
    s.push_str(". ");

    s.push_str("Workflow phases: ");
    let phases: Vec<String> = w
        .phases
        .iter()
        .enumerate()
        .map(|(i, ph)| {
            let mut p = format!("{}) {} — {}", i + 1, ph.id, ph.roles.join(", "));
            if ph.parallel {
                p.push_str(" [in parallel]");
            }
            if ph.gate.as_deref() == Some("user") {
                p.push_str(" [STOP for my approval]");
            }
            p
        })
        .collect();
    s.push_str(&phases.join("; "));
    s.push_str(". ");

    s.push_str(&format!("Task: {}. ", flat(task)));
    s.push_str(
        "Spin up the live teammates now via the team feature, drive them through the phases \
         over the file mailbox, and STOP at each approval gate to ask me before continuing.",
    );
    s
}

/// Load templates for the project context, find the chosen roster + workflow by
/// id, validate coverage, and compose the prompt — everything the review dialog
/// needs in one call. Resolves `$HOME`; the testable core is `spinup_preview_at`.
pub fn spinup_preview(
    project_path: Option<&str>,
    roster_id: &str,
    workflow_id: &str,
    task: &str,
) -> Result<SpinupPreview, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME not set".to_string())?;
    let project = project_path.map(|p| resolve_project_root(Path::new(p)));
    spinup_preview_at(&home, project.as_deref(), roster_id, workflow_id, task)
}

/// Testable core of `spinup_preview` over an injectable home/project root.
pub fn spinup_preview_at(
    home: &Path,
    project: Option<&Path>,
    roster_id: &str,
    workflow_id: &str,
    task: &str,
) -> Result<SpinupPreview, String> {
    let t = load_templates_at(home, project);
    let r = t
        .teams
        .iter()
        .find(|x| x.id == roster_id)
        .ok_or_else(|| format!("roster `{roster_id}` not found"))?;
    let w = t
        .workflows
        .iter()
        .find(|x| x.id == workflow_id)
        .ok_or_else(|| format!("workflow `{workflow_id}` not found"))?;
    Ok(SpinupPreview {
        roster_name: r.name.clone(),
        workflow_name: w.name.clone(),
        prompt: generate_spinup_prompt(r, w, task),
        coverage_problems: validate_roster_covers_workflow(r, w),
    })
}

// ── Tiny constrained block-YAML parser (no external dep) ──────────────────────

/// Parsed node of our constrained YAML subset.
#[derive(Clone, Debug, PartialEq)]
enum Yaml {
    Scalar(String),
    Bool(bool),
    /// Insertion-ordered map (small; linear lookup is fine).
    Map(Vec<(String, Yaml)>),
    Seq(Vec<Yaml>),
}

impl Yaml {
    fn as_str(&self) -> Option<&str> {
        match self {
            Yaml::Scalar(s) => Some(s),
            // A bare `true`/`false` used where a string is wanted reads back as text.
            Yaml::Bool(b) => Some(if *b { "true" } else { "false" }),
            _ => None,
        }
    }
    fn as_bool(&self) -> Option<bool> {
        match self {
            Yaml::Bool(b) => Some(*b),
            _ => None,
        }
    }
    fn as_map(&self) -> Option<&Vec<(String, Yaml)>> {
        match self {
            Yaml::Map(m) => Some(m),
            _ => None,
        }
    }
    fn as_seq(&self) -> Option<&Vec<Yaml>> {
        match self {
            Yaml::Seq(s) => Some(s),
            _ => None,
        }
    }
}

fn map_get<'a>(map: &'a [(String, Yaml)], key: &str) -> Option<&'a Yaml> {
    map.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}
fn map_str(map: &[(String, Yaml)], key: &str) -> Option<String> {
    map_get(map, key).and_then(|v| v.as_str()).map(|s| s.to_string())
}
fn map_bool(map: &[(String, Yaml)], key: &str) -> bool {
    map_get(map, key).and_then(|v| v.as_bool()).unwrap_or(false)
}
// Convenience for the build_* code which holds `&Vec<(String, Yaml)>`.
trait MapLookup {
    fn get(&self, key: &str) -> Option<&Yaml>;
}
impl MapLookup for Vec<(String, Yaml)> {
    fn get(&self, key: &str) -> Option<&Yaml> {
        map_get(self, key)
    }
}

/// A source line with its indentation (in spaces) and trimmed content.
struct Line {
    indent: usize,
    content: String,
}

/// Parse our constrained block-YAML grammar into a `Yaml` tree.
///
/// Grammar (strict, cockpit-owned files only):
/// * 2-space indentation; a literal TAB anywhere in indentation is an error.
/// * Blank lines and `#` comments are ignored — full-line, and inline (a `#`
///   preceded by whitespace, outside quotes/flow-brackets; a literal `#` in a
///   value must be quoted or attached with no leading space). Block-scalar prose
///   keeps `#` literal.
/// * Mapping entry: `key: value` (scalar), `key:` (nested block follows), or
///   `key: >` / `key: |` (folded / literal block scalar on the following deeper
///   lines).
/// * Sequence entry: `- value` or `- key: value` (an inline map whose remaining
///   keys sit on the following deeper lines).
/// * Inline flow sequence: `[a, b, c]` (no nested flow maps).
/// * Scalars: surrounding matching quotes are stripped; bare `true`/`false`
///   (any case) become booleans.
fn parse_yaml(text: &str) -> Result<Yaml, String> {
    let mut lines: Vec<Line> = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        // Reject tabs used for indentation — ambiguous width, banned in our grammar.
        let leading = &raw[..raw.len() - raw.trim_start().len()];
        if leading.contains('\t') {
            return Err(format!("tab in indentation on line {}", n + 1));
        }
        let indent = leading.len();
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        lines.push(Line {
            indent,
            content: trimmed.to_string(),
        });
    }
    if lines.is_empty() {
        return Ok(Yaml::Map(Vec::new()));
    }
    let mut i = 0usize;
    let node = parse_block(&lines, &mut i, lines[0].indent)?;
    if i != lines.len() {
        return Err(format!(
            "unexpected indentation at line content `{}`",
            lines[i].content
        ));
    }
    Ok(node)
}

/// Parse a block (map or sequence) whose entries sit exactly at `indent`.
fn parse_block(lines: &[Line], i: &mut usize, indent: usize) -> Result<Yaml, String> {
    if lines[*i].content.starts_with("- ") || lines[*i].content == "-" {
        parse_seq(lines, i, indent)
    } else {
        parse_map(lines, i, indent)
    }
}

fn parse_map(lines: &[Line], i: &mut usize, indent: usize) -> Result<Yaml, String> {
    let mut entries: Vec<(String, Yaml)> = Vec::new();
    while *i < lines.len() && lines[*i].indent == indent {
        let content = strip_comment(&lines[*i].content);
        if content.starts_with("- ") || content == "-" {
            break; // a sequence at this indent is not a map entry
        }
        let colon = find_key_colon(content)
            .ok_or_else(|| format!("expected `key: value`, got `{content}`"))?;
        let key = content[..colon].trim().to_string();
        let rest = content[colon + 1..].trim().to_string();
        *i += 1;

        let value = if rest == ">" || rest == "|" || rest == ">-" || rest == "|-" {
            parse_block_scalar(lines, i, indent, rest.starts_with('|'))
        } else if rest.is_empty() {
            // Nested block on the following deeper lines (map or seq), else empty.
            if *i < lines.len() && lines[*i].indent > indent {
                let child_indent = lines[*i].indent;
                parse_block(lines, i, child_indent)?
            } else {
                Yaml::Scalar(String::new())
            }
        } else {
            parse_inline(&rest)
        };
        entries.push((key, value));
    }
    Ok(Yaml::Map(entries))
}

fn parse_seq(lines: &[Line], i: &mut usize, indent: usize) -> Result<Yaml, String> {
    let mut items: Vec<Yaml> = Vec::new();
    while *i < lines.len() && lines[*i].indent == indent {
        let content = strip_comment(&lines[*i].content).to_string();
        if !(content.starts_with("- ") || content == "-") {
            break;
        }
        let rest = if content == "-" {
            String::new()
        } else {
            content[2..].trim().to_string()
        };
        *i += 1;

        if rest.is_empty() {
            // Item is the nested block below.
            if *i < lines.len() && lines[*i].indent > indent {
                let child_indent = lines[*i].indent;
                items.push(parse_block(lines, i, child_indent)?);
            } else {
                items.push(Yaml::Scalar(String::new()));
            }
        } else if let Some(colon) = find_key_colon(&rest) {
            // `- key: value` → a map whose first entry is inline, remaining entries
            // are the following lines indented deeper than this `- ` marker.
            let key = rest[..colon].trim().to_string();
            let vraw = rest[colon + 1..].trim().to_string();
            let first_val = if vraw.is_empty() {
                if *i < lines.len() && lines[*i].indent > indent {
                    let ci = lines[*i].indent;
                    parse_block(lines, i, ci)?
                } else {
                    Yaml::Scalar(String::new())
                }
            } else {
                parse_inline(&vraw)
            };
            let mut entries = vec![(key, first_val)];
            // Continuation keys live deeper than the `- ` marker indent.
            if *i < lines.len() && lines[*i].indent > indent {
                let cont_indent = lines[*i].indent;
                if let Yaml::Map(more) = parse_map(lines, i, cont_indent)? {
                    entries.extend(more);
                }
            }
            items.push(Yaml::Map(entries));
        } else {
            // `- scalar`
            items.push(parse_inline(&rest));
        }
    }
    Ok(Yaml::Seq(items))
}

/// Collect a folded (`>`) or literal (`|`) block scalar: all following lines
/// indented deeper than the owning key. Folded → join with spaces; literal →
/// join with newlines. Common leading indent is stripped.
fn parse_block_scalar(lines: &[Line], i: &mut usize, key_indent: usize, literal: bool) -> Yaml {
    let mut collected: Vec<String> = Vec::new();
    let mut base: Option<usize> = None;
    while *i < lines.len() && lines[*i].indent > key_indent {
        if base.is_none() {
            base = Some(lines[*i].indent);
        }
        collected.push(lines[*i].content.clone());
        *i += 1;
    }
    let joined = if literal {
        collected.join("\n")
    } else {
        collected.join(" ")
    };
    Yaml::Scalar(joined.trim().to_string())
}

/// Parse an inline scalar / flow-sequence value (no nested flow maps).
fn parse_inline(raw: &str) -> Yaml {
    let s = raw.trim();
    if s.starts_with('[') && s.ends_with(']') {
        let inner = &s[1..s.len() - 1];
        let items = inner
            .split(',')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .map(|p| parse_scalar(p))
            .collect();
        return Yaml::Seq(items);
    }
    parse_scalar(s)
}

/// A single scalar token: strip matching quotes; bare true/false → bool.
fn parse_scalar(tok: &str) -> Yaml {
    let t = tok.trim();
    if (t.starts_with('"') && t.ends_with('"') && t.len() >= 2)
        || (t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2)
    {
        return Yaml::Scalar(t[1..t.len() - 1].to_string());
    }
    match t.to_ascii_lowercase().as_str() {
        "true" => Yaml::Bool(true),
        "false" => Yaml::Bool(false),
        _ => Yaml::Scalar(t.to_string()),
    }
}

/// Strip a trailing inline comment: a `#` that is preceded by whitespace (or at
/// line start) and sits outside quotes and `[ ]` flow brackets. A `#` with no
/// leading space (e.g. `a#b`, or `#tag` mid-token) is kept. Block-scalar prose
/// lines are NOT passed through here, so a literal `#` there survives.
fn strip_comment(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut quote: Option<u8> = None;
    let mut depth = 0i32;
    let mut prev_ws = true; // start-of-line counts as "preceded by whitespace"
    for (idx, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'[' => depth += 1,
                b']' => depth -= 1,
                b'#' if depth == 0 && prev_ws => return s[..idx].trim_end(),
                _ => {}
            },
        }
        prev_ws = b == b' ' || b == b'\t';
    }
    s.trim_end()
}

/// Find the `:` that separates a mapping key from its value: the first `: ` or a
/// trailing `:`. Avoids splitting on a `:` inside a `[...]` flow seq or a quoted
/// run. Returns the byte index of that colon.
fn find_key_colon(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    for (idx, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'[' => depth += 1,
                b']' => depth -= 1,
                b':' if depth == 0 => {
                    // `key: value` (colon followed by space) or trailing `key:`.
                    if idx + 1 == bytes.len() || bytes[idx + 1] == b' ' {
                        return Some(idx);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

// ── Tests (sandbox-only; never touch the real ~/.claude) ──────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct Sandbox {
        root: PathBuf,
    }
    impl Sandbox {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("cockpit-tmpl-test-{tag}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Sandbox { root }
        }
        fn home(&self) -> PathBuf {
            self.root.join("home")
        }
        fn write(&self, rel: &str, contents: &str) {
            let p = self.root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, contents).unwrap();
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    // ── parser unit tests ────────────────────────────────────────────────────

    #[test]
    fn parses_nested_map_bool_and_model() {
        let y = parse_yaml(
            "name: dev-team\nroles:\n  dev:\n    agent: dev-agent\n    model: claude-opus-4-8\n    worktree: true\n    mode: live\n",
        )
        .unwrap();
        let m = y.as_map().unwrap();
        assert_eq!(map_str(m, "name").as_deref(), Some("dev-team"));
        let roles = map_get(m, "roles").unwrap().as_map().unwrap();
        let dev = map_get(roles, "dev").unwrap().as_map().unwrap();
        assert_eq!(map_str(dev, "agent").as_deref(), Some("dev-agent"));
        assert_eq!(map_str(dev, "model").as_deref(), Some("claude-opus-4-8"));
        assert_eq!(map_bool(dev, "worktree"), true);
        assert_eq!(map_str(dev, "mode").as_deref(), Some("live"));
    }

    #[test]
    fn parses_seq_of_maps_and_flow_seq() {
        let y = parse_yaml(
            "phases:\n  - id: scope\n    role: product-owner\n    gate: user\n  - id: build\n    roles: [dev, frontend]\n    parallel: true\n",
        )
        .unwrap();
        let phases = y.as_map().unwrap();
        let seq = map_get(phases, "phases").unwrap().as_seq().unwrap();
        assert_eq!(seq.len(), 2);
        let p0 = seq[0].as_map().unwrap();
        assert_eq!(map_str(p0, "id").as_deref(), Some("scope"));
        assert_eq!(map_str(p0, "role").as_deref(), Some("product-owner"));
        assert_eq!(map_str(p0, "gate").as_deref(), Some("user"));
        let p1 = seq[1].as_map().unwrap();
        let roles = map_get(p1, "roles").unwrap().as_seq().unwrap();
        let names: Vec<&str> = roles.iter().filter_map(|r| r.as_str()).collect();
        assert_eq!(names, vec!["dev", "frontend"]);
        assert_eq!(map_bool(p1, "parallel"), true);
    }

    #[test]
    fn folded_block_scalar_joins_with_spaces() {
        let y = parse_yaml("lead_hint: >\n  You are the lead.\n  Drive the team.\nname: x\n").unwrap();
        let m = y.as_map().unwrap();
        assert_eq!(
            map_str(m, "lead_hint").as_deref(),
            Some("You are the lead. Drive the team.")
        );
        // sibling key after the block scalar is still parsed
        assert_eq!(map_str(m, "name").as_deref(), Some("x"));
    }

    #[test]
    fn tab_indentation_is_a_parse_error() {
        let err = parse_yaml("roles:\n\tdev:\n").unwrap_err();
        assert!(err.contains("tab"), "got: {err}");
    }

    #[test]
    fn inline_comments_stripped_but_literal_hash_kept() {
        let y = parse_yaml(
            "name: dev-team   # the team\nmode: live  # watchable\ntag: \"a # b\"\nslug: c#d\nroles: [dev, qa]   # two\n",
        )
        .unwrap();
        let m = y.as_map().unwrap();
        assert_eq!(map_str(m, "name").as_deref(), Some("dev-team"));
        assert_eq!(map_str(m, "mode").as_deref(), Some("live"));
        assert_eq!(map_str(m, "tag").as_deref(), Some("a # b")); // quoted # kept
        assert_eq!(map_str(m, "slug").as_deref(), Some("c#d")); // no leading space → kept
        let roles = map_get(m, "roles").unwrap().as_seq().unwrap();
        let names: Vec<&str> = roles.iter().filter_map(|r| r.as_str()).collect();
        assert_eq!(names, vec!["dev", "qa"]);
    }

    #[test]
    fn colon_inside_flow_seq_not_split() {
        // `roles: [a, b]` — the parser must split on the `roles:` colon only.
        assert_eq!(find_key_colon("roles: [a, b]"), Some(5));
    }

    // ── roster loader tests ──────────────────────────────────────────────────

    #[test]
    fn loads_roster_with_roles_and_resolves_known_agent() {
        let sb = Sandbox::new("roster");
        sb.write("home/.claude/agents/dev-agent.md", "---\nname: dev-agent\n---\n");
        sb.write(
            "home/.claude/cockpit/teams/dev-team.yaml",
            "name: dev-team\ndescription: Standard build team.\nroles:\n  dev:\n    agent: dev-agent\n    worktree: true\n    mode: live\n  qa:\n    agent: qa-agent\n    mode: headless\n",
        );
        let t = load_templates_at(&sb.home(), None);
        assert_eq!(t.teams.len(), 1);
        let r = &t.teams[0];
        assert_eq!(r.id, "team:global:dev-team");
        assert_eq!(r.name, "dev-team");
        assert_eq!(r.scope, "global");
        assert!(r.parse_error.is_none());
        assert_eq!(r.roles.len(), 2);
        let dev = r.roles.iter().find(|x| x.role == "dev").unwrap();
        assert_eq!(dev.agent, "dev-agent");
        assert!(dev.worktree);
        assert_eq!(dev.mode, "live");
        // dev-agent resolves (file present) → no "not found" problem for it.
        assert!(!r.problems.iter().any(|p| p.contains("dev-agent")));
        // qa-agent has no file → soft problem (built-in?), but still listed.
        assert!(r.problems.iter().any(|p| p.contains("qa-agent")));
    }

    #[test]
    fn roster_bad_mode_is_a_problem_not_a_drop() {
        let sb = Sandbox::new("badmode");
        sb.write(
            "home/.claude/cockpit/teams/t.yaml",
            "name: t\nroles:\n  x:\n    agent: dev-agent\n    mode: turbo\n",
        );
        let t = load_templates_at(&sb.home(), None);
        let r = &t.teams[0];
        assert!(r.parse_error.is_none(), "bad mode must not blank the row");
        assert_eq!(r.roles.len(), 1);
        assert!(r.problems.iter().any(|p| p.contains("turbo")));
    }

    #[test]
    fn malformed_yaml_becomes_parse_error_not_panic() {
        let sb = Sandbox::new("malformed");
        sb.write("home/.claude/cockpit/teams/bad.yaml", "roles:\n\tdev: x\n");
        let t = load_templates_at(&sb.home(), None);
        assert_eq!(t.teams.len(), 1);
        assert!(t.teams[0].parse_error.is_some());
        assert!(t.teams[0].parse_error.as_deref().unwrap().contains("tab"));
    }

    // ── workflow loader tests ────────────────────────────────────────────────

    #[test]
    fn loads_workflow_unifying_role_and_roles() {
        let sb = Sandbox::new("wf");
        sb.write(
            "home/.claude/cockpit/workflows/ship-it.yaml",
            "name: ship-it\ndescription: Scope build qa.\nlead_hint: >\n  Drive the team\n  through the phases.\nphases:\n  - id: scope\n    role: product-owner\n    gate: user\n  - id: build\n    roles: [dev, frontend]\n    parallel: true\n",
        );
        let t = load_templates_at(&sb.home(), None);
        assert_eq!(t.workflows.len(), 1);
        let w = &t.workflows[0];
        assert_eq!(w.id, "workflow:global:ship-it");
        assert!(w.parse_error.is_none());
        assert_eq!(w.lead_hint.as_deref(), Some("Drive the team through the phases."));
        assert_eq!(w.phases.len(), 2);
        assert_eq!(w.phases[0].roles, vec!["product-owner"]);
        assert_eq!(w.phases[0].gate.as_deref(), Some("user"));
        assert_eq!(w.phases[1].roles, vec!["dev", "frontend"]);
        assert!(w.phases[1].parallel);
        assert!(w.phases[1].gate.is_none());
    }

    #[test]
    fn workflow_phase_without_roles_is_a_problem() {
        let sb = Sandbox::new("wfbad");
        sb.write(
            "home/.claude/cockpit/workflows/w.yaml",
            "name: w\nphases:\n  - id: lonely\n",
        );
        let t = load_templates_at(&sb.home(), None);
        let w = &t.workflows[0];
        assert!(w.parse_error.is_none());
        assert_eq!(w.phases.len(), 1);
        assert!(w.problems.iter().any(|p| p.contains("lonely")));
    }

    // ── scope + isolation tests ──────────────────────────────────────────────

    #[test]
    fn project_scope_templates_load_alongside_global() {
        let sb = Sandbox::new("scope");
        sb.write("home/.claude/cockpit/teams/g.yaml", "name: g\nroles:\n  a:\n    agent: dev-agent\n");
        sb.write("proj/.claude/cockpit/teams/p.yaml", "name: p\nroles:\n  b:\n    agent: dev-agent\n");
        // mark proj as a project root
        fs::create_dir_all(sb.root.join("proj/.git")).unwrap();
        let t = load_templates_at(&sb.home(), Some(&sb.root.join("proj")));
        let scopes: Vec<(&str, &str)> =
            t.teams.iter().map(|r| (r.name.as_str(), r.scope.as_str())).collect();
        assert!(scopes.contains(&("g", "global")));
        assert!(scopes.contains(&("p", "project")));
    }

    #[test]
    fn empty_or_absent_dirs_yield_empty_not_error() {
        let sb = Sandbox::new("empty");
        let t = load_templates_at(&sb.home(), None);
        assert!(t.teams.is_empty());
        assert!(t.workflows.is_empty());
    }

    // ── spin-up (step 2) tests ───────────────────────────────────────────────

    fn fixture(sb: &Sandbox) {
        sb.write("home/.claude/agents/dev-agent.md", "---\nname: dev-agent\n---\n");
        sb.write(
            "home/.claude/cockpit/teams/dev-team.yaml",
            "name: dev-team\nroles:\n  product-owner:\n    agent: product-owner-agent\n  dev:\n    agent: dev-agent\n    worktree: true\n  qa:\n    agent: qa-agent\n    mode: headless\n",
        );
        sb.write(
            "home/.claude/cockpit/workflows/ship-it.yaml",
            "name: ship-it\nlead_hint: >\n  Drive the team\n  through the phases.\nphases:\n  - id: scope\n    role: product-owner\n    gate: user\n  - id: build\n    roles: [dev]\n    parallel: true\n  - id: integrate\n    role: lead\n",
        );
    }

    #[test]
    fn workflow_roles_excludes_lead_and_dedups() {
        let sb = Sandbox::new("wfroles");
        fixture(&sb);
        let t = load_templates_at(&sb.home(), None);
        let w = &t.workflows[0];
        // scope→product-owner, build→dev, integrate→lead(excluded)
        assert_eq!(workflow_roles(w), vec!["product-owner", "dev"]);
    }

    #[test]
    fn coverage_passes_when_roster_covers_all_roles() {
        let sb = Sandbox::new("cover-ok");
        fixture(&sb);
        let t = load_templates_at(&sb.home(), None);
        let problems = validate_roster_covers_workflow(&t.teams[0], &t.workflows[0]);
        assert!(problems.is_empty(), "dev-team covers ship-it: {problems:?}");
    }

    #[test]
    fn coverage_flags_missing_role() {
        let sb = Sandbox::new("cover-miss");
        fixture(&sb);
        // a workflow that needs a `frontend` role the roster lacks
        sb.write(
            "home/.claude/cockpit/workflows/needs-fe.yaml",
            "name: needs-fe\nphases:\n  - id: build\n    roles: [dev, frontend]\n",
        );
        let t = load_templates_at(&sb.home(), None);
        let wf = t.workflows.iter().find(|w| w.name == "needs-fe").unwrap();
        let problems = validate_roster_covers_workflow(&t.teams[0], wf);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("frontend"));
    }

    #[test]
    fn generated_prompt_is_single_line_and_contains_substance() {
        let sb = Sandbox::new("prompt");
        fixture(&sb);
        let t = load_templates_at(&sb.home(), None);
        let p = generate_spinup_prompt(&t.teams[0], &t.workflows[0], "ship the\nnew dashboard");
        assert!(!p.contains('\n'), "prompt must be single-line (no premature submit)");
        assert!(p.contains("dev-team"));
        assert!(p.contains("Drive the team through the phases.")); // folded hint flattened
        assert!(p.contains("dev = dev-agent [own git worktree]"));
        assert!(p.contains("qa = qa-agent [headless]"));
        assert!(p.contains("scope")); // phase ids present
        assert!(p.contains("[STOP for my approval]")); // user gate surfaced
        assert!(p.contains("[in parallel]"));
        assert!(p.contains("ship the new dashboard")); // task newline flattened
    }

    #[test]
    fn spinup_preview_end_to_end() {
        let sb = Sandbox::new("preview");
        fixture(&sb);
        // Injectable core — no global $HOME mutation (safe under parallel tests).
        let pv = spinup_preview_at(
            &sb.home(),
            None,
            "team:global:dev-team",
            "workflow:global:ship-it",
            "do it",
        )
        .expect("preview ok");
        assert_eq!(pv.roster_name, "dev-team");
        assert_eq!(pv.workflow_name, "ship-it");
        assert!(pv.coverage_problems.is_empty());
        assert!(pv.prompt.contains("Task: do it."));

        let missing = spinup_preview_at(
            &sb.home(),
            None,
            "team:global:nope",
            "workflow:global:ship-it",
            "x",
        );
        assert!(missing.is_err());
    }

    /// Real-target smoke: compose a spin-up against the live `~/.claude/cockpit`
    /// (seeded with the example dev-team + ship-it). `#[ignore]`; run with
    /// `cargo test --lib spinup_real -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn spinup_real_corpus() {
        let pv = spinup_preview(
            None,
            "team:global:dev-team",
            "workflow:global:ship-it",
            "smoke: add export-to-CSV",
        );
        match pv {
            Ok(p) => {
                eprintln!("roster={} workflow={}", p.roster_name, p.workflow_name);
                eprintln!("coverage_problems={:?}", p.coverage_problems);
                eprintln!("--- prompt ---\n{}\n--------------", p.prompt);
                assert!(p.coverage_problems.is_empty(), "dev-team should cover ship-it");
            }
            Err(e) => eprintln!("(no seeded templates: {e})"),
        }
    }

    /// The shipped reference examples must parse clean against the real parser —
    /// guards against example/grammar drift. Reads the actual repo files.
    #[test]
    fn shipped_example_templates_parse_clean() {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/cockpit-templates");
        let known: HashSet<String> = ["product-owner-agent", "dev-agent", "frontend-agent", "qa-agent"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let roster_txt = fs::read_to_string(format!("{base}/teams/dev-team.yaml")).unwrap();
        let mut roster = Roster {
            id: String::new(), scope: "global".into(), name: String::new(),
            description: String::new(), path: String::new(), roles: Vec::new(),
            default_cwd: None, problems: Vec::new(), parse_error: None,
        };
        let node = parse_yaml(&roster_txt).expect("example roster parses");
        build_roster(&node, "global", "dev-team", &known, &mut roster);
        assert!(roster.parse_error.is_none());
        assert!(roster.problems.is_empty(), "example roster clean: {:?}", roster.problems);
        assert_eq!(roster.roles.len(), 4);
        assert!(roster.roles.iter().any(|r| r.role == "dev" && r.worktree && r.mode == "live"));
        assert!(roster.roles.iter().any(|r| r.role == "qa" && r.mode == "headless"));

        let wf_txt = fs::read_to_string(format!("{base}/workflows/ship-it.yaml")).unwrap();
        let mut wf = Workflow {
            id: String::new(), scope: "global".into(), name: String::new(),
            description: String::new(), path: String::new(), lead_hint: None,
            phases: Vec::new(), problems: Vec::new(), parse_error: None,
        };
        let node = parse_yaml(&wf_txt).expect("example workflow parses");
        build_workflow(&node, "global", &mut wf);
        assert!(wf.parse_error.is_none());
        assert!(wf.problems.is_empty(), "example workflow clean: {:?}", wf.problems);
        assert_eq!(wf.name, "ship-it");
        assert_eq!(wf.phases.len(), 4);
        // The shipped example's lead_hint is the folded ">" block starting
        // "Drive the team through the phases…". (The old "You are the team lead."
        // assertion was stale — that string is what generate_spinup_prompt PREPENDS
        // to the generated prompt, not the example file's lead_hint.)
        assert!(wf.lead_hint.as_deref().unwrap().starts_with("Drive the team"));
        assert_eq!(wf.phases[1].roles, vec!["dev", "frontend"]);
        assert!(wf.phases[1].parallel);
    }
}

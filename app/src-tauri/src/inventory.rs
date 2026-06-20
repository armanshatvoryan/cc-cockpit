//! Inventory mission-control reads (P2-F1) — the unified, cross-scope browser of
//! the CC toolkit: skills, subagents, plugins, MCP servers, across the global
//! `~/.claude` scope and an optional per-project `.claude/` scope.
//!
//! This is the genuine whitespace native CC doesn't cover: `claude mcp` /
//! `claude plugin` manage ONE domain at a time, current-dir only — nothing shows
//! all four toolkit types in one panel, across projects. So we READ the real
//! config files directly here (fast, no process spawn, no network). WRITES are a
//! separate concern (P2-F2) that delegates to `claude plugin enable/disable` /
//! `claude mcp` so we never hand-patch the shared 120 KB `~/.claude.json`.
//!
//! ## Safety boundary (non-negotiable)
//! * `.env` files are NEVER opened.
//! * MCP `env` blocks carry secrets — we emit env *key names* only, NEVER values
//!   (and the browser detail line shows just the command summary).
//! * Every reader is fault-tolerant: a malformed file becomes an item with a
//!   `parse_error`, never a hard failure that blanks the whole inventory.
//! * The reader core takes an INJECTABLE home/project root so unit tests run
//!   entirely against `$TMPDIR` fixtures — a test that touches live `~/.claude`
//!   is a failing test by definition.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde::Serialize;

/// One row in the inventory browser. Serialized camelCase for the frontend; the
/// `type` field is renamed explicitly (Rust keyword).
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InventoryItem {
    /// Stable id: `"<type>:<scope>:<name>"` — used as the `<For>` key + toggle target.
    pub id: String,
    pub name: String,
    /// `"skill" | "subagent" | "plugin" | "mcp"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// `"global" | "project"`.
    pub scope: String,
    /// Plugins/MCP carry real on/off state; skills/subagents are always "on"
    /// (file-driven — present == active).
    pub enabled: bool,
    /// Whether a future toggle UI applies (plugins/MCP yes; skills/subagents no —
    /// "remove the file to disable"). Read-only this slice; informs the row.
    pub toggleable: bool,
    /// Frontmatter / manifest description, trimmed. Empty string if none.
    pub desc: String,
    /// Secondary line: marketplace for a plugin, command summary for an MCP
    /// server (NEVER env values), source filename otherwise. `None` to hide.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Absolute source path, for "View" later. `None` for derived (e.g. plugin).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Set when this file/entry couldn't be parsed — surfaced as a `!PARSE` badge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
}

impl InventoryItem {
    fn new(kind: &str, scope: &str, name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            id: format!("{kind}:{scope}:{name}"),
            name,
            kind: kind.to_string(),
            scope: scope.to_string(),
            enabled: true,
            toggleable: false,
            desc: String::new(),
            detail: None,
            path: None,
            parse_error: None,
        }
    }
}

/// Public entry: resolve `$HOME`, then read global + (optional) project scope.
/// `project_path` is the active tab's working directory; `None` → global only.
/// The pane cwd can be a deep subdir, so we walk up to the nearest project root
/// (a dir holding `.claude/`, `.git`, or `.mcp.json`) before reading.
pub fn load_inventory(project_path: Option<&str>) -> Result<Vec<InventoryItem>, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME not set".to_string())?;
    let project = project_path.map(|p| resolve_project_root(Path::new(p)));
    Ok(load_inventory_at(&home, project.as_deref()))
}

/// Walk up from `start` to the nearest ancestor that looks like a project root
/// (holds `.claude/`, `.git`, or `.mcp.json`). Falls back to `start` itself.
/// Bounded to 8 levels so a stray path can never loop toward `/`.
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

/// Testable core: read everything under an injectable `home` (the `~`) + optional
/// `project` root. Never fails as a whole — each reader swallows its own errors
/// into `parse_error` items, so a broken file degrades one row, not the panel.
pub fn load_inventory_at(home: &Path, project: Option<&Path>) -> Vec<InventoryItem> {
    let claude = home.join(".claude");
    let claude_json = home.join(".claude.json");
    let mut items = Vec::new();

    // ── Skills ──────────────────────────────────────────────────────────────
    read_skills(&claude.join("skills"), "global", &mut items);
    if let Some(p) = project {
        read_skills(&p.join(".claude").join("skills"), "project", &mut items);
    }

    // ── Subagents ───────────────────────────────────────────────────────────
    read_subagents(&claude.join("agents"), "global", &mut items);
    if let Some(p) = project {
        read_subagents(&p.join(".claude").join("agents"), "project", &mut items);
    }

    // ── Plugins (global enabledPlugins map) ─────────────────────────────────
    read_plugins(&claude, "global", &mut items);
    if let Some(p) = project {
        // A project may override enable-state in its own settings.json.
        read_plugins(&p.join(".claude"), "project", &mut items);
    }

    // ── MCP servers ─────────────────────────────────────────────────────────
    read_mcp(&claude_json, project, &mut items);
    if let Some(p) = project {
        read_mcp_dot_file(&p.join(".mcp.json"), &mut items);
    }

    items
}

// ── Skills ───────────────────────────────────────────────────────────────────

/// Each `<dir>/<skill>/SKILL.md` → one read-only item (name + frontmatter desc).
fn read_skills(dir: &Path, scope: &str, out: &mut Vec<InventoryItem>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let skill_md = entry.path().join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let mut item = InventoryItem::new("skill", scope, dir_name.clone());
        item.path = Some(skill_md.to_string_lossy().to_string());
        match fs::read_to_string(&skill_md) {
            Ok(text) => {
                let fm = parse_frontmatter(&text);
                if let Some(n) = fm_get(&fm, "name") {
                    item.name = n;
                    item.id = format!("skill:{scope}:{}", item.name);
                }
                item.desc = fm_get(&fm, "description").unwrap_or_default();
            }
            Err(e) => item.parse_error = Some(format!("read failed: {e}")),
        }
        out.push(item);
    }
}

// ── Subagents ─────────────────────────────────────────────────────────────────

/// Each `<dir>/<name>.md` (YAML frontmatter) → one read-only item.
fn read_subagents(dir: &Path, scope: &str, out: &mut Vec<InventoryItem>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let mut item = InventoryItem::new("subagent", scope, stem);
        item.path = Some(path.to_string_lossy().to_string());
        match fs::read_to_string(&path) {
            Ok(text) => {
                let fm = parse_frontmatter(&text);
                if let Some(n) = fm_get(&fm, "name") {
                    item.name = n;
                    item.id = format!("subagent:{scope}:{}", item.name);
                }
                item.desc = fm_get(&fm, "description").unwrap_or_default();
                // Surface the model as the detail line when present.
                item.detail = fm_get(&fm, "model");
            }
            Err(e) => item.parse_error = Some(format!("read failed: {e}")),
        }
        out.push(item);
    }
}

// ── Plugins ───────────────────────────────────────────────────────────────────

/// Read `<claude_dir>/settings.json:enabledPlugins{}` (flat `name@marketplace`
/// -> bool map). Description comes from the installed plugin's manifest
/// (`<installPath>/.claude-plugin/plugin.json`), looked up via the global
/// `plugins/installed_plugins.json`. Toggle state is the bool itself.
fn read_plugins(claude_dir: &Path, scope: &str, out: &mut Vec<InventoryItem>) {
    let settings = claude_dir.join("settings.json");
    let Ok(text) = fs::read_to_string(&settings) else { return };
    let root: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            // One synthetic error row so a corrupt settings.json is visible, not silent.
            let mut item = InventoryItem::new("plugin", scope, "settings.json");
            item.parse_error = Some(format!("invalid JSON: {e}"));
            item.path = Some(settings.to_string_lossy().to_string());
            out.push(item);
            return;
        }
    };
    let Some(map) = root.get("enabledPlugins").and_then(|v| v.as_object()) else {
        return;
    };
    // Manifest descriptions are resolved from the GLOBAL install registry
    // regardless of scope (installs live under ~/.claude/plugins).
    let manifests = load_plugin_manifests(claude_dir);
    for (key, val) in map {
        let enabled = val.as_bool().unwrap_or(false);
        // key is "name@marketplace"; show name, marketplace as detail.
        let (name, marketplace) = key.split_once('@').unwrap_or((key.as_str(), ""));
        let mut item = InventoryItem::new("plugin", scope, name);
        item.id = format!("plugin:{scope}:{key}"); // full key — toggle target later
        item.enabled = enabled;
        item.toggleable = true;
        item.detail = if marketplace.is_empty() {
            None
        } else {
            Some(marketplace.to_string())
        };
        item.desc = manifests.get(key.as_str()).cloned().unwrap_or_default();
        out.push(item);
    }
}

/// Map `name@marketplace` -> manifest description, from `installed_plugins.json`
/// (`{plugins: {"name@mkt": [{installPath, ...}]}}`) + each install's
/// `.claude-plugin/plugin.json`. Best-effort; missing manifest -> no description.
fn load_plugin_manifests(claude_dir: &Path) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let reg = claude_dir.join("plugins").join("installed_plugins.json");
    let Ok(text) = fs::read_to_string(&reg) else { return out };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else { return out };
    let Some(plugins) = root.get("plugins").and_then(|v| v.as_object()) else { return out };
    for (key, installs) in plugins {
        let Some(install) = installs.as_array().and_then(|a| a.first()) else { continue };
        let Some(path) = install.get("installPath").and_then(|v| v.as_str()) else { continue };
        let manifest = Path::new(path).join(".claude-plugin").join("plugin.json");
        if let Ok(mtext) = fs::read_to_string(&manifest) {
            if let Ok(mjson) = serde_json::from_str::<serde_json::Value>(&mtext) {
                if let Some(d) = mjson.get("description").and_then(|v| v.as_str()) {
                    out.insert(key.clone(), d.trim().to_string());
                }
            }
        }
    }
    out
}

// ── MCP servers ────────────────────────────────────────────────────────────────

/// Read MCP servers from `~/.claude.json`:
///   * top-level `mcpServers{}` → global scope
///   * `projects["<abs>"].mcpServers{}` + `.enabledMcpjsonServers[]` /
///     `.disabledMcpjsonServers[]` → project scope (only the active project)
/// SECURITY: never emit `env` values — detail = command summary only.
fn read_mcp(claude_json: &Path, project: Option<&Path>, out: &mut Vec<InventoryItem>) {
    let Ok(text) = fs::read_to_string(claude_json) else { return };
    let root: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            let mut item = InventoryItem::new("mcp", "global", ".claude.json");
            item.parse_error = Some(format!("invalid JSON: {e}"));
            out.push(item);
            return;
        }
    };

    // Global servers — always enabled (no per-server disable at the top level).
    if let Some(servers) = root.get("mcpServers").and_then(|v| v.as_object()) {
        for (name, spec) in servers {
            let mut item = InventoryItem::new("mcp", "global", name);
            item.enabled = !spec.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false);
            item.toggleable = true;
            item.detail = Some(mcp_command_summary(spec));
            out.push(item);
        }
    }

    // Project servers — keyed by the absolute project path.
    let Some(proj) = project else { return };
    let key = proj.to_string_lossy().to_string();
    let Some(pobj) = root.get("projects").and_then(|v| v.get(&key)) else { return };
    let disabled: std::collections::HashSet<String> = pobj
        .get("disabledMcpjsonServers")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if let Some(servers) = pobj.get("mcpServers").and_then(|v| v.as_object()) {
        for (name, spec) in servers {
            let mut item = InventoryItem::new("mcp", "project", name);
            let off = disabled.contains(name)
                || spec.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false);
            item.enabled = !off;
            item.toggleable = true;
            item.detail = Some(mcp_command_summary(spec));
            out.push(item);
        }
    }
}

/// Read a shareable project-root `.mcp.json` (`{mcpServers: {...}}`). These are
/// "pending approval" until accepted; we surface them read-only with a detail tag.
fn read_mcp_dot_file(path: &Path, out: &mut Vec<InventoryItem>) {
    let Ok(text) = fs::read_to_string(path) else { return };
    let root: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            let mut item = InventoryItem::new("mcp", "project", ".mcp.json");
            item.parse_error = Some(format!("invalid JSON: {e}"));
            item.path = Some(path.to_string_lossy().to_string());
            out.push(item);
            return;
        }
    };
    let Some(servers) = root.get("mcpServers").and_then(|v| v.as_object()) else { return };
    for (name, spec) in servers {
        let mut item = InventoryItem::new("mcp", "project", name);
        item.id = format!("mcp:project:.mcp.json:{name}");
        item.toggleable = false; // shareable file; managed by accept/reject, not a toggle
        item.path = Some(path.to_string_lossy().to_string());
        let cmd = mcp_command_summary(spec);
        item.detail = Some(format!("{cmd} · .mcp.json"));
        out.push(item);
    }
}

/// One-line command summary of an MCP server spec — `command arg0 arg1…` for
/// stdio, or the URL for http/sse. NEVER includes `env` values; if the spec has
/// an `env` block we append `+N env` (key COUNT only) so the user knows secrets
/// exist without exposing them.
fn mcp_command_summary(spec: &serde_json::Value) -> String {
    let mut summary = if let Some(url) = spec.get("url").and_then(|v| v.as_str()) {
        url.to_string()
    } else {
        let cmd = spec.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let args = spec
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        format!("{cmd} {args}").trim().to_string()
    };
    if let Some(env) = spec.get("env").and_then(|v| v.as_object()) {
        if !env.is_empty() {
            summary.push_str(&format!("  (+{} env)", env.len()));
        }
    }
    if summary.len() > 120 {
        summary.truncate(117);
        summary.push_str("…");
    }
    summary
}

// ── Writes (P2-F2) — DELEGATED to native `claude` subcommands ──────────────────
//
// We never hand-patch `~/.claude/settings.json` / `~/.claude.json`. Native CC owns
// its own config writes (atomic, concurrency-safe), so a plugin toggle shells out
// to `claude plugin enable|disable <key> --scope <user|project>`. That sidesteps
// the spec's #1 risk (write contention on the 120 KB shared `~/.claude.json`) and
// the whole .tmp/.bak/mtime-reassert safe-write suite — native is the writer.
//
// MCP toggle is intentionally NOT here: native MCP has no `disable` verb (only a
// destructive `remove`), so an in-app MCP toggle would lose the server's config.
// MCP rows stay read-only until there's a safe native primitive.
//
// SECURITY: the plugin key is config-derived (read from `enabledPlugins`), not
// user-typed, but we still (a) validate it against a strict charset and (b) exec
// `claude` with an argv array — never a shell string — so nothing can inject.

/// Resolve the absolute `claude` binary path via a login shell (the GUI process
/// doesn't inherit the nvm/zsh PATH that `claude` lives on). Cached on success.
fn resolve_claude_bin() -> Result<String, String> {
    static CACHE: OnceLock<String> = OnceLock::new();
    if let Some(p) = CACHE.get() {
        return Ok(p.clone());
    }
    let out = Command::new("zsh")
        .args(["-lc", "command -v claude"])
        .output()
        .map_err(|e| format!("could not resolve claude: {e}"))?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || path.is_empty() {
        return Err("claude not found on the login-shell PATH".into());
    }
    let _ = CACHE.set(path.clone());
    Ok(path)
}

/// Plugin keys are `name@marketplace`. Allow only safe identifier characters so a
/// config-derived value can never become an extra argv token or shell payload.
fn validate_plugin_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > 200 {
        return Err("invalid plugin key length".into());
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '-' | '_' | '.' | '/'))
    {
        return Err(format!("plugin key has illegal characters: {key}"));
    }
    Ok(())
}

/// Map our inventory scope to the native `--scope` flag value.
fn scope_flag(scope: &str) -> Result<&'static str, String> {
    match scope {
        "global" => Ok("user"),
        "project" => Ok("project"),
        other => Err(format!("unknown scope: {other}")),
    }
}

/// Parse a plugin item id (`plugin:<scope>:<name@marketplace>`) into its parts.
fn parse_plugin_id(id: &str) -> Result<(String, String), String> {
    let mut parts = id.splitn(3, ':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("plugin"), Some(scope), Some(key)) if !scope.is_empty() && !key.is_empty() => {
            Ok((scope.to_string(), key.to_string()))
        }
        _ => Err(format!("not a plugin item id: {id}")),
    }
}

/// Build the exact argv for a plugin toggle. Pure + validated, so it is unit
/// tested without ever executing `claude`.
fn plugin_toggle_argv(id: &str, enable: bool) -> Result<Vec<String>, String> {
    let (scope, key) = parse_plugin_id(id)?;
    validate_plugin_key(&key)?;
    let flag = scope_flag(&scope)?;
    let verb = if enable { "enable" } else { "disable" };
    Ok(vec![
        "plugin".into(),
        verb.into(),
        key,
        "--scope".into(),
        flag.into(),
    ])
}

/// Toggle a plugin on/off by delegating to `claude plugin enable|disable`. The
/// caller (a Tauri command behind a confirm modal) passes the inventory item id;
/// on success the frontend re-reads the inventory so the row reflects disk truth.
pub fn toggle_plugin(id: &str, enable: bool) -> Result<(), String> {
    let argv = plugin_toggle_argv(id, enable)?;
    let bin = resolve_claude_bin()?;
    let out = Command::new(&bin)
        .args(&argv)
        .output()
        .map_err(|e| format!("running claude plugin failed: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let detail = if !stderr.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };
    Err(format!(
        "claude plugin {} failed: {detail}",
        if enable { "enable" } else { "disable" }
    ))
}

/// The human-readable command a confirm modal shows before running a toggle.
/// (Display only — the real exec uses the validated argv, never this string.)
pub fn plugin_toggle_preview(id: &str, enable: bool) -> Result<String, String> {
    let argv = plugin_toggle_argv(id, enable)?;
    Ok(format!("claude {}", argv.join(" ")))
}

// ── Audit matrix (P2-F5) — cross-project, read-only ────────────────────────────
//
// One grid showing the EFFECTIVE on/off of every plugin + MCP server across the
// open tabs' projects. Native CC can't show this — `claude plugin`/`claude mcp`
// list one project (the cwd) at a time. Columns = distinct project roots of the
// open tabs; rows = the union of plugins + MCP servers; cells = on/off/absent/
// error. Effective state per project = the project override if present, else the
// global value. Pure read (reuses `load_inventory_at` per project) → unit tested.

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuditColumn {
    /// Project dir basename, for the column header.
    pub label: String,
    /// Absolute project root (tooltip + key).
    pub project_path: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuditRow {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// One state per column (aligned): `"on" | "off" | "absent" | "error"`.
    pub cells: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuditMatrix {
    pub columns: Vec<AuditColumn>,
    pub rows: Vec<AuditRow>,
}

/// Public entry: resolve `$HOME`, dedupe the tabs' project roots (preserving
/// order), and compute the matrix. Empty `project_paths` → an empty matrix.
pub fn load_audit_matrix(project_paths: Vec<String>) -> Result<AuditMatrix, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME not set".to_string())?;
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for p in &project_paths {
        let root = resolve_project_root(Path::new(p));
        if seen.insert(root.to_string_lossy().to_string()) {
            roots.push(root);
        }
    }
    Ok(compute_audit(&home, &roots))
}

/// State letter for one inventory item.
fn cell_of(item: &InventoryItem) -> &'static str {
    if item.parse_error.is_some() {
        "error"
    } else if item.enabled {
        "on"
    } else {
        "off"
    }
}

/// Per-column lookup tables (global vs project, per type) + parse-broken flags.
struct ColIndex {
    plugin_global: HashMap<String, &'static str>,
    plugin_project: HashMap<String, &'static str>,
    mcp_global: HashMap<String, &'static str>,
    mcp_project: HashMap<String, &'static str>,
    plugin_broken: bool,
    mcp_broken: bool,
}

/// Build the matrix from each project root's inventory. Testable core.
pub fn compute_audit(home: &Path, roots: &[PathBuf]) -> AuditMatrix {
    let columns: Vec<AuditColumn> = roots
        .iter()
        .map(|r| AuditColumn {
            label: r
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| r.to_string_lossy().to_string()),
            project_path: r.to_string_lossy().to_string(),
        })
        .collect();

    // Row universe (sorted, deduped) discovered while indexing the columns.
    let mut plugin_rows: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();
    let mut mcp_rows: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut cols: Vec<ColIndex> = Vec::with_capacity(roots.len());

    for root in roots {
        let inv = load_inventory_at(home, Some(root.as_path()));
        let mut c = ColIndex {
            plugin_global: HashMap::new(),
            plugin_project: HashMap::new(),
            mcp_global: HashMap::new(),
            mcp_project: HashMap::new(),
            plugin_broken: false,
            mcp_broken: false,
        };
        for it in &inv {
            match it.kind.as_str() {
                "plugin" => {
                    let Ok((_scope, key)) = parse_plugin_id(&it.id) else { continue };
                    if !key.contains('@') {
                        // Synthetic settings.json parse-error row — not a real plugin.
                        if it.parse_error.is_some() {
                            c.plugin_broken = true;
                        }
                        continue;
                    }
                    let (name, mkt) = key
                        .split_once('@')
                        .map(|(n, m)| (n.to_string(), Some(m.to_string())))
                        .unwrap_or((key.clone(), None));
                    plugin_rows.entry(key.clone()).or_insert((name, mkt));
                    let st = cell_of(it);
                    if it.scope == "project" {
                        c.plugin_project.insert(key, st);
                    } else {
                        c.plugin_global.insert(key, st);
                    }
                }
                "mcp" => {
                    if it.name.ends_with(".json") {
                        if it.parse_error.is_some() {
                            c.mcp_broken = true;
                        }
                        continue;
                    }
                    mcp_rows.entry(it.name.clone()).or_insert_with(|| it.detail.clone());
                    let st = cell_of(it);
                    if it.scope == "project" {
                        c.mcp_project.insert(it.name.clone(), st);
                    } else {
                        c.mcp_global.insert(it.name.clone(), st);
                    }
                }
                _ => {}
            }
        }
        cols.push(c);
    }

    let mut rows: Vec<AuditRow> = Vec::new();
    for (key, (name, mkt)) in &plugin_rows {
        let cells = cols
            .iter()
            .map(|c| {
                c.plugin_project
                    .get(key)
                    .or_else(|| c.plugin_global.get(key))
                    .copied()
                    .unwrap_or(if c.plugin_broken { "error" } else { "absent" })
                    .to_string()
            })
            .collect();
        rows.push(AuditRow {
            id: format!("plugin:{key}"),
            name: name.clone(),
            kind: "plugin".into(),
            detail: mkt.clone(),
            cells,
        });
    }
    for (name, detail) in &mcp_rows {
        let cells = cols
            .iter()
            .map(|c| {
                c.mcp_project
                    .get(name)
                    .or_else(|| c.mcp_global.get(name))
                    .copied()
                    .unwrap_or(if c.mcp_broken { "error" } else { "absent" })
                    .to_string()
            })
            .collect();
        rows.push(AuditRow {
            id: format!("mcp:{name}"),
            name: name.clone(),
            kind: "mcp".into(),
            detail: detail.clone(),
            cells,
        });
    }

    AuditMatrix { columns, rows }
}

// ── Minimal YAML frontmatter parser (no dep) ────────────────────────────────────

/// Parse the leading `---\n … \n---` block into `(key, value)` pairs. Handles
/// the two shapes we need: `key: value` and block scalars
/// (`key: >-` / `>` / `|` followed by indented continuation lines, folded into
/// one space-joined string). Returns an empty Vec if there's no frontmatter.
/// Intentionally tiny: we only read `name` + `description` + `model`.
fn parse_frontmatter(text: &str) -> Vec<(String, String)> {
    let mut lines = text.lines();
    // Frontmatter must be the very first line.
    if lines.next().map(str::trim) != Some("---") {
        return Vec::new();
    }
    let mut body: Vec<&str> = Vec::new();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        body.push(line);
    }

    let mut out: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let line = body[i];
        // Only treat top-level (unindented) `key:` lines as fields.
        if line.starts_with(|c: char| c.is_whitespace()) || !line.contains(':') {
            i += 1;
            continue;
        }
        let (key, rest) = line.split_once(':').unwrap();
        let key = key.trim().to_string();
        let rest = rest.trim();
        if rest.starts_with('>') || rest.starts_with('|') {
            // Block scalar: gather more-indented following lines, fold to spaces.
            let mut parts: Vec<String> = Vec::new();
            i += 1;
            while i < body.len() {
                let l = body[i];
                if l.trim().is_empty() {
                    i += 1;
                    continue;
                }
                if l.starts_with(|c: char| c.is_whitespace()) {
                    parts.push(l.trim().to_string());
                    i += 1;
                } else {
                    break;
                }
            }
            out.push((key, parts.join(" ").trim().to_string()));
        } else {
            out.push((key, unquote(rest).to_string()));
            i += 1;
        }
    }
    out
}

fn fm_get(fm: &[(String, String)], key: &str) -> Option<String> {
    fm.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty())
}

/// Strip a single pair of surrounding quotes (YAML scalar) if present.
fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Build a throwaway home/project tree under a unique temp dir. Sandbox-only:
    /// these tests NEVER touch the real `~/.claude`.
    struct Sandbox {
        root: PathBuf,
    }
    impl Sandbox {
        fn new(tag: &str) -> Self {
            // Deterministic-ish unique dir without Date/rand (both banned in some
            // contexts): tag + process id + a static counter.
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "cockpit-inv-test-{tag}-{}-{n}",
                std::process::id()
            ));
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

    fn find<'a>(items: &'a [InventoryItem], kind: &str, name: &str) -> Option<&'a InventoryItem> {
        items.iter().find(|i| i.kind == kind && i.name == name)
    }

    #[test]
    fn skill_frontmatter_name_and_folded_desc() {
        let sb = Sandbox::new("skill");
        sb.write(
            "home/.claude/skills/brief/SKILL.md",
            "---\nname: brief\ndescription: >-\n  Generate a polished\n  business brief PDF.\n---\n\nbody",
        );
        let items = load_inventory_at(&sb.home(), None);
        let it = find(&items, "skill", "brief").expect("skill present");
        assert_eq!(it.scope, "global");
        assert_eq!(it.desc, "Generate a polished business brief PDF.");
        assert!(!it.toggleable, "skills are read-only");
        assert!(it.enabled, "present skill = enabled");
        assert!(it.parse_error.is_none());
    }

    #[test]
    fn subagent_reads_name_desc_model() {
        let sb = Sandbox::new("agent");
        sb.write(
            "home/.claude/agents/dev-agent.md",
            "---\nname: dev-agent\ndescription: Backend engineer.\nmodel: claude-opus-4-8\n---\n",
        );
        let items = load_inventory_at(&sb.home(), None);
        let it = find(&items, "subagent", "dev-agent").expect("subagent present");
        assert_eq!(it.desc, "Backend engineer.");
        assert_eq!(it.detail.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn plugin_enabled_disabled_bool_flip() {
        let sb = Sandbox::new("plugin");
        sb.write(
            "home/.claude/settings.json",
            r#"{"enabledPlugins":{"caveman@caveman":true,"swift-lsp@official":false}}"#,
        );
        let items = load_inventory_at(&sb.home(), None);
        let on = find(&items, "plugin", "caveman").expect("caveman present");
        assert!(on.enabled);
        assert!(on.toggleable);
        assert_eq!(on.detail.as_deref(), Some("caveman"));
        let off = find(&items, "plugin", "swift-lsp").expect("swift-lsp present");
        assert!(!off.enabled, "false in map = disabled");
    }

    #[test]
    fn mcp_global_and_project_scopes_with_disable_array() {
        let sb = Sandbox::new("mcp");
        let proj = sb.root.join("proj");
        let proj_key = proj.to_string_lossy().to_string();
        sb.write(
            "home/.claude.json",
            &format!(
                r#"{{
                  "mcpServers": {{ "sentry": {{ "url": "https://mcp.sentry.dev/mcp" }} }},
                  "projects": {{ "{proj_key}": {{
                    "mcpServers": {{
                      "fs": {{ "command": "npx", "args": ["fs-mcp","--root","."] }},
                      "db": {{ "command": "db-mcp" }}
                    }},
                    "disabledMcpjsonServers": ["db"]
                  }} }}
                }}"#
            ),
        );
        let items = load_inventory_at(&sb.home(), Some(&proj));
        let g = find(&items, "mcp", "sentry").expect("global mcp");
        assert_eq!(g.scope, "global");
        assert_eq!(g.detail.as_deref(), Some("https://mcp.sentry.dev/mcp"));
        let fs_srv = find(&items, "mcp", "fs").expect("project mcp fs");
        assert_eq!(fs_srv.scope, "project");
        assert!(fs_srv.enabled);
        assert_eq!(fs_srv.detail.as_deref(), Some("npx fs-mcp --root ."));
        let db = find(&items, "mcp", "db").expect("project mcp db");
        assert!(!db.enabled, "in disabledMcpjsonServers");
    }

    #[test]
    fn mcp_env_values_never_emitted() {
        let sb = Sandbox::new("secrets");
        sb.write(
            "home/.claude.json",
            r#"{"mcpServers":{"sec":{"command":"x","env":{"API_KEY":"super-secret-token-value","FOO":"barbar"}}}}"#,
        );
        let items = load_inventory_at(&sb.home(), None);
        // The secret string must appear NOWHERE in the serialized inventory.
        let blob = serde_json::to_string(&items).unwrap();
        assert!(!blob.contains("super-secret-token-value"), "secret leaked!");
        assert!(!blob.contains("barbar"), "secret leaked!");
        let sec = find(&items, "mcp", "sec").expect("mcp present");
        // Detail discloses that 2 env keys exist, by count only.
        assert!(sec.detail.as_deref().unwrap().contains("+2 env"));
    }

    #[test]
    fn malformed_settings_becomes_parse_error_not_panic() {
        let sb = Sandbox::new("badjson");
        sb.write("home/.claude/settings.json", "{ this is not json ");
        let items = load_inventory_at(&sb.home(), None);
        let err = items
            .iter()
            .find(|i| i.kind == "plugin" && i.parse_error.is_some())
            .expect("synthetic parse-error row");
        assert!(err.parse_error.as_deref().unwrap().contains("invalid JSON"));
    }

    #[test]
    fn empty_home_yields_empty_not_error() {
        let sb = Sandbox::new("empty");
        // home/.claude doesn't even exist.
        let items = load_inventory_at(&sb.home(), None);
        assert!(items.is_empty());
    }

    /// Read-only sanity against the developer's REAL `~/.claude` (never writes).
    /// `#[ignore]` so CI/the normal suite don't depend on a particular machine;
    /// run on demand: `cargo test --lib real_corpus_sanity -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_corpus_sanity() {
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let items = load_inventory_at(&home, None);
        let count = |k: &str| items.iter().filter(|i| i.kind == k).count();
        eprintln!(
            "real corpus: {} skills, {} subagents, {} plugins, {} mcp",
            count("skill"),
            count("subagent"),
            count("plugin"),
            count("mcp"),
        );
        assert!(count("skill") >= 5, "expected real skills");
        assert!(count("plugin") >= 1, "expected real plugins");
        // No empty descriptions panic; no env values leaked machine-wide.
        let blob = serde_json::to_string(&items).unwrap();
        assert!(!blob.contains("sk-ant"), "an api-key-looking string leaked");
    }

    // ── P2-F2 toggle: argv construction + validation (no exec) ──────────────

    #[test]
    fn plugin_toggle_argv_maps_scope_and_verb() {
        let on = plugin_toggle_argv("plugin:global:caveman@caveman", true).unwrap();
        assert_eq!(
            on,
            vec!["plugin", "enable", "caveman@caveman", "--scope", "user"]
        );
        let off = plugin_toggle_argv("plugin:project:foo@bar", false).unwrap();
        assert_eq!(off, vec!["plugin", "disable", "foo@bar", "--scope", "project"]);
    }

    #[test]
    fn plugin_toggle_preview_is_human_readable() {
        let p = plugin_toggle_preview("plugin:global:caveman@caveman", false).unwrap();
        assert_eq!(p, "claude plugin disable caveman@caveman --scope user");
    }

    // ── P2-F5 audit matrix ──────────────────────────────────────────────────

    #[test]
    fn audit_matrix_effective_state_across_projects() {
        let sb = Sandbox::new("audit");
        let proj_a = sb.root.join("projA");
        let proj_b = sb.root.join("projB");
        let b_key = proj_b.to_string_lossy().to_string();
        // Global: caveman ON.
        sb.write(
            "home/.claude/settings.json",
            r#"{"enabledPlugins":{"caveman@caveman":true}}"#,
        );
        // projA overrides caveman OFF (project scope); projB has an MCP server.
        sb.write(
            "projA/.claude/settings.json",
            r#"{"enabledPlugins":{"caveman@caveman":false}}"#,
        );
        sb.write(
            "home/.claude.json",
            &format!(
                r#"{{ "projects": {{ "{b_key}": {{ "mcpServers": {{ "db": {{ "command": "db" }} }} }} }} }}"#
            ),
        );

        let m = compute_audit(&sb.home(), &[proj_a.clone(), proj_b.clone()]);
        assert_eq!(m.columns.len(), 2);
        assert_eq!(m.columns[0].label, "projA");
        assert_eq!(m.columns[1].label, "projB");

        let plugin = m.rows.iter().find(|r| r.name == "caveman").expect("plugin row");
        // projA = project override OFF; projB = global ON.
        assert_eq!(plugin.cells, vec!["off", "on"]);

        let mcp = m.rows.iter().find(|r| r.name == "db").expect("mcp row");
        // db only exists in projB.
        assert_eq!(mcp.cells, vec!["absent", "on"]);
    }

    /// Read-only audit over two REAL project roots. `#[ignore]`; run on demand.
    #[test]
    #[ignore]
    fn real_audit_sanity() {
        let m = load_audit_matrix(vec![
            "/Users/armanshatvoran/Workflows".into(),
            "/Users/armanshatvoran/Workflows/cc-cockpit".into(),
        ])
        .unwrap();
        eprintln!(
            "audit: {} columns ({:?}), {} rows",
            m.columns.len(),
            m.columns.iter().map(|c| &c.label).collect::<Vec<_>>(),
            m.rows.len(),
        );
        for r in m.rows.iter().take(4) {
            eprintln!("  {} {} -> {:?}", r.kind, r.name, r.cells);
        }
        assert!(!m.columns.is_empty());
        assert!(m.rows.iter().all(|r| r.cells.len() == m.columns.len()));
    }

    #[test]
    fn audit_matrix_empty_when_no_projects() {
        let sb = Sandbox::new("audit-empty");
        sb.write("home/.claude/settings.json", r#"{"enabledPlugins":{}}"#);
        let m = compute_audit(&sb.home(), &[]);
        assert!(m.columns.is_empty());
        assert!(m.rows.is_empty());
    }

    #[test]
    fn plugin_toggle_rejects_injection_and_bad_ids() {
        // Shell metacharacters in the key are refused before any exec.
        assert!(plugin_toggle_argv("plugin:global:foo; rm -rf ~", true).is_err());
        assert!(plugin_toggle_argv("plugin:global:foo && bar", true).is_err());
        assert!(plugin_toggle_argv("plugin:global:foo`whoami`", true).is_err());
        assert!(plugin_toggle_argv("plugin:global:foo$(id)", true).is_err());
        // Not a plugin id / unknown scope.
        assert!(plugin_toggle_argv("skill:global:brief", true).is_err());
        assert!(plugin_toggle_argv("plugin:weird:foo@bar", true).is_err());
        assert!(plugin_toggle_argv("plugin:global:", true).is_err());
    }

    #[test]
    fn project_dot_mcp_file_is_read_only_pending() {
        let sb = Sandbox::new("dotmcp");
        let proj = sb.root.join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join(".mcp.json"),
            r#"{"mcpServers":{"shared":{"command":"shared-mcp"}}}"#,
        )
        .unwrap();
        sb.write("home/.claude.json", "{}");
        let items = load_inventory_at(&sb.home(), Some(&proj));
        let it = find(&items, "mcp", "shared").expect("shared mcp from .mcp.json");
        assert!(!it.toggleable, ".mcp.json servers managed by accept/reject");
        assert!(it.detail.as_deref().unwrap().contains(".mcp.json"));
    }
}

//! Token-burn meter (Wave C / C1) — a LOCAL scan of Claude Code's own transcript
//! files, aggregated into rolling 5h + 7d windows.
//!
//! ## What this is (and is not)
//! This is a **burn meter, not a limit meter**. v1 reads only what is already on
//! disk (`~/.claude/projects/*/*.jsonl`): no Keychain, no network, no OAuth, and
//! deliberately no "% of your plan" number — that would require the account's
//! real limits, which we cannot know locally without guessing.
//!
//! ## Why a cursor, not a re-read
//! The real corpus is ~2,120 files / 1.6 GB, of which ~461 files / 432 MB were
//! touched in the last 7 days. Re-reading that every 45s would be absurd, so the
//! scanner is incremental by construction:
//!   * files whose mtime is older than the window (+1h slack) are never opened;
//!   * each file carries a `(mtime, len, offset)` cursor — an unchanged
//!     `(mtime, len)` pair means "not one byte read this pass";
//!   * otherwise we read `offset..len` only (append-only tail), and a shrunken
//!     `len` (truncation / rotation) resets the cursor to a full rescan;
//!   * a tail that ends mid-line stops at the last `\n`, so a partially-flushed
//!     record is picked up whole on the next pass instead of being parsed broken.
//! Only the FIRST pass pays for the 432 MB; steady state reads the few KB that
//! agents actually appended. That first pass runs on the poller thread, never on
//! a UI/IPC path.
//!
//! ## Why records are retained
//! The windows are *rolling*, so a tail-only read is not enough on its own — old
//! records must age out. The scanner therefore keeps a de-duplicated record list
//! (one entry per assistant message: timestamp, model, token counts) and
//! recomputes the windows from it each pass, pruning anything past 7d.
//!
//! ## Fault tolerance (same contract as `inventory.rs`)
//! Every layer degrades instead of failing: an unreadable dir, an unreadable
//! file, a malformed line, a record missing `message.usage` — each is skipped,
//! never propagated. A broken transcript costs you one message in the total, not
//! the whole meter. The scan root is INJECTABLE so the tests run entirely against
//! `$TMPDIR` fixtures; a test that touches the live `~/.claude` is a failing test
//! by definition — the sole exception is the `#[ignore]`d `real_corpus_sanity`
//! opt-in, which is never part of a default `cargo test` run.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Rolling short window: 5 hours (Claude Code's own session window).
const FIVE_HOUR_MS: i64 = 5 * 60 * 60 * 1000;
/// Rolling long window: 7 days.
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Velocity is measured over the trailing 30 minutes.
const VELOCITY_MS: i64 = 30 * 60 * 1000;
/// Velocity denominator, in minutes.
const VELOCITY_MINUTES: f64 = 30.0;
/// A file whose mtime is older than the long window (+1h slack for clock skew /
/// timestamps written slightly before the flush) cannot hold an in-window record,
/// so it is never opened.
const MTIME_CUTOFF_MS: i64 = WEEK_MS + 60 * 60 * 1000;

// ── DTOs (camelCase — the C-2 frontend consumes these exact shapes) ───────────

/// Per-model split inside a window, sorted by `total_tokens` desc (ties by name).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    /// Raw model id as written by Claude Code, e.g. `claude-opus-5`.
    pub model: String,
    pub total_tokens: u64,
    pub messages: u64,
}

/// One rolling window's aggregate.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    /// `input + output + cache_creation + cache_read`.
    pub total_tokens: u64,
    pub output_tokens: u64,
    /// Non-cached input only (`usage.input_tokens`).
    pub input_tokens: u64,
    /// `cache_creation_input_tokens + cache_read_input_tokens`.
    pub cache_tokens: u64,
    /// Assistant messages counted in this window.
    pub messages: u64,
    pub by_model: Vec<ModelUsage>,
    /// Trailing-30-minute velocity: **input + output** tokens in the last 30 min
    /// / 30. Cache reads are deliberately excluded so this is comparable with the
    /// two window totals rendered beside it (also in+out); including them would
    /// inflate the rate by orders of magnitude on a cache-heavy corpus. Same
    /// measure on both windows by construction (30 min ⊂ 5h ⊂ 7d) — it is a "how
    /// hot is it right now" readout, not a per-window average.
    pub tokens_per_min: f64,
}

/// What the footer renders / what `usage_snapshot` returns.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub five_hour: UsageWindow,
    pub week: UsageWindow,
    /// Wall-clock ms at which this snapshot was computed (Unix epoch).
    pub computed_at_ms: i64,
    /// How long the pass took, ms — the cold first pass is the expensive one.
    pub scan_ms: u64,
}

// ── Scanner ──────────────────────────────────────────────────────────────────

/// Per-file incremental read state.
#[derive(Clone, Debug)]
struct Cursor {
    /// Last observed mtime, ms since epoch (0 when unknown).
    mtime_ms: i64,
    /// Last observed file length in bytes.
    len: u64,
    /// Bytes already consumed (always a line boundary).
    offset: u64,
}

/// One assistant message we have already accounted for.
#[derive(Clone, Debug)]
struct Record {
    ts_ms: i64,
    /// `message.id` — the dedup key (resumed sessions replay whole messages).
    id: String,
    /// Index into `UsageScanner::models` (interned; a corpus has a handful).
    model: u32,
    input: u64,
    output: u64,
    cache: u64,
}

/// Incremental scanner over `<home>/.claude/projects/*/*.jsonl`.
///
/// Owned by the poller thread and reused across passes — that reuse IS the
/// optimization: the cursors and the record list are what let pass N+1 read only
/// the bytes appended since pass N.
pub struct UsageScanner {
    projects_dir: PathBuf,
    cursors: HashMap<PathBuf, Cursor>,
    /// Message ids currently represented in `records` (prune keeps them in sync).
    seen: HashSet<String>,
    records: Vec<Record>,
    models: Vec<String>,
    model_idx: HashMap<String, u32>,
    /// Total bytes read off disk since construction. Diagnostic + the assertion
    /// hook the incremental-read tests use.
    bytes_read: u64,
}

impl UsageScanner {
    /// Build a scanner rooted at an injectable `home` (the `~`).
    pub fn new(home: &Path) -> Self {
        Self {
            projects_dir: home.join(".claude").join("projects"),
            cursors: HashMap::new(),
            seen: HashSet::new(),
            records: Vec::new(),
            models: Vec::new(),
            model_idx: HashMap::new(),
            bytes_read: 0,
        }
    }

    /// Total bytes read off disk since construction (never re-reads unchanged
    /// bytes, so this only grows by what was actually appended).
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Run one pass and aggregate. `now_ms` is injected so windows and the mtime
    /// cutoff are deterministic under test; production passes the wall clock.
    pub fn scan(&mut self, now_ms: i64) -> UsageSnapshot {
        let started = Instant::now();
        let mtime_cutoff = now_ms - MTIME_CUTOFF_MS;
        let record_cutoff = now_ms - WEEK_MS;

        let mut visited: HashSet<PathBuf> = HashSet::new();
        // `~/.claude/projects/<slug>/<session>.jsonl` — two levels, no recursion.
        if let Ok(projects) = fs::read_dir(&self.projects_dir) {
            for project in projects.flatten() {
                let Ok(files) = fs::read_dir(project.path()) else { continue };
                for file in files.flatten() {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let Ok(meta) = file.metadata() else { continue };
                    if !meta.is_file() {
                        continue;
                    }
                    // Keep the cursor alive even for skipped files: a session that
                    // goes quiet for a week and resumes must not force a rescan.
                    visited.insert(path.clone());
                    let mtime_ms = system_time_ms(meta.modified().ok());
                    if mtime_ms < mtime_cutoff {
                        continue; // too old to hold an in-window record — never opened
                    }
                    self.ingest_file(&path, meta.len(), mtime_ms, record_cutoff);
                }
            }
        }
        // Drop cursors for files that vanished (rotated/deleted sessions).
        self.cursors.retain(|p, _| visited.contains(p));

        self.prune(record_cutoff);
        let mut snap = self.aggregate(now_ms);
        snap.scan_ms = started.elapsed().as_millis() as u64;
        snap
    }

    /// Read the new tail of one file (or all of it, on first sight / truncation).
    fn ingest_file(&mut self, path: &Path, len: u64, mtime_ms: i64, record_cutoff: i64) {
        let prev = self.cursors.get(path).cloned();
        let mut start = match &prev {
            Some(c) => {
                if c.mtime_ms == mtime_ms && c.len == len {
                    return; // untouched since last pass — zero bytes read
                }
                if len < c.len {
                    // Truncated / rotated in place. Re-read from 0; the records we
                    // already took from it stay (they were real burn) and the
                    // re-read is idempotent thanks to message-id dedup.
                    0
                } else {
                    c.offset
                }
            }
            None => 0,
        };
        if start > len {
            start = 0; // defensive: cursor beyond EOF can only mean rewritten
        }

        // Unreadable this pass (transient EMFILE, a permission blip, a file that
        // vanished mid-walk): leave the cursor completely untouched so the next
        // pass retries. Advancing it would mark a file we never read as "seen at
        // this (mtime, len)", and the skip-if-unchanged fast path would then hide
        // it until something appended to it again.
        let Some(buf) = self.read_from(path, start) else { return };
        self.bytes_read += buf.len() as u64;
        // Stop at the last newline: a half-written record is not parsed now, and
        // is read whole next pass.
        let consumed = buf.iter().rposition(|&c| c == b'\n').map_or(0, |i| i + 1);
        for line in buf[..consumed].split(|&c| c == b'\n') {
            self.ingest_line(line, record_cutoff);
        }
        let consumed = consumed as u64;

        self.cursors.insert(
            path.to_path_buf(),
            Cursor {
                mtime_ms,
                len,
                offset: start + consumed,
            },
        );
    }

    /// Read `path` from `start` to EOF. `None` on any IO error (fault tolerance).
    fn read_from(&self, path: &Path, start: u64) -> Option<Vec<u8>> {
        let mut f = File::open(path).ok()?;
        if start > 0 {
            f.seek(SeekFrom::Start(start)).ok()?;
        }
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).ok()?;
        Some(buf)
    }

    /// Parse one transcript line into a record, if it is an assistant message
    /// carrying `message.usage`. Every failure mode is a silent skip.
    fn ingest_line(&mut self, line: &[u8], record_cutoff: i64) {
        if line.is_empty() {
            return;
        }
        // Cheap byte pre-filter before the expensive serde parse: both substrings
        // are necessary conditions for the record shape we want, and they knock
        // out the overwhelming majority of lines (user turns, tool results,
        // summaries) without allocating.
        let text = match std::str::from_utf8(line) {
            Ok(t) => t,
            Err(_) => return,
        };
        if !text.contains("\"usage\"") || !text.contains("\"assistant\"") {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            return;
        }
        let Some(msg) = v.get("message") else { return };
        let Some(usage) = msg.get("usage") else { return };

        let Some(id) = msg.get("id").and_then(|i| i.as_str()) else { return };
        if self.seen.contains(id) {
            return; // resumed session replayed a message we already counted
        }
        let Some(ts_ms) = v.get("timestamp").and_then(|t| t.as_str()).and_then(parse_iso8601_ms)
        else {
            return;
        };
        if ts_ms < record_cutoff {
            return; // already outside the long window — never enters the store
        }

        let u = |k: &str| usage.get(k).and_then(|n| n.as_u64()).unwrap_or(0);
        let input = u("input_tokens");
        let output = u("output_tokens");
        let cache = u("cache_creation_input_tokens") + u("cache_read_input_tokens");
        if input == 0 && output == 0 && cache == 0 {
            return; // nothing to account for
        }

        let model_name = msg.get("model").and_then(|m| m.as_str()).unwrap_or("unknown");
        let model = self.intern(model_name);
        self.seen.insert(id.to_string());
        self.records.push(Record {
            ts_ms,
            id: id.to_string(),
            model,
            input,
            output,
            cache,
        });
    }

    fn intern(&mut self, name: &str) -> u32 {
        if let Some(i) = self.model_idx.get(name) {
            return *i;
        }
        let i = self.models.len() as u32;
        self.models.push(name.to_string());
        self.model_idx.insert(name.to_string(), i);
        i
    }

    /// Drop records (and their dedup ids) that fell out of the long window.
    fn prune(&mut self, record_cutoff: i64) {
        if self.records.iter().all(|r| r.ts_ms >= record_cutoff) {
            return;
        }
        let mut kept = Vec::with_capacity(self.records.len());
        for r in self.records.drain(..) {
            if r.ts_ms >= record_cutoff {
                kept.push(r);
            } else {
                self.seen.remove(&r.id);
            }
        }
        self.records = kept;
    }

    /// Recompute both windows + velocity from the retained records.
    fn aggregate(&self, now_ms: i64) -> UsageSnapshot {
        let mut five = Acc::default();
        let mut week = Acc::default();
        let mut velocity_tokens: u64 = 0;
        let five_from = now_ms - FIVE_HOUR_MS;
        let week_from = now_ms - WEEK_MS;
        let vel_from = now_ms - VELOCITY_MS;

        for r in &self.records {
            if r.ts_ms < week_from {
                continue;
            }
            week.add(r);
            if r.ts_ms >= five_from {
                five.add(r);
            }
            if r.ts_ms >= vel_from {
                velocity_tokens += r.input + r.output;
            }
        }

        let tpm = velocity_tokens as f64 / VELOCITY_MINUTES;
        UsageSnapshot {
            five_hour: five.finish(&self.models, tpm),
            week: week.finish(&self.models, tpm),
            computed_at_ms: now_ms,
            scan_ms: 0,
        }
    }
}

/// Window accumulator (kept out of the DTO so the DTO stays a pure wire type).
#[derive(Default)]
struct Acc {
    input: u64,
    output: u64,
    cache: u64,
    messages: u64,
    by_model: HashMap<u32, (u64, u64)>, // model idx -> (tokens, messages)
}

impl Acc {
    fn add(&mut self, r: &Record) {
        self.input += r.input;
        self.output += r.output;
        self.cache += r.cache;
        self.messages += 1;
        let e = self.by_model.entry(r.model).or_insert((0, 0));
        e.0 += r.input + r.output + r.cache;
        e.1 += 1;
    }

    fn finish(self, models: &[String], tokens_per_min: f64) -> UsageWindow {
        let mut by_model: Vec<ModelUsage> = self
            .by_model
            .iter()
            .map(|(idx, (tokens, messages))| ModelUsage {
                model: models
                    .get(*idx as usize)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string()),
                total_tokens: *tokens,
                messages: *messages,
            })
            .collect();
        // Deterministic order for the UI: heaviest first, ties by name.
        by_model.sort_by(|a, b| {
            b.total_tokens
                .cmp(&a.total_tokens)
                .then_with(|| a.model.cmp(&b.model))
        });
        UsageWindow {
            total_tokens: self.input + self.output + self.cache,
            output_tokens: self.output,
            input_tokens: self.input,
            cache_tokens: self.cache,
            messages: self.messages,
            by_model,
            tokens_per_min,
        }
    }
}

// ── Time helpers (no chrono dependency) ──────────────────────────────────────

/// Wall-clock ms since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn system_time_ms(t: Option<SystemTime>) -> i64 {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Days since 1970-01-01 for a proleptic-Gregorian date (Howard Hinnant's
/// `days_from_civil`). Cheaper and smaller than pulling in a date crate for the
/// one format Claude Code writes.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse the transcript timestamp (`2026-08-14T14:26:44.209Z`) to epoch ms.
///
/// Claude Code always writes UTC with a `Z` suffix, so the trailing zone is
/// ignored rather than supported — a non-UTC stamp would be silently treated as
/// UTC, which for a burn meter is a sub-second-scale lie at worst. Returns
/// `None` on anything that isn't this shape, so a garbage line is just skipped.
fn parse_iso8601_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    if b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') || b[13] != b':' || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let y = num(0..4)?;
    let mo = num(5..7)?;
    let d = num(8..10)?;
    let h = num(11..13)?;
    let mi = num(14..16)?;
    let sec = num(17..19)?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut ms = 0i64;
    if b.len() > 20 && b[19] == b'.' {
        let mut frac: String = s[20..].chars().take_while(|c| c.is_ascii_digit()).collect();
        frac.truncate(3);
        while frac.len() < 3 {
            frac.push('0');
        }
        ms = frac.parse().ok()?;
    }
    Some((days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec) * 1000 + ms)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Throwaway `~` tree under a unique temp dir. Sandbox-only: these tests
    /// NEVER touch the real `~/.claude`.
    struct Sandbox {
        root: PathBuf,
    }
    impl Sandbox {
        fn new(tag: &str) -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("cockpit-usage-test-{tag}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Sandbox { root }
        }
        fn home(&self) -> PathBuf {
            self.root.join("home")
        }
        fn session(&self, project: &str, name: &str) -> PathBuf {
            let p = self
                .home()
                .join(".claude")
                .join("projects")
                .join(project)
                .join(format!("{name}.jsonl"));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            p
        }
        fn write(&self, project: &str, name: &str, body: &str) -> PathBuf {
            let p = self.session(project, name);
            fs::write(&p, body).unwrap();
            p
        }
        fn append(&self, path: &Path, body: &str) {
            use std::io::Write;
            let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
            f.write_all(body.as_bytes()).unwrap();
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// A fixed "now" so window math is exact: 2026-08-14T12:00:00.000Z.
    const NOW: i64 = 1_786_824_000_000;

    fn iso(ms: i64) -> String {
        // Inverse of `parse_iso8601_ms`, test-side only (civil_from_days).
        let days = ms.div_euclid(86_400_000);
        let rem = ms.rem_euclid(86_400_000);
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        let (h, mi, s, milli) = (
            rem / 3_600_000,
            (rem / 60_000) % 60,
            (rem / 1000) % 60,
            rem % 1000,
        );
        format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
    }

    /// One assistant transcript line, mirroring the real record shape.
    fn line(id: &str, ago_ms: i64, model: &str, input: u64, output: u64, cache: u64) -> String {
        format!(
            r#"{{"type":"assistant","uuid":"u-{id}","timestamp":"{ts}","sessionId":"s","message":{{"id":"{id}","model":"{model}","role":"assistant","type":"message","content":[],"usage":{{"input_tokens":{input},"output_tokens":{output},"cache_creation_input_tokens":{cache},"cache_read_input_tokens":0}}}}}}"#,
            ts = iso(NOW - ago_ms)
        ) + "\n"
    }

    const MIN: i64 = 60 * 1000;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;

    // ── aggregation ─────────────────────────────────────────────────────────

    #[test]
    fn aggregates_five_hour_and_week_windows_with_model_split() {
        let sb = Sandbox::new("agg");
        sb.write(
            "proj-a",
            "s1",
            &(line("m1", 1 * HOUR, "claude-opus-5", 10, 100, 1000)
                + &line("m2", 4 * HOUR, "claude-sonnet-4", 5, 50, 500)
                // Outside 5h, inside the week.
                + &line("m3", 3 * DAY, "claude-opus-5", 1, 10, 100)
                // Outside the week entirely — must not count anywhere.
                + &line("m4", 9 * DAY, "claude-opus-5", 999, 999, 999)),
        );

        let mut sc = UsageScanner::new(&sb.home());
        let snap = sc.scan(NOW);

        // 5h: m1 + m2
        assert_eq!(snap.five_hour.messages, 2);
        assert_eq!(snap.five_hour.input_tokens, 15);
        assert_eq!(snap.five_hour.output_tokens, 150);
        assert_eq!(snap.five_hour.cache_tokens, 1500);
        assert_eq!(snap.five_hour.total_tokens, 1665);
        // by_model sorted heaviest first.
        assert_eq!(snap.five_hour.by_model.len(), 2);
        assert_eq!(snap.five_hour.by_model[0].model, "claude-opus-5");
        assert_eq!(snap.five_hour.by_model[0].total_tokens, 1110);
        assert_eq!(snap.five_hour.by_model[0].messages, 1);
        assert_eq!(snap.five_hour.by_model[1].model, "claude-sonnet-4");
        assert_eq!(snap.five_hour.by_model[1].total_tokens, 555);

        // week: m1 + m2 + m3 (NOT m4)
        assert_eq!(snap.week.messages, 3);
        assert_eq!(snap.week.total_tokens, 1665 + 111);
        assert_eq!(snap.computed_at_ms, NOW);
    }

    #[test]
    fn dedups_repeated_message_ids_across_files() {
        let sb = Sandbox::new("dedup");
        // Same message id replayed in a resumed session's new transcript.
        sb.write("proj-a", "s1", &line("dup", 10 * MIN, "claude-opus-5", 1, 2, 3));
        sb.write("proj-a", "s2", &line("dup", 10 * MIN, "claude-opus-5", 1, 2, 3));
        sb.write("proj-b", "s3", &line("uniq", 10 * MIN, "claude-opus-5", 1, 2, 3));

        let mut sc = UsageScanner::new(&sb.home());
        let snap = sc.scan(NOW);
        assert_eq!(snap.five_hour.messages, 2, "the duplicate id counts once");
        assert_eq!(snap.five_hour.total_tokens, 12);
    }

    #[test]
    fn dedups_across_repeated_scans_of_a_rewritten_file() {
        let sb = Sandbox::new("dedup2");
        let p = sb.write("proj", "s", &line("a", 5 * MIN, "m", 1, 1, 1));
        let mut sc = UsageScanner::new(&sb.home());
        assert_eq!(sc.scan(NOW).five_hour.messages, 1);
        // Rewrite identical content at a different length → full rescan path.
        fs::write(&p, line("a", 5 * MIN, "m", 1, 1, 1) + &line("b", 5 * MIN, "m", 1, 1, 1)).unwrap();
        let snap = sc.scan(NOW + 1000);
        assert_eq!(snap.five_hour.messages, 2, "'a' must not be double counted");
    }

    #[test]
    fn tolerates_malformed_and_irrelevant_lines() {
        let sb = Sandbox::new("malformed");
        let body = String::new()
            + "not json at all\n"
            + "{\"type\":\"assistant\",\"message\":{\"usage\":{\n"          // truncated json
            + "{\"type\":\"user\",\"message\":{\"role\":\"user\"}}\n"        // wrong type
            + "{\"type\":\"assistant\",\"message\":{\"id\":\"x\",\"usage\":{}}}\n" // no timestamp
            + "\n"
            + &line("good", 5 * MIN, "claude-opus-5", 1, 1, 1)
            + "{\"type\":\"assistant\",\"timestamp\":\"nope\",\"message\":{\"id\":\"y\",\"usage\":{\"output_tokens\":5}}}\n"
            + &line("good2", 5 * MIN, "claude-opus-5", 1, 1, 1);
        sb.write("proj", "s", &body);

        let mut sc = UsageScanner::new(&sb.home());
        let snap = sc.scan(NOW);
        assert_eq!(snap.five_hour.messages, 2, "only the two well-formed records");
        assert_eq!(snap.five_hour.total_tokens, 6);
    }

    // ── incremental reads ───────────────────────────────────────────────────

    #[test]
    fn incremental_tail_read_never_rereads_unchanged_bytes() {
        let sb = Sandbox::new("tail");
        let first = line("a", 5 * MIN, "m", 1, 1, 1);
        let p = sb.write("proj", "s", &first);

        let mut sc = UsageScanner::new(&sb.home());
        let snap1 = sc.scan(NOW);
        let after_first = sc.bytes_read();
        assert_eq!(snap1.five_hour.messages, 1);
        assert_eq!(after_first, first.len() as u64, "first pass reads the file once");

        // Idle pass: nothing changed → not one byte read.
        let snap2 = sc.scan(NOW + 1000);
        assert_eq!(sc.bytes_read(), after_first, "unchanged file must not be re-read");
        assert_eq!(snap2.five_hour.messages, 1);

        // Append: only the new bytes are read.
        let second = line("b", 4 * MIN, "m", 2, 2, 2);
        sb.append(&p, &second);
        let snap3 = sc.scan(NOW + 2000);
        assert_eq!(
            sc.bytes_read(),
            after_first + second.len() as u64,
            "tail read only covers the appended bytes"
        );
        assert_eq!(snap3.five_hour.messages, 2);
        assert_eq!(snap3.five_hour.total_tokens, 3 + 6);
    }

    #[test]
    fn partial_trailing_line_is_not_consumed_until_complete() {
        let sb = Sandbox::new("partial");
        let full = line("a", 5 * MIN, "m", 1, 1, 1);
        let (head, tail) = full.split_at(full.len() / 2);
        let p = sb.write("proj", "s", head);

        let mut sc = UsageScanner::new(&sb.home());
        assert_eq!(sc.scan(NOW).five_hour.messages, 0, "half a record is not a record");

        sb.append(&p, tail);
        let snap = sc.scan(NOW + 1000);
        assert_eq!(snap.five_hour.messages, 1, "completed record counted once");
        assert_eq!(snap.five_hour.total_tokens, 3);
    }

    #[test]
    fn truncation_triggers_full_rescan() {
        let sb = Sandbox::new("trunc");
        let long = line("a", 5 * MIN, "m", 1, 1, 1) + &line("b", 5 * MIN, "m", 1, 1, 1);
        let p = sb.write("proj", "s", &long);

        let mut sc = UsageScanner::new(&sb.home());
        assert_eq!(sc.scan(NOW).five_hour.messages, 2);

        // Rotated in place: shorter file, entirely new content.
        let short = line("c", 5 * MIN, "m", 4, 0, 0);
        assert!(short.len() < long.len());
        fs::write(&p, &short).unwrap();
        let before = sc.bytes_read();
        let snap = sc.scan(NOW + 1000);
        assert_eq!(
            sc.bytes_read() - before,
            short.len() as u64,
            "a shrunken file is re-read from byte 0"
        );
        // a + b are retained (real burn, still in-window) and c is picked up.
        assert_eq!(snap.five_hour.messages, 3);
        assert_eq!(snap.five_hour.total_tokens, 3 + 3 + 4);
    }

    #[test]
    fn skips_files_whose_mtime_predates_the_window() {
        let sb = Sandbox::new("mtime");
        let p = sb.write("proj", "old", &line("old", 30 * MIN, "m", 1, 1, 1));
        // Backdate the file well past 7d+1h; its (recent) record must stay unread.
        let stale = SystemTime::UNIX_EPOCH
            + std::time::Duration::from_millis((NOW - 30 * DAY) as u64);
        File::options().write(true).open(&p).unwrap().set_modified(stale).unwrap();

        let mut sc = UsageScanner::new(&sb.home());
        let snap = sc.scan(NOW);
        assert_eq!(snap.week.messages, 0, "stale file is never opened");
        assert_eq!(sc.bytes_read(), 0, "not one byte read from a stale file");
    }

    #[test]
    fn records_age_out_of_the_week_window_on_a_later_pass() {
        let sb = Sandbox::new("prune");
        sb.write("proj", "s", &line("a", 6 * DAY, "m", 1, 1, 1));
        let mut sc = UsageScanner::new(&sb.home());
        assert_eq!(sc.scan(NOW).week.messages, 1);
        // Two days later the record is past 7d — it must fall out of the window.
        assert_eq!(sc.scan(NOW + 2 * DAY).week.messages, 0);
    }

    // ── velocity ────────────────────────────────────────────────────────────

    #[test]
    fn velocity_counts_only_the_trailing_thirty_minutes() {
        let sb = Sandbox::new("velocity");
        sb.write(
            "proj",
            "s",
            &(line("hot", 10 * MIN, "m", 100, 200, 300) // in the 30m window
                + &line("warm", 45 * MIN, "m", 999, 999, 999)), // outside it
        );
        let mut sc = UsageScanner::new(&sb.home());
        let snap = sc.scan(NOW);
        // in+out only (100 + 200) — the 300 cache tokens are excluded by design.
        assert_eq!(snap.five_hour.tokens_per_min, 300.0 / 30.0);
        // Same velocity on both windows (30m ⊂ 5h ⊂ 7d).
        assert_eq!(snap.week.tokens_per_min, snap.five_hour.tokens_per_min);
        // …but the windows' totals still include the older record.
        assert_eq!(snap.five_hour.total_tokens, 600 + 2997);
    }

    #[test]
    fn empty_corpus_is_a_zero_snapshot_not_an_error() {
        let sb = Sandbox::new("empty");
        let mut sc = UsageScanner::new(&sb.home()); // ~/.claude/projects doesn't exist
        let snap = sc.scan(NOW);
        assert_eq!(snap.five_hour, UsageWindow::default());
        assert_eq!(snap.week.total_tokens, 0);
        assert_eq!(snap.computed_at_ms, NOW);
        assert_eq!(sc.bytes_read(), 0);
    }

    // ── wire contract (C-2 depends on these exact names) ────────────────────

    #[test]
    fn serializes_camel_case_field_names() {
        let snap = UsageSnapshot {
            five_hour: UsageWindow {
                total_tokens: 1,
                output_tokens: 2,
                input_tokens: 3,
                cache_tokens: 4,
                messages: 5,
                by_model: vec![ModelUsage {
                    model: "m".into(),
                    total_tokens: 6,
                    messages: 7,
                }],
                tokens_per_min: 8.5,
            },
            week: UsageWindow::default(),
            computed_at_ms: 9,
            scan_ms: 10,
        };
        let json = serde_json::to_string(&snap).unwrap();
        for key in [
            "\"fiveHour\"",
            "\"week\"",
            "\"computedAtMs\"",
            "\"scanMs\"",
            "\"totalTokens\"",
            "\"outputTokens\"",
            "\"inputTokens\"",
            "\"cacheTokens\"",
            "\"messages\"",
            "\"byModel\"",
            "\"tokensPerMin\"",
            "\"model\"",
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
    }

    /// READ-ONLY sanity against the developer's REAL `~/.claude` — the fixtures
    /// prove the algorithm, this proves the algorithm is pointed at the shape the
    /// live corpus actually has (a wrong field name would show up as a plausible
    /// zero, not as a failure). `#[ignore]` so the normal suite stays tmpdir-only
    /// and machine-independent; run on demand:
    /// `cargo test --lib usage::tests::real_corpus_sanity -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_corpus_sanity() {
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let mut sc = UsageScanner::new(&home);
        let cold = sc.scan(now_ms());
        let cold_bytes = sc.bytes_read();
        let warm = sc.scan(now_ms());
        eprintln!(
            "cold: {} ms, {:.1} MB read, 5h={} tok / {} msg, week={} tok / {} msg, {:.0} tok/min",
            cold.scan_ms,
            cold_bytes as f64 / 1e6,
            cold.five_hour.total_tokens,
            cold.five_hour.messages,
            cold.week.total_tokens,
            cold.week.messages,
            cold.week.tokens_per_min,
        );
        eprintln!(
            "warm: {} ms, {} new bytes read; models: {:?}",
            warm.scan_ms,
            sc.bytes_read() - cold_bytes,
            warm.week.by_model.iter().map(|m| &m.model).collect::<Vec<_>>(),
        );
        assert!(warm.week.total_tokens > 0, "live corpus should have burn");
        assert!(
            warm.week.total_tokens >= warm.five_hour.total_tokens,
            "the week window contains the 5h window"
        );
    }

    // ── timestamp parsing ───────────────────────────────────────────────────

    #[test]
    fn parses_transcript_timestamps() {
        // Epoch itself.
        assert_eq!(parse_iso8601_ms("1970-01-01T00:00:00.000Z"), Some(0));
        // A real stamp taken from a live transcript.
        let t = parse_iso8601_ms("2026-08-14T14:26:44.209Z").unwrap();
        assert_eq!(iso(t), "2026-08-14T14:26:44.209Z");
        // Fraction optional.
        assert_eq!(
            parse_iso8601_ms("2026-08-14T14:26:44Z"),
            Some(t - 209),
        );
        // Garbage is rejected, never panics.
        for bad in ["", "nope", "2026-13-01T00:00:00Z", "2026-08-14X00:00:00Z", "20260814"] {
            assert_eq!(parse_iso8601_ms(bad), None, "{bad} should not parse");
        }
    }
}

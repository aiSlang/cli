//! Per-day persistent spend tracker (Decision 7, week 7 Thu).
//!
//! Records the accumulated USD spend per `(project_name, utc_date)`
//! pair in a small JSON file at `~/.aislang/spend.json`. Survives
//! kill -9 via the tempfile + rename atomic-write pattern.
//!
//! Schema (`schema_version=1`):
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "projects": {
//!     "support-agent": {
//!       "2026-05-27": { "micro_units": 12345 },
//!       "2026-05-28": { "micro_units":   500 }
//!     },
//!     "free-local": {
//!       "2026-05-27": { "micro_units": 0 }
//!     }
//!   }
//! }
//! ```
//!
//! v0.1 stores µ-USD only — no currency field per entry (the bundled
//! catalog is USD-only; mixed currencies are a v0.2 concern, same
//! limitation as elsewhere). When/if currencies multiply, a schema
//! bump to v2 reads v1 files as USD by default.
//!
//! `Money::Unlimited` call costs are skipped — they don't contribute
//! to the running total since "unbounded cost" isn't a finite number
//! to add.

use aislang_core::types::{Currency, Money};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use thiserror::Error;

// ============================================================================
// Wire types — `spend.json` schema
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpendFile {
    pub schema_version: u32,
    /// project_name → utc_date_string → entry
    #[serde(default)]
    pub projects: BTreeMap<String, BTreeMap<String, DayEntry>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DayEntry {
    /// µ-USD. v0.1 USD-only (see module docs). This is the
    /// AUTHORITATIVE today-total — the per-day cap check
    /// (`Session::turn`) reads this field.
    pub micro_units: i64,
    /// Per-model breakdown of today's spend (week 10 Tue). Keys are
    /// `"{provider}.{name}"` strings — the canonical
    /// `ModelId::to_string()` form so they match catalog ids verbatim.
    /// `#[serde(default)]` means v1.0 spend files (where this field
    /// didn't exist) load cleanly with an empty map; we never bump
    /// `schema_version` for this purely-additive field.
    ///
    /// Sums of `by_model` values are expected to equal `micro_units`
    /// for new writes; older v1.0 files have an empty map and a
    /// non-zero `micro_units` (no per-model attribution available).
    /// `aislang status --model-spend` (week 10 Thu) reads from
    /// `by_model`; `aislang status --spend` reads from `micro_units`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_model: BTreeMap<String, i64>,
    /// **Week 13 Thu addition — per-`main`-block spend.**
    ///
    /// Keys are `main` block labels (e.g. `"support_orchestrator"`).
    /// Parallel to `by_model`, but tracks tokens spent by the
    /// user-facing principal LLM declared in the .ais source via
    /// `main "X" { … }`.  Sums DO NOT contribute to `micro_units`
    /// when the main's `model.source = subscription` (no API charge);
    /// for `source = api` mains the contribution is identical to
    /// `by_model`.
    ///
    /// **v0.2 status: schema-only.**  Auto-recording requires v0.3's
    /// OpenAI-compat adapter for `aislang mcp-serve` — until then the
    /// host chat app's inference path bypasses every aislang
    /// instrumentation point, so this map only fills via a
    /// dedicated MCP tool (added in a Thu follow-up if it ships).
    /// `aislang status --main-spend` reads from this field and
    /// surfaces an empty-state hint pointing at the v0.3 carry-over.
    ///
    /// Same `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]`
    /// posture as `by_model`: forward-compat with v0.1 / v0.2-pre-Thu
    /// files, byte-stable serialization when no main spend is recorded.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_main: BTreeMap<String, i64>,
}

impl Default for DayEntry {
    fn default() -> Self {
        Self {
            micro_units: 0,
            by_model:    BTreeMap::new(),
            by_main:     BTreeMap::new(),    // week 13 Thu
        }
    }
}

impl Default for SpendFile {
    fn default() -> Self {
        Self { schema_version: 1, projects: BTreeMap::new() }
    }
}

const CURRENT_SCHEMA_VERSION: u32 = 1;

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Error)]
pub enum SpendError {
    #[error("spend file `{path}` not readable: {cause}")]
    Io { path: String, cause: String },

    #[error("spend file `{path}` not valid JSON: {cause}")]
    Decode { path: String, cause: String },

    #[error("spend file `{path}` schema_version = {found}, expected {expected}. \
             aislang v0.1 doesn't know how to read this; delete the file or \
             use a newer aislang.")]
    SchemaMismatch { path: String, found: u32, expected: u32 },

    #[error("could not determine $HOME for ~/.aislang/spend.json")]
    NoHome,
}

// ============================================================================
// `SpendTracker`
// ============================================================================

/// In-memory cache of the spend file + the file path. Thread-safe via
/// an internal `Mutex` — `turn()` calls `add()` through `&self`. Cheap
/// to clone the handle (Arc'd by callers that share); not Cloneable
/// itself because Mutex isn't Clone.
///
/// One tracker per CLI invocation. The CLI builds it during chat-
/// session open after resolving the project name.
pub struct SpendTracker {
    path:         PathBuf,
    project_name: String,
    file:         Mutex<SpendFile>,
}

impl SpendTracker {
    /// Default path: `~/.aislang/spend.json`. Read it (or seed with
    /// the default schema if missing). Used by production callers.
    pub fn open_default(project_name: &str) -> Result<Self, SpendError> {
        Self::at(project_name, default_spend_path()?)
    }

    /// Explicit-path constructor. Used by tests + by anyone who
    /// wants to override the default location (e.g. a CI run that
    /// shouldn't pollute the dev's spend file).
    pub fn at(project_name: &str, path: PathBuf) -> Result<Self, SpendError> {
        let file = read_or_default(&path)?;
        Ok(Self {
            path,
            project_name: project_name.to_string(),
            file:         Mutex::new(file),
        })
    }

    /// Current accumulated spend for today (UTC) under this tracker's
    /// project name. Returns `Money::Capped { Usd, … }` always — never
    /// Unlimited (we can't track unbounded spend).
    pub fn today_spend(&self) -> Money {
        let day = utc_today();
        let f   = self.file.lock().expect("spend file mutex");
        let mu  = f.projects.get(&self.project_name)
            .and_then(|p| p.get(&day))
            .map(|e| e.micro_units)
            .unwrap_or(0);
        Money::Capped { currency: Currency::Usd, micro_units: mu }
    }

    /// Per-model breakdown of today's spend for this tracker's
    /// project. Empty map when no data has been recorded yet. Used
    /// by `aislang status --model-spend` (week 10 Thu).
    pub fn today_by_model(&self) -> BTreeMap<String, Money> {
        let day = utc_today();
        let f   = self.file.lock().expect("spend file mutex");
        let Some(p) = f.projects.get(&self.project_name) else { return BTreeMap::new(); };
        let Some(e) = p.get(&day)                        else { return BTreeMap::new(); };
        e.by_model.iter()
            .map(|(k, &mu)| (k.clone(), Money::Capped {
                currency: Currency::Usd,
                micro_units: mu,
            }))
            .collect()
    }

    /// Per-`main`-block breakdown of today's spend (week 13 Thu).
    /// Parallel to [`today_by_model`] but reads from `DayEntry.by_main`.
    /// Empty map when no main-spend has been recorded today.
    ///
    /// **v0.2 status (week 13 Thu):** schema + read-side only.  No
    /// callsite writes to `by_main` yet — the host chat app's
    /// inference path (Claude Desktop, Cursor) is invisible to
    /// aislang.  Filling this map automatically requires v0.3's
    /// OpenAI-compatible adapter for `aislang mcp-serve` (see PLAN
    /// v0.3 carry-overs).  Until then the accessor returns
    /// `BTreeMap::new()` and `aislang status --main-spend` surfaces
    /// an empty-state hint.
    pub fn today_by_main(&self) -> BTreeMap<String, Money> {
        let day = utc_today();
        let f   = self.file.lock().expect("spend file mutex");
        let Some(p) = f.projects.get(&self.project_name) else { return BTreeMap::new(); };
        let Some(e) = p.get(&day)                        else { return BTreeMap::new(); };
        e.by_main.iter()
            .map(|(k, &mu)| (k.clone(), Money::Capped {
                currency: Currency::Usd,
                micro_units: mu,
            }))
            .collect()
    }

    /// Record one call's cost against today's spend + flush to disk
    /// atomically. `Money::Unlimited` costs are silently skipped
    /// (can't add unbounded to a finite total).
    ///
    /// `model_id_str` is the canonical `"<provider>.<name>"` form of
    /// the model the call hit (week 10 Tue). Pass `""` (empty string)
    /// when the model is unknown — the call still contributes to
    /// `micro_units` but is NOT attributed in `by_model`. This is the
    /// graceful-degradation path for tests and any future callers
    /// that don't know the resolved model.
    pub fn add(&self, model_id_str: &str, call_cost: &Money) -> Result<(), SpendError> {
        let mu = match call_cost.micro_units() {
            Some(m) => m,
            None    => return Ok(()),   // Unlimited: skip
        };
        if mu == 0 {
            // Optimization: don't write the file for zero-cost calls
            // (e.g. Ollama free-tier). Saves disk I/O for the common
            // local-model path.
            return Ok(());
        }
        let day = utc_today();
        {
            let mut f = self.file.lock().expect("spend file mutex");
            let by_day = f.projects.entry(self.project_name.clone()).or_default();
            let entry  = by_day.entry(day).or_insert_with(DayEntry::default);
            entry.micro_units = entry.micro_units.saturating_add(mu);
            // Per-model attribution (week 10 Tue). Empty model_id_str
            // means "caller doesn't know which model" — total still
            // moves but no by_model bucket is touched (avoids polluting
            // the map with `"" -> N` keys).
            if !model_id_str.is_empty() {
                let m_entry = entry.by_model.entry(model_id_str.to_owned()).or_insert(0);
                *m_entry = m_entry.saturating_add(mu);
            }
        }
        self.write_atomic()
    }

    /// Diagnostic accessor — path to the on-disk file.
    pub fn path(&self) -> &Path { &self.path }

    // ------------------------------------------------------------ //

    /// Tempfile + rename atomic write. Mirrors the pattern used by
    /// `aislang-resolver::lockfile` + the `code-stat` skill's cache.
    /// On a kill -9 mid-write, the old file is intact (rename is
    /// atomic on POSIX); on a kill BEFORE rename, the tempfile is
    /// orphaned but the original is untouched.
    fn write_atomic(&self) -> Result<(), SpendError> {
        let f    = self.file.lock().expect("spend file mutex");
        let json = serde_json::to_string_pretty(&*f)
            .map_err(|e| SpendError::Decode {
                path:  self.path.display().to_string(),
                cause: e.to_string(),
            })?;
        drop(f);   // release before any blocking I/O

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SpendError::Io {
                path:  parent.display().to_string(),
                cause: format!("mkdir: {e}"),
            })?;
        }
        let tmp = self.path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&tmp, json.as_bytes()).map_err(|e| SpendError::Io {
            path:  tmp.display().to_string(),
            cause: format!("write: {e}"),
        })?;
        std::fs::rename(&tmp, &self.path).map_err(|e| SpendError::Io {
            path:  self.path.display().to_string(),
            cause: format!("rename: {e}"),
        })?;
        Ok(())
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn read_or_default(path: &Path) -> Result<SpendFile, SpendError> {
    if !path.exists() {
        return Ok(SpendFile::default());
    }
    let bytes = std::fs::read(path).map_err(|e| SpendError::Io {
        path:  path.display().to_string(),
        cause: e.to_string(),
    })?;
    if bytes.is_empty() {
        return Ok(SpendFile::default());
    }
    let f: SpendFile = serde_json::from_slice(&bytes).map_err(|e| SpendError::Decode {
        path:  path.display().to_string(),
        cause: e.to_string(),
    })?;
    if f.schema_version != CURRENT_SCHEMA_VERSION {
        return Err(SpendError::SchemaMismatch {
            path:     path.display().to_string(),
            found:    f.schema_version,
            expected: CURRENT_SCHEMA_VERSION,
        });
    }
    Ok(f)
}

/// Default path: `$HOME/.aislang/spend.json`. Errors if $HOME isn't set.
fn default_spend_path() -> Result<PathBuf, SpendError> {
    let home = std::env::var_os("HOME").ok_or(SpendError::NoHome)?;
    Ok(PathBuf::from(home).join(".aislang").join("spend.json"))
}

/// Current UTC date as `YYYY-MM-DD`. Computed from
/// `SystemTime::now()` via Howard Hinnant's civil-from-days
/// algorithm — no chrono/time dep needed.
pub fn utc_today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    utc_date_from_epoch_secs(secs)
}

/// Format a UNIX epoch timestamp (in seconds, can be negative for
/// pre-1970 dates which we don't expect) as a `YYYY-MM-DD` UTC
/// string. Pure function — testable independently of system clock.
pub fn utc_date_from_epoch_secs(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Current UTC instant as an RFC3339 / ISO-8601 string with a `Z`
/// suffix. Used by week-10 state-file's `applied_at` field. No
/// sub-second precision (we don't need it) and no offset variants —
/// always UTC, always `Z`. Pure-w.r.t.-clock companion lives at
/// `utc_rfc3339_from_epoch_secs`.
pub fn utc_rfc3339_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    utc_rfc3339_from_epoch_secs(secs)
}

/// Pure: format a UNIX epoch timestamp as `YYYY-MM-DDTHH:MM:SSZ`.
/// Tested against known timestamps without touching the system clock.
pub fn utc_rfc3339_from_epoch_secs(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    let hh = (secs_of_day / 3_600) as u32;
    let mm = ((secs_of_day % 3_600) / 60) as u32;
    let ss = (secs_of_day % 60) as u32;
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Howard Hinnant's `civil_from_days` algorithm. Pure, branch-light,
/// handles negative inputs (pre-epoch dates) correctly. Returns
/// (year, month, day) where month and day are 1-based.
///
/// Source: <http://howardhinnant.github.io/date_algorithms.html>
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    // Shift epoch from 1970-01-01 to 0000-03-01 (a Gregorian-cycle
    // boundary that simplifies leap-year arithmetic).
    let z   = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;                  // [0, 146096]
    let yoe = (doe - doe/1460 + doe/36524 - doe/146096) / 365;  // [0, 399]
    let y   = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe/4 - yoe/100);         // [0, 365]
    let mp  = (5 * doy + 2) / 153;                         // [0, 11]
    let d   = (doy - (153 * mp + 2) / 5 + 1) as u32;       // [1, 31]
    let m   = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;  // [1, 12]
    let y   = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Per-process atomic counter — guarantees uniqueness for
    // parallel tests in the same binary regardless of system
    // clock resolution. macOS's SystemTime::now() resolves only
    // to microseconds, so two `tempdir()` calls scheduled within
    // the same µs would otherwise collide on the path and step
    // on each other's spend.json (caught in CI on macos-latest;
    // Linux nanos resolution masked the bug).
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aislang-spend-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---- utc_date_from_epoch_secs (pure, system-clock-free) ----

    #[test] fn epoch_zero_is_1970_01_01() {
        assert_eq!(utc_date_from_epoch_secs(0), "1970-01-01");
    }

    #[test] fn known_recent_date() {
        // 2026-05-20 00:00:00 UTC = 1779235200 seconds since epoch
        // (cross-checked with `date -u -d '2026-05-20' +%s`).
        assert_eq!(utc_date_from_epoch_secs(1_779_235_200), "2026-05-20");
        // 6 days later → 2026-05-26.
        assert_eq!(utc_date_from_epoch_secs(1_779_235_200 + 6 * 86_400), "2026-05-26");
    }

    #[test] fn known_leap_day() {
        // 2024-02-29 00:00:00 UTC = 1709164800
        assert_eq!(utc_date_from_epoch_secs(1_709_164_800), "2024-02-29");
    }

    #[test] fn end_of_month_boundary() {
        // 2026-12-31 23:59:59 UTC = 1798761599 (Dec 31, last second)
        assert_eq!(utc_date_from_epoch_secs(1_798_761_599), "2026-12-31");
        // Add 1s → next day, year rollover
        assert_eq!(utc_date_from_epoch_secs(1_798_761_600), "2027-01-01");
    }

    #[test] fn utc_today_returns_valid_date_string() {
        let s = utc_today();
        // Format: YYYY-MM-DD (10 chars)
        assert_eq!(s.len(), 10);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[7..8], "-");
        // The year part should be ≥ 2026 (we're running after that).
        let year: i32 = s[0..4].parse().expect("4-digit year");
        assert!(year >= 2026, "today's year should be ≥ 2026; got `{s}`");
    }

    // ---- SpendTracker open/empty path ----

    #[test] fn open_without_existing_file_starts_at_zero() {
        let dir = tempdir();
        let t = SpendTracker::at("demo", dir.join("spend.json")).unwrap();
        assert_eq!(t.today_spend(), Money::usd(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn open_with_existing_file_reads_today_spend() {
        let dir  = tempdir();
        let path = dir.join("spend.json");
        let today = utc_today();
        let seed = format!(
            r#"{{
                "schema_version": 1,
                "projects": {{
                    "demo": {{
                        "{today}": {{ "micro_units": 5_000 }}
                    }}
                }}
            }}"#);
        // serde_json doesn't like the underscore separator in
        // integers — strip it.
        let seed = seed.replace("5_000", "5000");
        std::fs::write(&path, seed).unwrap();
        let t = SpendTracker::at("demo", path).unwrap();
        assert_eq!(t.today_spend(), Money::usd(5_000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn schema_version_mismatch_errors_loudly() {
        let dir  = tempdir();
        let path = dir.join("spend.json");
        std::fs::write(&path,
            r#"{"schema_version": 99, "projects": {}}"#).unwrap();
        let err = SpendTracker::at("demo", path).err()
            .expect("must reject future schema");
        match err {
            SpendError::SchemaMismatch { found, expected, .. } => {
                assert_eq!(found, 99);
                assert_eq!(expected, 1);
            }
            other => panic!("expected SchemaMismatch; got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- add() + persist + round-trip ----

    #[test] fn add_persists_to_disk() {
        let dir  = tempdir();
        let path = dir.join("spend.json");
        {
            let t = SpendTracker::at("demo", path.clone()).unwrap();
            t.add("", &Money::usd(1_200)).unwrap();
            t.add("", &Money::usd(800)).unwrap();
        }
        // Re-open via a fresh tracker — should see 2000 µ-USD.
        let t2 = SpendTracker::at("demo", path).unwrap();
        assert_eq!(t2.today_spend(), Money::usd(2_000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn add_unlimited_is_a_noop() {
        // Unlimited cost can't be summed into a finite total — skip
        // silently. The tracker stays at zero.
        let dir  = tempdir();
        let path = dir.join("spend.json");
        let t = SpendTracker::at("demo", path).unwrap();
        t.add("", &Money::Unlimited).unwrap();
        assert_eq!(t.today_spend(), Money::usd(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn add_zero_is_a_noop_and_skips_disk_write() {
        // Zero-cost calls (Ollama free-tier) shouldn't trigger disk
        // I/O on every chat turn. Pin: after add(0), the file
        // remains absent on disk if the tracker started without one.
        let dir  = tempdir();
        let path = dir.join("spend.json");
        let t = SpendTracker::at("demo", path.clone()).unwrap();
        t.add("", &Money::usd(0)).unwrap();
        assert_eq!(t.today_spend(), Money::usd(0));
        assert!(!path.exists(),
            "zero-cost add must not write the file; got file at {}",
            path.display());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- multi-project + multi-day isolation ----

    #[test] fn projects_dont_interfere() {
        let dir  = tempdir();
        let path = dir.join("spend.json");
        let t_a = SpendTracker::at("project_a", path.clone()).unwrap();
        t_a.add("", &Money::usd(100)).unwrap();
        // Open a DIFFERENT project against the SAME file.
        let t_b = SpendTracker::at("project_b", path).unwrap();
        // project_b's spend is independent.
        assert_eq!(t_b.today_spend(), Money::usd(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn historical_days_dont_pollute_today() {
        let dir  = tempdir();
        let path = dir.join("spend.json");
        // Seed a non-today date entry. Today's spend must be zero.
        let seed = r#"{
            "schema_version": 1,
            "projects": {
                "demo": {
                    "2000-01-01": { "micro_units": 999999 }
                }
            }
        }"#;
        std::fs::write(&path, seed).unwrap();
        let t = SpendTracker::at("demo", path).unwrap();
        assert_eq!(t.today_spend(), Money::usd(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- atomic write: tempfile cleaned up on success ----

    #[test] fn atomic_write_leaves_no_tempfile() {
        let dir  = tempdir();
        let path = dir.join("spend.json");
        let t = SpendTracker::at("demo", path.clone()).unwrap();
        t.add("", &Money::usd(100)).unwrap();
        // The .tmp.<pid> sibling should be gone after rename success.
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().into_string().unwrap_or_default())
            .collect();
        for name in &entries {
            assert!(!name.contains(".tmp."),
                "orphan tempfile after successful write: `{name}`");
        }
        // The actual spend.json IS there.
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- atomic write: rename semantic via existing-file replacement ----

    #[test] fn rename_replaces_existing_file_atomically() {
        // Simulates the kill -9 between write-tempfile and rename:
        // before the rename, the original file is intact. After the
        // rename, the new content is there. There's never a moment
        // where the file is half-written.
        //
        // We can't actually test the kill -9 path in a unit test;
        // what we CAN test is that the second add() doesn't lose
        // the first one (which would happen if the rename overwrote
        // partial content).
        let dir  = tempdir();
        let path = dir.join("spend.json");
        let t = SpendTracker::at("demo", path.clone()).unwrap();
        t.add("", &Money::usd(100)).unwrap();
        let after_first = std::fs::read_to_string(&path).unwrap();
        t.add("", &Money::usd(200)).unwrap();
        let after_second = std::fs::read_to_string(&path).unwrap();
        // Both writes succeeded; running total is 300.
        assert!(after_first.contains("100"));
        assert!(after_second.contains("300"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- corrupt file ----

    #[test] fn corrupt_json_errors_with_path() {
        let dir  = tempdir();
        let path = dir.join("spend.json");
        std::fs::write(&path, "{not json").unwrap();
        let err = SpendTracker::at("demo", path.clone()).err()
            .expect("must reject corrupt JSON");
        match err {
            SpendError::Decode { path: p, .. } => {
                assert!(p.contains("spend.json"));
            }
            other => panic!("expected Decode error; got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- week 10 Mon: by_model field + RFC3339 helper ----

    #[test] fn day_entry_default_is_zero_total_and_empty_by_model() {
        let d = DayEntry::default();
        assert_eq!(d.micro_units, 0);
        assert!(d.by_model.is_empty());
    }

    #[test] fn day_entry_v1_file_without_by_model_loads_cleanly() {
        // Older spend.json files (before week 10 Mon) wrote only
        // `micro_units`. They MUST keep loading after the schema
        // extension — the `#[serde(default)]` on `by_model` is what
        // makes this safe. This is the load-bearing back-compat pin.
        let dir = tempdir();
        let path = dir.join("spend.json");
        let legacy_json = r#"{
            "schema_version": 1,
            "projects": {
                "support-agent": {
                    "2026-05-20": { "micro_units": 12345 }
                }
            }
        }"#;
        std::fs::write(&path, legacy_json).unwrap();
        let tracker = SpendTracker::at("support-agent", path.clone()).unwrap();
        // `today_spend` for 2026-05-20 reads from the loaded file
        // structure regardless of what "today" is. We can't easily
        // set today; instead probe the in-memory file directly.
        let f = tracker.file.lock().unwrap();
        let entry = f.projects.get("support-agent").unwrap().get("2026-05-20").unwrap();
        assert_eq!(entry.micro_units, 12345);
        assert!(entry.by_model.is_empty(),
            "v1 file with no by_model field must load with empty map");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn day_entry_with_by_model_round_trips() {
        // A spend.json that DID carry per-model data must serialise
        // and parse back identically. Pin the JSON-key spelling
        // (`by_model`) since `aislang status --model-spend` queries
        // it.
        let mut entry = DayEntry::default();
        entry.micro_units = 5000;
        entry.by_model.insert("anthropic.claude_haiku_4_5".into(), 3000);
        entry.by_model.insert("openai.gpt_4o_mini".into(), 2000);
        let s = serde_json::to_string(&entry).unwrap();
        // Spelling pin — JSON key MUST be `by_model` (snake_case, no
        // hyphens, no `byModel`).
        assert!(s.contains("\"by_model\""),
            "field must serialise as `by_model`: {s}");
        let back: DayEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(back, entry);
    }

    #[test] fn empty_by_model_omitted_from_json() {
        // PII / size hygiene: an empty by_model map MUST NOT serialise
        // as `"by_model":{}` — that'd inflate every entry forever once
        // we ship the schema. `#[serde(skip_serializing_if = "BTreeMap::is_empty")]`
        // is what makes this work; the test pins the policy.
        let entry = DayEntry {
            micro_units: 12,
            by_model: BTreeMap::new(),
            by_main:  BTreeMap::new(),
        };
        let s = serde_json::to_string(&entry).unwrap();
        assert!(!s.contains("by_model"),
            "empty by_model must be omitted from JSON; got: {s}");
        // v0.2 Thu addition: same policy must apply to `by_main`.
        assert!(!s.contains("by_main"),
            "empty by_main must be omitted from JSON; got: {s}");
        assert!(s.contains("\"micro_units\":12"),
            "micro_units must still serialise normally: {s}");
    }

    #[test] fn day_entry_v1_file_without_by_main_loads_cleanly() {
        // Week 13 Thu addition: just as v0.1 files lack `by_model`
        // and load cleanly via `#[serde(default)]`, v0.2-pre-Thu
        // files lack `by_main` and must also load cleanly without
        // a schema_version bump.  Forward-compat regression guard.
        let s = r#"{"micro_units":42,"by_model":{"openai.gpt_4o":42}}"#;
        let entry: DayEntry = serde_json::from_str(s)
            .expect("v0.2-pre-Thu shape must deserialize");
        assert_eq!(entry.micro_units, 42);
        assert_eq!(entry.by_model.len(), 1);
        assert!(entry.by_main.is_empty(),
            "by_main must default to empty when absent from JSON; got {entry:?}");
    }

    #[test] fn day_entry_v0_2_thu_full_shape_round_trips() {
        // The full week-13-Thu shape with by_main populated must
        // round-trip cleanly through serde + preserve every field.
        let entry = DayEntry {
            micro_units: 5_000,
            by_model:    [("anthropic.claude_haiku_4_5".to_string(), 5_000)]
                .into_iter().collect(),
            by_main:     [("support_orchestrator".to_string(), 1_200)]
                .into_iter().collect(),
        };
        let s    = serde_json::to_string(&entry).unwrap();
        assert!(s.contains("\"by_main\""),
            "populated by_main must serialise as `by_main`: {s}");
        let back: DayEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(back, entry);
    }

    // ---- utc_rfc3339_from_epoch_secs (pure, system-clock-free) ----

    #[test] fn rfc3339_epoch_zero_is_1970_01_01t00_00_00z() {
        assert_eq!(utc_rfc3339_from_epoch_secs(0),
                   "1970-01-01T00:00:00Z");
    }

    #[test] fn rfc3339_known_timestamps_match_iso_8601() {
        // 2026-05-20 00:00:00 UTC = 1779235200 (cross-checked with
        // `date -u -d '2026-05-20T00:00:00' +%s`). Adding 1h2m3s.
        let base = 1_779_235_200i64;
        assert_eq!(utc_rfc3339_from_epoch_secs(base),
                   "2026-05-20T00:00:00Z");
        assert_eq!(utc_rfc3339_from_epoch_secs(base + 3_723),
                   "2026-05-20T01:02:03Z");
        // End-of-day rollover: one second past 23:59:59 → next-day 00:00:00.
        assert_eq!(utc_rfc3339_from_epoch_secs(base + 86_400),
                   "2026-05-21T00:00:00Z");
    }

    // ---- week 10 Tue: add() populates by_model when model_id_str is set ----

    #[test] fn add_with_model_id_str_populates_by_model() {
        // Happy path: the empty by_model becomes a single-entry map
        // keyed by the canonical "<provider>.<name>" string.
        let dir  = tempdir();
        let path = dir.join("spend.json");
        let t = SpendTracker::at("demo", path.clone()).unwrap();
        t.add("anthropic.claude_haiku_4_5", &Money::usd(1_200)).unwrap();
        let by = t.today_by_model();
        assert_eq!(by.len(), 1);
        assert_eq!(by.get("anthropic.claude_haiku_4_5"),
            Some(&Money::usd(1_200)),
            "by_model entry must match the canonical model id and full cost");
        // Total moves too.
        assert_eq!(t.today_spend(), Money::usd(1_200));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn add_with_empty_model_id_str_skips_by_model() {
        // Graceful-degradation sentinel: an empty model_id_str means
        // "caller doesn't know the model". Total still moves; by_model
        // stays empty so we don't pollute the breakdown with a `"" -> N`
        // bucket.
        let dir  = tempdir();
        let path = dir.join("spend.json");
        let t = SpendTracker::at("demo", path.clone()).unwrap();
        t.add("", &Money::usd(500)).unwrap();
        assert_eq!(t.today_spend(), Money::usd(500));
        assert!(t.today_by_model().is_empty(),
            "empty model_id_str must not create a by_model bucket");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn add_accumulates_per_model_across_calls() {
        // Two calls to the same model → its by_model bucket sums; total
        // sums to the same. Two calls to different models → two
        // by_model buckets; total sums all.
        let dir  = tempdir();
        let path = dir.join("spend.json");
        let t = SpendTracker::at("demo", path.clone()).unwrap();
        t.add("anthropic.claude_haiku_4_5", &Money::usd(1_000)).unwrap();
        t.add("anthropic.claude_haiku_4_5", &Money::usd(500)).unwrap();
        t.add("openai.gpt_4o_mini",         &Money::usd(200)).unwrap();
        let by = t.today_by_model();
        assert_eq!(by.get("anthropic.claude_haiku_4_5"), Some(&Money::usd(1_500)));
        assert_eq!(by.get("openai.gpt_4o_mini"),         Some(&Money::usd(200)));
        assert_eq!(t.today_spend(),                       Money::usd(1_700),
            "total = sum of by_model buckets (1500+200)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn today_by_model_empty_when_no_data() {
        // Fresh tracker, no add() calls → today_by_model is empty.
        let dir  = tempdir();
        let path = dir.join("spend.json");
        let t = SpendTracker::at("demo", path).unwrap();
        assert!(t.today_by_model().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn add_unlimited_does_not_populate_by_model() {
        // Unlimited is skipped silently (already pinned by
        // add_unlimited_is_a_noop). Verify the by_model map ALSO
        // stays empty — no half-state where total skipped but model
        // bucket got 0.
        let dir  = tempdir();
        let path = dir.join("spend.json");
        let t = SpendTracker::at("demo", path).unwrap();
        t.add("anthropic.claude_haiku_4_5", &Money::Unlimited).unwrap();
        assert!(t.today_by_model().is_empty());
        assert_eq!(t.today_spend(), Money::usd(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn add_zero_cost_does_not_populate_by_model() {
        // Zero-cost (Ollama free-tier) skips disk write AND
        // by_model attribution. The local-model path stays a
        // by_model-free zone — keeps dashboards clean of "5 calls
        // costing nothing" noise.
        let dir  = tempdir();
        let path = dir.join("spend.json");
        let t = SpendTracker::at("demo", path).unwrap();
        t.add("ollama.llama_3_1_8b", &Money::usd(0)).unwrap();
        assert!(t.today_by_model().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn rfc3339_now_has_z_suffix_and_t_separator() {
        // Smoke-test the wall-clock variant. We can't predict the
        // exact value but we CAN pin the shape — 20 chars exactly,
        // `YYYY-MM-DDTHH:MM:SSZ`.
        let now = utc_rfc3339_now();
        assert_eq!(now.len(), 20, "RFC3339 minute-precision form is 20 chars; got `{now}`");
        assert_eq!(now.chars().nth(10), Some('T'),
            "char 11 must be `T` (date/time separator); got `{now}`");
        assert_eq!(now.chars().last(), Some('Z'),
            "final char must be `Z` (UTC zone); got `{now}`");
    }
}

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DayEntry {
    /// µ-USD. v0.1 USD-only (see module docs).
    pub micro_units: i64,
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

    /// Record one call's cost against today's spend + flush to disk
    /// atomically. `Money::Unlimited` costs are silently skipped
    /// (can't add unbounded to a finite total).
    pub fn add(&self, call_cost: &Money) -> Result<(), SpendError> {
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
            let entry  = by_day.entry(day).or_insert(DayEntry { micro_units: 0 });
            entry.micro_units = entry.micro_units.saturating_add(mu);
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

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aislang-spend-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
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
            t.add(&Money::usd(1_200)).unwrap();
            t.add(&Money::usd(800)).unwrap();
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
        t.add(&Money::Unlimited).unwrap();
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
        t.add(&Money::usd(0)).unwrap();
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
        t_a.add(&Money::usd(100)).unwrap();
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
        t.add(&Money::usd(100)).unwrap();
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
        t.add(&Money::usd(100)).unwrap();
        let after_first = std::fs::read_to_string(&path).unwrap();
        t.add(&Money::usd(200)).unwrap();
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
}

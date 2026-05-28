//! Week 10 Mon — applied-stack state file.
//!
//! `aislang apply` records what it just deployed, where, and when, in
//! a small JSON file at `<source_dir>/.aislang/state.json`. The file
//! powers three downstream workflows:
//!
//!   - **`aislang destroy` idempotency** (Wed): if the state file says
//!     the stack is already torn down (`applied_at: null`), destroy
//!     exits 0 immediately instead of erroring on missing stack.yml.
//!   - **`aislang status`** (Thu): renders project + applied-at +
//!     drift, queries `docker compose ps` to show service health.
//!   - **Compliance audit trail**: the file plus `aislang.lock` plus
//!     `spend.json` cover the three pillars EU AI Act Art. 15 + ISO
//!     42001 reviewers ask about — what was deployed (state.json),
//!     under what model resolution (lockfile), at what cost
//!     (spend.json).
//!
//! Decision 25 (PLAN week-10): path is `<source_dir>/.aislang/state.json`,
//! mirroring spend.json's hidden-dir grouping (modulo path location —
//! spend is in `$HOME`, state is project-local).
//!
//! Decision 26: schema is `schema_version`-prefixed for forward compat,
//! same pattern as `spend.json` + `aislang.lock`.
//!
//! Decision 30: state file is SEPARATE from the lockfile — distinct
//! lifecycles. Lockfile pins resolution-time decisions; state file
//! tracks apply-time facts.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

// ============================================================================
// Wire types — `state.json` schema
// ============================================================================

/// One applied (or formerly-applied) stack.
///
/// `applied_at: None` means the stack was torn down by a previous
/// `aislang destroy` — the file is kept for audit-trail continuity
/// rather than removed. `Some("<RFC3339 UTC>")` means the stack is
/// currently considered up (per the state file's own bookkeeping —
/// `aislang status` cross-checks with `docker compose ps` for the
/// actual runtime truth).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateFile {
    pub schema_version:  u32,
    /// RFC3339 UTC instant of the most recent successful apply.
    /// `None` (skipped from JSON) when the stack is torn down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at:      Option<String>,
    /// Hex SHA-256 of the `.ais` source file at apply time. Lets
    /// `aislang status` flag "source has changed since apply" drift
    /// without needing the source file to be re-hashed live (it
    /// might have moved or been edited).
    pub source_hash:     String,
    /// Catalog version that resolved the apply's model bindings.
    /// Same value as `aislang.lock`'s top-level `catalog_version` —
    /// recorded HERE so destroy + status work even when the lockfile
    /// has been deleted between apply and the next command.
    pub catalog_version: String,
    /// Absolute path to the emitted `stack.yml`. Recorded so
    /// `aislang destroy` can `docker compose -f <path> down -v` even
    /// when invoked from a different cwd than the apply was.
    pub stack_yaml_path: PathBuf,
    /// Project name (`project "<name>"` from the .ais). Surfaced by
    /// `aislang status`'s human-readable summary line.
    pub project_name:    String,
}

const CURRENT_SCHEMA_VERSION: u32 = 1;

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Error)]
pub enum StateError {
    #[error("state file `{path}` not readable: {cause}")]
    Io { path: String, cause: String },

    #[error("state file `{path}` not valid JSON: {cause}")]
    Decode { path: String, cause: String },

    #[error("state file `{path}` schema_version = {found}, expected {expected}. \
             aislang v0.1 doesn't know how to read this; delete the file or \
             use a newer aislang.")]
    SchemaMismatch { path: String, found: u32, expected: u32 },
}

// ============================================================================
// Public API
// ============================================================================

/// Default file location: `<source_dir>/.aislang/state.json`.
/// Pure — doesn't touch the filesystem; pair with `read` / `write_atomic`.
pub fn state_path_in(source_dir: &Path) -> PathBuf {
    source_dir.join(".aislang").join("state.json")
}

/// Read the state file. `Ok(None)` when the file is absent (the
/// "before first apply" case, which is normal — callers must NOT
/// treat absence as an error). `Ok(Some(_))` on a successful read.
/// `Err` on I/O / decode / schema-version mismatches.
pub fn read(path: &Path) -> Result<Option<StateFile>, StateError> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let f: StateFile = serde_json::from_str(&s).map_err(|e| StateError::Decode {
                path:  path.display().to_string(),
                cause: e.to_string(),
            })?;
            if f.schema_version != CURRENT_SCHEMA_VERSION {
                return Err(StateError::SchemaMismatch {
                    path:     path.display().to_string(),
                    found:    f.schema_version,
                    expected: CURRENT_SCHEMA_VERSION,
                });
            }
            Ok(Some(f))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StateError::Io {
            path:  path.display().to_string(),
            cause: e.to_string(),
        }),
    }
}

/// Atomically write the state file at `path` (creating the parent
/// `.aislang/` dir if needed). Tempfile + rename — on a kill -9 in
/// the middle of the write the prior file is untouched; on a kill
/// BEFORE rename the tempfile is orphaned but the original is safe.
/// Mirrors the pattern in `spend::SpendTracker::write_atomic`.
pub fn write_atomic(path: &Path, state: &StateFile) -> Result<(), StateError> {
    let json = serde_json::to_string_pretty(state).map_err(|e| StateError::Decode {
        path:  path.display().to_string(),
        cause: e.to_string(),
    })?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| StateError::Io {
            path:  parent.display().to_string(),
            cause: format!("mkdir: {e}"),
        })?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, json.as_bytes()).map_err(|e| StateError::Io {
        path:  tmp.display().to_string(),
        cause: format!("write: {e}"),
    })?;
    std::fs::rename(&tmp, path).map_err(|e| StateError::Io {
        path:  path.display().to_string(),
        cause: format!("rename: {e}"),
    })?;
    Ok(())
}

/// Mark the state as torn down: load the file (if present), set
/// `applied_at = None`, rewrite atomically. No-op when the file
/// doesn't exist (nothing to mark torn down). Called by week 10
/// Wed's `aislang destroy` after a successful `docker compose down`.
pub fn mark_torn_down(path: &Path) -> Result<(), StateError> {
    let Some(mut state) = read(path)? else { return Ok(()); };
    state.applied_at = None;
    write_atomic(path, &state)
}

/// Convenience constructor — populate a fresh `StateFile` for the
/// happy-path apply. `applied_at` is stamped to the current UTC
/// RFC3339 instant via `spend::utc_rfc3339_now`. Tests should
/// prefer building the struct directly so they can pin the
/// timestamp deterministically.
pub fn new_applied(
    project_name:    impl Into<String>,
    source_hash:     impl Into<String>,
    catalog_version: impl Into<String>,
    stack_yaml_path: PathBuf,
) -> StateFile {
    StateFile {
        schema_version:  CURRENT_SCHEMA_VERSION,
        applied_at:      Some(crate::spend::utc_rfc3339_now()),
        source_hash:     source_hash.into(),
        catalog_version: catalog_version.into(),
        stack_yaml_path,
        project_name:    project_name.into(),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Per-process atomic counter — guarantees uniqueness for
    // parallel tests regardless of system clock resolution.
    // See spend.rs's `tempdir()` for the macOS-microsecond
    // collision rationale.
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aislang-state-{}-{}-{}",
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

    fn sample_state() -> StateFile {
        StateFile {
            schema_version:  1,
            applied_at:      Some("2026-05-28T15:30:00Z".into()),
            source_hash:     "deadbeef".repeat(8),  // 64-hex (sha256 length)
            catalog_version: "2026-05-25".into(),
            stack_yaml_path: PathBuf::from("/abs/path/stack.yml"),
            project_name:    "support-agent".into(),
        }
    }

    // ---- path helper ----

    #[test] fn state_path_in_appends_dot_aislang_state_json() {
        let p = state_path_in(Path::new("/foo/bar"));
        assert_eq!(p, PathBuf::from("/foo/bar/.aislang/state.json"));
    }

    // ---- read ----

    #[test] fn read_returns_none_for_missing_file() {
        // "Before first apply" is the common case for new projects;
        // callers MUST get None, not an error.
        let dir = tempdir();
        let path = dir.join(".aislang/state.json");
        assert!(matches!(read(&path), Ok(None)),
            "missing file must return Ok(None), not Err");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn read_returns_some_for_valid_file() {
        let dir = tempdir();
        let path = dir.join("state.json");
        let s = sample_state();
        write_atomic(&path, &s).unwrap();
        let back = read(&path).unwrap().expect("must be Some");
        assert_eq!(back, s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn read_future_schema_version_errors_loudly() {
        // Forward-compat: a v2 file produced by a future aislang must
        // NOT silently load with v1 semantics. Surface the mismatch
        // so the user knows to upgrade aislang.
        let dir = tempdir();
        let path = dir.join("state.json");
        let json = r#"{
            "schema_version": 99,
            "applied_at":     "2030-01-01T00:00:00Z",
            "source_hash":    "abc",
            "catalog_version":"2030-x",
            "stack_yaml_path":"/tmp/stack.yml",
            "project_name":   "x"
        }"#;
        std::fs::write(&path, json).unwrap();
        match read(&path) {
            Err(StateError::SchemaMismatch { found: 99, expected: 1, .. }) => (),
            other => panic!("expected SchemaMismatch{{99,1}}, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn read_corrupt_json_errors_with_path() {
        let dir = tempdir();
        let path = dir.join("state.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        match read(&path) {
            Err(StateError::Decode { path: p, .. }) => {
                assert!(p.contains("state.json"),
                    "decode error must mention the path; got `{p}`");
            }
            other => panic!("expected Decode error, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- write_atomic ----

    #[test] fn write_atomic_round_trips() {
        let dir = tempdir();
        let path = dir.join(".aislang/state.json");
        let s = sample_state();
        write_atomic(&path, &s).unwrap();
        let back = read(&path).unwrap().expect("must be Some");
        assert_eq!(back, s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn write_atomic_creates_parent_dir() {
        // `.aislang/` doesn't exist in a fresh project dir; write
        // must mkdir -p, not error on ENOENT.
        let dir = tempdir();
        let path = dir.join(".aislang/state.json");
        assert!(!dir.join(".aislang").exists());
        write_atomic(&path, &sample_state()).unwrap();
        assert!(dir.join(".aislang").is_dir());
        assert!(path.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn write_atomic_leaves_no_tempfile() {
        // The rename step must clean up; no orphan `.tmp.<pid>` files
        // left behind in steady state.
        let dir = tempdir();
        let path = dir.join("state.json");
        write_atomic(&path, &sample_state()).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(),
            "tempfiles must be renamed away; found: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn write_atomic_overwrites_existing_file() {
        let dir = tempdir();
        let path = dir.join("state.json");
        write_atomic(&path, &sample_state()).unwrap();
        let mut updated = sample_state();
        updated.applied_at = Some("2027-01-01T00:00:00Z".into());
        write_atomic(&path, &updated).unwrap();
        let back = read(&path).unwrap().unwrap();
        assert_eq!(back.applied_at, Some("2027-01-01T00:00:00Z".into()),
            "second write must overwrite the first, not append");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- mark_torn_down ----

    #[test] fn mark_torn_down_nulls_applied_at() {
        let dir = tempdir();
        let path = dir.join("state.json");
        write_atomic(&path, &sample_state()).unwrap();
        mark_torn_down(&path).unwrap();
        let after = read(&path).unwrap().unwrap();
        assert_eq!(after.applied_at, None,
            "applied_at must be cleared after mark_torn_down");
        // Other fields preserved — audit trail intact.
        assert_eq!(after.project_name,    "support-agent");
        assert_eq!(after.catalog_version, "2026-05-25");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn mark_torn_down_is_idempotent() {
        // Calling mark_torn_down twice doesn't error — second call
        // just rewrites applied_at: null over applied_at: null.
        let dir = tempdir();
        let path = dir.join("state.json");
        write_atomic(&path, &sample_state()).unwrap();
        mark_torn_down(&path).unwrap();
        mark_torn_down(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn mark_torn_down_no_op_when_file_absent() {
        // Calling mark_torn_down on a missing file is a clean no-op
        // (matches Wed's destroy idempotency contract — destroy with
        // nothing to destroy is success).
        let dir = tempdir();
        let path = dir.join(".aislang/state.json");
        mark_torn_down(&path).unwrap();  // must NOT error
        assert!(!path.exists(),
            "no-op call must not create the file as a side-effect");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- applied_at JSON serialisation ----

    #[test] fn applied_at_none_omitted_from_json() {
        // `#[serde(skip_serializing_if = "Option::is_none")]` policy
        // pin: a torn-down state.json shouldn't carry `"applied_at": null`
        // — it should OMIT the field. Cleaner audit-trail diffs.
        let mut s = sample_state();
        s.applied_at = None;
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("applied_at"),
            "torn-down state must omit applied_at from JSON; got: {json}");
    }

    #[test] fn applied_at_some_carries_rfc3339_in_json() {
        let s = sample_state();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"applied_at\":\"2026-05-28T15:30:00Z\""),
            "applied_at must serialise as the RFC3339 string verbatim; got: {json}");
    }

    // ---- new_applied helper ----

    #[test] fn new_applied_stamps_current_rfc3339_timestamp() {
        let s = new_applied("p", "h", "c", PathBuf::from("/x/stack.yml"));
        let at = s.applied_at.expect("new_applied always sets applied_at");
        // Shape pin — 20-char UTC RFC3339 form.
        assert_eq!(at.len(), 20);
        assert_eq!(at.chars().last(), Some('Z'));
        assert_eq!(s.schema_version,  1);
        assert_eq!(s.project_name,    "p");
        assert_eq!(s.source_hash,     "h");
        assert_eq!(s.catalog_version, "c");
        assert_eq!(s.stack_yaml_path, PathBuf::from("/x/stack.yml"));
    }
}

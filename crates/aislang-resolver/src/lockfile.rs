//! `aislang.lock` — the resolution cache. Captures the per-block
//! decisions made during resolution so subsequent runs can reuse them
//! without re-querying the catalog, re-hashing prompt files, or
//! re-resolving OCI digests.
//!
//! Schema (v0.1):
//!   schema_version  : u32   — bump on format break; today: 1
//!   catalog_version : str   — Catalog.version at last resolve
//!   source_hash     : str   — SHA-256 hex of the .ais source bytes
//!   model_bindings  : map   — `model.label` → ModelId
//!   prompt_hashes   : map   — `prompt.label` → SHA-256 hex of file
//!   oci_digests     : map   — `mcp_server.label` → digest string
//!                              (empty if the user gave a tag-only
//!                              `image = "...:tag"` and no live
//!                              registry resolution has happened yet
//!                              — v0.2 adds registry lookup; v0.1
//!                              records whatever the AST has)
//!
//! Reconciliation: when reading an existing lockfile, compare against
//! current source + catalog. Source-hash mismatch or catalog-version
//! mismatch triggers a `Stale { reasons }` result so the CLI can show
//! the user exactly what changed and trigger a re-resolution.

use crate::ResolvedProject;
use aislang_core::ast::ModelId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

// ============================================================================
// Types
// ============================================================================

/// Current lockfile schema version. Bump when the on-disk format
/// changes in a non-backward-compatible way; older lockfiles then
/// trigger a `Stale` diff with a clear "schema mismatch" reason.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    pub schema_version:  u32,
    pub catalog_version: String,
    pub source_hash:     String,
    pub model_bindings:  BTreeMap<String, ModelId>,
    pub prompt_hashes:   BTreeMap<String, String>,
    pub oci_digests:     BTreeMap<String, String>,

    /// v0.3 Week 16 Wed — per-module source hashes for multi-module
    /// projects (Decision 44.1).  Empty for single-file projects
    /// (`source_hash` above is authoritative); populated by
    /// [`Lockfile::build`] when the resolved project carries a
    /// non-empty `module_source_hashes` map.
    ///
    /// Reconciliation (see [`reconcile`]): when this map is non-empty,
    /// EACH module's hash must match the current source; ANY mismatch
    /// (add / remove / change) triggers `Stale`.  Over-broad invalidation
    /// is deliberate per PLAN-v0_3.md Week 16 Wed row — per-module
    /// incremental is a v0.5+ carry-over.
    ///
    /// `#[serde(default, skip_serializing_if = BTreeMap::is_empty)]`
    /// keeps v0.2 lockfiles byte-stable (no `modules` field appears
    /// when the project is single-file) and lets v0.2 lockfiles
    /// deserialize cleanly on v0.3 (treated as single-file).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub modules: BTreeMap<String, String>,
}

#[derive(Debug, Error)]
pub enum LockfileError {
    #[error("read failed for `{}`: {source}", path.display())]
    Read { path: PathBuf, #[source] source: std::io::Error },

    #[error("parse failed for `{}`: {source}", path.display())]
    Parse { path: PathBuf, #[source] source: serde_json::Error },

    #[error("write failed for `{}`: {source}", path.display())]
    Write { path: PathBuf, #[source] source: std::io::Error },

    #[error("serialize failed: {source}")]
    Serialize { #[source] source: serde_json::Error },
}

/// Result of comparing an on-disk lockfile to the current source +
/// catalog state. Tells the CLI whether to reuse cached bindings or
/// re-resolve (and if re-resolving, what to mention in the diff).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockfileDiff {
    /// No lockfile on disk. Resolve fresh, write the new lockfile.
    Fresh,
    /// Lockfile matches current source + catalog. Reuse bindings.
    Match,
    /// Lockfile diverges. Re-resolve and rewrite; show `reasons` to
    /// the user so they understand what triggered it.
    Stale { reasons: Vec<String> },
}

// ============================================================================
// Construction
// ============================================================================

impl Lockfile {
    /// Build a fresh lockfile from a successful resolution.
    pub fn build(source: &str, resolved: &ResolvedProject) -> Self {
        let oci_digests = resolved
            .project
            .mcp_servers
            .iter()
            .map(|m| {
                (
                    m.label.clone(),
                    m.image.digest.clone().unwrap_or_default(),
                )
            })
            .collect();

        Self {
            schema_version:  SCHEMA_VERSION,
            catalog_version: resolved.catalog_version.clone(),
            source_hash:     sha256_hex(source.as_bytes()),
            model_bindings:  resolved.model_bindings.clone(),
            prompt_hashes:   resolved.prompt_hashes.clone(),
            oci_digests,
            // v0.3 Week 16 Wed: empty for single-file projects (the
            // `source_hash` field above is authoritative there);
            // populated by [`Lockfile::build_multi`] for multi-module.
            modules: BTreeMap::new(),
        }
    }

    /// v0.3 Week 16 Wed — build a lockfile from a multi-module
    /// [`ResolvedProject`].  Differences from [`Lockfile::build`]:
    ///
    /// - `source_hash` is set to the empty string — multi-module
    ///   projects have no single canonical source.  `modules` is the
    ///   source-of-truth.
    /// - `modules` is populated from `resolved.module_source_hashes`.
    ///
    /// Callers determine which builder to call based on whether the
    /// resolved project came from [`crate::resolve`] (single-file)
    /// or [`crate::resolve_multi`] (multi-module).
    pub fn build_multi(resolved: &ResolvedProject) -> Self {
        let oci_digests = resolved
            .project
            .mcp_servers
            .iter()
            .map(|m| (m.label.clone(), m.image.digest.clone().unwrap_or_default()))
            .collect();
        Self {
            schema_version:  SCHEMA_VERSION,
            catalog_version: resolved.catalog_version.clone(),
            source_hash:     String::new(),
            model_bindings:  resolved.model_bindings.clone(),
            prompt_hashes:   resolved.prompt_hashes.clone(),
            oci_digests,
            modules:         resolved.module_source_hashes.clone(),
        }
    }
}

// ============================================================================
// I/O
// ============================================================================

/// Read a lockfile from disk. Returns `Ok(None)` if the file doesn't
/// exist (first-run case); `Ok(Some(lockfile))` if it exists and
/// parses; `Err(...)` if it exists but can't be read or doesn't parse.
pub fn read(path: &Path) -> Result<Option<Lockfile>, LockfileError> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(LockfileError::Read { path: path.to_path_buf(), source: e }),
    };
    let lock: Lockfile = serde_json::from_str(&content)
        .map_err(|e| LockfileError::Parse { path: path.to_path_buf(), source: e })?;
    Ok(Some(lock))
}

/// Write a lockfile to disk. Pretty-printed JSON with a trailing
/// newline (POSIX convention; helps diff tools).
pub fn write(path: &Path, lockfile: &Lockfile) -> Result<(), LockfileError> {
    let mut content = serde_json::to_string_pretty(lockfile)
        .map_err(|e| LockfileError::Serialize { source: e })?;
    content.push('\n');
    std::fs::write(path, content)
        .map_err(|e| LockfileError::Write { path: path.to_path_buf(), source: e })
}

// ============================================================================
// Reconciliation
// ============================================================================

/// Compare a (possibly-absent) on-disk lockfile against the current
/// source bytes + catalog version + freshly-resolved prompt hashes.
/// The result tells the caller whether to reuse cached bindings or
/// re-resolve, and (if stale) lists the specific reasons.
///
/// `current_prompt_hashes` comes from the just-completed
/// `resolve()` run — it's the on-disk content hashes for every
/// `prompt.label`. We compare them against the lockfile's stored
/// hashes to detect "user edited a prompt file but didn't touch the
/// .ais source" — without this, the .ais-source-only check returns
/// `Match` and the lockfile keeps the stale hash forever, which then
/// trips the chat-time prompt-hash verification with a confusing
/// "Re-run aislang plan to refresh the lockfile" error (when the
/// user just DID).
pub fn reconcile(
    source:                  &str,
    existing:                Option<&Lockfile>,
    current_catalog_version: &str,
    current_prompt_hashes:   &BTreeMap<String, String>,
) -> LockfileDiff {
    let lock = match existing {
        None    => return LockfileDiff::Fresh,
        Some(l) => l,
    };

    let mut reasons = Vec::new();

    if lock.schema_version != SCHEMA_VERSION {
        reasons.push(format!(
            "lockfile schema version {} not supported by this aislang \
             (current: {})",
            lock.schema_version, SCHEMA_VERSION,
        ));
    }

    // v0.3 Week 16 Wed: skip the single-file source-hash check when
    // the lockfile carries a `modules` table — that lockfile was
    // written by a multi-module project and `source_hash` is empty
    // (sentinel; see `Lockfile::build_multi`).  Callers reconcile
    // multi-module lockfiles via [`reconcile_multi`] instead, which
    // checks the per-module hashes against the current sources.
    let current_source_hash = sha256_hex(source.as_bytes());
    if lock.modules.is_empty() && lock.source_hash != current_source_hash {
        reasons.push(format!(
            "source file changed (lockfile hash starts {}, current starts {})",
            short(&lock.source_hash),
            short(&current_source_hash),
        ));
    }

    if lock.catalog_version != current_catalog_version {
        reasons.push(format!(
            "catalog version changed: lockfile {}, current {}",
            lock.catalog_version, current_catalog_version,
        ));
    }

    // Prompt-file content changes (week-7 hotfix). Two failure modes:
    //   1. Same prompt label, different content: hash diff → stale.
    //   2. Prompt added / removed: key-set diff → stale.
    // We check both directions: every current entry must match
    // the lockfile, AND the lockfile must not carry stale entries
    // for prompts the user removed from the source.
    for (label, current_hash) in current_prompt_hashes {
        match lock.prompt_hashes.get(label) {
            Some(lock_hash) if lock_hash == current_hash => {}
            Some(lock_hash) => {
                reasons.push(format!(
                    "prompt `{label}` content changed (lockfile hash \
                     starts {}, current starts {})",
                    short(lock_hash),
                    short(current_hash),
                ));
            }
            None => {
                reasons.push(format!(
                    "prompt `{label}` added in source (not in lockfile)"));
            }
        }
    }
    for label in lock.prompt_hashes.keys() {
        if !current_prompt_hashes.contains_key(label) {
            reasons.push(format!(
                "prompt `{label}` removed from source (present in lockfile)"));
        }
    }

    if reasons.is_empty() {
        LockfileDiff::Match
    } else {
        LockfileDiff::Stale { reasons }
    }
}

/// v0.3 Week 16 Wed — reconcile a multi-module lockfile against the
/// current per-module source hashes.  Mirrors [`reconcile`] but
/// substitutes the single-file `source_hash` check with a per-module
/// hash diff.
///
/// `current_module_source_hashes` is keyed by module name; values
/// are SHA-256 hex of the raw source bytes (matching
/// [`crate::resolve_multi`]'s output).  Any module added, removed,
/// or changed since the lockfile was written triggers `Stale` with
/// per-module reasons — matching the prompt-hash diff style.
///
/// Per PLAN-v0_3.md Week 16 Wed: over-broad invalidation by design
/// (one module changes → re-resolve all modules).  Per-module
/// incremental rebuilds are v0.5+.
pub fn reconcile_multi(
    existing:                     Option<&Lockfile>,
    current_catalog_version:      &str,
    current_module_source_hashes: &BTreeMap<String, String>,
    current_prompt_hashes:        &BTreeMap<String, String>,
) -> LockfileDiff {
    let lock = match existing {
        None    => return LockfileDiff::Fresh,
        Some(l) => l,
    };

    let mut reasons = Vec::new();

    if lock.schema_version != SCHEMA_VERSION {
        reasons.push(format!(
            "lockfile schema version {} not supported by this aislang \
             (current: {})",
            lock.schema_version, SCHEMA_VERSION,
        ));
    }

    if lock.catalog_version != current_catalog_version {
        reasons.push(format!(
            "catalog version changed: lockfile {}, current {}",
            lock.catalog_version, current_catalog_version,
        ));
    }

    // Per-module source-hash diff.  Mirrors the prompt-hash diff
    // style (existing v0.2 code below) — three failure modes:
    //   1. Module modified  → hash differs
    //   2. Module added     → key in current, missing in lockfile
    //   3. Module removed   → key in lockfile, missing in current
    // Every difference is added as its own reason so the user sees
    // the exact module(s) involved rather than "something changed".
    for (mod_name, current_hash) in current_module_source_hashes {
        match lock.modules.get(mod_name) {
            Some(lock_hash) if lock_hash == current_hash => {}
            Some(lock_hash) => {
                reasons.push(format!(
                    "module `{mod_name}` source changed (lockfile hash \
                     starts {}, current starts {})",
                    short(lock_hash),
                    short(current_hash),
                ));
            }
            None => {
                reasons.push(format!(
                    "module `{mod_name}` added to manifest \
                     (not in lockfile)"));
            }
        }
    }
    for mod_name in lock.modules.keys() {
        if !current_module_source_hashes.contains_key(mod_name) {
            reasons.push(format!(
                "module `{mod_name}` removed from manifest \
                 (present in lockfile)"));
        }
    }

    // Prompt-hash diff — identical to single-file [`reconcile`]'s
    // logic.  Multi-module prompts use `<module>.<label>` keys, so
    // adding/removing a module is also reflected here as prompt-hash
    // key adds/removes; that's redundant with the module-hash diff
    // above but cheap and adds detail to the diagnostic.
    for (label, current_hash) in current_prompt_hashes {
        match lock.prompt_hashes.get(label) {
            Some(lock_hash) if lock_hash == current_hash => {}
            Some(lock_hash) => {
                reasons.push(format!(
                    "prompt `{label}` content changed (lockfile hash \
                     starts {}, current starts {})",
                    short(lock_hash),
                    short(current_hash),
                ));
            }
            None => {
                reasons.push(format!(
                    "prompt `{label}` added in source (not in lockfile)"));
            }
        }
    }
    for label in lock.prompt_hashes.keys() {
        if !current_prompt_hashes.contains_key(label) {
            reasons.push(format!(
                "prompt `{label}` removed from source (present in lockfile)"));
        }
    }

    if reasons.is_empty() {
        LockfileDiff::Match
    } else {
        LockfileDiff::Stale { reasons }
    }
}

// ============================================================================
// Internals
// ============================================================================

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// First 12 hex chars of a SHA-256 digest — used in diff messages so
/// users see something glanceable rather than the full 64-char string.
fn short(h: &str) -> &str {
    if h.len() >= 12 { &h[..12] } else { h }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use aislang_core::ast::ModelId;

    fn synthetic_lockfile() -> Lockfile {
        let mut bindings = BTreeMap::new();
        bindings.insert(
            "fast".to_string(),
            ModelId { provider: "test".into(), name: "active".into() },
        );
        let mut prompts = BTreeMap::new();
        prompts.insert("system".to_string(), "a".repeat(64));
        let mut digests = BTreeMap::new();
        digests.insert("docs".to_string(), String::new());
        Lockfile {
            schema_version:  SCHEMA_VERSION,
            catalog_version: "test-1".into(),
            source_hash:     "b".repeat(64),
            model_bindings:  bindings,
            prompt_hashes:   prompts,
            oci_digests:     digests,
            // v0.3 Week 16 Wed: single-file synthetic lockfile, so
            // modules stays empty (drives the "no `modules` field in
            // JSON" forward-compat test below).
            modules:         BTreeMap::new(),
        }
    }

    // ---------- I/O round-trip ----------

    #[test] fn write_then_read_round_trips() {
        let dir = std::env::temp_dir().join("aislang-lock-roundtrip");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.lock");
        let lock = synthetic_lockfile();
        write(&path, &lock).expect("write");
        let read_back = read(&path).expect("read").expect("Some");
        assert_eq!(lock, read_back);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn read_missing_returns_none() {
        let path = std::env::temp_dir().join("aislang-lock-does-not-exist.lock");
        let _ = std::fs::remove_file(&path);
        let result = read(&path).expect("read on missing file is Ok(None)");
        assert!(result.is_none());
    }

    #[test] fn read_garbage_returns_parse_error() {
        let dir = std::env::temp_dir().join("aislang-lock-garbage");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("garbage.lock");
        std::fs::write(&path, b"{ this is not valid json").unwrap();
        let err = read(&path).err().expect("must error");
        let s = format!("{err}");
        assert!(s.contains("parse failed"));
        assert!(s.contains("garbage.lock"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn written_lockfile_is_pretty_json_with_trailing_newline() {
        let dir = std::env::temp_dir().join("aislang-lock-prettiness");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pretty.lock");
        write(&path, &synthetic_lockfile()).expect("write");
        let bytes = std::fs::read(&path).unwrap();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.ends_with('\n'), "lockfile must end with a newline");
        assert!(s.contains('\n'), "lockfile must be pretty-printed (multi-line)");
        assert!(s.contains("\"schema_version\": 1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- reconcile() ----------

    /// Convenience: an empty current prompt-hashes map. Most pre-week-7
    /// reconcile tests don't exercise prompts.
    fn no_prompts() -> BTreeMap<String, String> { BTreeMap::new() }

    #[test] fn reconcile_no_existing_returns_fresh() {
        let diff = reconcile("source", None, "any-version", &no_prompts());
        assert_eq!(diff, LockfileDiff::Fresh);
    }

    #[test] fn reconcile_match_when_everything_aligned() {
        let source = "the source bytes";
        let mut lock = synthetic_lockfile();
        lock.source_hash = sha256_hex(source.as_bytes());
        // Ensure lockfile + current have identical prompt hash sets
        // (`synthetic_lockfile` populates two prompts).
        let diff = reconcile(source, Some(&lock), &lock.catalog_version,
                             &lock.prompt_hashes);
        assert_eq!(diff, LockfileDiff::Match);
    }

    #[test] fn reconcile_stale_when_source_changed() {
        let mut lock = synthetic_lockfile();
        lock.source_hash = sha256_hex(b"the old source");
        let diff = reconcile("the NEW source bytes", Some(&lock),
                             &lock.catalog_version, &lock.prompt_hashes);
        match diff {
            LockfileDiff::Stale { reasons } => {
                assert!(reasons.iter().any(|r| r.contains("source file changed")),
                    "expected 'source file changed' reason; got: {reasons:?}");
            }
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    #[test] fn reconcile_stale_when_catalog_changed() {
        let source = "src";
        let mut lock = synthetic_lockfile();
        lock.source_hash = sha256_hex(source.as_bytes());
        lock.catalog_version = "old-catalog".into();
        let diff = reconcile(source, Some(&lock), "new-catalog",
                             &lock.prompt_hashes);
        match diff {
            LockfileDiff::Stale { reasons } => {
                assert!(reasons.iter().any(|r|
                    r.contains("catalog version changed") &&
                    r.contains("old-catalog") &&
                    r.contains("new-catalog")
                ));
            }
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    #[test] fn reconcile_stale_collects_multiple_reasons() {
        // Both source-changed AND catalog-changed at once.
        let mut lock = synthetic_lockfile();
        lock.source_hash = sha256_hex(b"old source");
        lock.catalog_version = "old".into();
        let diff = reconcile("new source", Some(&lock), "new-cat",
                             &lock.prompt_hashes);
        match diff {
            LockfileDiff::Stale { reasons } => {
                assert_eq!(reasons.len(), 2,
                    "expected 2 reasons (source + catalog); got: {reasons:?}");
            }
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    #[test] fn reconcile_stale_on_unsupported_schema_version() {
        let mut lock = synthetic_lockfile();
        lock.schema_version = 999;
        lock.source_hash = sha256_hex(b"src");
        let diff = reconcile("src", Some(&lock), &lock.catalog_version,
                             &lock.prompt_hashes);
        match diff {
            LockfileDiff::Stale { reasons } => {
                assert!(reasons.iter().any(|r| r.contains("schema version 999")));
            }
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    // ---------- Prompt-hash reconciliation (week-7 hotfix) ----------
    //
    // Regression tests for the bug observed week 7 Thu:
    // user edited a prompt file (system.md) without touching the .ais
    // source, ran `aislang plan`, and the lockfile didn't refresh —
    // then `aislang chat` complained about a hash mismatch and told
    // them to re-run plan. The fix: reconcile() now compares
    // prompt_hashes too.

    #[test] fn reconcile_stale_when_prompt_content_changed() {
        let source = "src";
        let mut lock = synthetic_lockfile();
        lock.source_hash = sha256_hex(source.as_bytes());
        lock.prompt_hashes.insert("system".to_string(),
            "old_hash_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into());
        // Same label, different content hash.
        let mut current = BTreeMap::new();
        current.insert("system".to_string(),
            "new_hash_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into());
        let diff = reconcile(source, Some(&lock), &lock.catalog_version, &current);
        match diff {
            LockfileDiff::Stale { reasons } => {
                assert!(reasons.iter().any(|r|
                    r.contains("prompt `system` content changed")),
                    "expected `prompt content changed` reason; got: {reasons:?}");
            }
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    #[test] fn reconcile_stale_when_prompt_added_in_source() {
        let source = "src";
        let mut lock = synthetic_lockfile();
        lock.source_hash = sha256_hex(source.as_bytes());
        lock.prompt_hashes.clear();  // lockfile knows of no prompts
        let mut current = BTreeMap::new();
        current.insert("system".into(), "h".repeat(64));
        let diff = reconcile(source, Some(&lock), &lock.catalog_version, &current);
        match diff {
            LockfileDiff::Stale { reasons } => {
                assert!(reasons.iter().any(|r|
                    r.contains("prompt `system` added in source")),
                    "expected `prompt added` reason; got: {reasons:?}");
            }
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    #[test] fn reconcile_stale_when_prompt_removed_from_source() {
        let source = "src";
        let mut lock = synthetic_lockfile();
        lock.source_hash = sha256_hex(source.as_bytes());
        lock.prompt_hashes.insert("orphan".into(), "h".repeat(64));
        // Current has none.
        let diff = reconcile(source, Some(&lock), &lock.catalog_version,
                             &BTreeMap::new());
        match diff {
            LockfileDiff::Stale { reasons } => {
                assert!(reasons.iter().any(|r|
                    r.contains("prompt `orphan` removed from source")),
                    "expected `prompt removed` reason; got: {reasons:?}");
            }
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    #[test] fn reconcile_match_when_prompts_identical_across_runs() {
        // No drift on either side → Match. Pins that the prompt
        // check doesn't become a noisy false-positive.
        let source = "src";
        let mut lock = synthetic_lockfile();
        lock.source_hash = sha256_hex(source.as_bytes());
        lock.prompt_hashes.insert("system".into(), "h".repeat(64));
        let mut current = BTreeMap::new();
        current.insert("system".into(), "h".repeat(64));
        let diff = reconcile(source, Some(&lock), &lock.catalog_version, &current);
        assert_eq!(diff, LockfileDiff::Match);
    }

    // ---------- Lockfile::build ----------

    #[test] fn build_from_resolved_captures_all_fields() {
        use aislang_core::ast::*;
        let mut project = Project::default();
        project.name = "demo".to_string();
        // Add an mcp_server with an explicit digest so oci_digests
        // captures something non-empty.
        project.mcp_servers.push(McpServer {
            label: "docs".to_string(),
            image: aislang_core::types::OciImage {
                registry:   "ghcr.io".to_string(),
                repository: "x/y".to_string(),
                tag:        Some("1.0".to_string()),
                digest:     Some("sha256:abcdef".to_string()),
            },
            mount:   None,
            env:     None,
            secrets: None,
        });

        let mut model_bindings = BTreeMap::new();
        model_bindings.insert(
            "fast".to_string(),
            ModelId { provider: "anthropic".into(), name: "claude_haiku_4_5".into() },
        );
        let resolved = ResolvedProject {
            project:         project,
            catalog_version: "v-test".to_string(),
            model_bindings,
            prompt_hashes:   BTreeMap::new(),
            ..Default::default()
        };
        let source = "project \"demo\" { }";
        let lock = Lockfile::build(source, &resolved);

        assert_eq!(lock.schema_version,  SCHEMA_VERSION);
        assert_eq!(lock.catalog_version, "v-test");
        assert_eq!(lock.source_hash,     sha256_hex(source.as_bytes()));
        assert_eq!(lock.model_bindings.len(), 1);
        assert_eq!(lock.oci_digests.get("docs").unwrap(), "sha256:abcdef");
    }

    #[test] fn build_records_empty_digest_for_tag_only_image() {
        // mcp_server with image tag but no @sha256: digest — recorded
        // as empty string per the v0.1 schema. v0.2 may grow real
        // registry lookup here.
        use aislang_core::ast::*;
        let mut project = Project::default();
        project.mcp_servers.push(McpServer {
            label: "docs".to_string(),
            image: aislang_core::types::OciImage {
                registry:   "ghcr.io".to_string(),
                repository: "x/y".to_string(),
                tag:        Some("1.0".to_string()),
                digest:     None,
            },
            mount: None, env: None, secrets: None,
        });
        let resolved = ResolvedProject {
            project,
            catalog_version: "v-test".to_string(),
            model_bindings: BTreeMap::new(),
            prompt_hashes:  BTreeMap::new(),
            ..Default::default()
        };
        let lock = Lockfile::build("src", &resolved);
        assert_eq!(lock.oci_digests.get("docs").unwrap(), "");
    }

    // ---------------------------------------------------------------- //
    // v0.3 Week 16 Wed — Lockfile multi-module fields + reconcile_multi
    // ---------------------------------------------------------------- //

    /// Helper: build a synthetic multi-module ResolvedProject for the
    /// build_multi + reconcile_multi tests.  Two modules `m1` and `m2`
    /// with distinct source hashes; one prompt per module.
    fn synthetic_multi_resolved() -> ResolvedProject {
        let mut hashes = BTreeMap::new();
        hashes.insert("m1".to_string(), "a".repeat(64));
        hashes.insert("m2".to_string(), "b".repeat(64));
        let mut prompts = BTreeMap::new();
        prompts.insert("m1.sys".to_string(), "1".repeat(64));
        prompts.insert("m2.sys".to_string(), "2".repeat(64));
        ResolvedProject {
            catalog_version: "test-cat-1".to_string(),
            module_source_hashes: hashes,
            prompt_hashes: prompts,
            ..Default::default()
        }
    }

    #[test] fn build_multi_clears_source_hash_and_populates_modules() {
        let resolved = synthetic_multi_resolved();
        let lock = Lockfile::build_multi(&resolved);
        // source_hash is the sentinel-empty value — modules is auth.
        assert_eq!(lock.source_hash, "",
            "build_multi must NOT populate source_hash (modules drives it)");
        assert_eq!(lock.modules.len(), 2);
        assert!(lock.modules.contains_key("m1"));
        assert!(lock.modules.contains_key("m2"));
        assert_eq!(lock.modules["m1"], resolved.module_source_hashes["m1"]);
    }

    #[test] fn modules_field_omitted_from_json_when_empty() {
        // Forward-compat: single-file lockfiles must serialize WITHOUT
        // the `modules` field so v0.2 lockfile fixtures (if any) stay
        // byte-stable + the typical single-file lockfile stays
        // visually clean.
        let lock = synthetic_lockfile(); // single-file (modules empty)
        let json = serde_json::to_string(&lock).expect("serialize");
        assert!(!json.contains("\"modules\""),
            "single-file lockfile must NOT carry a `modules` field; \
             got: {json}");
    }

    #[test] fn modules_field_round_trips_through_json_when_populated() {
        let resolved = synthetic_multi_resolved();
        let lock = Lockfile::build_multi(&resolved);
        let json = serde_json::to_string(&lock).expect("serialize");
        assert!(json.contains("\"modules\""),
            "multi-module lockfile must serialize `modules` field; got: {json}");
        let back: Lockfile = serde_json::from_str(&json).expect("round-trip");
        assert_eq!(back.modules, lock.modules);
    }

    #[test] fn v02_lockfile_without_modules_field_deserializes_cleanly() {
        // Pin forward-compat: a JSON shape with no `modules` key
        // (v0.2 on-disk format) must deserialize on v0.3 with
        // modules = empty BTreeMap.  This protects users who upgrade
        // mid-project from spurious schema-mismatch errors.
        let v02_json = r#"{
            "schema_version":   1,
            "catalog_version":  "x",
            "source_hash":      "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "model_bindings":   {},
            "prompt_hashes":    {},
            "oci_digests":      {}
        }"#;
        let lock: Lockfile = serde_json::from_str(v02_json).expect("parse v0.2");
        assert!(lock.modules.is_empty(),
            "missing `modules` field must default to empty");
    }

    #[test] fn reconcile_multi_matches_when_hashes_align() {
        let resolved = synthetic_multi_resolved();
        let lock = Lockfile::build_multi(&resolved);
        let diff = reconcile_multi(
            Some(&lock),
            &resolved.catalog_version,
            &resolved.module_source_hashes,
            &resolved.prompt_hashes,
        );
        assert_eq!(diff, LockfileDiff::Match,
            "all hashes identical → reconcile_multi must report Match");
    }

    #[test] fn reconcile_multi_stale_when_one_module_changed() {
        let resolved = synthetic_multi_resolved();
        let lock = Lockfile::build_multi(&resolved);
        let mut current = resolved.module_source_hashes.clone();
        // Mutate m1's source hash — simulate user editing m1.ais.
        current.insert("m1".to_string(), "c".repeat(64));
        let diff = reconcile_multi(
            Some(&lock),
            &resolved.catalog_version,
            &current,
            &resolved.prompt_hashes,
        );
        match diff {
            LockfileDiff::Stale { reasons } => {
                assert!(reasons.iter().any(|r| r.contains("module `m1`")),
                    "reasons must name the changed module; got: {reasons:?}");
                assert!(reasons.iter().any(|r| r.contains("source changed")),
                    "reasons must explain what changed; got: {reasons:?}");
            }
            other => panic!("expected Stale; got {other:?}"),
        }
    }

    #[test] fn reconcile_multi_stale_when_module_added_to_manifest() {
        let resolved = synthetic_multi_resolved();
        let lock = Lockfile::build_multi(&resolved);
        let mut current = resolved.module_source_hashes.clone();
        current.insert("m3".to_string(), "d".repeat(64));
        let diff = reconcile_multi(
            Some(&lock),
            &resolved.catalog_version,
            &current,
            &resolved.prompt_hashes,
        );
        match diff {
            LockfileDiff::Stale { reasons } => {
                assert!(reasons.iter().any(|r|
                    r.contains("module `m3`") && r.contains("added")),
                    "reasons must name the newly-added module; got: {reasons:?}");
            }
            other => panic!("expected Stale; got {other:?}"),
        }
    }

    #[test] fn reconcile_multi_stale_when_module_removed_from_manifest() {
        let resolved = synthetic_multi_resolved();
        let lock = Lockfile::build_multi(&resolved);
        let mut current = resolved.module_source_hashes.clone();
        current.remove("m2");
        let diff = reconcile_multi(
            Some(&lock),
            &resolved.catalog_version,
            &current,
            &resolved.prompt_hashes,
        );
        match diff {
            LockfileDiff::Stale { reasons } => {
                assert!(reasons.iter().any(|r|
                    r.contains("module `m2`") && r.contains("removed")),
                    "reasons must name the removed module; got: {reasons:?}");
            }
            other => panic!("expected Stale; got {other:?}"),
        }
    }

    #[test] fn reconcile_multi_fresh_when_no_lockfile() {
        let resolved = synthetic_multi_resolved();
        let diff = reconcile_multi(
            None,
            &resolved.catalog_version,
            &resolved.module_source_hashes,
            &resolved.prompt_hashes,
        );
        assert_eq!(diff, LockfileDiff::Fresh,
            "no on-disk lockfile → Fresh, same as the single-file case");
    }

    #[test] fn reconcile_single_file_ignores_modules_field_when_empty() {
        // Pin that a single-file Lockfile (modules empty) still
        // reconciles via the legacy source_hash check unchanged.
        // This is the regression-guard for v0.2 lockfiles after the
        // Wed schema extension.
        let mut lock = synthetic_lockfile();   // modules: empty
        let source = "project \"demo\" { }";
        lock.source_hash = sha256_hex(source.as_bytes());
        // Reuse the legacy reconcile() — must still report Match.
        let diff = reconcile(
            source, Some(&lock), &lock.catalog_version, &lock.prompt_hashes);
        assert_eq!(diff, LockfileDiff::Match);
    }
}

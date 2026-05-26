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
/// source bytes + catalog version. The result tells the caller whether
/// to reuse cached bindings or re-resolve, and (if stale) lists the
/// specific reasons for the user.
pub fn reconcile(
    source:                  &str,
    existing:                Option<&Lockfile>,
    current_catalog_version: &str,
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

    let current_source_hash = sha256_hex(source.as_bytes());
    if lock.source_hash != current_source_hash {
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

    #[test] fn reconcile_no_existing_returns_fresh() {
        let diff = reconcile("source", None, "any-version");
        assert_eq!(diff, LockfileDiff::Fresh);
    }

    #[test] fn reconcile_match_when_everything_aligned() {
        let source = "the source bytes";
        let mut lock = synthetic_lockfile();
        lock.source_hash = sha256_hex(source.as_bytes());
        let diff = reconcile(source, Some(&lock), &lock.catalog_version);
        assert_eq!(diff, LockfileDiff::Match);
    }

    #[test] fn reconcile_stale_when_source_changed() {
        let mut lock = synthetic_lockfile();
        lock.source_hash = sha256_hex(b"the old source");
        let diff = reconcile("the NEW source bytes", Some(&lock), &lock.catalog_version);
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
        let diff = reconcile(source, Some(&lock), "new-catalog");
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
        let diff = reconcile("new source", Some(&lock), "new-cat");
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
        let diff = reconcile("src", Some(&lock), &lock.catalog_version);
        match diff {
            LockfileDiff::Stale { reasons } => {
                assert!(reasons.iter().any(|r| r.contains("schema version 999")));
            }
            other => panic!("expected Stale, got {other:?}"),
        }
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
        };
        let lock = Lockfile::build("src", &resolved);
        assert_eq!(lock.oci_digests.get("docs").unwrap(), "");
    }
}

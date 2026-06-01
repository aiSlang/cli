//! v0.3 Week 16 Mon — multi-module project manifest (`aislang.toml`).
//!
//! Decision 44.1 (pre-locked at v0.3 Sunday): `aislang.toml` declares
//! the **project root** + the **set of `*.ais` modules** that compose
//! it.  Single-file projects (everything v0.2 ships) work unchanged —
//! the CLI auto-detects "no `aislang.toml` in this directory" and
//! treats the `.ais` file as a 1-file project.
//!
//! ## Schema (Decision 44.1)
//!
//! ```toml
//! [project]
//! name    = "support-stack"
//! version = "0.1.0"
//!
//! [modules]
//! architect = "agents/architect.ais"
//! coder     = "agents/coder.ais"
//! reviewer  = "agents/reviewer.ais"
//! ```
//!
//! - `[project] name` (required, String) — project label, parallel to
//!   the `project "<name>" { … }` declaration inside a `.ais` file.
//!   For multi-module projects the in-`.ais` `project` keyword block
//!   is dropped (Week 16 Wed wires this); for v0.2 single-file projects
//!   the `.ais` keeps the block.
//! - `[project] version` (required, String) — semver-compatible.  Used
//!   by the lockfile schema (Week 16 Wed).
//! - `[modules]` (required, map) — `<module-name>` → `<relative-path>`.
//!   Convention: module name = file stem (e.g. `architect.ais` exports
//!   `architect.<resource>`); the explicit table makes the rename
//!   path painless + matches Cargo's `[lib] path =` idiom.
//!
//! Forward-compat: unknown top-level tables / fields are ignored at
//! parse time (Decision 44.1 sidebar — same pattern as v0.1's
//! `skip_serializing_if` on AST fields).  Tue Week 16 may grow
//! `[dependencies]` for cross-project linking; Wed grows the lockfile
//! schema.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Parsed `aislang.toml` manifest.  Returned by [`parse_manifest_str`]
/// (parse from text) and [`load_manifest`] (parse from a directory).
///
/// `modules` is a `BTreeMap` (not `HashMap`) so iteration order is
/// deterministic — every downstream artifact (resolver project hash,
/// lockfile module list, K8s ConfigMap items) is stable across runs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProjectManifest {
    pub project: ProjectMeta,

    /// Module name → path relative to the manifest's directory.
    ///
    /// `serde(default)` so an empty / missing `[modules]` table parses
    /// to an empty map — multi-module projects need ≥1 module, but
    /// the emptiness check happens in [`ProjectManifest::validate`]
    /// (returns a structured error with a helpful hint) rather than
    /// in serde (returns a generic "missing field" message).
    #[serde(default)]
    pub modules: BTreeMap<String, PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProjectMeta {
    pub name:    String,
    pub version: String,
}

/// Errors returned by [`parse_manifest_str`] / [`load_manifest`] /
/// [`ProjectManifest::validate`].
///
/// Variant-per-failure-mode so the CLI can render a source-aware
/// diagnostic (file path + line/col from serde when available) +
/// pick the right exit code.
#[derive(Debug)]
pub enum ManifestError {
    /// `toml::from_str` returned a parse / deserialization error.
    /// `inner` carries the original `toml::de::Error` (preserves
    /// span info the CLI can render).  The display impl prefixes
    /// with the manifest path when known.
    Parse { path: Option<PathBuf>, inner: toml::de::Error },

    /// `std::fs::read_to_string` failed.
    Io { path: PathBuf, source: std::io::Error },

    /// Post-deserialize validation rule rejected.  Used for "modules
    /// table empty" + future structural rules that serde can't express.
    Invalid { path: Option<PathBuf>, message: String },
}

impl std::error::Error for ManifestError {}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse { path, inner } => {
                if let Some(p) = path {
                    write!(f, "failed to parse {}: {inner}", p.display())
                } else {
                    write!(f, "failed to parse aislang.toml: {inner}")
                }
            }
            Self::Io { path, source } => write!(
                f, "failed to read {}: {source}", path.display()),
            Self::Invalid { path, message } => {
                if let Some(p) = path {
                    write!(f, "invalid manifest {}: {message}", p.display())
                } else {
                    write!(f, "invalid manifest: {message}")
                }
            }
        }
    }
}

/// Parse a manifest from in-memory TOML text.  Used by tests +
/// indirectly by [`load_manifest`].
///
/// Does NOT run [`ProjectManifest::validate`] — call it explicitly
/// once you've enriched the error context (e.g. attached the file
/// path).  Keeping parse + validate separate matches how
/// `aislang_core::validate` splits parser + checker.
pub fn parse_manifest_str(text: &str) -> Result<ProjectManifest, ManifestError> {
    toml::from_str::<ProjectManifest>(text)
        .map_err(|inner| ManifestError::Parse { path: None, inner })
}

impl ProjectManifest {
    /// Post-deserialize structural validation.  Today: `[modules]`
    /// table must be non-empty (a manifest with no modules is a
    /// pointless artifact — the caller should just delete it +
    /// fall back to single-file mode).
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.modules.is_empty() {
            return Err(ManifestError::Invalid {
                path:    None,
                message: "`[modules]` table is empty; either declare \
                         at least one module or remove `aislang.toml` \
                         and fall back to single-file mode".to_string(),
            });
        }
        Ok(())
    }
}

/// Filename the manifest discovery functions look for.  Pinned as a
/// constant so future-grep `"aislang.toml"` finds one source of truth.
pub const MANIFEST_FILE_NAME: &str = "aislang.toml";

/// Attempt to load + parse + validate the manifest in `dir`.
///
/// Three outcomes — distinguished by callers so the CLI can pick the
/// right diagnostic / fallback:
///
/// - `Ok(Some(manifest))` — manifest exists, parses, validates.
/// - `Ok(None)` — manifest does NOT exist at `dir/aislang.toml`.
///   Callers fall back to v0.2 single-file mode.
/// - `Err(_)` — manifest exists but failed to read / parse / validate.
///   Callers surface the error unmodified (full path + line + reason).
pub fn load_manifest(dir: &Path) -> Result<Option<ProjectManifest>, ManifestError> {
    let path = dir.join(MANIFEST_FILE_NAME);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|source| {
        ManifestError::Io { path: path.clone(), source }
    })?;
    let manifest = toml::from_str::<ProjectManifest>(&text)
        .map_err(|inner| ManifestError::Parse {
            path: Some(path.clone()),
            inner,
        })?;
    manifest.validate().map_err(|e| match e {
        ManifestError::Invalid { message, .. } => ManifestError::Invalid {
            path: Some(path), message,
        },
        other => other,
    })?;
    Ok(Some(manifest))
}

/// Walk `dir` (one level deep — Decision 44.1: modules live where the
/// manifest says they live, the walker is for "what does this project
/// look like on disk" UX, not for resource discovery).  Returns all
/// `*.ais` files sorted by path (deterministic order, matches every
/// other emitter's sort-before-emit rule).
///
/// Used by:
///
/// - Tue Week 16: cross-module symbol-table construction will iterate
///   the manifest's `[modules]` table (NOT this walker — the manifest
///   is authoritative).  The walker is for the `aislang validate <dir>`
///   diagnostic "found N orphan `.ais` files in the project root that
///   aren't listed in `[modules]`" hint.
/// - Today: tests + the single-file fallback (when no manifest exists,
///   the CLI picks the single discovered `.ais` and treats it as the
///   project).
pub fn discover_ais_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path  = entry.path();
        if path.is_file()
            && path.extension().and_then(|e| e.to_str()) == Some("ais")
        {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

// ========================================================================== //
// Tests
// ========================================================================== //

#[cfg(test)]
mod tests {
    use super::*;

    // ------- parse_manifest_str: happy paths ------- //

    #[test] fn parses_minimal_manifest_with_one_module() {
        let text = r#"
[project]
name    = "demo"
version = "0.1.0"

[modules]
only = "only.ais"
"#;
        let m = parse_manifest_str(text).expect("parse");
        assert_eq!(m.project.name,    "demo");
        assert_eq!(m.project.version, "0.1.0");
        assert_eq!(m.modules.len(), 1);
        assert_eq!(m.modules.get("only"), Some(&PathBuf::from("only.ais")));
        m.validate().expect("validate");
    }

    #[test] fn parses_manifest_with_nested_module_paths() {
        // Module paths can be sub-directory relative — agents/architect.ais
        // is the canonical example.  Round-trips the path verbatim.
        let text = r#"
[project]
name    = "demo"
version = "0.1.0"

[modules]
architect = "agents/architect.ais"
coder     = "agents/coder.ais"
"#;
        let m = parse_manifest_str(text).expect("parse");
        assert_eq!(m.modules.len(), 2);
        assert_eq!(
            m.modules.get("architect"),
            Some(&PathBuf::from("agents/architect.ais")));
        assert_eq!(
            m.modules.get("coder"),
            Some(&PathBuf::from("agents/coder.ais")));
    }

    #[test] fn modules_iterate_in_sorted_order() {
        // BTreeMap pins iteration order — downstream artifacts depend
        // on stable module order (lockfile module list, hash inputs).
        let text = r#"
[project]
name    = "demo"
version = "0.1.0"

[modules]
zebra = "z.ais"
alpha = "a.ais"
mango = "m.ais"
"#;
        let m = parse_manifest_str(text).expect("parse");
        let keys: Vec<&String> = m.modules.keys().collect();
        assert_eq!(keys, vec!["alpha", "mango", "zebra"]);
    }

    // ------- parse_manifest_str: serde errors ------- //

    #[test] fn rejects_missing_project_section() {
        let text = r#"
[modules]
only = "only.ais"
"#;
        let err = parse_manifest_str(text).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("project"),
            "error should mention the missing `project` section; \
             got: {msg}");
    }

    #[test] fn rejects_missing_project_name() {
        let text = r#"
[project]
version = "0.1.0"
"#;
        let err = parse_manifest_str(text).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("name"),
            "error should name the missing `name` field; got: {msg}");
    }

    #[test] fn rejects_missing_project_version() {
        let text = r#"
[project]
name = "demo"
"#;
        let err = parse_manifest_str(text).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("version"),
            "error should name the missing `version` field; got: {msg}");
    }

    // ------- ProjectManifest::validate ------- //

    #[test] fn validate_rejects_empty_modules_table() {
        // No [modules] table at all — serde(default) parses to empty
        // map, validate must catch it.
        let text = r#"
[project]
name    = "demo"
version = "0.1.0"
"#;
        let m   = parse_manifest_str(text).expect("parse");
        let err = m.validate().unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("modules") && msg.contains("empty"),
            "validate should name the empty modules table; got: {msg}");
        assert!(msg.contains("single-file"),
            "validate should hint at the single-file fallback; \
             got: {msg}");
    }

    // ------- Forward-compat: unknown tables / fields ------- //

    #[test] fn ignores_unknown_top_level_tables() {
        // Forward-compat: future versions may add [dependencies]
        // (Tue Week 16), [workspace] (v0.4 multi-project), …; today's
        // parser must not reject manifests carrying them.
        let text = r#"
[project]
name    = "demo"
version = "0.1.0"

[modules]
only = "only.ais"

[future_table]
foo = "bar"
"#;
        let m = parse_manifest_str(text).expect(
            "unknown top-level tables must be ignored \
             for forward-compat");
        assert_eq!(m.project.name, "demo");
    }

    // ------- load_manifest: filesystem paths ------- //

    #[test] fn load_manifest_returns_none_when_file_absent() {
        // Single-file fallback signal: no aislang.toml → Ok(None).
        let dir = std::env::temp_dir().join(format!(
            "aislang-manifest-absent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let result = load_manifest(&dir).expect(
            "absent manifest is Ok(None), not Err");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.is_none(),
            "no aislang.toml → fallback signal");
    }

    #[test] fn load_manifest_reads_and_validates_from_disk() {
        let dir = std::env::temp_dir().join(format!(
            "aislang-manifest-present-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join(MANIFEST_FILE_NAME), r#"
[project]
name    = "from-disk"
version = "0.2.0"

[modules]
hello = "hello.ais"
"#).expect("write manifest");
        let m = load_manifest(&dir).expect("load")
            .expect("file present → Some");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(m.project.name,    "from-disk");
        assert_eq!(m.project.version, "0.2.0");
    }

    #[test] fn load_manifest_attaches_path_to_parse_error() {
        // Parse errors must carry the source path so the CLI's
        // diagnostic names the bad file.
        let dir = std::env::temp_dir().join(format!(
            "aislang-manifest-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let bad = dir.join(MANIFEST_FILE_NAME);
        std::fs::write(&bad, "this is not toml = = =")
            .expect("write bad");
        let err = load_manifest(&dir).unwrap_err();
        let msg = format!("{err}");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(msg.contains("aislang.toml"),
            "error should name the bad file; got: {msg}");
    }

    // ------- discover_ais_files ------- //

    #[test] fn discover_walks_one_level_and_sorts() {
        let dir = std::env::temp_dir().join(format!(
            "aislang-discover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("zebra.ais"),  "").expect("z");
        std::fs::write(dir.join("alpha.ais"),  "").expect("a");
        std::fs::write(dir.join("mango.ais"),  "").expect("m");
        // Distractors that must NOT match.
        std::fs::write(dir.join("README.md"),  "").expect("md");
        std::fs::write(dir.join("aislang.toml"),"").expect("toml");

        let files = discover_ais_files(&dir).expect("walk");
        let _ = std::fs::remove_dir_all(&dir);
        let names: Vec<String> = files.iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["alpha.ais", "mango.ais", "zebra.ais"]);
    }

    #[test] fn discover_on_nonexistent_dir_returns_empty_vec() {
        let dir = std::env::temp_dir().join(format!(
            "aislang-discover-missing-{}", std::process::id()));
        // Intentionally NOT created.
        let files = discover_ais_files(&dir).expect("non-error");
        assert!(files.is_empty());
    }
}

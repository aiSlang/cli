//! v0.3 Week 17 Tue — project input loading, dir-or-file dispatch.
//!
//! Hoisted from `aislang-cli` so `aislang mcp-serve`'s
//! `aislang_chat_turn` MCP tool can use the same dispatcher.
//! Single-file inputs follow the v0.2 pipeline (read → validate →
//! catalog-check → resolve); directory inputs walk `aislang.toml`
//! → per-module parse → `check_multi` → `resolve_multi`.
//!
//! ## Why this lives in `aislang-runtime` rather than `aislang-cli`
//!
//! mcp-serve is the chat-time entry point that needs to:
//! - find the agent across modules in the merged project,
//! - read prompt bodies from the right module's source dir,
//! - cache the merged `ResolvedProject` per `(project_root,
//!   agent_label)` session key.
//!
//! All three concerns are downstream consumers of [`LoadedProject`],
//! so the loader sits in the runtime crate (where mcp-serve already
//! lives) + the CLI calls it as a library function.  The CLI wraps
//! the result with K8s-specific `ProjectSources` separately (K8s
//! lives in its own crate to keep this one free of `aislang-k8s`'s
//! kubeapi-dep weight).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aislang_core::ParseError;
use aislang_resolver::{ModuleInput, ResolvedProject};

/// Output of [`load_project_input`].  Carries everything mcp-serve
/// + the CLI need to operate on a project regardless of whether the
/// input was a single `.ais` file or a multi-module dir.
#[derive(Debug)]
pub struct LoadedProject {
    /// Resolved project — catalog bindings + prompt hashes.  Single-
    /// file: bare-label keys.  Multi-module: `<module>.<label>` keys
    /// per the Wed Week 16 resolver contract.
    pub resolved: ResolvedProject,

    /// In-core validate + catalog warnings, accumulated.  For multi-
    /// module each warning is prefixed `module `<name>`: …`.
    pub warnings: Vec<aislang_core::Warning>,

    /// True when the input was a dir holding `aislang.toml`.  Drives
    /// downstream branches: lockfile reconciler picks
    /// `reconcile_multi` vs `reconcile`; K8s emitter picks the
    /// per-module CM layout; mcp-serve picks the cross-module agent
    /// lookup path.
    pub is_multi_module: bool,

    /// Project-root directory.  Single-file: parent of the `.ais`
    /// file.  Multi-module: the manifest's directory.  Lockfile +
    /// emitted artifacts live here.
    pub project_root: PathBuf,

    /// Single-file: full `.ais` source bytes (used by the v0.2
    /// `reconcile`).  Multi-module: empty — per-module hashes in
    /// `resolved.module_source_hashes` drive `reconcile_multi`.
    pub single_file_source: String,

    /// Project name (state.json + display).  Single-file: the
    /// `project "<name>" { … }` block's label.  Multi-module:
    /// `aislang.toml`'s `[project] name`.
    pub project_name: String,

    /// Per-module `.ais` source bytes.  Empty for single-file.  CLI
    /// uses this to populate `aislang_k8s::ProjectSources.modules`.
    pub modules: BTreeMap<String, String>,

    /// Raw `aislang.toml` body for multi-module; `None` for single-
    /// file.  CLI uses this to populate `ProjectSources.manifest`.
    pub manifest: Option<String>,

    /// Per-prompt bodies.  Single-file: keyed by `prompt.label`.
    /// Multi-module: keyed by `<module>.<label>` matching the
    /// resolver's `prompt_hashes` convention.
    pub prompt_bodies: BTreeMap<String, String>,

    /// v0.3 Week 17 Tue follow-up — per-module canonical source
    /// directory (where each module's `.ais` file lives).  Used by
    /// the multi-module session builder to resolve RELATIVE paths
    /// inside each module's blocks (`mcp_server.mount`,
    /// future `skill.exec_dir`, …) against the AUTHOR's mental
    /// model: paths are relative to the `.ais` file the author
    /// wrote them in, NOT the project root.
    ///
    /// Example: if `agents/coder.ais` declares
    /// `mcp_server "docs" { mount = "../knowledge/" }`, the path
    /// resolves against `agents/` → `examples/multi-module-demo/
    /// knowledge/`, NOT against the project root (which would have
    /// landed at `examples/knowledge/`).
    ///
    /// Empty for single-file projects (`project_root` is the
    /// authoritative source dir there).
    pub module_dirs: BTreeMap<String, PathBuf>,
}

impl LoadedProject {
    /// Deterministic content hash used by state.json.  Single-file:
    /// SHA-256 of the `.ais` source bytes (matches v0.2 lockfile
    /// `source_hash`).  Multi-module: SHA-256 of a stable
    /// concatenation `module:<name>||<body>||...` (mirrors the K8s
    /// pod-template `aislang.project-hash` shape, minus manifest +
    /// lockfile since state.json wants source-inputs only).
    pub fn effective_source_hash(&self) -> String {
        if self.is_multi_module {
            let mut combined = Vec::with_capacity(2048);
            for (name, body) in &self.modules {
                combined.extend_from_slice(b"module:");
                combined.extend_from_slice(name.as_bytes());
                combined.extend_from_slice(b"||");
                combined.extend_from_slice(body.as_bytes());
                combined.extend_from_slice(b"||");
            }
            crate::session::hex_sha256(&combined)
        } else {
            crate::session::hex_sha256(self.single_file_source.as_bytes())
        }
    }

    /// v0.3 Week 17 Tue — agent lookup that honors the multi-module
    /// `<module>.<agent>` qualified label convention.
    ///
    /// `aislang chat <agent> <dir>` lets the user type just the bare
    /// agent label (`coder`) without remembering the module prefix
    /// — `find_agent_label` resolves that against the merged project's
    /// agent list:
    ///
    /// - **Exact match** wins first.  Pinned for single-file projects
    ///   (`coder` matches `coder` directly) + the user-explicit
    ///   `<module>.<agent>` form (`coder.coder` matches `coder.coder`
    ///   directly).
    /// - **Suffix match on `.<input>`** picks the unique module-
    ///   qualified label ending in `.<input>` — `coder` matches
    ///   `coder.coder` (only one such suffix in multi-module-demo).
    ///   Ambiguous suffix matches return `None` so the caller can
    ///   prompt the user for the explicit `<module>.<agent>` form.
    ///
    /// Returns the matched label verbatim (e.g. `coder.coder` for
    /// multi-module dispatch).  Caller uses it as the
    /// `agent_label` to look up in `resolved.project.agents`.
    pub fn find_agent_label(&self, input: &str) -> Option<String> {
        // Exact-match fast path — covers single-file + explicit
        // <module>.<agent> form.
        if self.resolved.project.agents.iter().any(|a| a.label == input) {
            return Some(input.to_string());
        }
        // Suffix match: a label ending in `.<input>` is the multi-
        // module dispatch from a bare agent name.
        let suffix = format!(".{input}");
        let matches: Vec<&str> = self.resolved.project.agents.iter()
            .map(|a| a.label.as_str())
            .filter(|l| l.ends_with(&suffix))
            .collect();
        if matches.len() == 1 {
            return Some(matches[0].to_string());
        }
        // Either zero matches (unknown agent) or multiple matches
        // (ambiguous between modules — user must disambiguate via
        // the explicit `<module>.<agent>` form).
        None
    }

    /// v0.3 Week 17 Thu — `aislang chat <pipeline-label> <project>`
    /// dispatches through `patterns::orchestrator::run`.  Same
    /// resolution rules as [`find_agent_label`]: exact match, then
    /// unique suffix match.
    ///
    /// Returns the qualified pipeline label (e.g. `coder.review_cycle`
    /// for multi-module) so callers can look it up in
    /// `resolved.project.pipelines` directly.
    pub fn find_pipeline_label(&self, input: &str) -> Option<String> {
        if self.resolved.project.pipelines.iter().any(|p| p.label == input) {
            return Some(input.to_string());
        }
        let suffix = format!(".{input}");
        let matches: Vec<&str> = self.resolved.project.pipelines.iter()
            .map(|p| p.label.as_str())
            .filter(|l| l.ends_with(&suffix))
            .collect();
        if matches.len() == 1 {
            return Some(matches[0].to_string());
        }
        None
    }

    /// v0.3 Week 17 Fri — `aislang chat <branch-label> <project>`
    /// dispatches through `patterns::router::route` then
    /// `patterns::orchestrator::run` against the matched case.  Same
    /// resolution rules as [`find_agent_label`] /
    /// [`find_pipeline_label`].
    pub fn find_branch_label(&self, input: &str) -> Option<String> {
        if self.resolved.project.branches.iter().any(|b| b.label == input) {
            return Some(input.to_string());
        }
        let suffix = format!(".{input}");
        let matches: Vec<&str> = self.resolved.project.branches.iter()
            .map(|b| b.label.as_str())
            .filter(|l| l.ends_with(&suffix))
            .collect();
        if matches.len() == 1 {
            return Some(matches[0].to_string());
        }
        None
    }
}

/// Errors `load_project_input` can produce.  Variant-per-failure-
/// mode so callers (CLI + mcp-serve) can render context-appropriate
/// diagnostics — CLI prints source-aware caret diagnostics for
/// `ParseSingle` / `ParseModule`; mcp-serve flattens to an MCP
/// error message.
#[derive(Debug)]
pub enum ProjectLoadError {
    /// Filesystem read failed.  Path is baked into `message`.
    Io { message: String },

    /// `aislang.toml` missing / unparseable / fails the non-empty-
    /// modules-table rule.
    Manifest { message: String },

    /// Single-file parse error.  Caller has full source + structured
    /// errors so it can render line:col + caret diagnostics.
    ParseSingle {
        source: String,
        errors: Vec<ParseError>,
    },

    /// Multi-module parse error.  Same shape as `ParseSingle` but
    /// the module name is part of the surface so the CLI's stderr
    /// preamble can name which file failed.
    ParseModule {
        module: String,
        source: String,
        errors: Vec<ParseError>,
    },

    /// Checker, catalog, or resolver rejected the input.
    Semantic { message: String },
}

impl std::fmt::Display for ProjectLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io       { message }    => write!(f, "{message}"),
            Self::Manifest { message }    => write!(f, "{message}"),
            Self::Semantic { message }    => write!(f, "{message}"),
            Self::ParseSingle { errors, .. } => {
                writeln!(f, "parse error:")?;
                for e in errors { writeln!(f, "  {}", e.message)?; }
                Ok(())
            }
            Self::ParseModule { module, errors, .. } => {
                writeln!(f, "module `{module}`: parse error:")?;
                for e in errors { writeln!(f, "  {}", e.message)?; }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ProjectLoadError {}

/// Load a project from a CLI positional path arg.  Auto-detects
/// dir vs file and dispatches to the matching pipeline.
pub fn load_project_input(input: &Path) -> Result<LoadedProject, ProjectLoadError> {
    if input.is_dir() {
        load_multi_module_project(input)
    } else {
        load_single_file_project(input)
    }
}

fn load_single_file_project(file: &Path) -> Result<LoadedProject, ProjectLoadError> {
    let source = std::fs::read_to_string(file).map_err(|e| ProjectLoadError::Io {
        message: format!("read {}: {e}", file.display()),
    })?;
    // `Path::parent` returns `Some("")` (NOT None) for single-
    // component relative paths like `foo.ais` — guard the empty
    // case so canonicalize doesn't choke on `""`.
    let source_dir = match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => std::path::Path::new("."),
    };

    let (project, mut warnings) = match aislang_core::validate(&source) {
        Ok(v)  => v,
        Err(aislang_core::ValidateError::Parse(errors)) => {
            return Err(ProjectLoadError::ParseSingle { source, errors });
        }
        Err(other) => {
            return Err(ProjectLoadError::Semantic {
                message: format!("{other}"),
            });
        }
    };

    let cat_warnings = aislang_resolver::check_against_bundled_catalog(&project)
        .map_err(|e| ProjectLoadError::Semantic {
            message: format!("catalog error: {e}"),
        })?;
    warnings.extend(cat_warnings);

    let resolved = aislang_resolver::resolve_against_bundled_catalog(&project, source_dir)
        .map_err(|e| ProjectLoadError::Semantic {
            message: format!("resolve error: {e}"),
        })?;

    let mut prompt_bodies = BTreeMap::new();
    for prompt in &project.prompts {
        let abs = if prompt.file.is_absolute() {
            prompt.file.clone()
        } else {
            source_dir.join(&prompt.file)
        };
        let body = std::fs::read_to_string(&abs).map_err(|e| ProjectLoadError::Io {
            message: format!(
                "read prompt `{}` from {}: {e}", prompt.label, abs.display()),
        })?;
        prompt_bodies.insert(prompt.label.clone(), body);
    }

    let project_name = resolved.project.name.clone();
    Ok(LoadedProject {
        resolved,
        warnings,
        is_multi_module: false,
        project_root: source_dir.to_path_buf(),
        single_file_source: source,
        project_name,
        modules: BTreeMap::new(),
        manifest: None,
        prompt_bodies,
        module_dirs: BTreeMap::new(),
    })
}

fn load_multi_module_project(dir: &Path) -> Result<LoadedProject, ProjectLoadError> {
    let manifest = aislang_core::manifest::load_manifest(dir)
        .map_err(|e| ProjectLoadError::Manifest { message: format!("{e}") })?
        .ok_or_else(|| ProjectLoadError::Manifest {
            message: format!(
                "no `aislang.toml` in directory `{}`; either point at a `.ais` \
                 file directly or add a manifest declaring `[modules]`",
                dir.display()),
        })?;
    let manifest_body = std::fs::read_to_string(
        dir.join(aislang_core::manifest::MANIFEST_FILE_NAME))
        .map_err(|e| ProjectLoadError::Io {
            message: format!("read aislang.toml: {e}"),
        })?;

    let mut module_inputs:   BTreeMap<String, ModuleInput>           = BTreeMap::new();
    let mut module_projects: BTreeMap<String, aislang_core::ast::Project> = BTreeMap::new();
    let mut module_sources:  BTreeMap<String, String>                = BTreeMap::new();
    let mut module_prompt_bodies: BTreeMap<String, String>           = BTreeMap::new();
    let mut module_dirs:     BTreeMap<String, PathBuf>               = BTreeMap::new();
    let mut warnings = Vec::new();

    for (name, rel_path) in &manifest.modules {
        let abs = dir.join(rel_path);
        let source = std::fs::read_to_string(&abs).map_err(|e| ProjectLoadError::Io {
            message: format!("module `{name}`: read {}: {e}", abs.display()),
        })?;
        // Parse-only per module — the single-file `validate()` runs
        // the checker which errors on any cross-module `use` ref.
        // Multi-module checking happens below via `check_multi`.
        let project = aislang_core::parser::parse(&source).map_err(|errors|
            ProjectLoadError::ParseModule {
                module: name.clone(),
                source: source.clone(),
                errors,
            })?;

        let module_dir = abs.parent().unwrap_or(dir).to_path_buf();
        for prompt in &project.prompts {
            let prompt_abs = if prompt.file.is_absolute() {
                prompt.file.clone()
            } else {
                module_dir.join(&prompt.file)
            };
            let body = std::fs::read_to_string(&prompt_abs)
                .map_err(|e| ProjectLoadError::Io {
                    message: format!(
                        "module `{name}`: read prompt `{}` from {}: {e}",
                        prompt.label, prompt_abs.display()),
                })?;
            module_prompt_bodies.insert(format!("{name}.{}", prompt.label), body);
        }

        module_projects.insert(name.clone(), project.clone());
        module_sources.insert(name.clone(), source.clone());
        // Canonicalize the module's dir so downstream relative-path
        // resolution (mcp_server.mount, future skill exec dirs, ...)
        // operates on stable absolute paths regardless of the CWD
        // the user invoked aislang from.
        let canonical_module_dir = std::fs::canonicalize(&module_dir)
            .unwrap_or_else(|_| module_dir.clone());
        module_dirs.insert(name.clone(), canonical_module_dir);
        module_inputs.insert(name.clone(), ModuleInput {
            project,
            source,
            source_dir: module_dir,
        });
    }

    let check_warnings = aislang_core::checker::check_multi(&module_projects)
        .map_err(|e| ProjectLoadError::Semantic {
            message: format!("validate error: {e}"),
        })?;
    warnings.extend(check_warnings);

    for (name, project) in &module_projects {
        let cat = aislang_resolver::check_against_bundled_catalog(project)
            .map_err(|e| ProjectLoadError::Semantic {
                message: format!("module `{name}` catalog error: {e}"),
            })?;
        warnings.extend(cat.into_iter().map(|w| aislang_core::Warning {
            message: format!("module `{name}`: {}", w.message),
        }));
    }

    let catalog = aislang_catalog::bundled()
        .map_err(|e| ProjectLoadError::Semantic {
            message: format!("bundled catalog load: {e}"),
        })?;
    let resolved = aislang_resolver::resolve_multi(&module_inputs, &catalog)
        .map_err(|e| ProjectLoadError::Semantic {
            message: format!("resolve error: {e}"),
        })?;

    Ok(LoadedProject {
        resolved,
        warnings,
        is_multi_module: true,
        project_root: dir.to_path_buf(),
        single_file_source: String::new(),
        project_name: manifest.project.name.clone(),
        modules: module_sources,
        manifest: Some(manifest_body),
        prompt_bodies: module_prompt_bodies,
        module_dirs,
    })
}

// ========================================================================== //
// Tests
// ========================================================================== //

#[cfg(test)]
mod tests {
    use super::*;

    fn examples_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..").join("..")
            .join("examples")
    }

    #[test] fn loads_single_file_v02_fixture_byte_stably() {
        let path = examples_dir().join("support-agent.ais");
        let loaded = load_project_input(&path).expect("v0.2 fixture must load");
        assert!(!loaded.is_multi_module);
        assert_eq!(loaded.project_name, "support-agent");
        assert!(!loaded.single_file_source.is_empty());
        assert!(loaded.modules.is_empty(),
            "single-file must NOT populate modules");
        assert!(loaded.manifest.is_none());
        // The single-file path should expose at least the demo agent.
        assert!(loaded.resolved.project.agents.iter().any(|a| a.label == "support"));
    }

    #[test] fn loads_multi_module_demo_fixture() {
        let dir = examples_dir().join("multi-module-demo");
        let loaded = load_project_input(&dir).expect("dir fixture must load");
        assert!(loaded.is_multi_module);
        assert_eq!(loaded.project_name, "multi-module-demo");
        assert!(loaded.single_file_source.is_empty(),
            "multi-module must NOT populate single_file_source");
        assert_eq!(loaded.modules.len(), 2);
        assert!(loaded.modules.contains_key("architect"));
        assert!(loaded.modules.contains_key("coder"));
        assert!(loaded.manifest.is_some());
        // Merged project agents have module-qualified labels.
        let labels: Vec<&str> = loaded.resolved.project.agents.iter()
            .map(|a| a.label.as_str()).collect();
        assert!(labels.contains(&"architect.architect"),
            "merged agent labels are module-qualified; got: {labels:?}");
        assert!(labels.contains(&"coder.coder"),
            "merged agent labels are module-qualified; got: {labels:?}");
    }

    #[test] fn find_agent_label_exact_match_for_single_file() {
        let path = examples_dir().join("support-agent.ais");
        let loaded = load_project_input(&path).expect("load");
        // Single-file projects keep bare labels — exact match wins.
        assert_eq!(loaded.find_agent_label("support"),
                   Some("support".to_string()));
    }

    #[test] fn find_agent_label_suffix_match_for_multi_module() {
        let dir = examples_dir().join("multi-module-demo");
        let loaded = load_project_input(&dir).expect("load");
        // User types `coder` → loader resolves to `coder.coder`.
        assert_eq!(loaded.find_agent_label("coder"),
                   Some("coder.coder".to_string()));
        // Explicit module-qualified form also works.
        assert_eq!(loaded.find_agent_label("architect.architect"),
                   Some("architect.architect".to_string()));
    }

    #[test] fn find_agent_label_returns_none_for_unknown_input() {
        let path = examples_dir().join("support-agent.ais");
        let loaded = load_project_input(&path).expect("load");
        assert_eq!(loaded.find_agent_label("ghost"), None);
    }

    #[test] fn effective_source_hash_differs_single_vs_multi() {
        // Pin that single-file vs multi-module produce different
        // hashes for the same conceptual project — state.json keys
        // shouldn't collide across the two input shapes.
        let s = load_project_input(&examples_dir().join("support-agent.ais"))
            .expect("single-file");
        let m = load_project_input(&examples_dir().join("multi-module-demo"))
            .expect("multi-module");
        let h_s = s.effective_source_hash();
        let h_m = m.effective_source_hash();
        assert_ne!(h_s, h_m,
            "single-file + multi-module must produce distinct state hashes");
        assert_eq!(h_s.len(), 64, "SHA-256 hex is 64 chars; got: {h_s}");
        assert_eq!(h_m.len(), 64);
    }
}

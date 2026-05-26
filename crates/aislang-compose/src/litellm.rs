//! LiteLLM proxy config generation — `litellm_config.yaml`.
//!
//! LiteLLM is a Python service that accepts a YAML config defining
//! a *model list* (aliases users invoke, each mapped to a concrete
//! provider/model pair) plus optional router settings (fallbacks,
//! retry, rate-limit). This module generates that file from a
//! [`ResolvedProject`] + a [`Catalog`].
//!
//! v0.1 Monday: only the `model_list` is populated — one entry per
//! `model.label`, mapped to the catalog entry's `litellm_name`. No
//! API-key plumbing (Wed) or fallback routing (Thu) yet.

use aislang_catalog::Catalog;
use aislang_resolver::ResolvedProject;
use serde::Serialize;
use thiserror::Error;

// ============================================================================
// Public types
// ============================================================================

/// Top-level shape of `litellm_config.yaml`. Documented at
/// <https://docs.litellm.ai/docs/proxy/configs>.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiteLlmConfig {
    pub model_list: Vec<ModelEntry>,
    // Thursday: pub router_settings: Option<RouterSettings>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelEntry {
    /// The alias users invoke (matches `model.label` from .ais source).
    /// Stays stable across LLM-provider swaps — that's the whole point
    /// of the indirection.
    pub model_name: String,
    pub litellm_params: LiteLlmParams,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiteLlmParams {
    /// Provider-prefixed model name in LiteLLM's convention:
    /// `anthropic/claude-haiku-4-5`, `openai/gpt-4o-mini`,
    /// `ollama/llama3.1:8b`, etc. Sourced from the catalog entry's
    /// `litellm_name` field.
    pub model: String,
    // Wednesday: pub api_key: Option<String>,    // "os.environ/KEY"
    // Thursday will not add fields here — fallbacks live in
    // router_settings on the top-level config, not per-entry.
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("catalog entry `{0}.{1}` not found (referenced from resolved model binding) \
             — was the lockfile written against a different catalog version?")]
    MissingCatalogEntry(String, String),

    #[error("YAML serialization failed: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

// ============================================================================
// Public API
// ============================================================================

/// Build the typed `LiteLlmConfig` from a resolution + a catalog.
/// Each entry in `resolved.model_bindings` becomes one `model_list`
/// row. Order is deterministic — iteration over the `BTreeMap` is
/// sorted by label, so generated YAML is diff-stable.
pub fn build_config(resolved: &ResolvedProject, catalog: &Catalog)
    -> Result<LiteLlmConfig, ConfigError>
{
    let mut model_list = Vec::with_capacity(resolved.model_bindings.len());
    for (label, model_id) in &resolved.model_bindings {
        let entry = catalog.find(model_id).ok_or_else(||
            ConfigError::MissingCatalogEntry(
                model_id.provider.clone(),
                model_id.name.clone(),
            ))?;
        model_list.push(ModelEntry {
            model_name: label.clone(),
            litellm_params: LiteLlmParams {
                model: entry.litellm_name.clone(),
            },
        });
    }
    Ok(LiteLlmConfig { model_list })
}

/// Same as [`build_config`] but returns the YAML-serialized string
/// ready to write to disk (or mount into the litellm container).
pub fn generate_config(resolved: &ResolvedProject, catalog: &Catalog)
    -> Result<String, ConfigError>
{
    let cfg = build_config(resolved, catalog)?;
    Ok(serde_yaml::to_string(&cfg)?)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use aislang_core::ast::ModelId;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn examples_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..").join("..").join("examples")
    }

    fn resolve_demo() -> (ResolvedProject, Catalog) {
        let src = std::fs::read_to_string(examples_dir().join("support-agent.ais"))
            .expect("read demo");
        let (project, _) = aislang_core::validate(&src).expect("validate");
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = aislang_resolver::resolve(&project, &catalog, &examples_dir())
            .expect("resolve");
        (resolved, catalog)
    }

    // ---------- happy path ----------

    #[test] fn demo_emits_one_entry_per_model_block() {
        let (resolved, catalog) = resolve_demo();
        let cfg = build_config(&resolved, &catalog).expect("build");
        // Demo has exactly one `model` block: `fast`.
        assert_eq!(cfg.model_list.len(), 1);
        let entry = &cfg.model_list[0];
        assert_eq!(entry.model_name, "fast");
        // Demo resolves to ollama.llama_3_1_8b under cost-opt → its
        // catalog `litellm_name` is `ollama/llama3.1:8b`.
        assert_eq!(entry.litellm_params.model, "ollama/llama3.1:8b");
    }

    #[test] fn yaml_round_trips_through_serde_yaml() {
        let (resolved, catalog) = resolve_demo();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        // Structural validity: parse it back.
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml)
            .expect("emitted YAML must parse back");
        // Sanity: top-level `model_list` is a sequence.
        assert!(parsed["model_list"].as_sequence().is_some());
    }

    #[test] fn yaml_contains_expected_keys() {
        let (resolved, catalog) = resolve_demo();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        assert!(yaml.contains("model_list:"));
        assert!(yaml.contains("model_name: fast"));
        assert!(yaml.contains("litellm_params:"));
        assert!(yaml.contains("model: ollama/llama3.1:8b"));
    }

    // ---------- multi-model determinism ----------

    /// Synthetic resolved project with multiple model bindings so we
    /// can pin the ordering invariant (BTreeMap sorts by label).
    fn synthetic_multi_model() -> (ResolvedProject, Catalog) {
        use aislang_core::ast::Project;
        let mut bindings = BTreeMap::new();
        bindings.insert("zeta".to_string(),
            ModelId { provider: "ollama".into(), name: "llama_3_1_8b".into() });
        bindings.insert("alpha".to_string(),
            ModelId { provider: "anthropic".into(), name: "claude_haiku_4_5".into() });
        bindings.insert("middle".to_string(),
            ModelId { provider: "openai".into(), name: "gpt_4o_mini".into() });

        let resolved = ResolvedProject {
            project:         Project { name: "x".into(), ..Project::default() },
            catalog_version: "test".into(),
            model_bindings:  bindings,
            prompt_hashes:   BTreeMap::new(),
        };
        let catalog = aislang_catalog::bundled().expect("bundled");
        (resolved, catalog)
    }

    #[test] fn multiple_bindings_emitted_in_sorted_order() {
        let (resolved, catalog) = synthetic_multi_model();
        let cfg = build_config(&resolved, &catalog).expect("build");
        let names: Vec<&str> = cfg.model_list.iter()
            .map(|e| e.model_name.as_str()).collect();
        // BTreeMap iteration is alphabetical — alpha, middle, zeta.
        assert_eq!(names, vec!["alpha", "middle", "zeta"]);
    }

    #[test] fn determinism_two_builds_produce_identical_yaml() {
        let (resolved, catalog) = synthetic_multi_model();
        let y1 = generate_config(&resolved, &catalog).expect("first");
        let y2 = generate_config(&resolved, &catalog).expect("second");
        assert_eq!(y1, y2, "back-to-back generation must be byte-identical");
    }

    // ---------- error cases ----------

    #[test] fn missing_catalog_entry_errors() {
        use aislang_core::ast::Project;
        let mut bindings = BTreeMap::new();
        // ID that's NOT in the bundled catalog — simulates a
        // lockfile-vs-catalog mismatch.
        bindings.insert("fast".to_string(),
            ModelId { provider: "anthropic".into(), name: "phantom_v99".into() });

        let resolved = ResolvedProject {
            project:         Project { name: "x".into(), ..Project::default() },
            catalog_version: "test".into(),
            model_bindings:  bindings,
            prompt_hashes:   BTreeMap::new(),
        };
        let catalog = aislang_catalog::bundled().expect("bundled");

        let err = build_config(&resolved, &catalog).err()
            .expect("missing entry must error");
        let s = format!("{err}");
        assert!(s.contains("anthropic.phantom_v99"),
            "error must name the missing entry; got: {s}");
        assert!(s.contains("not found"));
    }

    // ---------- empty case ----------

    #[test] fn empty_model_bindings_yields_empty_model_list() {
        use aislang_core::ast::Project;
        let resolved = ResolvedProject {
            project:         Project { name: "x".into(), ..Project::default() },
            catalog_version: "test".into(),
            model_bindings:  BTreeMap::new(),
            prompt_hashes:   BTreeMap::new(),
        };
        let catalog = aislang_catalog::bundled().expect("bundled");
        let cfg = build_config(&resolved, &catalog).expect("build");
        assert!(cfg.model_list.is_empty());
    }

    #[test] fn yaml_serialization_uses_block_style() {
        // serde_yaml defaults to block-style for non-trivial structures
        // (which is what LiteLLM expects). Quick guard — flow-style would
        // produce `model_list: [{...}]` on one line, harder to read in
        // the file and unusual for config files.
        let (resolved, catalog) = resolve_demo();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        assert!(yaml.contains("\n  litellm_params:\n"),
            "expected block-style nesting; got:\n{yaml}");
    }
}

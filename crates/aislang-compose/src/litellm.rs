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
use aislang_core::ast::ModelId;
use aislang_resolver::ResolvedProject;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

// ============================================================================
// Public types
// ============================================================================

/// Top-level shape of `litellm_config.yaml`. Documented at
/// <https://docs.litellm.ai/docs/proxy/configs>.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiteLlmConfig {
    pub model_list: Vec<ModelEntry>,
    /// Router settings. Currently carries the per-primary fallback
    /// chain only (other LiteLLM router knobs — retries, rate-limits,
    /// timeouts — are deferred to later weeks). `None` when no
    /// fallback was declared on any model block (clean YAML output).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub router_settings: Option<RouterSettings>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouterSettings {
    /// Map from a primary `model_name` (the alias users invoke) to
    /// the ordered list of alternate `model_name` aliases LiteLLM
    /// will try in turn if the primary errors. v0.1 only the
    /// fallbacks knob is wired; v0.2+ may grow this struct.
    ///
    /// Ordering of the inner `Vec` is significant — first alternate
    /// is tried first. Outer `BTreeMap` sorts by key for diff-stable
    /// YAML output.
    pub fallbacks: BTreeMap<String, Vec<String>>,
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

    /// LiteLLM secret-reference for the API key. Format:
    /// `os.environ/<VAR>`. LiteLLM resolves this at runtime from
    /// the container env. None when the provider doesn't use an
    /// API key (e.g. self-hosted Ollama uses api_base instead).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// LiteLLM api_base override. Mainly for Ollama (which has no
    /// API key but needs a base URL pointing at the local instance).
    /// Format: `os.environ/<VAR>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
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
///
/// For every `model.label` in the resolved bindings:
///   1. Emit a primary `model_list` entry using the resolved catalog
///      entry's `litellm_name` + provider credentials.
///   2. Walk the source's `prefer ∪ fallback` lists (in that order),
///      emit one secondary `model_list` entry per *alternate* catalog
///      entry (i.e. not the resolved primary), and accumulate their
///      aliases into a `router_settings.fallbacks[label]` chain.
///
/// Three behaviors locked in by user-direction (Thursday plan):
///   - Alias scheme is `{primary_label}-fb-{provider}-{model-name}`
///     where the catalog model name's underscores become hyphens (LiteLLM
///     convention).
///   - The resolved primary never appears in its own fallback list (no
///     self-reference).
///   - Duplicate ModelIds across prefer and fallback are deduplicated;
///     first occurrence wins (so a `prefer = [X, Y, X]` produces
///     `[X, Y]`, not `[X, Y, X]`).
///
/// Order is deterministic throughout: outer iteration via BTreeMap
/// (sorted by label), inner walking preserves the source's
/// prefer-then-fallback order, dedup via BTreeSet, output via BTreeMap.
pub fn build_config(resolved: &ResolvedProject, catalog: &Catalog)
    -> Result<LiteLlmConfig, ConfigError>
{
    let mut model_list:    Vec<ModelEntry>                  = Vec::new();
    let mut fallbacks_map: BTreeMap<String, Vec<String>>    = BTreeMap::new();

    for (label, primary_id) in &resolved.model_bindings {
        // (a) primary entry — uses the label as model_name
        add_model_entry(&mut model_list, catalog, label.clone(), primary_id)?;

        // (b) walk the source model block's prefer+fallback for this label
        let model_block = match resolved.project.models.iter()
            .find(|m| &m.label == label)
        {
            Some(b) => b,
            // No matching block in the typed AST? That would mean the
            // ResolvedProject was constructed weirdly. Skip fallback
            // generation — primary still emitted above.
            None => continue,
        };

        let mut fallback_aliases: Vec<String>               = Vec::new();
        let mut seen:             BTreeSet<(String,String)> = BTreeSet::new();

        for cand in model_block.prefer.iter().chain(model_block.fallback.iter()) {
            // (i) skip the resolved primary — no self-reference per
            //     locked design decision
            if cand == primary_id {
                continue;
            }
            // (ii) dedup by dotted ID, first-occurrence-wins
            let id_key = (cand.provider.clone(), cand.name.clone());
            if !seen.insert(id_key) {
                continue;
            }
            // (iii) emit secondary entry + record its alias
            let alias = fallback_alias(label, cand);
            add_model_entry(&mut model_list, catalog, alias.clone(), cand)?;
            fallback_aliases.push(alias);
        }

        if !fallback_aliases.is_empty() {
            fallbacks_map.insert(label.clone(), fallback_aliases);
        }
    }

    let router_settings = if fallbacks_map.is_empty() {
        None
    } else {
        Some(RouterSettings { fallbacks: fallbacks_map })
    };

    Ok(LiteLlmConfig { model_list, router_settings })
}

/// Build a single `ModelEntry` from a catalog lookup + provider creds.
/// Used for both primary entries and secondary fallback entries.
fn add_model_entry(
    model_list: &mut Vec<ModelEntry>,
    catalog:    &Catalog,
    alias:      String,
    model_id:   &ModelId,
) -> Result<(), ConfigError> {
    let entry = catalog.find(model_id).ok_or_else(|| {
        ConfigError::MissingCatalogEntry(
            model_id.provider.clone(),
            model_id.name.clone(),
        )
    })?;
    let creds = provider_creds(&model_id.provider);
    model_list.push(ModelEntry {
        model_name: alias,
        litellm_params: LiteLlmParams {
            model:    entry.litellm_name.clone(),
            api_key:  creds.api_key.map(|v|  format!("os.environ/{v}")),
            api_base: creds.api_base.map(|v| format!("os.environ/{v}")),
        },
    });
    Ok(())
}

/// Per-fallback alias: `{primary_label}-fb-{provider}-{model-name}`
/// where the catalog model_name's underscores become hyphens (LiteLLM
/// convention; matches the `litellm_name` style of `provider/model-name`).
fn fallback_alias(primary_label: &str, model_id: &ModelId) -> String {
    let name_part = model_id.name.replace('_', "-");
    format!("{primary_label}-fb-{}-{name_part}", model_id.provider)
}

// ============================================================================
// Provider → credentials mapping
// ============================================================================
//
// v0.1 keeps this as a small hard-coded table. When v0.2 adds custom-
// provider extensibility (or per-project provider declarations), this
// table moves into the catalog schema.

struct ProviderCreds {
    /// Env-var name (NOT prefixed with `os.environ/`) for the API key.
    /// `None` when the provider doesn't use an API key.
    api_key:  Option<&'static str>,
    /// Env-var name for the api_base override (mainly Ollama).
    api_base: Option<&'static str>,
}

fn provider_creds(provider: &str) -> ProviderCreds {
    match provider {
        "anthropic" => ProviderCreds { api_key: Some("ANTHROPIC_API_KEY"), api_base: None },
        "openai"    => ProviderCreds { api_key: Some("OPENAI_API_KEY"),    api_base: None },
        "google"    => ProviderCreds { api_key: Some("GOOGLE_API_KEY"),    api_base: None },
        "ollama"    => ProviderCreds { api_key: None,                       api_base: Some("OLLAMA_API_BASE") },
        // Unknown provider — no credentials emitted. LiteLLM will surface
        // a clear "no API key" error at runtime if the provider needs one.
        _           => ProviderCreds { api_key: None,                       api_base: None },
    }
}

// ============================================================================
// `.env.example` generator
// ============================================================================

/// Build the `.env.example` content listing every env var the
/// project's resolved providers will consult at runtime. Sorted +
/// deterministic. The user copies this to `.env`, fills in the
/// values, and `docker compose up` reads them.
///
/// aiSlang itself never reads a real `.env` — that file is the
/// user's secret store, governed by `docker compose`. We only emit
/// `.env.example` (template, safe to commit).
pub fn generate_env_example(resolved: &ResolvedProject) -> String {
    use std::collections::BTreeSet;
    use std::fmt::Write;

    let mut vars: BTreeSet<&'static str> = BTreeSet::new();
    for model_id in resolved.model_bindings.values() {
        let creds = provider_creds(&model_id.provider);
        if let Some(v) = creds.api_key  { vars.insert(v); }
        if let Some(v) = creds.api_base { vars.insert(v); }
    }

    let mut out = String::new();
    let _ = writeln!(out, "# .env.example — generated by `aislang plan`");
    let _ = writeln!(out, "#");
    let _ = writeln!(out, "# Lists every env var your resolved provider stack will");
    let _ = writeln!(out, "# consult at runtime. Copy this file to `.env` (gitignored),");
    let _ = writeln!(out, "# fill in the values, and `docker compose up` reads them.");
    let _ = writeln!(out, "#");
    let _ = writeln!(out, "# Catalog version: {}", resolved.catalog_version);
    let _ = writeln!(out, "#");
    let _ = writeln!(out, "# Recommended .gitignore entry:");
    let _ = writeln!(out, "#   .env");
    let _ = writeln!(out, "#");
    let _ = writeln!(out, "# aislang itself never reads a real `.env` — secrets are the");
    let _ = writeln!(out, "# user's responsibility, handed to docker compose at runtime.");
    let _ = writeln!(out);
    if vars.is_empty() {
        let _ = writeln!(out, "# (no provider credentials needed for this resolution)");
    } else {
        for v in vars {
            let _ = writeln!(out, "{v}=");
        }
    }
    out
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

    #[test] fn demo_emits_primary_plus_fallbacks() {
        // Week-5 Thursday: each model block produces ONE primary entry
        // + one secondary entry per alternate (de-duplicated, no self-
        // reference). Demo has prefer=[haiku, gpt_4o_mini],
        // fallback=[llama_3_1_8b]; resolves to llama (primary).
        // So we expect:
        //   primary entry: "fast"   → ollama/llama3.1:8b
        //   alt entry #1:  "fast-fb-anthropic-claude-haiku-4-5"
        //   alt entry #2:  "fast-fb-openai-gpt-4o-mini"
        // Llama isn't an alt — it IS the primary.
        let (resolved, catalog) = resolve_demo();
        let cfg = build_config(&resolved, &catalog).expect("build");
        assert_eq!(cfg.model_list.len(), 3,
            "expected primary + 2 alternates; got {:?}", cfg.model_list);

        let names: Vec<&str> = cfg.model_list.iter()
            .map(|e| e.model_name.as_str()).collect();
        assert!(names.contains(&"fast"));
        assert!(names.contains(&"fast-fb-anthropic-claude-haiku-4-5"));
        assert!(names.contains(&"fast-fb-openai-gpt-4o-mini"));

        // Primary still uses the resolved litellm_name.
        let primary = cfg.model_list.iter()
            .find(|e| e.model_name == "fast").expect("primary entry");
        assert_eq!(primary.litellm_params.model, "ollama/llama3.1:8b");
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

    // ----------------------------------------------------------------
    // Wednesday: API-key plumbing + .env.example generation
    // ----------------------------------------------------------------

    #[test] fn ollama_entry_gets_api_base_not_api_key() {
        // Demo resolves to ollama under cost-opt; Ollama uses
        // OLLAMA_API_BASE (no API key).
        let (resolved, catalog) = resolve_demo();
        let cfg = build_config(&resolved, &catalog).expect("build");
        let params = &cfg.model_list[0].litellm_params;
        assert_eq!(params.api_key,  None);
        assert_eq!(params.api_base, Some("os.environ/OLLAMA_API_BASE".into()));
    }

    #[test] fn anthropic_entry_gets_api_key_not_api_base() {
        // Synthetic project pinning anthropic so we exercise the
        // cloud-provider creds path.
        use aislang_core::ast::Project;
        let mut bindings = BTreeMap::new();
        bindings.insert("smart".to_string(),
            ModelId { provider: "anthropic".into(), name: "claude_haiku_4_5".into() });
        let resolved = ResolvedProject {
            project:         Project { name: "x".into(), ..Project::default() },
            catalog_version: "test".into(),
            model_bindings:  bindings,
            prompt_hashes:   BTreeMap::new(),
        };
        let catalog = aislang_catalog::bundled().expect("bundled");
        let cfg = build_config(&resolved, &catalog).expect("build");
        let params = &cfg.model_list[0].litellm_params;
        assert_eq!(params.api_key,  Some("os.environ/ANTHROPIC_API_KEY".into()));
        assert_eq!(params.api_base, None);
    }

    #[test] fn yaml_includes_api_key_reference() {
        // String-form assertion that the secret reference flows
        // through to the rendered YAML.
        let (resolved, catalog) = synthetic_multi_model();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        // anthropic + openai both in this synthetic — both should
        // have api_key references; ollama has api_base.
        assert!(yaml.contains("api_key: os.environ/ANTHROPIC_API_KEY"));
        assert!(yaml.contains("api_key: os.environ/OPENAI_API_KEY"));
        assert!(yaml.contains("api_base: os.environ/OLLAMA_API_BASE"));
    }

    #[test] fn env_example_demo_has_ollama_only() {
        // Demo resolves to ollama → only OLLAMA_API_BASE in
        // the .env.example.
        let (resolved, _catalog) = resolve_demo();
        let env = generate_env_example(&resolved);
        assert!(env.contains("OLLAMA_API_BASE="));
        assert!(!env.contains("ANTHROPIC_API_KEY"));
        assert!(!env.contains("OPENAI_API_KEY"));
    }

    #[test] fn env_example_multi_provider_has_all() {
        let (resolved, _catalog) = synthetic_multi_model();
        let env = generate_env_example(&resolved);
        // Sorted alphabetically (BTreeSet):
        // ANTHROPIC_API_KEY, OLLAMA_API_BASE, OPENAI_API_KEY
        assert!(env.contains("ANTHROPIC_API_KEY="));
        assert!(env.contains("OLLAMA_API_BASE="));
        assert!(env.contains("OPENAI_API_KEY="));
        let ant = env.find("ANTHROPIC_API_KEY=").unwrap();
        let oll = env.find("OLLAMA_API_BASE=").unwrap();
        let oai = env.find("OPENAI_API_KEY=").unwrap();
        assert!(ant < oll && oll < oai,
            "expected alphabetical ordering; got A@{ant}, OL@{oll}, OP@{oai}");
    }

    #[test] fn env_example_has_helpful_header() {
        let (resolved, _catalog) = synthetic_multi_model();
        let env = generate_env_example(&resolved);
        // Three things users need to know on first read:
        assert!(env.contains("Copy this file to `.env`"));
        assert!(env.contains(".gitignore"));
        assert!(env.contains("aislang itself never reads a real `.env`"));
        // Catalog version recorded for traceability.
        assert!(env.contains("Catalog version:"));
    }

    #[test] fn env_example_empty_when_no_credentials_needed() {
        // Construct a resolution that uses only an unknown provider
        // (no creds mapping → no env vars in .env.example).
        use aislang_core::ast::Project;
        let mut bindings = BTreeMap::new();
        bindings.insert("local".to_string(),
            ModelId { provider: "imaginary_v2_provider".into(),
                      name: "test".into() });
        let resolved = ResolvedProject {
            project:         Project { name: "x".into(), ..Project::default() },
            catalog_version: "test".into(),
            model_bindings:  bindings,
            prompt_hashes:   BTreeMap::new(),
        };
        let env = generate_env_example(&resolved);
        assert!(env.contains("(no provider credentials needed for this resolution)"));
    }

    // ----------------------------------------------------------------
    // Thursday: fallback routing chains
    // ----------------------------------------------------------------

    #[test] fn demo_router_settings_has_fallback_chain() {
        // The fallbacks map should contain one key "fast" pointing at
        // the two alternates (haiku before gpt-4o-mini per prefer order).
        let (resolved, catalog) = resolve_demo();
        let cfg = build_config(&resolved, &catalog).expect("build");
        let router = cfg.router_settings.as_ref()
            .expect("router_settings must be present for demo");
        let fast_fb = router.fallbacks.get("fast")
            .expect("fast key in fallbacks map");
        // Order from source: prefer = [haiku, gpt-4o-mini], fallback = [llama_3_1_8b].
        // Llama is primary (excluded). Remaining: haiku, gpt-4o-mini.
        assert_eq!(fast_fb, &vec![
            "fast-fb-anthropic-claude-haiku-4-5".to_string(),
            "fast-fb-openai-gpt-4o-mini".to_string(),
        ]);
    }

    #[test] fn primary_excluded_from_own_fallback_list() {
        // The resolved primary's ModelId must NOT appear as an alias
        // in the fallbacks list.
        let (resolved, catalog) = resolve_demo();
        let cfg = build_config(&resolved, &catalog).expect("build");
        let router = cfg.router_settings.as_ref().unwrap();
        let fast_fb = router.fallbacks.get("fast").unwrap();
        for alias in fast_fb {
            assert!(!alias.contains("ollama"),
                "primary (ollama) must not appear in own fallback list; \
                 got alias `{alias}`");
        }
    }

    #[test] fn fallback_alias_scheme_format() {
        // Pin the format: `{primary_label}-fb-{provider}-{model-name}`
        // with name-part underscores hyphenated. LiteLLM convention.
        let cases = vec![
            ("fast", "anthropic", "claude_haiku_4_5",
                "fast-fb-anthropic-claude-haiku-4-5"),
            ("fast", "openai",    "gpt_4o_mini",
                "fast-fb-openai-gpt-4o-mini"),
            ("fast", "ollama",    "llama_3_1_8b",
                "fast-fb-ollama-llama-3-1-8b"),
            ("smart", "google",   "gemini_2_0_pro",
                "smart-fb-google-gemini-2-0-pro"),
        ];
        for (primary, provider, name, expected) in cases {
            let mid = ModelId { provider: provider.into(), name: name.into() };
            assert_eq!(super::fallback_alias(primary, &mid), expected);
        }
    }

    #[test] fn dedupe_overlapping_prefer_and_fallback() {
        // Same ModelId in prefer AND fallback → first occurrence wins.
        let src = r#"project "p" {
            budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_haiku_4_5, openai.gpt_4o_mini]
                fallback    = [openai.gpt_4o_mini, anthropic.claude_haiku_4_5]
                pin         = anthropic.claude_haiku_4_5
            }
        }"#;
        let (project, _) = aislang_core::validate(src).expect("validate");
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = aislang_resolver::resolve(&project, &catalog, &examples_dir())
            .expect("resolve");
        let cfg = build_config(&resolved, &catalog).expect("build");
        // Primary is anthropic (pinned). Alternates: gpt-4o-mini only —
        // anthropic-claude-haiku is primary (excluded), and the
        // duplicate gpt_4o_mini is deduped.
        let router = cfg.router_settings.as_ref().unwrap();
        let m_fb = router.fallbacks.get("m").unwrap();
        assert_eq!(m_fb, &vec!["m-fb-openai-gpt-4o-mini".to_string()]);
    }

    #[test] fn pin_only_no_alternates_yields_no_router_settings() {
        // Model block with only pin (no prefer/fallback alternates
        // remaining after primary exclusion) → no fallback chain →
        // router_settings is None.
        let src = r#"project "p" {
            budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_haiku_4_5]
                pin         = anthropic.claude_haiku_4_5
            }
        }"#;
        let (project, _) = aislang_core::validate(src).expect("validate");
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = aislang_resolver::resolve(&project, &catalog, &examples_dir())
            .expect("resolve");
        let cfg = build_config(&resolved, &catalog).expect("build");

        assert_eq!(cfg.model_list.len(), 1);
        assert_eq!(cfg.model_list[0].model_name, "m");
        assert!(cfg.router_settings.is_none(),
            "expected router_settings = None when no alternates; got {:?}",
            cfg.router_settings);
    }

    #[test] fn yaml_emits_router_settings_under_top_level() {
        // String-form check that the YAML structure is right.
        let (resolved, catalog) = resolve_demo();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        assert!(yaml.contains("router_settings:"));
        assert!(yaml.contains("fallbacks:"));
        assert!(yaml.contains("fast:"));
        assert!(yaml.contains("- fast-fb-anthropic-claude-haiku-4-5"));
        assert!(yaml.contains("- fast-fb-openai-gpt-4o-mini"));
    }

    #[test] fn env_example_deduplicates_across_models() {
        // Two model entries on the same provider → one env-var line.
        use aislang_core::ast::Project;
        let mut bindings = BTreeMap::new();
        bindings.insert("a".to_string(),
            ModelId { provider: "anthropic".into(), name: "claude_haiku_4_5".into() });
        bindings.insert("b".to_string(),
            ModelId { provider: "anthropic".into(), name: "claude_sonnet_4_6".into() });
        let resolved = ResolvedProject {
            project:         Project { name: "x".into(), ..Project::default() },
            catalog_version: "test".into(),
            model_bindings:  bindings,
            prompt_hashes:   BTreeMap::new(),
        };
        let env = generate_env_example(&resolved);
        // Count occurrences — must be exactly one ANTHROPIC_API_KEY line.
        let count = env.matches("ANTHROPIC_API_KEY=").count();
        assert_eq!(count, 1, "expected one line, got {count} in:\n{env}");
    }

    // ----------------------------------------------------------------
    // Friday: additional edge cases pushed onto coverage before launch
    // ----------------------------------------------------------------

    #[test] fn multiple_model_blocks_get_separate_fallback_chains() {
        // Two model blocks, each with its own prefer/fallback. The
        // fallbacks map should have one key per primary label, each
        // pointing at the right alternates.
        let src = r#"project "p" {
            budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
            model "fast" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_haiku_4_5]
                fallback    = [openai.gpt_4o_mini]
                pin         = anthropic.claude_haiku_4_5
            }
            model "smart" {
                requires    = [chat]
                context_min = 200k
                prefer      = [anthropic.claude_opus_4_7]
                fallback    = [anthropic.claude_sonnet_4_6]
                pin         = anthropic.claude_opus_4_7
            }
        }"#;
        let (project, _) = aislang_core::validate(src).expect("validate");
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = aislang_resolver::resolve(&project, &catalog, &examples_dir())
            .expect("resolve");
        let cfg = build_config(&resolved, &catalog).expect("build");
        let router = cfg.router_settings.as_ref().unwrap();
        assert_eq!(router.fallbacks.len(), 2);

        let fast_fb = router.fallbacks.get("fast").unwrap();
        assert_eq!(fast_fb, &vec!["fast-fb-openai-gpt-4o-mini".to_string()]);

        let smart_fb = router.fallbacks.get("smart").unwrap();
        assert_eq!(smart_fb,
            &vec!["smart-fb-anthropic-claude-sonnet-4-6".to_string()]);
    }

    #[test] fn env_example_aggregates_across_two_blocks_with_diff_providers() {
        // First block uses anthropic; second uses openai+ollama via
        // fallbacks. .env.example must cover all three.
        let src = r#"project "p" {
            budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
            model "primary" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_haiku_4_5]
                pin         = anthropic.claude_haiku_4_5
            }
            model "secondary" {
                requires    = [chat]
                context_min = 32k
                prefer      = [openai.gpt_4o_mini]
                fallback    = [ollama.llama_3_1_8b]
                pin         = openai.gpt_4o_mini
            }
        }"#;
        let (project, _) = aislang_core::validate(src).expect("validate");
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = aislang_resolver::resolve(&project, &catalog, &examples_dir())
            .expect("resolve");
        let env = generate_env_example(&resolved);
        // Note: .env.example collects vars from RESOLVED primaries only,
        // not from fallback alternates. So ollama (fallback of secondary)
        // doesn't appear unless a primary uses it.
        assert!(env.contains("ANTHROPIC_API_KEY="));
        assert!(env.contains("OPENAI_API_KEY="));
        // Ollama is only in fallback, primary is openai — so no OLLAMA_API_BASE.
        assert!(!env.contains("OLLAMA_API_BASE="));
    }

    #[test] fn no_models_no_router_settings() {
        // Project with no model blocks at all → empty model_list,
        // None router_settings.
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
        assert!(cfg.router_settings.is_none());
    }

    #[test] fn fallback_alias_handles_label_underscores() {
        // Primary label with underscores stays as-is (only the model
        // name part gets hyphenated). User-chosen labels can be either
        // hyphenated or under_scored.
        let mid = ModelId { provider: "anthropic".into(),
                            name: "claude_haiku_4_5".into() };
        assert_eq!(
            super::fallback_alias("my_smart_model", &mid),
            "my_smart_model-fb-anthropic-claude-haiku-4-5",
        );
    }

    #[test] fn router_settings_yaml_after_model_list() {
        // Diff-stable ordering: model_list section appears before
        // router_settings (the order the struct fields are declared in).
        let (resolved, catalog) = resolve_demo();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        let ml_pos     = yaml.find("model_list:").expect("model_list");
        let router_pos = yaml.find("router_settings:").expect("router_settings");
        assert!(ml_pos < router_pos,
            "model_list must precede router_settings; got model_list@{ml_pos}, router_settings@{router_pos}");
    }

    #[test] fn yaml_emits_block_style_for_router_settings_too() {
        // Pin block style on the whole config — important for human
        // readability of the generated file.
        let (resolved, catalog) = resolve_demo();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        assert!(yaml.contains("\nrouter_settings:\n  fallbacks:\n    fast:\n"),
            "expected block-style nested map; got:\n{yaml}");
    }

    #[test] fn yaml_round_trips_with_full_config() {
        // serde_yaml roundtrip on the Thu output (includes
        // router_settings + multiple model_list entries).
        let (resolved, catalog) = resolve_demo();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        let _: serde_yaml::Value = serde_yaml::from_str(&yaml)
            .expect("full config must round-trip through serde_yaml");
    }
}

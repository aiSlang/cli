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

    /// Phase 2 Week 15 Tue follow-up — `litellm_settings.callbacks`
    /// is LiteLLM's opt-in switch for built-in integrations.
    /// `["otel"]` activates the OpenTelemetry emitter (LiteLLM
    /// reads OTEL_EXPORTER_OTLP_ENDPOINT + protocol + service_name
    /// at startup but doesn't ship spans unless this callback is
    /// present in its config — having the env vars without the
    /// callback was the gap that made Tue's pod-wiring invisible
    /// in Jaeger).  Same value lands in both Compose-target +
    /// k8s-target litellm_config.yaml since the generator is one
    /// source of truth across deploy targets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub litellm_settings: Option<LiteLlmSettings>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiteLlmSettings {
    /// LiteLLM callbacks list — `["otel"]` for OTel-GenAI span
    /// emission to the resolved `OTEL_EXPORTER_OTLP_ENDPOINT`.
    /// Other callbacks LiteLLM ships (langfuse, helicone, datadog,
    /// braintrust, …) are user-add via `helm upgrade --set` post-
    /// Phase 2; v0.2 emits the OTel one by default since it's
    /// what the bundled Jaeger pod consumes.
    pub callbacks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouterSettings {
    /// LiteLLM router fallbacks — list of single-key dicts:
    ///   ```yaml
    ///   fallbacks:
    ///     - fast:  ["fast-fb-anthropic-claude-haiku-4-5", "fast-fb-openai-gpt-4o-mini"]
    ///     - smart: ["smart-fb-google-gemini-2-0-pro"]
    ///   ```
    ///
    /// Not a YAML map. LiteLLM iterates this field expecting each
    /// element to be a `dict` and raises
    ///   `ValueError: Item '<key>' is not a dictionary`
    /// at startup if you emit it as a top-level mapping
    /// (`fallbacks: { fast: [...] }`). That's why each entry is
    /// itself a one-key `BTreeMap` — LiteLLM sees a `list[dict]`,
    /// gets the `key` + `value` per element, and validates each as
    /// a dict.
    ///
    /// Outer order is alphabetical by the single key (we sort at
    /// build time) so the emitted YAML is diff-stable across plan
    /// runs.
    pub fallbacks: Vec<BTreeMap<String, Vec<String>>>,
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
    let mut model_list: Vec<ModelEntry>                       = Vec::new();
    // Collect (primary_label, fallback_aliases) pairs first; the
    // list-of-single-key-dicts is built in alphabetical order below.
    let mut fallback_pairs: BTreeMap<String, Vec<String>>     = BTreeMap::new();

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
            // (c) Primary's fallback chain: walk every alternate in
            //     declared order.  v0.2 Wed follow-up: ALSO register a
            //     cascade chain for each fb alias itself, pointing at
            //     the rest of the tail.  Without this, picking
            //     `fast-fb-openai-gpt-4o-mini` from Open WebUI's
            //     dropdown surfaces a raw 401 from OpenAI when the
            //     key is missing — there's no router-level fallback
            //     to drop down to the next alternate.  Surfaced by
            //     live Open WebUI testing.
            //
            // The chain is strict tail-of-list: alias[i] falls back
            // to alias[i+1..].  Last alias gets no entry (no tail to
            // cascade into).  Output stays diff-stable because the
            // entries land in `fallback_pairs` keyed by alias name,
            // and the outer BTreeMap sorts alphabetically.
            for i in 0..fallback_aliases.len().saturating_sub(1) {
                let alias_at_i  = fallback_aliases[i].clone();
                let tail: Vec<String> = fallback_aliases[i + 1 ..].to_vec();
                fallback_pairs.insert(alias_at_i, tail);
            }
            fallback_pairs.insert(label.clone(), fallback_aliases);
        }
    }

    let router_settings = if fallback_pairs.is_empty() {
        None
    } else {
        // Materialize as a list of single-key dicts. LiteLLM iterates
        // this field expecting `dict` items; a top-level YAML mapping
        // here crashes the proxy at startup with
        //   ValueError: Item '<key>' is not a dictionary
        // (litellm/router.py::validate_fallbacks). See RouterSettings
        // docs above. BTreeMap iteration order → alphabetical outer
        // ordering → diff-stable YAML.
        let fallbacks: Vec<BTreeMap<String, Vec<String>>> = fallback_pairs
            .into_iter()
            .map(|(label, aliases)| {
                let mut m = BTreeMap::new();
                m.insert(label, aliases);
                m
            })
            .collect();
        Some(RouterSettings { fallbacks })
    };

    // Phase 2 Week 15 Tue follow-up: opt LiteLLM into the OTel
    // emitter unconditionally.  Required even when the proxy is
    // running with OTEL_EXPORTER_OTLP_ENDPOINT set — LiteLLM
    // doesn't auto-emit spans from env vars alone; the callbacks
    // list is the explicit switch.  Same generated YAML lands in
    // both Compose-target + k8s-target deployments since this
    // generator is the single source of truth across emitters.
    let litellm_settings = Some(LiteLlmSettings {
        callbacks: vec!["otel".to_string()],
    });

    Ok(LiteLlmConfig { model_list, router_settings, litellm_settings })
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
//
// TODO(v0.2): unify with `secret` blocks — see LANGUAGE-SPEC §5.6.
// The direction is to make user-declared `secret` blocks the canonical
// surface for provider credentials, with this table becoming a fallback
// for projects that don't declare them explicitly.

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

/// Compute the union of env-var names the project's resolved
/// provider stack will consult at runtime.
///
/// Two sources merged + deduplicated:
///   1. Provider-derived creds (`anthropic` → ANTHROPIC_API_KEY,
///      `ollama` → OLLAMA_API_BASE, …) for every resolved primary
///      `ModelId`.
///   2. User-declared `secret { source = "env://X" }` blocks (X
///      contributed verbatim).
///
/// Returned `BTreeSet<String>` is alphabetically sorted — both
/// `generate_env_example` (the .env.example template) and
/// `build_litellm_service` (the runtime container's env-forwarding
/// list) consume the same set, so .env.example and what actually
/// reaches LiteLLM at runtime stay in lockstep. If they ever
/// diverge the user gets a silent "variable empty, can't reach
/// provider" error — which is what shipped before this helper
/// landed.
pub fn referenced_env_vars(resolved: &ResolvedProject) -> BTreeSet<String> {
    let mut vars: BTreeSet<String> = BTreeSet::new();

    // (1) Resolved primaries — what `aislang chat` and the deployed
    //     `fast` alias dial at first request.
    for model_id in resolved.model_bindings.values() {
        let creds = provider_creds(&model_id.provider);
        if let Some(v) = creds.api_key  { vars.insert(v.to_string()); }
        if let Some(v) = creds.api_base { vars.insert(v.to_string()); }
    }

    // (2) v0.2 Tue follow-up: walk every ModelId in prefer + fallback
    //     too.  `build_config` emits a model_list entry per ModelId
    //     in `prefer ∪ fallback` (so LiteLLM's router_settings can
    //     chain them); each entry carries `api_key: os.environ/X`
    //     references.  Without their env vars in `.env.example` (and
    //     forwarded into the litellm container by build_litellm_service),
    //     the alternates dial with an empty key and the user hits an
    //     AuthError the moment they pick a fallback alias from Open
    //     WebUI's dropdown.
    //
    //     The pre-Tue-follow-up demo masked this — the resolver picked
    //     the free Ollama fallback as primary, so the .env.example
    //     listed OLLAMA_API_BASE only.  Anthropic/OpenAI keys WERE
    //     referenced in the emitted litellm_config.yaml but weren't
    //     in .env.example → silent gap that surfaced as an AuthError
    //     in live Open WebUI testing.  See user-reported bug for the
    //     full trail.
    for block in &resolved.project.models {
        for id in block.prefer.iter().chain(block.fallback.iter()) {
            let creds = provider_creds(&id.provider);
            if let Some(v) = creds.api_key  { vars.insert(v.to_string()); }
            if let Some(v) = creds.api_base { vars.insert(v.to_string()); }
        }
    }

    // (3) User-declared `secret` blocks with `env://VAR` URIs.
    for s in &resolved.project.secrets {
        if let Some(var) = s.source.strip_prefix("env://") {
            vars.insert(var.to_string());
        }
    }
    vars
}

/// Build the `.env.example` content listing every env var the
/// project's resolved providers will consult at runtime. Sorted +
/// deterministic. The user copies this to `.env`, fills in the
/// values, and `docker compose up` reads them.
///
/// aiSlang itself never reads a real `.env` — that file is the
/// user's secret store, governed by `docker compose`. We only emit
/// `.env.example` (template, safe to commit).
///
/// Env vars come from two sources in v0.1 (Option C, week-5 follow-up):
///   1. **Provider creds** auto-derived from each resolved primary's
///      `provider` (anthropic → `ANTHROPIC_API_KEY`, ollama →
///      `OLLAMA_API_BASE`, …). The implicit path.
///   2. **User-declared `secret` blocks** with `env://VAR` URIs. The
///      declared path — surfaces explicit dependencies on env vars
///      that aren't tied to a model provider (DB creds, webhook
///      tokens, GitHub PAT for an MCP server, …).
///
/// The two sources are merged into a single sorted env-var list (dedup
/// via `BTreeSet`). Secret blocks with non-`env://` sources (e.g.
/// `file://./.secrets/db.json`) are listed in a separate informational
/// comment block — aiSlang's emit doesn't load them, but surfacing them
/// in `.env.example` documents the project's full secret surface in
/// one place.
///
/// LANGUAGE-SPEC §5.6 (v0.2 direction): user-declared `secret` blocks
/// become the canonical credential surface; the provider-creds table
/// here becomes a fallback for projects that don't declare them.
pub fn generate_env_example(resolved: &ResolvedProject) -> String {
    use std::fmt::Write;

    // Env vars come from `referenced_env_vars` — shared with the
    // litellm container env-forwarding logic so .env.example and
    // what reaches LiteLLM at runtime stay in lockstep.
    let vars = referenced_env_vars(resolved);

    // Non-env-backed secret sources (file://, future vault://…) get
    // listed in the informational footer block. Not part of the
    // env-var set; can't be forwarded as runtime env.
    let mut other_sources: Vec<(&str, &str)> = Vec::new();
    for s in &resolved.project.secrets {
        if s.source.strip_prefix("env://").is_none() {
            other_sources.push((s.label.as_str(), s.source.as_str()));
        }
    }
    other_sources.sort();

    let mut out = String::new();
    let _ = writeln!(out, "# .env.example — generated by `aislang plan`");
    let _ = writeln!(out, "#");
    let _ = writeln!(out, "# Lists every env var your resolved provider stack will");
    let _ = writeln!(out, "# consult at runtime. Copy this file to `.env` (gitignored),");
    let _ = writeln!(out, "# fill in the values, and `docker compose up` reads them.");
    let _ = writeln!(out, "#");
    let _ = writeln!(out, "# Sources (merged + alphabetized):");
    let _ = writeln!(out, "#   - provider credentials derived from resolved models");
    let _ = writeln!(out, "#   - `secret` blocks with `env://` URIs (user-declared)");
    let _ = writeln!(out, "#");
    let _ = writeln!(out, "# Catalog version: {}", resolved.catalog_version);
    let _ = writeln!(out, "#");
    let _ = writeln!(out, "# Recommended .gitignore entry:");
    let _ = writeln!(out, "#   .env");
    let _ = writeln!(out, "#");
    let _ = writeln!(out, "# aislang itself never reads a real `.env` — secrets are the");
    let _ = writeln!(out, "# user's responsibility, handed to docker compose at runtime.");
    let _ = writeln!(out);
    if vars.is_empty() && other_sources.is_empty() {
        let _ = writeln!(out, "# (no provider credentials needed for this resolution)");
    } else {
        for v in &vars {
            let _ = writeln!(out, "{v}=");
        }
    }
    if !other_sources.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "# Non-env-backed secret sources (informational — aislang does not");
        let _ = writeln!(out, "# load these; mount or fetch them at deploy time):");
        for (label, source) in &other_sources {
            let _ = writeln!(out, "#   secret \"{label}\": {source}");
        }
    }
    // Cross-platform Ollama-on-host hint, emitted AFTER the env var
    // listings so it doesn't collide with substring searches over
    // the variable-assignment block. The litellm service emits
    // `extra_hosts: [host.docker.internal:host-gateway]` (see
    // aislang-compose::build_litellm_service) so this URL works
    // uniformly on Mac/Windows/Linux. Phrasing avoids the literal
    // `OLLAMA_API_BASE=` substring so the hint text doesn't read
    // as a variable assignment.
    if vars.iter().any(|v| v == "OLLAMA_API_BASE") {
        let _ = writeln!(out);
        let _ = writeln!(out, "# --- Ollama-on-host hint ---");
        let _ = writeln!(out, "# If you run Ollama as a container (or native binary) on this");
        let _ = writeln!(out, "# same machine with port 11434 published, set the value above to");
        let _ = writeln!(out, "#   http://host.docker.internal:11434");
        let _ = writeln!(out, "# The compose stack wires `host.docker.internal` to the host");
        let _ = writeln!(out, "# gateway so this URL resolves uniformly on Mac, Windows, and Linux.");
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

    /// Test helper: find the fallback list for a given primary label
    /// inside `RouterSettings.fallbacks` (which is a list-of-single-
    /// key-dicts, not a map — see `RouterSettings` docs).
    fn find_fallbacks<'a>(router: &'a RouterSettings, label: &str)
        -> Option<&'a Vec<String>>
    {
        router.fallbacks.iter().find_map(|m| m.get(label))
    }

    // ---------- happy path ----------

    #[test] fn demo_emits_primary_plus_fallbacks() {
        // Each model block produces ONE primary entry + one secondary
        // entry per alternate (de-duplicated, no self-reference).
        //
        // v0.2 Tue follow-up: resolver now walks prefer in declared
        // order — see resolver §`resolve_one_model`.  Demo has
        // prefer=[haiku, gpt-4o-mini], fallback=[llama3]; resolves to
        // haiku (prefer[0]).  So we expect:
        //   primary entry: "fast"   → anthropic/claude-haiku-4-5
        //   alt entry #1:  "fast-fb-openai-gpt-4o-mini"
        //   alt entry #2:  "fast-fb-ollama-llama-3-1-8b"
        // (No `fast-fb-anthropic-…` because haiku IS the primary.)
        let (resolved, catalog) = resolve_demo();
        let cfg = build_config(&resolved, &catalog).expect("build");
        assert_eq!(cfg.model_list.len(), 3,
            "expected primary + 2 alternates; got {:?}", cfg.model_list);

        let names: Vec<&str> = cfg.model_list.iter()
            .map(|e| e.model_name.as_str()).collect();
        assert!(names.contains(&"fast"));
        assert!(names.contains(&"fast-fb-openai-gpt-4o-mini"));
        assert!(names.contains(&"fast-fb-ollama-llama-3-1-8b"));
        assert!(!names.contains(&"fast-fb-anthropic-claude-haiku-4-5"),
            "haiku IS the primary now; must NOT appear as its own fallback");

        // Primary uses the resolved litellm_name (Haiku via Anthropic).
        let primary = cfg.model_list.iter()
            .find(|e| e.model_name == "fast").expect("primary entry");
        assert_eq!(primary.litellm_params.model, "anthropic/claude-haiku-4-5");
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
        // v0.2 Tue follow-up: primary is now anthropic/claude-haiku
        // (prefer[0]), not ollama (the old cost-pierced winner).
        let (resolved, catalog) = resolve_demo();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        assert!(yaml.contains("model_list:"));
        assert!(yaml.contains("model_name: fast"));
        assert!(yaml.contains("litellm_params:"));
        assert!(yaml.contains("model: anthropic/claude-haiku-4-5"));
        // Ollama still appears, but as a fallback now.
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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

    #[test] fn ollama_fallback_entry_gets_api_base_not_api_key() {
        // v0.2 Tue follow-up: ollama appears as a FALLBACK entry now
        // (primary is haiku per prefer order).  Find the Ollama entry
        // explicitly rather than indexing model_list[0].
        let (resolved, catalog) = resolve_demo();
        let cfg = build_config(&resolved, &catalog).expect("build");
        let ollama = cfg.model_list.iter()
            .find(|e| e.litellm_params.model == "ollama/llama3.1:8b")
            .expect("ollama fallback entry must be present");
        assert_eq!(ollama.litellm_params.api_key,  None);
        assert_eq!(ollama.litellm_params.api_base,
                   Some("os.environ/OLLAMA_API_BASE".into()));
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
            ..Default::default()
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

    #[test] fn env_example_demo_includes_every_provider_in_prefer_and_fallback() {
        // v0.2 Tue follow-up: with strict prefer order, the demo
        // resolves to anthropic.claude_haiku_4_5 as the PRIMARY.  The
        // emitter still walks prefer ∪ fallback to build the fallback
        // model_list entries (so LiteLLM router_settings can chain
        // them).  So .env.example must include API keys for EVERY
        // provider referenced in prefer or fallback — anthropic +
        // openai + ollama for the demo.
        let (resolved, _catalog) = resolve_demo();
        let env = generate_env_example(&resolved);
        assert!(env.contains("ANTHROPIC_API_KEY="),
            "primary haiku → need ANTHROPIC_API_KEY; got:\n{env}");
        assert!(env.contains("OPENAI_API_KEY="),
            "fallback gpt-4o-mini → need OPENAI_API_KEY; got:\n{env}");
        assert!(env.contains("OLLAMA_API_BASE="),
            "fallback llama3 → need OLLAMA_API_BASE; got:\n{env}");
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

    #[test] fn env_example_includes_ollama_host_hint_when_ollama_resolved() {
        // The .env.example surfaces the host.docker.internal:11434
        // recipe whenever OLLAMA_API_BASE appears in the var list —
        // regardless of whether Ollama is primary or just a fallback.
        // (After v0.2 Tue follow-up the demo's primary is haiku, but
        // ollama still appears as a fallback so the hint fires.)
        let (resolved, _) = resolve_demo();
        let env = generate_env_example(&resolved);
        assert!(env.contains("Ollama-on-host hint"),
            "footer must announce the Ollama hint when OLLAMA_API_BASE present:\n{env}");
        assert!(env.contains("http://host.docker.internal:11434"),
            "footer must spell out the working URL:\n{env}");
        // Must follow the env-var listings — placed in the footer
        // intentionally so substring searches over the var lines
        // don't conflate the hint URL with an actual assignment.
        let var_line = env.find("OLLAMA_API_BASE=").unwrap();
        let hint     = env.find("Ollama-on-host hint").unwrap();
        assert!(var_line < hint,
            "Ollama hint must come AFTER the env-var listings; \
             got var@{var_line}, hint@{hint}");
        // Hint must NOT contain the literal `OLLAMA_API_BASE=` (which
        // would re-introduce the substring-collision bug it was
        // restructured to avoid).
        let hint_block = &env[hint..];
        assert!(!hint_block.contains("OLLAMA_API_BASE="),
            "hint block must not contain `OLLAMA_API_BASE=` literal — \
             that collides with substring searches over the var block. \
             Got hint block:\n{hint_block}");
    }

    #[test] fn env_example_omits_ollama_hint_when_no_ollama_resolved() {
        // Anthropic-only project → no OLLAMA_API_BASE → no Ollama
        // hint clutter in the header. Keeps generated files terse.
        use aislang_core::ast::Project;
        let mut bindings = BTreeMap::new();
        bindings.insert("smart".to_string(),
            ModelId { provider: "anthropic".into(),
                      name: "claude_haiku_4_5".into() });
        let resolved = ResolvedProject {
            project:         Project { name: "x".into(), ..Project::default() },
            catalog_version: "test".into(),
            model_bindings:  bindings,
            prompt_hashes:   BTreeMap::new(),
            ..Default::default()
        };
        let env = generate_env_example(&resolved);
        assert!(!env.contains("Ollama-on-host hint"),
            "Ollama hint must be omitted when no OLLAMA_API_BASE in vars:\n{env}");
        assert!(!env.contains("host.docker.internal"));
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
            ..Default::default()
        };
        let env = generate_env_example(&resolved);
        assert!(env.contains("(no provider credentials needed for this resolution)"));
    }

    // ----------------------------------------------------------------
    // Thursday: fallback routing chains
    // ----------------------------------------------------------------

    #[test] fn demo_router_settings_has_fallback_chain() {
        // The fallbacks map should contain one key "fast" pointing at
        // the alternates in prefer-then-fallback order, with the
        // resolved primary excluded.
        //
        // v0.2 Tue follow-up: demo resolves to haiku (prefer[0]) now.
        // Walking prefer ∪ fallback = [haiku, gpt-4o-mini, llama3] +
        // dropping haiku (primary) leaves [gpt-4o-mini, llama3].
        let (resolved, catalog) = resolve_demo();
        let cfg = build_config(&resolved, &catalog).expect("build");
        let router = cfg.router_settings.as_ref()
            .expect("router_settings must be present for demo");
        let fast_fb = find_fallbacks(router, "fast")
            .expect("fast key in fallbacks list");
        assert_eq!(fast_fb, &vec![
            "fast-fb-openai-gpt-4o-mini".to_string(),
            "fast-fb-ollama-llama-3-1-8b".to_string(),
        ]);
    }

    #[test] fn primary_excluded_from_own_fallback_list() {
        // The resolved primary's ModelId must NOT appear as an alias
        // in the fallbacks list.  v0.2 Tue follow-up: primary is haiku
        // (anthropic) now.
        let (resolved, catalog) = resolve_demo();
        let cfg = build_config(&resolved, &catalog).expect("build");
        let router = cfg.router_settings.as_ref().unwrap();
        let fast_fb = find_fallbacks(router, "fast").unwrap();
        for alias in fast_fb {
            assert!(!alias.contains("anthropic"),
                "primary (haiku via anthropic) must not appear in own \
                 fallback list; got alias `{alias}`");
        }
    }

    // ── Wed follow-up: per-fb-alias cascade chains ───────────────
    //
    // Surfaced by live Open WebUI testing: explicitly picking
    // `fast-fb-openai-gpt-4o-mini` from the dropdown without an
    // OPENAI_API_KEY surfaced a raw 401 because LiteLLM had no
    // fallback chain registered for that alias itself.  Fix: each
    // fb alias registers the REMAINING TAIL of the chain as its
    // own fallback, so dropping into a fallback model still
    // cascades to the next-best on failure.

    #[test] fn each_fb_alias_cascades_to_remaining_tail() {
        // Demo prefer = [haiku, gpt-4o-mini], fallback = [llama3].
        // After v0.2 Tue resolver fix, primary = haiku.  fb chain:
        //   fast → [fast-fb-openai-gpt-4o-mini, fast-fb-ollama-llama-3-1-8b]
        //   fast-fb-openai-gpt-4o-mini → [fast-fb-ollama-llama-3-1-8b]
        //   (last alias has no tail → no entry)
        let (resolved, catalog) = resolve_demo();
        let cfg = build_config(&resolved, &catalog).expect("build");
        let router = cfg.router_settings.as_ref()
            .expect("router_settings must be present for demo");

        // Primary's full chain.
        let fast_fb = find_fallbacks(router, "fast").unwrap();
        assert_eq!(fast_fb, &vec![
            "fast-fb-openai-gpt-4o-mini".to_string(),
            "fast-fb-ollama-llama-3-1-8b".to_string(),
        ]);

        // Middle alias's tail (just the last alias).
        let mid_fb = find_fallbacks(router, "fast-fb-openai-gpt-4o-mini")
            .expect("middle fb alias must have its own cascade entry");
        assert_eq!(mid_fb, &vec![
            "fast-fb-ollama-llama-3-1-8b".to_string(),
        ]);

        // Last alias has no tail → no entry.
        assert!(find_fallbacks(router, "fast-fb-ollama-llama-3-1-8b").is_none(),
            "last fb alias has no tail; must NOT appear as a fallbacks key");
    }

    #[test] fn three_alternates_emit_three_cascade_entries() {
        // Synthetic project with 4-deep alternate chain to pin the
        // per-i loop's correctness:
        //   prefer   = [A, B, C]
        //   fallback = [D]
        // → primary = A (the test catalog ranks A first satisfier)
        // → fb chain = [B, C, D]
        // Cascade entries:
        //   A   → [B, C, D]
        //   B   → [C, D]      ← per-fb cascade #1
        //   C   → [D]         ← per-fb cascade #2
        //   D   → (no entry)
        use aislang_core::ast::Project;
        let src = r#"project "p" {
            budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_haiku_4_5,
                               openai.gpt_4o_mini,
                               anthropic.claude_sonnet_4_6]
                fallback    = [ollama.llama_3_1_8b]
            }
        }"#;
        let (project, _) = aislang_core::validate(src).expect("validate");
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = aislang_resolver::resolve(&project, &catalog, &examples_dir())
            .expect("resolve");
        let cfg = build_config(&resolved, &catalog).expect("build");
        let router = cfg.router_settings.as_ref().expect("router_settings");

        // primary = haiku (prefer[0]); tail = [gpt-4o-mini, sonnet, llama]
        let m_chain = find_fallbacks(router, "m").unwrap();
        assert_eq!(m_chain.len(), 3, "primary cascade should have 3 alternates");

        // First fb alias's cascade should drop the first item.
        let first_fb_chain = find_fallbacks(router, &m_chain[0])
            .expect("first fb alias should have a tail cascade");
        assert_eq!(first_fb_chain, &m_chain[1..].to_vec());

        // Second fb alias.
        let second_fb_chain = find_fallbacks(router, &m_chain[1])
            .expect("second fb alias should have a tail cascade");
        assert_eq!(second_fb_chain, &m_chain[2..].to_vec());

        // Last fb alias → no entry (no tail).
        assert!(find_fallbacks(router, &m_chain[2]).is_none(),
            "last fb alias `{}` must NOT have a cascade entry", m_chain[2]);

        // Suppress unused import warning when this file is built
        // standalone (Project is referenced via the validate() return).
        let _ = std::any::TypeId::of::<Project>();
    }

    #[test] fn single_alternate_does_not_emit_self_cascade_entry() {
        // One-deep alternate chain: prefer = [haiku], fallback = [llama]
        // → primary = haiku, fb chain = [llama]
        // Cascade entries:
        //   primary → [llama]
        //   llama   → (no entry — only fb alias, no tail)
        // Pins that the for-loop's `saturating_sub(1)` correctly
        // handles the boundary (no off-by-one emitting an empty
        // cascade entry).
        let src = r#"project "p" {
            budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_haiku_4_5]
                fallback    = [ollama.llama_3_1_8b]
            }
        }"#;
        let (project, _) = aislang_core::validate(src).expect("validate");
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = aislang_resolver::resolve(&project, &catalog, &examples_dir())
            .expect("resolve");
        let cfg = build_config(&resolved, &catalog).expect("build");
        let router = cfg.router_settings.as_ref().expect("router_settings");

        let primary_chain = find_fallbacks(router, "m").unwrap();
        assert_eq!(primary_chain.len(), 1);
        let only_fb = &primary_chain[0];
        assert!(find_fallbacks(router, only_fb).is_none(),
            "single-alternate chain must NOT register a self-cascade \
             entry for its only fb alias; got fallbacks for `{only_fb}`");
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
        let m_fb = find_fallbacks(router, "m").unwrap();
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
        // The fallbacks shape is a list-of-single-key-dicts (see
        // RouterSettings docs for the LiteLLM bootstrap requirement)
        // so we look for `- fast:` (list-item with the primary as a
        // single dict key) rather than just `fast:` (which would be
        // a top-level map, the v0.1-rejected shape).
        //
        // v0.2 Tue follow-up: primary is haiku now → not in fallbacks;
        // remaining alternates are gpt-4o-mini + llama3.
        let (resolved, catalog) = resolve_demo();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        assert!(yaml.contains("router_settings:"));
        assert!(yaml.contains("fallbacks:"));
        assert!(yaml.contains("- fast:"),
            "fallbacks entry must be a list-item single-key dict (`- fast:`); \
             got:\n{yaml}");
        assert!(yaml.contains("- fast-fb-openai-gpt-4o-mini"));
        assert!(yaml.contains("- fast-fb-ollama-llama-3-1-8b"));
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
            ..Default::default()
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

        let fast_fb = find_fallbacks(router, "fast").unwrap();
        assert_eq!(fast_fb, &vec!["fast-fb-openai-gpt-4o-mini".to_string()]);

        let smart_fb = find_fallbacks(router, "smart").unwrap();
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
        // v0.2 Tue follow-up: .env.example now collects vars from BOTH
        // resolved primaries AND every ModelId in prefer + fallback —
        // because `build_config` emits a model_list entry per alternate
        // (with `api_key: os.environ/X` references that LiteLLM tries
        // to dereference at startup).  See `referenced_env_vars` for
        // the rationale.
        //
        // So ollama (fallback of `secondary`) now appears in
        // .env.example even though no primary uses it.  The old "no
        // OLLAMA_API_BASE" expectation was the bug that produced
        // AuthErrors when Open WebUI users picked a fallback alias
        // from the model dropdown.
        assert!(env.contains("ANTHROPIC_API_KEY="));
        assert!(env.contains("OPENAI_API_KEY="));
        assert!(env.contains("OLLAMA_API_BASE="),
            "ollama is a declared fallback; its env var must surface in \
             .env.example so the litellm container can dial it when \
             that alias is picked from the UI / a /v1/chat/completions \
             call");
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
            ..Default::default()
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

    // ── Phase 2 Week 15 Tue follow-up — LiteLLM OTel callback ──

    #[test] fn litellm_settings_enables_otel_callback() {
        // Just setting OTEL_EXPORTER_OTLP_ENDPOINT on the LiteLLM
        // container env (Tue's pod-wiring) is necessary BUT NOT
        // SUFFICIENT to make LiteLLM emit spans — the proxy needs
        // `litellm_settings.callbacks` to include "otel".  Without
        // it LiteLLM reads the env vars + silently does nothing
        // with them.  Pin via the generated YAML body.
        let (resolved, catalog) = resolve_demo();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        assert!(yaml.contains("litellm_settings:"),
            "litellm_config.yaml must include a litellm_settings: section; got:\n{yaml}");
        // Sequence shape (not a mapping) — LiteLLM expects callbacks
        // as a list of strings.
        assert!(yaml.contains("callbacks:"),
            "litellm_settings must declare callbacks: …; got:\n{yaml}");
        assert!(yaml.contains("- otel"),
            "callbacks list must include `otel` for OTel emission; got:\n{yaml}");
    }

    #[test] fn litellm_settings_yaml_section_present_for_minimal_project() {
        // The OTel callback opt-in fires for EVERY project, not just
        // ones declaring fancy models — a minimal config still wants
        // spans landing in Jaeger if the operator wires the endpoint.
        use aislang_core::ast::Project;
        let resolved = ResolvedProject {
            project:         Project { name: "x".into(), ..Project::default() },
            catalog_version: "test".into(),
            model_bindings:  BTreeMap::new(),
            prompt_hashes:   BTreeMap::new(),
            ..Default::default()
        };
        let catalog = aislang_catalog::bundled().expect("bundled");
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        assert!(yaml.contains("litellm_settings:") && yaml.contains("- otel"),
            "OTel callback must be unconditional (every project gets it); got:\n{yaml}");
    }

    #[test] fn yaml_fallbacks_is_a_sequence_of_dicts_not_a_map() {
        // Regression test for the v0.1 boot-time crash documented on
        // RouterSettings: emitting `fallbacks: {fast: [...]}` makes
        // LiteLLM crash at startup with
        //   ValueError: Item 'fast' is not a dictionary
        // The correct shape is a list of single-key dicts.
        //
        // Parse the emitted YAML back through serde_yaml and assert
        // structurally — we're checking the WIRE FORMAT, not text.
        let (resolved, catalog) = resolve_demo();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        let value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let fallbacks = &value["router_settings"]["fallbacks"];
        assert!(fallbacks.is_sequence(),
            "router_settings.fallbacks MUST be a YAML sequence (list); \
             a mapping crashes LiteLLM at boot. Got: {fallbacks:?}");
        // Each entry in the sequence must itself be a mapping (the
        // single-key {primary_label: [aliases]} dict).
        for (i, entry) in fallbacks.as_sequence().unwrap().iter().enumerate() {
            assert!(entry.is_mapping(),
                "fallbacks[{i}] must be a mapping; got {entry:?}");
            let m = entry.as_mapping().unwrap();
            assert_eq!(m.len(), 1,
                "fallbacks[{i}] must be a single-key mapping; got {} keys",
                m.len());
        }
    }

    #[test] fn yaml_emits_block_style_for_router_settings_too() {
        // Pin block style on the whole config — important for human
        // readability of the generated file.
        //
        // The fallbacks field is a list-of-single-key-dicts (required
        // by LiteLLM at boot; see RouterSettings docs), so the nesting
        // is `fallbacks:\n  - fast:\n      - alias…` rather than the
        // older `fallbacks:\n  fast:\n    - alias…` map shape.
        let (resolved, catalog) = resolve_demo();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        assert!(yaml.contains("\nrouter_settings:\n  fallbacks:\n  - fast:\n"),
            "expected block-style list-of-single-key-dicts; got:\n{yaml}");
    }

    #[test] fn yaml_round_trips_with_full_config() {
        // serde_yaml roundtrip on the Thu output (includes
        // router_settings + multiple model_list entries).
        let (resolved, catalog) = resolve_demo();
        let yaml = generate_config(&resolved, &catalog).expect("generate");
        let _: serde_yaml::Value = serde_yaml::from_str(&yaml)
            .expect("full config must round-trip through serde_yaml");
    }

    // ----------------------------------------------------------------
    // Option C (week-5 follow-up): user-declared secret blocks
    // contribute to .env.example. See LANGUAGE-SPEC §5.6.
    // ----------------------------------------------------------------

    /// Build a resolution carrying a hand-rolled `secrets` list. The
    /// resolver doesn't touch secret blocks (they pass through from
    /// validate), so we can synthesize one directly.
    fn synthetic_with_secrets(secrets: Vec<aislang_core::ast::Secret>)
        -> ResolvedProject
    {
        use aislang_core::ast::Project;
        let mut project = Project { name: "x".into(), ..Project::default() };
        project.secrets = secrets;
        let mut bindings = BTreeMap::new();
        // One ollama primary so we have ONE provider-derived var
        // (OLLAMA_API_BASE) to dedupe against.
        bindings.insert("fast".to_string(),
            ModelId { provider: "ollama".into(), name: "llama_3_1_8b".into() });
        ResolvedProject {
            project,
            catalog_version: "test".into(),
            model_bindings:  bindings,
            prompt_hashes:   BTreeMap::new(),
            ..Default::default()
        }
    }

    fn mk_secret(label: &str, source: &str) -> aislang_core::ast::Secret {
        aislang_core::ast::Secret {
            label:  label.into(),
            source: source.into(),
            scope:  Vec::new(),
        }
    }

    #[test] fn env_example_includes_env_uri_secrets() {
        // A user-declared secret with `env://GITHUB_PAT` should appear
        // in the env-var list alongside the provider-derived
        // OLLAMA_API_BASE.
        let resolved = synthetic_with_secrets(vec![
            mk_secret("gh_token", "env://GITHUB_PAT"),
        ]);
        let env = generate_env_example(&resolved);
        assert!(env.contains("GITHUB_PAT="),
            "user-declared env:// secret missing from .env.example:\n{env}");
        assert!(env.contains("OLLAMA_API_BASE="),
            "provider-derived var also missing:\n{env}");
        // Alphabetical ordering: GITHUB_PAT before OLLAMA_API_BASE.
        let gh = env.find("GITHUB_PAT=").unwrap();
        let ol = env.find("OLLAMA_API_BASE=").unwrap();
        assert!(gh < ol, "expected alphabetical merge order; got {gh}, {ol}");
    }

    #[test] fn env_example_dedupes_secret_against_provider_creds() {
        // If a user declares `secret "ob" { source = "env://OLLAMA_API_BASE" }`
        // and the same var is provider-derived, it must appear exactly
        // once in the output. v0.2 will fold this into a single canonical
        // source; for v0.1 we just dedupe via BTreeSet.
        let resolved = synthetic_with_secrets(vec![
            mk_secret("ob", "env://OLLAMA_API_BASE"),
        ]);
        let env = generate_env_example(&resolved);
        let count = env.matches("OLLAMA_API_BASE=").count();
        assert_eq!(count, 1,
            "expected one OLLAMA_API_BASE line after dedup, got {count}:\n{env}");
    }

    #[test] fn env_example_lists_non_env_sources_in_footer() {
        // file:// (and any future non-env:// scheme) doesn't contribute
        // an env var — it shows up only in the informational footer.
        let resolved = synthetic_with_secrets(vec![
            mk_secret("db_creds", "file://./.secrets/db.json"),
        ]);
        let env = generate_env_example(&resolved);
        // No env var line for it.
        assert!(!env.contains("db_creds="));
        // But the source string appears in the footer comment.
        assert!(env.contains("Non-env-backed secret sources"),
            "missing footer section:\n{env}");
        assert!(env.contains("secret \"db_creds\": file://./.secrets/db.json"),
            "missing source annotation:\n{env}");
        // And the line is commented (starts with `#`).
        let line = env.lines()
            .find(|l| l.contains("db_creds"))
            .expect("footer line present");
        assert!(line.trim_start().starts_with('#'),
            "non-env source must be commented out, not active: {line:?}");
    }

    #[test] fn env_example_handles_mix_of_env_and_file_sources() {
        // Two secrets, one of each kind. env:// contributes a var,
        // file:// goes to the footer.
        let resolved = synthetic_with_secrets(vec![
            mk_secret("gh_token", "env://GITHUB_PAT"),
            mk_secret("db_creds", "file://./.secrets/db.json"),
        ]);
        let env = generate_env_example(&resolved);
        assert!(env.contains("GITHUB_PAT="),       "env:// secret in vars");
        assert!(env.contains("OLLAMA_API_BASE="),  "provider var present");
        assert!(!env.contains("db_creds="),        "file:// not a var");
        assert!(env.contains("file://./.secrets/db.json"),
                                                  "file:// in footer");
        // Sanity: footer appears AFTER the env vars.
        let gh_pos     = env.find("GITHUB_PAT=").unwrap();
        let footer_pos = env.find("Non-env-backed").unwrap();
        assert!(gh_pos < footer_pos,
            "env vars must precede footer; got {gh_pos} vs {footer_pos}");
    }

    #[test] fn env_example_documents_two_sources_in_header() {
        // The header explains where vars come from — important for the
        // user to understand the v0.1 dual-flow without reading the spec.
        let resolved = synthetic_with_secrets(vec![]);
        let env = generate_env_example(&resolved);
        assert!(env.contains("provider credentials derived from resolved models"),
            "header missing provider-source documentation:\n{env}");
        assert!(env.contains("`secret` blocks with `env://` URIs"),
            "header missing secret-block documentation:\n{env}");
    }
}

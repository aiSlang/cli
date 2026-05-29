//! aiSlang resolver — sits between `aislang-core` (parser + checker)
//! and `aislang-compose` (emitter). Bridges the two without forcing
//! either to know about the catalog directly (avoids the
//! aislang-core ↔ aislang-catalog cycle).
//!
//! Week 3 Wed: catalog ID validation against a catalog —
//! ModelIds in `model.prefer` / `model.fallback` / `model.pin` must
//! exist; deprecated entries produce warnings rather than errors
//! (compliance-audit-trail decision per PLAN §Locked design decisions).
//!
//! Week 4 Mon (this commit): model selection — pick one concrete catalog
//! entry per `model` block based on declared constraints + prefer/
//! fallback ordering + pin enforcement + budget.optimize_for tiebreaker.
//! Algorithm per LANGUAGE-SPEC-v0.md §6.
//!
//! Week 4 Tue–Fri (planned): prompt file hashing, OCI image
//! tag-to-digest pinning, lockfile reconciliation, Compose emission.

pub mod lockfile;
pub use lockfile::{Lockfile, LockfileDiff, LockfileError};

use aislang_catalog::{Catalog, CatalogModel};
use aislang_core::ast::{Model, ModelId, Project};
use aislang_core::types::OptimizeFor;
use aislang_core::Warning;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

// ============================================================================
// Public entry point
// ============================================================================

/// Validate every `ModelId` in the project's model blocks against the
/// catalog. Returns the list of non-fatal warnings (deprecated entries
/// in particular), or the first hard error (unknown catalog ID).
pub fn check_catalog_ids(project: &Project, catalog: &Catalog)
    -> Result<Vec<Warning>, String>
{
    let mut warnings = Vec::new();
    for model in &project.models {
        for (i, m_id) in model.prefer.iter().enumerate() {
            check_one_model_id(catalog, m_id,
                &model.label, &format!("prefer[{i}]"), &mut warnings)?;
        }
        for (i, m_id) in model.fallback.iter().enumerate() {
            check_one_model_id(catalog, m_id,
                &model.label, &format!("fallback[{i}]"), &mut warnings)?;
        }
        if let Some(pin) = &model.pin {
            check_one_model_id(catalog, pin,
                &model.label, "pin", &mut warnings)?;
        }
    }
    Ok(warnings)
}

/// Convenience helper for production callers: load the bundled catalog
/// and run [`check_catalog_ids`] against it.
pub fn check_against_bundled_catalog(project: &Project)
    -> Result<Vec<Warning>, String>
{
    let catalog = aislang_catalog::bundled()
        .map_err(|e| format!("failed to load bundled catalog: {e}"))?;
    check_catalog_ids(project, &catalog)
}

// ============================================================================
// Internals
// ============================================================================

fn check_one_model_id(
    catalog:   &Catalog,
    model_id:  &ModelId,
    src_label: &str,
    src_field: &str,
    warnings:  &mut Vec<Warning>,
) -> Result<(), String> {
    let entry = catalog.find(model_id).ok_or_else(|| {
        format!(
            "model `{src_label}` field `{src_field}` references catalog ID \
             `{}.{}` which is not in catalog {}",
            model_id.provider, model_id.name, catalog.version,
        )
    })?;

    if entry.deprecated {
        warnings.push(Warning {
            message: format!(
                "model `{src_label}` field `{src_field}` references deprecated \
                 catalog entry `{}.{}` in catalog {}; pinned use is still \
                 honoured for compliance-audit trails — see LANGUAGE-SPEC §5.5",
                model_id.provider, model_id.name, catalog.version,
            ),
        });
    }
    Ok(())
}

// ============================================================================
// Model resolver — week 4 Monday
// ============================================================================

/// The output of resolution: the original project plus the per-`model`-
/// block catalog-entry choices plus per-`prompt`-block content hashes
/// (Tuesday). Week 4 Wed adds oci_digests; week 4 Thu's Compose emitter
/// consumes the whole struct.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedProject {
    pub project:         Project,
    pub catalog_version: String,
    /// Map from `model.label` → catalog model ID chosen by the resolver.
    /// `BTreeMap` for deterministic JSON ordering (lockfile reproducibility).
    pub model_bindings:  BTreeMap<String, ModelId>,
    /// Map from `prompt.label` → SHA-256 hex digest of the file content
    /// at resolution time. Used by the lockfile (Wed) to detect prompt-
    /// file edits between runs and by the Compose emitter (Thu) to
    /// embed the hash as a config value for downstream cache-busting.
    pub prompt_hashes:   BTreeMap<String, String>,
    // Wed: pub oci_digests:   BTreeMap<String, String>,
}

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("model `{label}`: no catalog model satisfies constraints.\n{detail}")]
    NoCandidate { label: String, detail: String },

    #[error("model `{label}`: pin `{}.{}` violates constraints — {reason}",
            pin.provider, pin.name)]
    PinViolates { label: String, pin: ModelId, reason: String },

    #[error("prompt `{label}`: cannot read file `{}` (resolved from source dir): {source}",
            path.display())]
    PromptFileMissing {
        label:  String,
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Resolve every `model` and `prompt` block in the project against the
/// catalog and source-directory tree.
///
/// `source_dir` is the directory containing the `.ais` file — used to
/// resolve relative `prompt.file` paths. If a prompt's file path is
/// absolute, source_dir is ignored for that prompt.
///
/// Algorithm per LANGUAGE-SPEC-v0.md §6.
pub fn resolve(project: &Project, catalog: &Catalog, source_dir: &Path)
    -> Result<ResolvedProject, ResolveError>
{
    // ── models ───────────────────────────────────────────────────────
    let optimize_for = project.budget.as_ref()
        .map(|b| b.optimize_for)
        .unwrap_or(OptimizeFor::Cost);

    let mut model_bindings = BTreeMap::new();
    for model in &project.models {
        let chosen = resolve_one_model(model, catalog, optimize_for)?;
        model_bindings.insert(model.label.clone(), chosen);
    }

    // ── prompts (Tuesday) ────────────────────────────────────────────
    let mut prompt_hashes = BTreeMap::new();
    for prompt in &project.prompts {
        let abs_path = resolve_prompt_path(&prompt.file, source_dir);
        let hash = hash_file(&abs_path).map_err(|e| ResolveError::PromptFileMissing {
            label:  prompt.label.clone(),
            path:   abs_path,
            source: e,
        })?;
        prompt_hashes.insert(prompt.label.clone(), hash);
    }

    Ok(ResolvedProject {
        project:         project.clone(),
        catalog_version: catalog.version.clone(),
        model_bindings,
        prompt_hashes,
    })
}

/// Convenience: resolve against the bundled catalog.
pub fn resolve_against_bundled_catalog(project: &Project, source_dir: &Path)
    -> Result<ResolvedProject, ResolveError>
{
    let catalog = aislang_catalog::bundled()
        .map_err(|e| ResolveError::NoCandidate {
            label: "<bundled-catalog-load>".to_string(),
            detail: format!("failed to load bundled catalog: {e}"),
        })?;
    resolve(project, &catalog, source_dir)
}

// ============================================================================
// Prompt-file hashing — week 4 Tuesday
// ============================================================================

/// Resolve a prompt's `file` field into an absolute filesystem path.
/// Absolute paths are returned as-is; relative paths are joined onto
/// `source_dir` (the directory containing the `.ais` file).
fn resolve_prompt_path(file: &Path, source_dir: &Path) -> PathBuf {
    if file.is_absolute() {
        file.to_path_buf()
    } else {
        source_dir.join(file)
    }
}

/// SHA-256 hex digest of the byte content at `path`. Returns
/// `std::io::Error` on file-not-found / permission / read failure;
/// the caller wraps it in a `ResolveError::PromptFileMissing` with
/// the prompt label for context.
fn hash_file(path: &Path) -> Result<String, std::io::Error> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn resolve_one_model(
    block:         &Model,
    catalog:       &Catalog,
    _optimize_for: OptimizeFor,
) -> Result<ModelId, ResolveError> {
    // ── Spec-conformant prefer/fallback semantics (v0.2 Tue follow-up).
    //
    // LANGUAGE-SPEC §4.4 reads:
    //   prefer   : List<ModelId>   # ordered preference
    //   fallback : List<ModelId>   # used if no `prefer` candidate
    //                              # satisfies constraints
    //
    // The v0.1 resolver built a candidate pool from prefer ∪ fallback
    // and then let `optimize_for` (cost / latency / quality) re-sort
    // the pool — which silently overrode the declared preference order
    // whenever the cheapest/fastest entry sat in `fallback`.  Users
    // who wrote `prefer = [anthropic.claude_haiku_4_5]` reasonably
    // expected Haiku, but a free `fallback = [ollama.llama_3_1_8b]`
    // would win under `optimize_for = cost` (the demo's default).
    // See the Week 13 Tue follow-up PR for the live demo that
    // surfaced this.
    //
    // New behaviour:
    //   1. Walk `prefer` in declared order.  First candidate that
    //      satisfies every hard constraint (capabilities, context_min,
    //      cost_max, latency_p95, residency intersect) wins.
    //   2. If NO prefer candidate satisfies, walk `fallback` in declared
    //      order; first satisfier wins.
    //   3. If neither, surface the per-entry rejections (unchanged).
    //
    // `optimize_for` becomes effectively informational at the resolver
    // layer.  Catalog IDs are unique, so a single `ModelId` in prefer
    // never matches two catalog rows — there's no resolution-time tie
    // to break.  v0.2 keeps the field on the AST (and threads it
    // through here for future use as a cost-routing hint at runtime
    // / for v0.3+ multi-row catalog matches), but it no longer
    // re-orders prefer.

    // Step 1: filter catalog entries against the model block's hard
    // constraints, collecting per-entry rejection reasons for the
    // "no candidate" diagnostic.
    let mut candidates: Vec<&CatalogModel> = Vec::new();
    let mut rejections: Vec<(String, String)> = Vec::new();

    for cat in &catalog.models {
        match check_satisfies(block, cat) {
            Ok(()) => candidates.push(cat),
            Err(why) => rejections.push((cat.id.clone(), why)),
        }
    }

    // Step 2: pin enforcement (if set, pin MUST be the selection).
    // Pin must appear in prefer or fallback (declared spec) AND
    // satisfy every constraint.  Independent of prefer-order walk
    // below: pin short-circuits everything else.
    if let Some(pin) = &block.pin {
        let in_prefer    = block.prefer.iter().any(|p| p == pin);
        let in_fallback  = block.fallback.iter().any(|f| f == pin);
        let in_candidates = candidates.iter().any(|c| matches_id(c, pin));

        if (in_prefer || in_fallback) && in_candidates {
            return Ok(pin.clone());
        }

        // Pin didn't make it through — figure out why.
        let pin_dotted = format!("{}.{}", pin.provider, pin.name);
        let reason = match rejections.iter().find(|(id, _)| *id == pin_dotted) {
            Some((_, r)) => format!("constraint mismatch: {r}"),
            None => {
                if in_prefer || in_fallback {
                    "satisfies constraints but is not in catalog (bug?)".to_string()
                } else {
                    "pin must also appear in prefer or fallback".to_string()
                }
            }
        };
        return Err(ResolveError::PinViolates {
            label: block.label.clone(),
            pin:   pin.clone(),
            reason,
        });
    }

    // Step 3: walk `prefer` in declared order.  First candidate that
    // satisfies all hard constraints wins.  Spec-conformant.
    for pref_id in &block.prefer {
        if let Some(c) = candidates.iter().find(|c| matches_id(c, pref_id)) {
            return Ok(parse_catalog_id(&c.id));
        }
    }

    // Step 4: no `prefer` candidate satisfies — walk `fallback`.
    for fall_id in &block.fallback {
        if let Some(c) = candidates.iter().find(|c| matches_id(c, fall_id)) {
            return Ok(parse_catalog_id(&c.id));
        }
    }

    // Step 5: nothing in prefer or fallback satisfied — surface the
    // per-entry rejections (same diagnostic shape as v0.1).
    let detail = if rejections.is_empty() {
        "catalog has no models at all".to_string()
    } else if block.prefer.is_empty() && block.fallback.is_empty() {
        "model block has empty prefer + fallback + no pin; \
         must specify at least one of these so the resolver \
         knows which catalog entries to consider".to_string()
    } else {
        let lines: Vec<String> = rejections.iter()
            .map(|(id, r)| format!("    - {id}: {r}"))
            .collect();
        format!("constraint-rejections per catalog entry:\n{}",
                lines.join("\n"))
    };
    Err(ResolveError::NoCandidate {
        label: block.label.clone(),
        detail,
    })
}

/// Returns `Ok(())` if the catalog entry satisfies every hard constraint
/// declared on the model block; `Err(reason)` with a human-readable
/// rejection cause otherwise.
///
/// Deprecation is intentionally NOT a constraint failure here — the
/// Wednesday catalog check already emits a warning when a `model.prefer`
/// / `fallback` / `pin` references a deprecated entry. The compliance-
/// audit-trail use case (locked design decision) requires that pinning
/// a deprecated entry succeed. Leaving deprecated entries in the
/// candidate set means non-pin paths also retain access; if avoidance
/// is wanted for the default optimize_for selection, a future `--strict`
/// flag (post-v0.1) could exclude them.
fn check_satisfies(block: &Model, cat: &CatalogModel) -> Result<(), String> {
    // Capabilities: block.requires ⊆ cat.capabilities
    for req in &block.requires {
        if !cat.capabilities.contains(req) {
            return Err(format!("missing capability `{}`", capability_str(*req)));
        }
    }

    // Context window: cat.context_max >= block.context_min
    if cat.context_max.units < block.context_min.units {
        return Err(format!(
            "context window {} < required {}",
            cat.context_max.units, block.context_min.units,
        ));
    }

    // Cost: if block.cost_max set, cat.cost_input ≤ cost_max.numerator.
    // v0.1 ASSUMPTION (see PLAN week-4 risk #1): both sides are per-
    // 1k-tokens, so direct micro_units comparison is meaningful. A
    // future spec extension that allows per-1M-tokens denominators
    // will need denominator unit-checking here first.
    if let Some(rate) = &block.cost_max {
        // Money's Ord impl gives the right semantics:
        //   Unlimited catalog cost > any Capped numerator → exclude.
        //   Capped(X) > Capped(Y) → standard arithmetic comparison.
        if cat.cost_input > rate.numerator {
            let lhs = cat.cost_input.micro_units()
                .map(|m| format!("{m} µ-USD"))
                .unwrap_or_else(|| "unlimited".to_string());
            let rhs = rate.numerator.micro_units()
                .map(|m| format!("{m} µ-USD"))
                .unwrap_or_else(|| "unlimited".to_string());
            return Err(format!(
                "cost {lhs}/1k-tok > max allowed {rhs}/1k-tok"));
        }
    }

    // Latency: cat.latency_p95 ≤ block.latency_p95 (if set)
    if let Some(d) = &block.latency_p95 {
        if cat.latency_p95.nanos > d.nanos {
            return Err(format!(
                "p95 latency {}ns > allowed {}ns",
                cat.latency_p95.nanos, d.nanos,
            ));
        }
    }

    // Residency: block.residency ⊆ cat.residency (every region the
    // user requires must be supported by the catalog model)
    if let Some(allowed) = &block.residency {
        for region in allowed {
            if !cat.residency.contains(region) {
                return Err(format!("missing residency `{}`",
                    residency_str(*region)));
            }
        }
    }

    Ok(())
}

fn matches_id(cat: &CatalogModel, mid: &ModelId) -> bool {
    cat.id == format!("{}.{}", mid.provider, mid.name)
}

fn parse_catalog_id(id_str: &str) -> ModelId {
    // catalog "id" is the dotted "provider.name" form — guaranteed
    // well-formed at catalog load time (the deserializer would have
    // failed otherwise on the typed shims).
    let (provider, name) = id_str.split_once('.').unwrap_or((id_str, ""));
    ModelId {
        provider: provider.to_string(),
        name:     name.to_string(),
    }
}

/// Capability → user-facing string. Used in rejection messages so
/// "missing capability `tool_use`" is more readable than
/// "missing capability `ToolUse`".
fn capability_str(c: aislang_core::types::Capability) -> &'static str {
    use aislang_core::types::Capability::*;
    match c {
        Chat              => "chat",
        Embedding         => "embedding",
        ToolUse           => "tool_use",
        Vision            => "vision",
        Audio             => "audio",
        Reasoning         => "reasoning",
        LargeContext      => "large_context",
        StructuredOutput  => "structured_output",
    }
}

fn residency_str(r: aislang_core::types::Residency) -> &'static str {
    use aislang_core::types::Residency::*;
    match r {
        Us   => "us",
        Eu   => "eu",
        Uk   => "uk",
        Apac => "apac",
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use aislang_catalog::CatalogModel;
    use aislang_core::types::{Capability, Currency, Duration, Money, Residency, Size};

    /// Synthetic test catalog so tests have a deprecated entry without
    /// touching production catalog.json.
    fn synthetic_catalog() -> Catalog {
        Catalog {
            version: "test-2026-05-27".into(),
            models: vec![
                CatalogModel {
                    id: "test.active_model".into(),
                    provider: "test".into(),
                    litellm_name: "test/active".into(),
                    capabilities: vec![Capability::Chat],
                    context_max: Size { units: 100 * 1024 },
                    cost_input:  Money::Capped { currency: Currency::Usd, micro_units: 1_000 },
                    cost_output: Money::Capped { currency: Currency::Usd, micro_units: 5_000 },
                    latency_p95: Duration { nanos: 1_000_000_000 },
                    residency:   vec![Residency::Us],
                    deprecated: false,
                },
                CatalogModel {
                    id: "test.legacy_model".into(),
                    provider: "test".into(),
                    litellm_name: "test/legacy".into(),
                    capabilities: vec![Capability::Chat],
                    context_max: Size { units: 50 * 1024 },
                    cost_input:  Money::Capped { currency: Currency::Usd, micro_units: 100 },
                    cost_output: Money::Capped { currency: Currency::Usd, micro_units: 500 },
                    latency_p95: Duration { nanos: 2_000_000_000 },
                    residency:   vec![Residency::Us],
                    deprecated: true,
                },
            ],
        }
    }

    /// Test helper: parse + run the in-core checker + run catalog
    /// validation against the supplied catalog. Returns Vec<Warning>
    /// on success (combining both stages' warnings) or the first
    /// hard error encountered.
    fn validate_with(body: &str, catalog: &Catalog) -> Result<Vec<Warning>, String> {
        let src = format!(
            r#"project "p" {{
                budget {{ hard_cap_per_day = $1   hard_cap_per_run = $0.01 }}
                {body}
            }}"#
        );
        let (project, mut warnings) = aislang_core::validate(&src)
            .map_err(|e| format!("{e}"))?;
        let cat_warnings = check_catalog_ids(&project, catalog)?;
        warnings.extend(cat_warnings);
        Ok(warnings)
    }

    // --- bundled-catalog tests ---

    #[test] fn known_bundled_catalog_id_passes_cleanly() {
        let body = r#"
            model "fast" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_haiku_4_5]
            }
        "#;
        let cat = aislang_catalog::bundled().expect("bundled");
        let warnings = validate_with(body, &cat).expect("known ID must pass");
        assert!(warnings.is_empty(), "expected no warnings; got {warnings:?}");
    }

    #[test] fn unknown_catalog_id_in_prefer_fails() {
        let body = r#"
            model "fast" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.fake]
            }
        "#;
        let cat = aislang_catalog::bundled().expect("bundled");
        let err = validate_with(body, &cat).err()
            .expect("unknown catalog ID must fail");
        assert!(err.contains("references catalog ID `anthropic.fake`"),
            "error did not name the offending ID: {err}");
        assert!(err.contains("which is not in catalog"));
    }

    #[test] fn unknown_catalog_id_in_fallback_fails() {
        let body = r#"
            model "fast" {
                requires    = [chat]
                context_min = 32k
                fallback    = [openai.imaginary_v9]
            }
        "#;
        let cat = aislang_catalog::bundled().expect("bundled");
        let err = validate_with(body, &cat).err().expect("must fail");
        assert!(err.contains("field `fallback[0]`"));
        assert!(err.contains("openai.imaginary_v9"));
    }

    #[test] fn unknown_catalog_id_in_pin_fails() {
        let body = r#"
            model "fast" {
                requires    = [chat]
                context_min = 32k
                pin         = anthropic.never_existed
            }
        "#;
        let cat = aislang_catalog::bundled().expect("bundled");
        let err = validate_with(body, &cat).err().expect("must fail");
        assert!(err.contains("field `pin`"));
        assert!(err.contains("anthropic.never_existed"));
    }

    // --- synthetic-catalog tests (deprecated path) ---

    #[test] fn deprecated_pin_succeeds_with_warning() {
        // Compliance-audit-trail decision: pin = deprecated must succeed.
        let body = r#"
            model "compliance" {
                requires    = [chat]
                context_min = 32k
                pin         = test.legacy_model
            }
        "#;
        let warnings = validate_with(body, &synthetic_catalog())
            .expect("deprecated entry must NOT be a hard error");
        assert_eq!(warnings.len(), 1, "expected exactly one warning");
        let msg = &warnings[0].message;
        assert!(msg.contains("deprecated"));
        assert!(msg.contains("test.legacy_model"));
        assert!(msg.contains("pin"));
        assert!(msg.contains("compliance"));
    }

    #[test] fn deprecated_in_prefer_emits_warning() {
        let body = r#"
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [test.active_model, test.legacy_model]
            }
        "#;
        let warnings = validate_with(body, &synthetic_catalog()).expect("must succeed");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("test.legacy_model"));
        assert!(warnings[0].message.contains("prefer[1]"));
    }

    #[test] fn multiple_deprecated_refs_emit_multiple_warnings() {
        let body = r#"
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [test.legacy_model]
                fallback    = [test.legacy_model]
            }
        "#;
        let warnings = validate_with(body, &synthetic_catalog())
            .expect("two deprecated refs must succeed");
        assert_eq!(warnings.len(), 2);
    }

    #[test] fn active_only_yields_no_warnings() {
        let body = r#"
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [test.active_model]
            }
        "#;
        let warnings = validate_with(body, &synthetic_catalog())
            .expect("active entry must pass cleanly");
        assert!(warnings.is_empty());
    }

    // ----------------------------------------------------------------
    // Model resolver — week 4 Monday
    // ----------------------------------------------------------------

    /// Build a project from a body fragment, then resolve against a
    /// supplied catalog. Returns the resolved binding for the named
    /// `model` block (or the first one if name is empty).
    ///
    /// Uses a tempfile-less dummy source dir — tests that pass bodies
    /// with no `prompt` blocks don't touch the filesystem. Tests
    /// involving prompt hashing use their own source-dir setup below.
    fn resolve_model(body: &str, catalog: &Catalog, model_label: &str)
        -> Result<ModelId, ResolveError>
    {
        let src = format!(
            r#"project "p" {{
                budget {{ hard_cap_per_day = $1   hard_cap_per_run = $0.01 }}
                {body}
            }}"#
        );
        let (project, _warns) = aislang_core::validate(&src)
            .unwrap_or_else(|e| panic!("validate failed: {e}"));
        let resolved = resolve(&project, catalog, std::path::Path::new("."))?;
        let label = if model_label.is_empty() { "m" } else { model_label };
        Ok(resolved.model_bindings.get(label)
            .unwrap_or_else(|| panic!("no binding for `{label}`"))
            .clone())
    }

    /// A small richer-than-synthetic catalog with cost/latency variation
    /// across two entries (cheap + slow, expensive + fast) so optimize_for
    /// tests have meaningful candidates to choose between.
    fn variety_catalog() -> Catalog {
        Catalog {
            version: "test-variety".into(),
            models: vec![
                CatalogModel {
                    id: "test.cheap_slow".into(),
                    provider: "test".into(),
                    litellm_name: "test/cheap_slow".into(),
                    capabilities: vec![Capability::Chat, Capability::ToolUse],
                    context_max: Size { units: 32 * 1024 },
                    cost_input:  Money::Capped { currency: Currency::Usd, micro_units: 100 },     // cheaper
                    cost_output: Money::Capped { currency: Currency::Usd, micro_units: 500 },
                    latency_p95: Duration { nanos: 5_000_000_000 },                       // slower
                    residency:   vec![Residency::Us, Residency::Eu],
                    deprecated: false,
                },
                CatalogModel {
                    id: "test.fast_expensive".into(),
                    provider: "test".into(),
                    litellm_name: "test/fast_expensive".into(),
                    capabilities: vec![Capability::Chat, Capability::ToolUse],
                    context_max: Size { units: 32 * 1024 },
                    cost_input:  Money::Capped { currency: Currency::Usd, micro_units: 10_000 },  // pricier
                    cost_output: Money::Capped { currency: Currency::Usd, micro_units: 50_000 },
                    latency_p95: Duration { nanos: 500_000_000 },                          // faster
                    residency:   vec![Residency::Us],
                    deprecated: false,
                },
                CatalogModel {
                    id: "test.small_context".into(),
                    provider: "test".into(),
                    litellm_name: "test/small".into(),
                    capabilities: vec![Capability::Chat],
                    context_max: Size { units: 8 * 1024 },                                 // too small
                    cost_input:  Money::Capped { currency: Currency::Usd, micro_units: 50 },
                    cost_output: Money::Capped { currency: Currency::Usd, micro_units: 250 },
                    latency_p95: Duration { nanos: 200_000_000 },
                    residency:   vec![Residency::Us],
                    deprecated: false,
                },
            ],
        }
    }

    #[test] fn demo_model_resolves_via_prefer_order() {
        // End-to-end on the actual support-agent.ais model block against
        // the real bundled catalog.
        //
        // LANGUAGE-SPEC §4.4 says `prefer` is an ORDERED preference list
        // and `fallback` is "used if no `prefer` candidate satisfies
        // constraints".  v0.2 Tue follow-up made the resolver match the
        // spec: walk prefer in declared order, first satisfier wins;
        // fall back only when no prefer item satisfies.
        //
        // The demo declares `prefer = [haiku, gpt-4o-mini]` + `fallback
        // = [llama3]`.  Haiku is in the catalog, satisfies every hard
        // constraint (chat+tool_use, 200k≥32k context, $0.001/1k ≤ the
        // demo's $0.001/1k cost_max, 1.5s ≤ 2s latency_p95, us+eu
        // residency intersect non-empty).  So it wins — regardless of
        // `optimize_for`.
        //
        // The v0.1 behaviour (cost optimization piercing prefer order)
        // is gone: a free Ollama fallback no longer silently displaces
        // a declared paid preference.  Users who want cost-driven
        // selection should declare it in prefer order directly.
        let src = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..").join("..")
                .join("examples").join("support-agent.ais")
        ).expect("read demo");
        let (project, _) = aislang_core::validate(&src)
            .expect("demo must validate");
        let cat = aislang_catalog::bundled().expect("bundled catalog");
        let examples_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..").join("..").join("examples");
        let resolved = resolve(&project, &cat, &examples_dir).expect("must resolve");
        let fast = resolved.model_bindings.get("fast")
            .expect("model `fast` binding");
        assert_eq!(fast,
            &ModelId { provider: "anthropic".into(), name: "claude_haiku_4_5".into() },
            "prefer[0] = anthropic.claude_haiku_4_5 satisfies every \
             constraint and therefore wins under the v0.2 Tue follow-up \
             resolver semantics (LANGUAGE-SPEC §4.4)");
    }

    #[test] fn first_prefer_wins_when_satisfying() {
        // Spec-conformant: prefer[0] is picked when it satisfies all
        // constraints, regardless of optimize_for.
        let body = r#"
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [test.cheap_slow, test.fast_expensive]
            }
        "#;
        let id = resolve_model(body, &variety_catalog(), "m")
            .expect("must resolve");
        assert_eq!(id.name, "cheap_slow",
            "prefer[0] wins when satisfying — `optimize_for = cost` is \
             default but no longer reorders prefer (v0.2 Tue follow-up)");
    }

    #[test] fn optimize_for_latency_does_not_override_prefer_order() {
        // v0.1: `optimize_for = latency` reordered prefer to pick the
        // fastest satisfier.  v0.2 Tue follow-up: optimize_for is a
        // no-op for selection; prefer[0] wins whenever it satisfies.
        //
        // This test pins that change: prefer = [cheap_slow,
        // fast_expensive] + optimize_for = latency MUST yield
        // cheap_slow (the declared first preference), NOT fast_expensive.
        let src = r#"project "p" {
            budget {
                hard_cap_per_day = $1   hard_cap_per_run = $0.01
                optimize_for     = latency
            }
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [test.cheap_slow, test.fast_expensive]
            }
        }"#;
        let (project, _) = aislang_core::validate(src).expect("validate");
        let resolved = resolve(&project, &variety_catalog(), std::path::Path::new(".")).expect("resolve");
        assert_eq!(resolved.model_bindings.get("m").unwrap().name, "cheap_slow",
            "prefer[0] = test.cheap_slow MUST win — optimize_for is no \
             longer a re-orderer (v0.2 Tue follow-up; see resolver \
             rewrite comment for the spec-vs-v0.1 contrast)");
    }

    #[test] fn optimize_for_cost_does_not_override_prefer_order() {
        // Symmetric to the latency test above.  `optimize_for = cost`
        // (the demo's default) MUST NOT cause the cheaper fallback to
        // displace a satisfying prefer[0].  This is the literal bug
        // the user surfaced via Open WebUI live testing.
        let src = r#"project "p" {
            budget {
                hard_cap_per_day = $1   hard_cap_per_run = $0.01
                optimize_for     = cost
            }
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [test.fast_expensive]
                fallback    = [test.cheap_slow]
            }
        }"#;
        let (project, _) = aislang_core::validate(src).expect("validate");
        let resolved = resolve(&project, &variety_catalog(), std::path::Path::new(".")).expect("resolve");
        assert_eq!(resolved.model_bindings.get("m").unwrap().name, "fast_expensive",
            "prefer[0] = fast_expensive satisfies → wins.  cheap_slow \
             stays in the fallback chain (LiteLLM router_settings) but \
             does not become the primary alias.  This is the literal \
             user-reported bug — `optimize_for = cost` no longer hijacks \
             prefer order.");
    }

    #[test] fn cost_max_constraint_skips_first_prefer_falls_to_second() {
        // The hard-constraint path: prefer[0] violates `cost_max`, so
        // it's not in `candidates`, so the resolver walks past it to
        // prefer[1].  Pins that the prefer-walk doesn't blindly pick
        // unconditionally — it still respects the constraint set.
        let src = r#"project "p" {
            budget { hard_cap_per_day = $1   hard_cap_per_run = $0.01 }
            model "m" {
                requires    = [chat]
                context_min = 32k
                cost_max    = $0.05 per 1k_tokens
                prefer      = [test.fast_expensive, test.cheap_slow]
            }
        }"#;
        // variety_catalog: fast_expensive costs $0.01/k input ($10000 µ
        // per 1024 tokens), well above $0.05/k = $50000µ / 1024.
        // Actually compute: cost_max = $0.05 per 1k_tokens = 50_000µ /
        // 1024.  fast_expensive cost_input = 10_000µ / 1024.
        // 10_000µ < 50_000µ → fast_expensive satisfies.  So this body
        // would pick fast_expensive, not the desired skip.
        //
        // Drop cost_max way down so fast_expensive (10_000µ/1024) fails
        // and cheap_slow (50µ/1024) passes.
        let src = r#"project "p" {
            budget { hard_cap_per_day = $1   hard_cap_per_run = $0.01 }
            model "m" {
                requires    = [chat]
                context_min = 32k
                cost_max    = $0.0001 per 1k_tokens
                prefer      = [test.fast_expensive, test.cheap_slow]
            }
        }"#;
        let (project, _) = aislang_core::validate(src).expect("validate");
        let resolved = resolve(&project, &variety_catalog(), std::path::Path::new(".")).expect("resolve");
        assert_eq!(resolved.model_bindings.get("m").unwrap().name, "cheap_slow",
            "prefer[0] fast_expensive ($0.01/k > cost_max $0.0001/k) \
             fails the constraint → resolver walks to prefer[1] cheap_slow");
    }

    #[test] fn optimize_for_quality_picks_first_preference() {
        let src = r#"project "p" {
            budget {
                hard_cap_per_day = $1   hard_cap_per_run = $0.01
                optimize_for     = quality
            }
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [test.fast_expensive, test.cheap_slow]
            }
        }"#;
        let (project, _) = aislang_core::validate(src).expect("validate");
        let resolved = resolve(&project, &variety_catalog(), std::path::Path::new(".")).expect("resolve");
        // First in prefer wins under `quality` — regardless of cost/latency.
        assert_eq!(resolved.model_bindings.get("m").unwrap().name, "fast_expensive");
    }

    #[test] fn pin_succeeds_when_satisfying_and_in_prefer() {
        let body = r#"
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [test.cheap_slow, test.fast_expensive]
                pin         = test.fast_expensive
            }
        "#;
        let id = resolve_model(body, &variety_catalog(), "m")
            .expect("pin should succeed");
        assert_eq!(id.name, "fast_expensive");
    }

    #[test] fn pin_violates_constraint_errors_with_reason() {
        // small_context has context_max = 8k, but block requires 32k.
        let body = r#"
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [test.small_context]
                pin         = test.small_context
            }
        "#;
        let err = resolve_model(body, &variety_catalog(), "m")
            .err().expect("must error");
        let s = format!("{err}");
        assert!(s.contains("test.small_context"));
        assert!(s.contains("violates constraints"));
        assert!(s.contains("context window"));
    }

    #[test] fn pin_not_in_prefer_or_fallback_errors() {
        let body = r#"
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [test.cheap_slow]
                pin         = test.fast_expensive
            }
        "#;
        let err = resolve_model(body, &variety_catalog(), "m")
            .err().expect("must error");
        let s = format!("{err}");
        assert!(s.contains("must also appear in prefer or fallback"),
            "got: {s}");
    }

    #[test] fn no_candidate_lists_per_entry_rejections() {
        // Require an unrealistically-large context window — no catalog
        // entry satisfies. Error should list rejection per entry.
        let body = r#"
            model "m" {
                requires    = [chat]
                context_min = 10000000
                prefer      = [test.cheap_slow, test.fast_expensive, test.small_context]
            }
        "#;
        let err = resolve_model(body, &variety_catalog(), "m")
            .err().expect("must error");
        let s = format!("{err}");
        assert!(s.contains("no catalog model satisfies"));
        assert!(s.contains("context window"));
        assert!(s.contains("test.cheap_slow"));
        assert!(s.contains("test.fast_expensive"));
        assert!(s.contains("test.small_context"));
    }

    #[test] fn empty_prefer_and_fallback_errors_with_hint() {
        // A model block with only `requires` + `context_min` (no
        // prefer/fallback/pin) — must error clearly.
        let body = r#"
            model "m" {
                requires    = [chat]
                context_min = 32k
            }
        "#;
        let err = resolve_model(body, &variety_catalog(), "m")
            .err().expect("must error");
        let s = format!("{err}");
        assert!(s.contains("empty prefer + fallback + no pin"),
            "got: {s}");
    }

    #[test] fn residency_filter_excludes_us_only_when_eu_required() {
        // fast_expensive is us-only; cheap_slow has both. Require eu →
        // only cheap_slow remains.
        let body = r#"
            model "m" {
                requires    = [chat]
                context_min = 32k
                residency   = [eu]
                prefer      = [test.fast_expensive, test.cheap_slow]
            }
        "#;
        let id = resolve_model(body, &variety_catalog(), "m")
            .expect("must resolve");
        assert_eq!(id.name, "cheap_slow");
    }

    #[test] fn capability_filter_excludes_missing_tool_use() {
        // small_context has only [chat] (no tool_use); require both.
        let body = r#"
            model "m" {
                requires    = [chat, tool_use]
                context_min = 8k
                prefer      = [test.small_context]
            }
        "#;
        let err = resolve_model(body, &variety_catalog(), "m")
            .err().expect("must error");
        let s = format!("{err}");
        assert!(s.contains("missing capability `tool_use`"));
    }

    #[test] fn fallback_used_when_no_prefer_matches() {
        // prefer entry doesn't satisfy; fallback does.
        let body = r#"
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [test.small_context]
                fallback    = [test.cheap_slow]
            }
        "#;
        let id = resolve_model(body, &variety_catalog(), "m")
            .expect("must resolve via fallback");
        assert_eq!(id.name, "cheap_slow");
    }

    #[test] fn resolved_project_has_catalog_version() {
        // White-box sanity: the version threads through.
        let src = r#"project "p" {
            budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
            model "m" {
                requires = [chat]  context_min = 32k
                prefer   = [test.cheap_slow]
            }
        }"#;
        let (project, _) = aislang_core::validate(src).expect("validate");
        let resolved = resolve(&project, &variety_catalog(), std::path::Path::new(".")).expect("resolve");
        assert_eq!(resolved.catalog_version, "test-variety");
    }

    // ----------------------------------------------------------------
    // Prompt-file hashing — week 4 Tuesday
    // ----------------------------------------------------------------

    use std::path::{Path, PathBuf};

    fn examples_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..").join("..").join("examples")
    }

    /// Build a project with a `prompt` block whose file path is the
    /// supplied relative path; resolve against the supplied source dir.
    fn resolve_with_prompt(file_path: &str, source_dir: &Path)
        -> Result<ResolvedProject, ResolveError>
    {
        let src = format!(
            r#"project "p" {{
                budget {{ hard_cap_per_day = $1   hard_cap_per_run = $0.01 }}
                prompt "system" {{ file = "{file_path}"  version = "1.0.0" }}
            }}"#
        );
        let (project, _) = aislang_core::validate(&src).expect("validate");
        resolve(&project, &variety_catalog(), source_dir)
    }

    #[test] fn prompt_file_produces_64hex_hash() {
        // examples/prompts/system.md exists in the repo; hash it.
        let resolved = resolve_with_prompt("./prompts/system.md", &examples_dir())
            .expect("must resolve");
        let hash = resolved.prompt_hashes.get("system")
            .expect("system prompt hashed");
        assert_eq!(hash.len(), 64, "SHA-256 hex digest should be 64 chars");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "expected lowercase hex; got `{hash}`");
    }

    #[test] fn prompt_hash_is_deterministic() {
        // Two resolutions of the same file produce identical hashes —
        // crucial for lockfile-bit-for-bit reproducibility (Wed).
        let r1 = resolve_with_prompt("./prompts/system.md", &examples_dir())
            .expect("first resolve");
        let r2 = resolve_with_prompt("./prompts/system.md", &examples_dir())
            .expect("second resolve");
        assert_eq!(r1.prompt_hashes, r2.prompt_hashes);
    }

    #[test] fn missing_prompt_file_errors_with_label_and_path() {
        let err = resolve_with_prompt("./does-not-exist.md", &examples_dir())
            .err().expect("must error");
        let s = format!("{err}");
        assert!(s.contains("prompt `system`"));
        assert!(s.contains("does-not-exist.md"));
        // The std::io::Error display is appended via #[source].
        assert!(s.contains("cannot read file"));
    }

    #[test] fn absolute_prompt_path_bypasses_source_dir() {
        // Construct an absolute path to the existing demo file; pass an
        // unrelated source_dir. Absolute file should win.
        let abs = examples_dir().join("prompts").join("system.md");
        let abs_str = abs.to_string_lossy().to_string();
        let unrelated_dir = std::env::temp_dir();
        let src = format!(
            r#"project "p" {{
                budget {{ hard_cap_per_day = $1   hard_cap_per_run = $0.01 }}
                prompt "system" {{ file = "{abs_str}"  version = "1.0.0" }}
            }}"#
        );
        let (project, _) = aislang_core::validate(&src).expect("validate");
        let resolved = resolve(&project, &variety_catalog(), &unrelated_dir)
            .expect("absolute path must resolve regardless of source_dir");
        assert_eq!(resolved.prompt_hashes.get("system").unwrap().len(), 64);
    }

    #[test] fn different_content_produces_different_hash() {
        // Use std::env::temp_dir + std::fs to create two same-named
        // prompts under different dirs with different bytes — assert
        // resulting hashes differ. This pins the "hash is
        // content-sensitive" property of SHA-256, separate from any
        // particular file the demo references.
        let tmp_a = std::env::temp_dir().join("aislang-test-prompts-a");
        let tmp_b = std::env::temp_dir().join("aislang-test-prompts-b");
        std::fs::create_dir_all(&tmp_a).expect("mkdir a");
        std::fs::create_dir_all(&tmp_b).expect("mkdir b");
        std::fs::write(tmp_a.join("p.md"), b"alpha").expect("write a");
        std::fs::write(tmp_b.join("p.md"), b"beta -- different bytes").expect("write b");

        let r_a = resolve_with_prompt("./p.md", &tmp_a).expect("a");
        let r_b = resolve_with_prompt("./p.md", &tmp_b).expect("b");
        assert_ne!(r_a.prompt_hashes.get("system"),
                   r_b.prompt_hashes.get("system"),
            "different content must produce different SHA-256 digests");

        // Cleanup; ignore errors.
        let _ = std::fs::remove_dir_all(&tmp_a);
        let _ = std::fs::remove_dir_all(&tmp_b);
    }

    #[test] fn resolved_project_json_includes_prompt_hashes() {
        let resolved = resolve_with_prompt("./prompts/system.md", &examples_dir())
            .expect("must resolve");
        let json = serde_json::to_string_pretty(&resolved).expect("serialize");
        assert!(json.contains("\"prompt_hashes\""));
        assert!(json.contains("\"system\""));
    }
}

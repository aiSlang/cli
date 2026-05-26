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

use aislang_catalog::{Catalog, CatalogModel};
use aislang_core::ast::{Model, ModelId, Project};
use aislang_core::types::OptimizeFor;
use aislang_core::Warning;
use serde::Serialize;
use std::collections::BTreeMap;
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
/// block catalog-entry choices. Week 4 Tue/Wed add prompt_hashes and
/// oci_digests fields; week 4 Thu's Compose emitter consumes the whole
/// struct.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedProject {
    pub project:         Project,
    pub catalog_version: String,
    /// Map from `model.label` → catalog model ID chosen by the resolver.
    /// `BTreeMap` for deterministic JSON ordering (lockfile reproducibility).
    pub model_bindings:  BTreeMap<String, ModelId>,
    // Tue: pub prompt_hashes: BTreeMap<String, String>,
    // Wed: pub oci_digests:   BTreeMap<String, String>,
}

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("model `{label}`: no catalog model satisfies constraints.\n{detail}")]
    NoCandidate { label: String, detail: String },

    #[error("model `{label}`: pin `{}.{}` violates constraints — {reason}",
            pin.provider, pin.name)]
    PinViolates { label: String, pin: ModelId, reason: String },
}

/// Resolve every `model` block in the project against the catalog.
/// Algorithm per LANGUAGE-SPEC-v0.md §6.
pub fn resolve(project: &Project, catalog: &Catalog)
    -> Result<ResolvedProject, ResolveError>
{
    let optimize_for = project.budget.as_ref()
        .map(|b| b.optimize_for)
        .unwrap_or(OptimizeFor::Cost);

    let mut model_bindings = BTreeMap::new();
    for model in &project.models {
        let chosen = resolve_one_model(model, catalog, optimize_for)?;
        model_bindings.insert(model.label.clone(), chosen);
    }

    Ok(ResolvedProject {
        project:         project.clone(),
        catalog_version: catalog.version.clone(),
        model_bindings,
    })
}

/// Convenience: resolve against the bundled catalog.
pub fn resolve_against_bundled_catalog(project: &Project)
    -> Result<ResolvedProject, ResolveError>
{
    let catalog = aislang_catalog::bundled()
        .map_err(|e| ResolveError::NoCandidate {
            label: "<bundled-catalog-load>".to_string(),
            detail: format!("failed to load bundled catalog: {e}"),
        })?;
    resolve(project, &catalog)
}

fn resolve_one_model(
    block:        &Model,
    catalog:      &Catalog,
    optimize_for: OptimizeFor,
) -> Result<ModelId, ResolveError> {
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

    // Step 2: order candidates by `prefer` then `fallback`. Spec §6:
    // ordered contains only catalog entries that BOTH (a) satisfy
    // constraints AND (b) are named in prefer/fallback.
    let mut ordered: Vec<&CatalogModel> = Vec::new();
    for pref_id in &block.prefer {
        if let Some(c) = candidates.iter().find(|c| matches_id(c, pref_id)) {
            ordered.push(*c);
        }
    }
    for fall_id in &block.fallback {
        if !ordered.iter().any(|c| matches_id(c, fall_id)) {
            if let Some(c) = candidates.iter().find(|c| matches_id(c, fall_id)) {
                ordered.push(*c);
            }
        }
    }

    // Step 3: pin enforcement (if set, pin MUST be selected).
    if let Some(pin) = &block.pin {
        if ordered.iter().any(|c| matches_id(c, pin)) {
            return Ok(pin.clone());
        }
        // Pin didn't make it into ordered — figure out why.
        let pin_dotted = format!("{}.{}", pin.provider, pin.name);
        let reason = match rejections.iter().find(|(id, _)| *id == pin_dotted) {
            Some((_, r)) => format!("constraint mismatch: {r}"),
            None => {
                let in_prefer   = block.prefer.iter().any(|p| p == pin);
                let in_fallback = block.fallback.iter().any(|f| f == pin);
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

    // Step 4: no pin — error if nothing satisfies, else apply
    // optimize_for tiebreaker over the ordered candidates.
    if ordered.is_empty() {
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
        return Err(ResolveError::NoCandidate {
            label: block.label.clone(),
            detail,
        });
    }

    let chosen: &CatalogModel = match optimize_for {
        OptimizeFor::Cost => ordered.iter()
            .min_by_key(|c| c.cost_input.micro_units)
            .copied()
            .expect("ordered non-empty"),
        OptimizeFor::Latency => ordered.iter()
            .min_by_key(|c| c.latency_p95.nanos)
            .copied()
            .expect("ordered non-empty"),
        OptimizeFor::Quality => ordered[0],   // first preference wins
    };

    Ok(parse_catalog_id(&chosen.id))
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
        if cat.cost_input.micro_units > rate.numerator.micro_units {
            return Err(format!(
                "cost {} µ-USD/1k-tok > max allowed {} µ-USD/1k-tok",
                cat.cost_input.micro_units, rate.numerator.micro_units,
            ));
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
                    cost_input:  Money { currency: Currency::Usd, micro_units: 1_000 },
                    cost_output: Money { currency: Currency::Usd, micro_units: 5_000 },
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
                    cost_input:  Money { currency: Currency::Usd, micro_units: 100 },
                    cost_output: Money { currency: Currency::Usd, micro_units: 500 },
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
        let resolved = resolve(&project, catalog)?;
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
                    cost_input:  Money { currency: Currency::Usd, micro_units: 100 },     // cheaper
                    cost_output: Money { currency: Currency::Usd, micro_units: 500 },
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
                    cost_input:  Money { currency: Currency::Usd, micro_units: 10_000 },  // pricier
                    cost_output: Money { currency: Currency::Usd, micro_units: 50_000 },
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
                    cost_input:  Money { currency: Currency::Usd, micro_units: 50 },
                    cost_output: Money { currency: Currency::Usd, micro_units: 250 },
                    latency_p95: Duration { nanos: 200_000_000 },
                    residency:   vec![Residency::Us],
                    deprecated: false,
                },
            ],
        }
    }

    #[test] fn demo_model_resolves_under_cost_optimization() {
        // End-to-end on the actual support-agent.ais model block against
        // the real bundled catalog with `budget.optimize_for = cost`.
        //
        // Per LANGUAGE-SPEC §6: `ordered` is built from
        // prefer + fallback (all satisfying entries together), then
        // optimize_for picks from that union. Under `cost`, the
        // CHEAPEST satisfying entry wins — not the first in prefer.
        //
        // The demo's fallback `ollama.llama_3_1_8b` is FREE (cost=0)
        // and satisfies every constraint (chat+tool_use, 128k≥32k,
        // 2s≤2s, us+eu residency). So it wins the cost tiebreaker
        // over the preferred Anthropic/OpenAI entries.
        //
        // This is the correct spec behavior, even if unintuitive on
        // first read. The demo shows that local-LLM fallbacks are
        // selected when they're free — a desirable property! Users
        // who want prefer-order-dominant selection should set
        // `optimize_for = quality` (next test).
        let src = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..").join("..")
                .join("examples").join("support-agent.ais")
        ).expect("read demo");
        let (project, _) = aislang_core::validate(&src)
            .expect("demo must validate");
        let cat = aislang_catalog::bundled().expect("bundled catalog");
        let resolved = resolve(&project, &cat).expect("must resolve");
        let fast = resolved.model_bindings.get("fast")
            .expect("model `fast` binding");
        assert_eq!(fast,
            &ModelId { provider: "ollama".into(), name: "llama_3_1_8b".into() },
            "under optimize_for=cost the free Ollama fallback wins; \
             see test comment for the spec-§6 semantics");
    }

    #[test] fn optimize_for_cost_picks_cheapest() {
        // Both prefer entries satisfy; cost-optimized picks the cheaper.
        let body = r#"
            model "m" {
                requires    = [chat]
                context_min = 32k
                prefer      = [test.cheap_slow, test.fast_expensive]
            }
        "#;
        let id = resolve_model(body, &variety_catalog(), "m")
            .expect("must resolve");
        assert_eq!(id.name, "cheap_slow");
    }

    #[test] fn optimize_for_latency_picks_fastest() {
        // Override optimize_for via budget block.
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
        let resolved = resolve(&project, &variety_catalog()).expect("resolve");
        assert_eq!(resolved.model_bindings.get("m").unwrap().name, "fast_expensive");
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
        let resolved = resolve(&project, &variety_catalog()).expect("resolve");
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
        let resolved = resolve(&project, &variety_catalog()).expect("resolve");
        assert_eq!(resolved.catalog_version, "test-variety");
    }
}

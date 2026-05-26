//! aiSlang resolver — sits between `aislang-core` (parser + checker)
//! and `aislang-compose` (emitter). Bridges the two without forcing
//! either to know about the catalog directly (avoids the
//! aislang-core ↔ aislang-catalog cycle).
//!
//! Week 3 Wed (this week): catalog ID validation against a catalog —
//! ModelIds in `model.prefer` / `model.fallback` / `model.pin` must
//! exist; deprecated entries produce warnings rather than errors
//! (compliance-audit-trail decision per PLAN §Locked design decisions).
//!
//! Week 4 (next): model selection (pick one concrete catalog entry per
//! `model` block based on constraints), prompt file hashing, OCI image
//! tag-to-digest pinning, lockfile reconciliation.

use aislang_catalog::Catalog;
use aislang_core::ast::{ModelId, Project};
use aislang_core::Warning;

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
}

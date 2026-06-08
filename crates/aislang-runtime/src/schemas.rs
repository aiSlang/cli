//! v0.4 Week 21 Thu — JSON inter-agent contract.
//!
//! Pins the schemas under `examples/agents/_schemas/` into the
//! binary via `include_str!` + exposes parsed-`serde_json::Value`
//! handles for the runtime + structural-validation helpers for
//! the unit tests.
//!
//! ## Why include_str! instead of jsonschema crate
//!
//! v0.4 W21 Thu ships a minimal structural validator (required-
//! field presence + top-level type check) so the inter-agent
//! contract has a compile-time test surface without pulling in
//! the heavyweight `jsonschema` crate (~75 KLoC of build-time
//! dep weight).  Full draft-2020-12 validation lands in v0.4
//! Phase 2 if the orchestrator demo surfaces real schema
//! mismatches the structural check missed.
//!
//! ## Schemas
//!
//! - [`ARCHITECT_OUTPUT`] — Architect → Coder / DB handoff.
//! - [`CODER_OUTPUT`]     — Coder → Reviewer / Tester handoff.
//! - [`REVIEWER_VERDICT`] — Reviewer → Tester / Coder-revision.
//! - [`TESTER_OUTPUT`]    — Tester → loop-terminator / human.
//! - [`ERROR_ENVELOPE`]   — universal hard-error wire shape;
//!   any agent's reply that conforms to this triggers the
//!   runtime's pipeline abort path (mirrors W21 Mon's
//!   `is_hard_error_signal` markdown detector for the JSON wire).

use serde_json::Value;

// ============================================================================
// Schema sources — pinned at build time via include_str!.
// ============================================================================

pub const ARCHITECT_OUTPUT: &str = include_str!(
    "../../../examples/agents/_schemas/architect_output.schema.json");
pub const CODER_OUTPUT:     &str = include_str!(
    "../../../examples/agents/_schemas/coder_output.schema.json");
pub const REVIEWER_VERDICT: &str = include_str!(
    "../../../examples/agents/_schemas/reviewer_verdict.schema.json");
pub const TESTER_OUTPUT:    &str = include_str!(
    "../../../examples/agents/_schemas/tester_output.schema.json");
pub const ERROR_ENVELOPE:   &str = include_str!(
    "../../../examples/agents/_schemas/error_envelope.schema.json");

// ============================================================================
// Structural validation — required-field presence + top-level
// type check.  Enough to catch the 80% of model output drift
// the tool-call mode is supposed to prevent at the LiteLLM
// wire boundary; full draft-2020-12 validation lands in
// Phase 2 if needed.
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    /// Top-level was not a JSON object (number / array / null / …).
    NotAnObject,
    /// A field listed in the schema's `required` array is missing
    /// from the candidate payload.
    MissingRequired { field: String },
    /// Schema itself didn't parse as a JSON object — used by tests
    /// to assert that the schemas-as-checked-in are themselves
    /// well-formed.
    BadSchema { cause: String },
}

/// Pull the top-level `required: [...]` array out of a JSON Schema
/// document.  Returns an empty Vec if the schema doesn't have a
/// top-level `required` (legal under draft-2020-12 but not the
/// shape we ship today).
pub fn required_fields(schema_json: &str) -> Result<Vec<String>, ValidationError> {
    let schema: Value = serde_json::from_str(schema_json)
        .map_err(|e| ValidationError::BadSchema { cause: e.to_string() })?;
    let Some(req) = schema.get("required").and_then(|r| r.as_array()) else {
        return Ok(Vec::new());
    };
    Ok(req.iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect())
}

/// Structural-validate a candidate payload string against a
/// schema string.  Today's check: payload parses as JSON →
/// payload top-level is an object → every field in the schema's
/// `required` list is present.  Enough to catch the common
/// agent-output drift modes; full constraint validation lands
/// in v0.4 Phase 2 if needed.
pub fn structurally_validate(
    schema_json:    &str,
    candidate_json: &str,
) -> Result<(), ValidationError> {
    let required = required_fields(schema_json)?;
    let payload: Value = serde_json::from_str(candidate_json)
        .map_err(|e| ValidationError::BadSchema { cause: e.to_string() })?;
    let obj = payload.as_object().ok_or(ValidationError::NotAnObject)?;
    for field in &required {
        if !obj.contains_key(field) {
            return Err(ValidationError::MissingRequired {
                field: field.clone()
            });
        }
    }
    Ok(())
}

/// Quick-check that a payload conforms to the error envelope shape
/// (the canonical hard-error wire format).  Runtime callers use
/// this on every step's reply BEFORE passing it to the next step;
/// any payload that conforms triggers the pipeline-abort path —
/// the JSON-wire analogue of W21 Mon's `is_hard_error_signal`
/// markdown detector.
pub fn looks_like_error_envelope(candidate_json: &str) -> bool {
    structurally_validate(ERROR_ENVELOPE, candidate_json).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── canonical happy-path payloads (must validate) ─────────

    const ARCHITECT_OK: &str = r##"{
        "block_name": "dataset",
        "purpose": "Declare training data for the eval block to bind metrics against.",
        "fields": [
            { "name": "path", "type": "Path", "required": true,
              "description": "filesystem path to the dataset" }
        ],
        "grammar_example": "dataset \"x\" { path = \"./x.jsonl\" }",
        "language_spec_snippet": "§4.<N> dataset declares a training/eval data source with...",
        "decision_implications": {
            "new_refkind": "Dataset",
            "checker_invariants": [],
            "decision_conflicts": [],
            "byte_stable_v0_2_roundtrip": true
        }
    }"##;

    const CODER_OK: &str = r##"{
        "patch": {
            "ast_struct_rust": "#[derive(...)] pub struct Dataset { ... }",
            "parser_converter": {
                "tool_call": {
                    "block_name": "dataset", "type_name": "Dataset", "fields": []
                }
            },
            "checker_rust": "fn validate_datasets(...) { ... }",
            "test_fixtures_rust": "#[test] fn dataset_block_parses() { ... }"
        },
        "test_coverage": [
            { "test_name": "dataset_block_parses", "kind": "happy_path",
              "covers_field": "path" }
        ],
        "notes": {
            "new_refkind_variant": "Dataset",
            "files_edited_beyond_four": []
        }
    }"##;

    const REVIEWER_ACCEPT_OK: &str = r##"{
        "verdict": "ACCEPT",
        "rationale": "byte-stable; canonical RawBlock shape"
    }"##;

    const REVIEWER_FAIL_OK: &str = r##"{
        "verdict": "FAIL",
        "rationale": "parser-converter uses hcl::* types",
        "issues": [
            { "location": "parser-converter section",
              "category": "hcl_imports",
              "fix": "Replace `hcl::Block` with `RawBlock` per the canonical to_<block> shape." }
        ]
    }"##;

    const TESTER_OK: &str = r##"{
        "test_plan": [
            { "test_name": "dataset_block_parses",
              "covers_field": "path", "kind": "happy_path" }
        ],
        "skill_invocation": "cargo test --workspace --no-fail-fast -- dataset_block_parses",
        "verdict": "ACCEPT",
        "rationale": "coverage hits every Architect-declared check"
    }"##;

    const ERROR_ENVELOPE_OK: &str = r##"{
        "error": {
            "code": "input_malformed",
            "message": "Architect's design is missing the Block name field; cannot proceed."
        }
    }"##;

    #[test]
    fn every_shipped_schema_parses_as_valid_json() {
        // Tier-0: the schema files we check in must themselves
        // be parseable JSON.  Caught a missing comma in the
        // initial coder_output schema draft.
        for (name, src) in [
            ("architect_output", ARCHITECT_OUTPUT),
            ("coder_output",     CODER_OUTPUT),
            ("reviewer_verdict", REVIEWER_VERDICT),
            ("tester_output",    TESTER_OUTPUT),
            ("error_envelope",   ERROR_ENVELOPE),
        ] {
            let parsed: Result<Value, _> = serde_json::from_str(src);
            assert!(parsed.is_ok(),
                "schema `{name}` failed to parse: {:?}", parsed.err());
        }
    }

    #[test]
    fn happy_path_payloads_validate_against_their_schemas() {
        structurally_validate(ARCHITECT_OUTPUT, ARCHITECT_OK)
            .expect("architect happy-path must validate");
        structurally_validate(CODER_OUTPUT, CODER_OK)
            .expect("coder happy-path must validate");
        structurally_validate(REVIEWER_VERDICT, REVIEWER_ACCEPT_OK)
            .expect("reviewer ACCEPT must validate");
        structurally_validate(REVIEWER_VERDICT, REVIEWER_FAIL_OK)
            .expect("reviewer FAIL must validate");
        structurally_validate(TESTER_OUTPUT, TESTER_OK)
            .expect("tester happy-path must validate");
        structurally_validate(ERROR_ENVELOPE, ERROR_ENVELOPE_OK)
            .expect("error envelope must validate");
    }

    #[test]
    fn missing_required_field_is_rejected() {
        // Strip the `purpose` field from the Architect happy
        // payload — must surface as MissingRequired.
        let bad = r##"{
            "block_name": "dataset",
            "fields": [],
            "grammar_example": "",
            "language_spec_snippet": "",
            "decision_implications": {}
        }"##;
        match structurally_validate(ARCHITECT_OUTPUT, bad) {
            Err(ValidationError::MissingRequired { field }) => {
                assert_eq!(field, "purpose");
            }
            other => panic!("expected MissingRequired purpose, got {other:?}"),
        }
    }

    #[test]
    fn non_object_top_level_is_rejected() {
        match structurally_validate(ARCHITECT_OUTPUT, "[1, 2, 3]") {
            Err(ValidationError::NotAnObject) => { /* expected */ }
            other => panic!("expected NotAnObject, got {other:?}"),
        }
    }

    #[test]
    fn looks_like_error_envelope_distinguishes_real_errors_from_agent_outputs() {
        // Real error envelope → true.
        assert!(looks_like_error_envelope(ERROR_ENVELOPE_OK));
        // Architect's normal output (no top-level `error` key) → false.
        assert!(!looks_like_error_envelope(ARCHITECT_OK));
        // Coder's normal output → false.
        assert!(!looks_like_error_envelope(CODER_OK));
        // Reviewer ACCEPT verdict → false.
        assert!(!looks_like_error_envelope(REVIEWER_ACCEPT_OK));
        // Malformed JSON → false (closed against ambiguity).
        assert!(!looks_like_error_envelope("not json at all"));
        // Bare empty object → false (lacks `error` key).
        assert!(!looks_like_error_envelope("{}"));
    }

    #[test]
    fn required_fields_extracts_expected_arrays() {
        // Pin the schemas' top-level required arrays so a
        // future schema rewrite that drops a required field
        // trips this test loudly.
        let arch_req = required_fields(ARCHITECT_OUTPUT).expect("ok");
        assert!(arch_req.contains(&"block_name".into()));
        assert!(arch_req.contains(&"fields".into()));
        assert!(arch_req.contains(&"decision_implications".into()));

        let coder_req = required_fields(CODER_OUTPUT).expect("ok");
        assert!(coder_req.contains(&"patch".into()));
        assert!(coder_req.contains(&"test_coverage".into()));

        let rev_req = required_fields(REVIEWER_VERDICT).expect("ok");
        assert!(rev_req.contains(&"verdict".into()));

        let test_req = required_fields(TESTER_OUTPUT).expect("ok");
        assert!(test_req.contains(&"verdict".into()));
        assert!(test_req.contains(&"skill_invocation".into()));

        let err_req = required_fields(ERROR_ENVELOPE).expect("ok");
        assert_eq!(err_req, vec!["error"]);
    }
}

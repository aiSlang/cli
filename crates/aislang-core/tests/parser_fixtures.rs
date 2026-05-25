//! Parser fixture tests — the harness for the week-2 parser implementation.
//!
//! For each `.ais` example file in `examples/`, this test crate maintains
//! a paired `.expected-ast.json` file: the canonical AST the parser must
//! produce. The expected AST is built in Rust here (single source of truth),
//! serialized to JSON pretty-printed, and compared to the on-disk fixture.
//!
//! Workflow when an example changes:
//!   1. Edit `examples/<name>.ais` and update the matching `expected_X()`
//!      function below.
//!   2. Regenerate fixtures:
//!        $ cargo test --test parser_fixtures -- --ignored update_fixtures
//!   3. Inspect the diff in `examples/<name>.expected-ast.json` and commit
//!      both files together.
//!
//! The week-2 parser will plug into [`assert_parser_matches_expected`] —
//! marked `#[ignore]` until then.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use aislang_core::ast::*;
use aislang_core::types::*;

// ============================================================================
// Path helpers
// ============================================================================

fn examples_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is `crates/aislang-core`; examples live at the
    // workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("examples")
}

fn fixture_path(name: &str) -> PathBuf {
    examples_dir().join(format!("{name}.expected-ast.json"))
}

fn source_path(name: &str) -> PathBuf {
    examples_dir().join(format!("{name}.ais"))
}

// ============================================================================
// Expected ASTs — Rust is the source of truth; the JSON files are generated
// ============================================================================

fn expected_minimal() -> Project {
    Project {
        name:   "minimal".into(),
        budget: Some(Budget {
            hard_cap_per_day: Money { currency: Currency::Usd, micro_units: 10_000_000 },
            hard_cap_per_run: Money { currency: Currency::Usd, micro_units:    100_000 },
            optimize_for:     OptimizeFor::Cost,
        }),
        ..Project::default()
    }
}

fn expected_support_agent() -> Project {
    let model_fast = Model {
        label:       "fast".into(),
        requires:    vec![Capability::Chat, Capability::ToolUse],
        context_min: Size { units: 32 * 1024 },                  // 32k
        cost_max:    Some(Rate {
            numerator:   Money { currency: Currency::Usd, micro_units: 1_000 }, // $0.001
            denominator: Size  { units: 1024 },                                  // 1k tokens
        }),
        latency_p95: Some(Duration { nanos: 2_000_000_000 }),    // 2s
        residency:   Some(vec![Residency::Eu, Residency::Us]),
        prefer:      vec![
            ModelId { provider: "anthropic".into(), name: "claude_haiku_4_5".into() },
            ModelId { provider: "openai".into(),    name: "gpt_4o_mini".into() },
        ],
        fallback:    vec![
            ModelId { provider: "ollama".into(), name: "llama_3_1_8b".into() },
        ],
        pin: None,
    };

    let prompt_system = Prompt {
        label:   "system".into(),
        file:    PathBuf::from("./prompts/system.md"),
        version: SemVer::from_literal("1.0.0").expect("valid SemVer"),
    };

    let mcp_docs = McpServer {
        label: "docs".into(),
        image: OciImage {
            registry:   "ghcr.io".into(),
            repository: "example/mcp_filesystem".into(),
            tag:        Some("0.3.1".into()),
            digest:     None,
        },
        mount:   Some(PathBuf::from("./knowledge/")),
        env:     None,
        secrets: None,
    };

    let agent_support = Agent {
        label:         "support".into(),
        model:         Ref { kind: RefKind::Model,     label: "fast".into()   },
        system_prompt: Ref { kind: RefKind::Prompt,    label: "system".into() },
        tools:         vec![Ref { kind: RefKind::McpServer, label: "docs".into() }],
    };

    let eval_accuracy = Eval {
        label:      "factual_accuracy".into(),
        dataset:    PathBuf::from("./evals/qa.jsonl"),
        metric:     EvalMetric::ExactMatch,
        threshold:  Percentage { ratio: 0.85 },
        aggregator: EvalAggregator::Mean,
        agent:      None,
    };

    let deploy_local = Deploy {
        label:  "local".into(),
        target: DeployTarget::Compose,
        expose: Some(BTreeMap::from([("port".to_string(), 8080u16)])),
    };

    Project {
        name:        "support-agent".into(),
        budget:      Some(Budget {
            hard_cap_per_day: Money { currency: Currency::Usd, micro_units: 50_000_000 },
            hard_cap_per_run: Money { currency: Currency::Usd, micro_units:    500_000 },
            optimize_for:     OptimizeFor::Cost,
        }),
        models:      vec![model_fast],
        prompts:     vec![prompt_system],
        mcp_servers: vec![mcp_docs],
        agents:      vec![agent_support],
        evals:       vec![eval_accuracy],
        secrets:     vec![],
        deploys:     vec![deploy_local],
    }
}

// ============================================================================
// Common assertions
// ============================================================================

/// Serialize `expected` to pretty JSON in the canonical form. All fixtures
/// must round-trip through this; this is the *only* place that defines what
/// "pretty" means.
fn render(expected: &Project) -> String {
    let mut s = serde_json::to_string_pretty(expected).expect("serializable AST");
    s.push('\n');                     // POSIX-style trailing newline
    s
}

/// Assert that the on-disk fixture matches the expected AST, both
/// structurally (via deserialize) and textually (via pretty serialize). If
/// the textual comparison fails, the diff is the problem the developer must
/// resolve — usually by regenerating fixtures.
fn assert_fixture_matches(name: &str, expected: &Project) {
    let path = fixture_path(name);
    let on_disk = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!(
            "fixture file {} missing or unreadable: {e}\n\
             Run: cargo test --test parser_fixtures -- --ignored update_fixtures",
            path.display()
        ));

    // Structural: on-disk JSON deserializes back to the same Project
    let parsed: Project = serde_json::from_str(&on_disk)
        .unwrap_or_else(|e| panic!("fixture {} is invalid JSON or schema-incompatible: {e}", path.display()));
    assert_eq!(
        &parsed, expected,
        "deserialized fixture does not equal expected AST for `{name}` \
         — fixture is stale relative to ast.rs/types.rs"
    );

    // Textual: the rendered expected matches the on-disk bytes verbatim.
    // This catches drift in serialization format (field order, indentation).
    let rendered = render(expected);
    assert_eq!(
        rendered, on_disk,
        "fixture `{name}` is out of date.\n\
         Run: cargo test --test parser_fixtures -- --ignored update_fixtures\n\
         Then review the diff and commit alongside the .ais change.",
    );
}

/// Write the rendered expected AST to the fixture file. Used only by the
/// `#[ignored] update_fixtures` test, intentionally not invoked by default.
fn write_fixture(name: &str, expected: &Project) {
    let path = fixture_path(name);
    fs::write(&path, render(expected))
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
    eprintln!("wrote {}", path.display());
}

// ============================================================================
// Tests — run by default
// ============================================================================

#[test]
fn source_files_exist() {
    for name in ["minimal", "support-agent"] {
        let p = source_path(name);
        assert!(p.exists(), "missing source fixture: {}", p.display());
    }
}

#[test]
fn minimal_fixture_matches() {
    assert_fixture_matches("minimal", &expected_minimal());
}

#[test]
fn support_agent_fixture_matches() {
    assert_fixture_matches("support-agent", &expected_support_agent());
}

// ============================================================================
// Test — opt-in: regenerate fixture files on disk
// ============================================================================

#[test]
#[ignore = "writes to examples/; opt-in via `--ignored update_fixtures`"]
fn update_fixtures() {
    write_fixture("minimal",       &expected_minimal());
    write_fixture("support-agent", &expected_support_agent());
}

// ============================================================================
// Tests — gated on parser implementation (week 2)
// ============================================================================

#[test]
#[ignore = "parser stub — enable when chumsky grammar lands in week 2"]
fn parser_produces_expected_minimal() {
    assert_parser_matches_expected("minimal", &expected_minimal());
}

#[test]
#[ignore = "parser stub — enable when chumsky grammar lands in week 2"]
fn parser_produces_expected_support_agent() {
    assert_parser_matches_expected("support-agent", &expected_support_agent());
}

fn assert_parser_matches_expected(name: &str, expected: &Project) {
    let src = fs::read_to_string(source_path(name)).expect("source file");
    let parsed = aislang_core::validate(&src)
        .unwrap_or_else(|e| panic!("validate({name}) failed: {e}"));
    assert_eq!(&parsed, expected,
        "parser output for `{name}` does not match expected AST");
}

#[allow(dead_code)]
fn _ensure_path_helper_compiles(_: &Path) {} // silences unused-import warning
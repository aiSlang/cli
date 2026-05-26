//! Error-fixture integration tests — pin source-aware diagnostic
//! rendering against on-disk malformed `.ais` files.
//!
//! For each fixture in `examples/errors/`, load the source, run it
//! through the full `aislang_core::validate()` pipeline, and assert
//! the resulting `ValidateError::Parse(...)` renders with the expected
//! line:col + caret + context-line shape via
//! `aislang_core::diagnostic::render_with_source`.
//!
//! The assertions deliberately compute expected line numbers from the
//! source itself rather than hard-coding them — so editing a fixture's
//! header comment doesn't break the test.

use std::fs;
use std::path::PathBuf;

use aislang_core::diagnostic::{render_with_source, Severity};
use aislang_core::ValidateError;

fn errors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("..")
        .join("examples").join("errors")
}

fn load(filename: &str) -> String {
    let path = errors_dir().join(filename);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn missing_list_comma_renders_source_snippet() {
    let source = load("missing-list-comma.ais");

    // The offending line is the one that contains `chat tool_use` (no
    // comma). Compute its 1-based line number from the source so the
    // assertion survives header-comment edits to the fixture.
    let offending_line_zero_idx = source
        .lines()
        .position(|l| l.contains("chat tool_use"))
        .expect("fixture must contain the malformed `chat tool_use` token");
    let offending_line = offending_line_zero_idx + 1;
    let prev_line     = offending_line - 1;
    let next_line     = offending_line + 1;

    // Validate must fail with Parse(..) carrying at least one span.
    let err = aislang_core::validate(&source)
        .err()
        .expect("missing-list-comma fixture must produce a parse error");
    let errors = match err {
        ValidateError::Parse(es) => es,
        other => panic!("expected ValidateError::Parse, got: {other:?}"),
    };
    assert!(!errors.is_empty(), "Parse(..) must contain at least one error");
    let first = &errors[0];
    let span = first.span.clone()
        .expect("grammar errors must carry chumsky span info");

    // Render the diagnostic and assert structural invariants.
    let rendered = render_with_source(
        &source, span.clone(), Severity::Error, &first.message,
    );

    // Header line: `error at line N, column M: <message>`
    let header_expected = format!("error at line {offending_line},");
    assert!(
        rendered.contains(&header_expected),
        "rendered diagnostic missing expected header `{header_expected}`; \
         full rendering:\n{rendered}"
    );

    // The offending source line itself must appear, with line number
    // in the gutter (`<n> |`).
    let gutter_offending = format!("{offending_line} |");
    assert!(
        rendered.contains(&gutter_offending),
        "missing offending-line gutter `{gutter_offending}`; got:\n{rendered}"
    );
    assert!(
        rendered.contains("chat tool_use"),
        "rendered diagnostic should echo the offending source text; got:\n{rendered}"
    );

    // Context lines: one before + one after the offending line.
    assert!(
        rendered.contains(&format!("{prev_line} |")),
        "missing pre-context line `{prev_line} |`; got:\n{rendered}"
    );
    assert!(
        rendered.contains(&format!("{next_line} |")),
        "missing post-context line `{next_line} |`; got:\n{rendered}"
    );

    // Caret pointer present.
    assert!(
        rendered.contains('^'),
        "rendered diagnostic must include a caret pointer; got:\n{rendered}"
    );

    // The chumsky error message should mention what was expected. Don't
    // hard-code the full set (chumsky may legitimately reorder them) —
    // just ensure the comma is listed as one of the expected tokens.
    assert!(
        first.message.contains(',') || first.message.contains("','"),
        "parser message should expect `,` as a valid next token; got: {}",
        first.message
    );
}

#[test]
fn fixture_directory_layout_is_complete() {
    // Sanity guard: every `.ais` file in examples/errors/ must have a
    // matching test function above. When a new fixture is added, this
    // test trips until the developer adds the test pair.
    let dir = errors_dir();
    let mut on_disk: Vec<String> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.ends_with(".ais").then_some(name)
        })
        .collect();
    on_disk.sort();

    // The expected set — keep this in lockstep with the per-fixture
    // test functions in this file.
    let expected = vec![
        "missing-list-comma.ais".to_string(),
    ];

    assert_eq!(
        on_disk, expected,
        "examples/errors/ contents differ from the test inventory in \
         error_fixtures.rs — add a new test for the new fixture (or \
         remove from disk if no longer relevant), then update the \
         `expected` vec in this test"
    );
}

//! Eval-dataset loader + result types for `aislang eval` (week 8).
//!
//! Datasets are JSONL — one JSON object per line — with two extras
//! over strict JSONL:
//!   - **Empty lines** are skipped (any line that's all whitespace
//!     after trimming).
//!   - **Comment lines** are skipped (after trimming, line starts
//!     with `#`). Lets users annotate test cases inline, matching
//!     the `.ais` line-comment convention.
//!
//! Required fields per case:
//!   - `input`     — user message sent to the agent.
//!   - `expected`  — gold-standard response for metric scoring.
//!
//! Optional fields:
//!   - `id` — user-chosen identifier for traceability in reports.
//!     When absent, defaults to `"line_<N>"` (1-indexed source-file
//!     line number AFTER comment/empty filtering).
//!
//! Other fields are silently ignored — forward-compat for v0.2
//! additions like `tags`, `weight`, `metadata`. Strict-mode rejection
//! would block legitimate v0.2 datasets from being inspectable in
//! v0.1.
//!
//! ## Error reporting precision
//!
//! Errors include the **1-indexed source-file line number**
//! (counting empty + comment lines too, so the user can `sed -n
//! '<N>p' dataset.jsonl` to see exactly the offending line).

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

// ============================================================================
// Types
// ============================================================================

/// One eval case — input fed to the agent, expected gold response.
/// `id` is traceability-only; the runner doesn't use it for scoring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalCase {
    pub id:       String,
    pub input:    String,
    pub expected: String,
}

#[derive(Debug, Error)]
pub enum DatasetError {
    #[error("dataset file `{path}` not readable: {cause}")]
    Io { path: String, cause: String },

    #[error("dataset `{path}` line {line}: not valid JSON: {cause}")]
    BadJson { path: String, line: usize, cause: String },

    #[error("dataset `{path}` line {line}: top-level value must be a JSON object")]
    NotObject { path: String, line: usize },

    #[error("dataset `{path}` line {line}: missing required field `{field}`")]
    MissingField { path: String, line: usize, field: String },

    #[error("dataset `{path}` line {line}: field `{field}` must be a string")]
    BadFieldType { path: String, line: usize, field: String },
}

// ============================================================================
// Public API
// ============================================================================

/// Load a JSONL eval dataset from disk into a Vec of typed cases.
///
/// Behavior:
///   - Empty lines and comment lines (starting with `#` after trim)
///     are silently skipped.
///   - All other lines must parse as a JSON object with `input` and
///     `expected` string fields.
///   - `id` field is optional; defaults to `"line_<N>"` where N is
///     the 1-indexed source-file line number for that case.
///   - Errors carry the precise line number for `sed -n '<N>p'`-able
///     diagnostics.
pub fn load_dataset(path: &Path) -> Result<Vec<EvalCase>, DatasetError> {
    let bytes = std::fs::read(path).map_err(|e| DatasetError::Io {
        path:  path.display().to_string(),
        cause: e.to_string(),
    })?;
    let text = std::str::from_utf8(&bytes).map_err(|e| DatasetError::Io {
        path:  path.display().to_string(),
        cause: format!("not valid UTF-8: {e}"),
    })?;
    parse_dataset(path, text)
}

/// Same logic as `load_dataset` but takes pre-loaded text. Exposed
/// for tests + any caller that already has the bytes in memory.
pub fn parse_dataset(path: &Path, text: &str) -> Result<Vec<EvalCase>, DatasetError> {
    let path_str = path.display().to_string();
    let mut cases = Vec::new();
    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1; // 1-indexed for user-facing errors
        let trimmed = raw_line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            // Skip empty + comment lines silently.
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(raw_line)
            .map_err(|e| DatasetError::BadJson {
                path:  path_str.clone(),
                line:  line_no,
                cause: e.to_string(),
            })?;
        let obj = match value {
            serde_json::Value::Object(o) => o,
            _ => return Err(DatasetError::NotObject {
                path: path_str.clone(),
                line: line_no,
            }),
        };
        let input    = require_string(&obj, "input",    &path_str, line_no)?;
        let expected = require_string(&obj, "expected", &path_str, line_no)?;
        let id       = optional_string(&obj, "id", &path_str, line_no)?
            .unwrap_or_else(|| format!("line_{line_no}"));
        cases.push(EvalCase { id, input, expected });
    }
    Ok(cases)
}

// ============================================================================
// Helpers
// ============================================================================

fn require_string(
    obj:    &serde_json::Map<String, serde_json::Value>,
    field:  &str,
    path:   &str,
    line:   usize,
) -> Result<String, DatasetError> {
    let v = obj.get(field).ok_or_else(|| DatasetError::MissingField {
        path:  path.to_string(),
        line,
        field: field.to_string(),
    })?;
    v.as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| DatasetError::BadFieldType {
            path:  path.to_string(),
            line,
            field: field.to_string(),
        })
}

fn optional_string(
    obj:    &serde_json::Map<String, serde_json::Value>,
    field:  &str,
    path:   &str,
    line:   usize,
) -> Result<Option<String>, DatasetError> {
    match obj.get(field) {
        None => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(DatasetError::BadFieldType {
            path:  path.to_string(),
            line,
            field: field.to_string(),
        }),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> &'static Path { Path::new("test.jsonl") }

    // ---- happy path ----

    #[test] fn empty_file_is_valid_zero_case_dataset() {
        let cases = parse_dataset(p(), "").unwrap();
        assert!(cases.is_empty());
    }

    #[test] fn single_well_formed_line() {
        let text = r#"{"input": "hi", "expected": "hello"}"#;
        let cases = parse_dataset(p(), text).unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].input, "hi");
        assert_eq!(cases[0].expected, "hello");
        // No `id` field → default to "line_1".
        assert_eq!(cases[0].id, "line_1");
    }

    #[test] fn id_field_overrides_default() {
        let text = r#"{"id": "q42", "input": "x", "expected": "y"}"#;
        let cases = parse_dataset(p(), text).unwrap();
        assert_eq!(cases[0].id, "q42");
    }

    #[test] fn multiple_lines_preserve_order() {
        let text = "{\"input\":\"a\",\"expected\":\"1\"}\n{\"input\":\"b\",\"expected\":\"2\"}\n{\"input\":\"c\",\"expected\":\"3\"}";
        let cases = parse_dataset(p(), text).unwrap();
        assert_eq!(cases.iter().map(|c| c.input.clone()).collect::<Vec<_>>(),
                   vec!["a", "b", "c"]);
    }

    #[test] fn extra_fields_silently_ignored() {
        // Forward-compat for v0.2 schema additions (tags, weight, etc.).
        let text = r#"{"input":"x","expected":"y","tags":["math"],"weight":2}"#;
        let cases = parse_dataset(p(), text).unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].input, "x");
    }

    // ---- comment + empty-line skipping (Decision 14 + user follow-up) ----

    #[test] fn empty_lines_skipped() {
        // Blank lines between cases shouldn't trip parsing or
        // perturb line numbering inside reports.
        let text = "\n\n{\"input\":\"a\",\"expected\":\"1\"}\n\n{\"input\":\"b\",\"expected\":\"2\"}\n\n";
        let cases = parse_dataset(p(), text).unwrap();
        assert_eq!(cases.len(), 2);
        // The first case is on line 3 (after two empty lines).
        assert_eq!(cases[0].id, "line_3");
        assert_eq!(cases[1].id, "line_5");
    }

    #[test] fn comment_lines_skipped() {
        // Lines beginning with `#` (after leading whitespace) are
        // skipped — same convention as `.ais` line comments.
        let text = "\
# Cases derived from the support-agent's knowledge base.
# Update with `aislang eval --regenerate-baseline`.

{\"input\":\"who are you?\",\"expected\":\"the support agent\"}
   # indented comment — still skipped
{\"input\":\"list files\",\"expected\":\"README.md\"}";
        let cases = parse_dataset(p(), text).unwrap();
        assert_eq!(cases.len(), 2);
        // Cases land at the right source-file line numbers despite
        // the surrounding comments.
        assert_eq!(cases[0].id, "line_4");
        assert_eq!(cases[1].id, "line_6");
    }

    #[test] fn lines_with_only_whitespace_skipped() {
        // Spaces / tabs alone count as empty.
        let text = "  \t  \n{\"input\":\"x\",\"expected\":\"y\"}\n   ";
        let cases = parse_dataset(p(), text).unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].id, "line_2");
    }

    // ---- error paths with precise line numbers ----

    #[test] fn malformed_json_reports_line_number() {
        let text = "{\"input\":\"a\",\"expected\":\"1\"}\n{not json\n{\"input\":\"b\",\"expected\":\"2\"}";
        let err = parse_dataset(p(), text).err().unwrap();
        match err {
            DatasetError::BadJson { line, .. } => assert_eq!(line, 2),
            other => panic!("expected BadJson with line=2; got {other:?}"),
        }
    }

    #[test] fn missing_input_reports_field_and_line() {
        // Comment on line 1, missing-input on line 2.
        let text = "# preamble\n{\"expected\":\"hello\"}";
        let err = parse_dataset(p(), text).err().unwrap();
        match err {
            DatasetError::MissingField { line, field, .. } => {
                assert_eq!(line, 2);
                assert_eq!(field, "input");
            }
            other => panic!("expected MissingField input/line=2; got {other:?}"),
        }
    }

    #[test] fn missing_expected_reports_field_and_line() {
        let text = "{\"input\":\"hi\"}";
        let err = parse_dataset(p(), text).err().unwrap();
        match err {
            DatasetError::MissingField { field, .. } => assert_eq!(field, "expected"),
            other => panic!("expected MissingField expected; got {other:?}"),
        }
    }

    #[test] fn non_string_field_reports_bad_type() {
        // `input` is an integer instead of a string.
        let text = r#"{"input": 42, "expected": "y"}"#;
        let err = parse_dataset(p(), text).err().unwrap();
        match err {
            DatasetError::BadFieldType { field, .. } => assert_eq!(field, "input"),
            other => panic!("expected BadFieldType; got {other:?}"),
        }
    }

    #[test] fn top_level_array_rejected() {
        // JSONL convention: each line is a JSON OBJECT, not a list.
        // A line that's a top-level array is a likely user mistake.
        let text = r#"["input", "expected"]"#;
        let err = parse_dataset(p(), text).err().unwrap();
        match err {
            DatasetError::NotObject { line, .. } => assert_eq!(line, 1),
            other => panic!("expected NotObject; got {other:?}"),
        }
    }

    #[test] fn missing_file_io_error() {
        let err = load_dataset(Path::new("/tmp/does-not-exist-eval-9b2f.jsonl"))
            .err().unwrap();
        match err {
            DatasetError::Io { path, .. } => {
                assert!(path.contains("does-not-exist-eval-9b2f.jsonl"));
            }
            other => panic!("expected Io error; got {other:?}"),
        }
    }

    // ---- EvalCase JSON round-trip (for the eventual report serialization) ----

    #[test] fn eval_case_json_round_trips() {
        let c = EvalCase {
            id:       "q1".into(),
            input:    "what is 2+2?".into(),
            expected: "4".into(),
        };
        let s = serde_json::to_string(&c).unwrap();
        let back: EvalCase = serde_json::from_str(&s).unwrap();
        assert_eq!(c, back);
    }
}

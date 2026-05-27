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

pub mod metric;

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

// ============================================================================
// Result types (the JSON wire format `aislang eval` writes to stdout)
// ============================================================================

/// One case's outcome after running the agent against `case.input`.
/// Persists every field needed for governance / audit-trail review:
/// the model's actual output, the score against the expected gold,
/// per-case latency. v0.2 may add `model_binding_used` (when
/// fallbacks fire mid-case) and `tokens_in / tokens_out`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    pub id:         String,
    pub input:      String,
    pub expected:   String,
    pub actual:     String,
    pub score:      f64,
    pub latency_ms: u64,
}

/// Top-level eval report. Stdout-emitted JSON shape for one eval
/// run. Both per-case + aggregate evidence in one JSON object.
///
/// v0.1 fields are the audit-trail floor — see PLAN week-8 §"Governance
/// / certification angle" for the v0.2 additions (catalog version,
/// lockfile source-hash, per-case model binding, UTC timestamps).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    pub eval_label:      String,
    pub dataset_path:    String,
    pub metric:          String,         // EvalMetric serialized form
    pub aggregator:      String,         // EvalAggregator serialized form
    pub threshold:       f64,
    pub aggregate_score: f64,
    pub pass:            bool,
    pub cases:           Vec<CaseResult>,
}

// ============================================================================
// Aggregation
// ============================================================================
//
// v0.1 ships `mean` only (the default per the parser). Min / max /
// p95 land Wed. Future-proofed: the public `aggregate()` function
// already dispatches on `EvalAggregator`, so adding variants
// changes the dispatcher only, not callers.

/// Combine per-case scores into a single aggregate. Empty input
/// returns NaN — caller surfaces "dataset had zero cases" rather
/// than a misleading 0.0. (NaN propagates through pass/fail
/// comparisons sensibly: any `NaN >= threshold` is `false`.)
pub fn aggregate(agg: aislang_core::types::EvalAggregator, scores: &[f64]) -> f64 {
    use aislang_core::types::EvalAggregator::*;
    match agg {
        Mean          => mean(scores),
        Min           => min(scores),
        Max           => max(scores),
        PercentileP95 => p95(scores),
    }
}

fn mean(scores: &[f64]) -> f64 {
    if scores.is_empty() { return f64::NAN; }
    scores.iter().sum::<f64>() / scores.len() as f64
}

fn min(scores: &[f64]) -> f64 {
    scores.iter().copied().fold(f64::NAN, |acc, v| {
        if acc.is_nan() { v } else { acc.min(v) }
    })
}

fn max(scores: &[f64]) -> f64 {
    scores.iter().copied().fold(f64::NAN, |acc, v| {
        if acc.is_nan() { v } else { acc.max(v) }
    })
}

/// 95th percentile via `sorted[ceil(0.95 * n) - 1]`. The "look up
/// the 95th-percentile index" convention matches what most BI tools
/// use; for n=1 returns the single score; for n=2 returns the max.
fn p95(scores: &[f64]) -> f64 {
    if scores.is_empty() { return f64::NAN; }
    let mut sorted: Vec<f64> = scores.iter().copied().collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len() as f64;
    let idx = (0.95 * n).ceil() as usize;
    sorted[idx.saturating_sub(1).min(sorted.len() - 1)]
}

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

    // ---- aggregators (Wed work, brought forward for Thu's runner) ----

    use aislang_core::types::EvalAggregator;

    #[test] fn mean_of_typical_scores() {
        let s = aggregate(EvalAggregator::Mean, &[0.0, 0.5, 1.0]);
        assert!((s - 0.5).abs() < 1e-9);
    }

    #[test] fn mean_empty_is_nan() {
        assert!(aggregate(EvalAggregator::Mean, &[]).is_nan());
    }

    #[test] fn min_max_typical() {
        let xs = [0.4, 0.9, 0.6];
        assert!((aggregate(EvalAggregator::Min, &xs) - 0.4).abs() < 1e-9);
        assert!((aggregate(EvalAggregator::Max, &xs) - 0.9).abs() < 1e-9);
    }

    #[test] fn p95_of_20_returns_19th_sorted() {
        // 20 scores from 0.0 to 0.95 in 0.05 steps. ceil(0.95 * 20) =
        // 19; index 18 → 0.90. Pin so the percentile-index
        // convention doesn't drift.
        let xs: Vec<f64> = (0..20).map(|i| i as f64 * 0.05).collect();
        let p = aggregate(EvalAggregator::PercentileP95, &xs);
        assert!((p - 0.90).abs() < 1e-9, "expected 0.90, got {p}");
    }

    #[test] fn p95_single_score() {
        let p = aggregate(EvalAggregator::PercentileP95, &[0.7]);
        assert!((p - 0.7).abs() < 1e-9);
    }

    #[test] fn eval_report_json_round_trips() {
        let r = EvalReport {
            eval_label:      "factual_accuracy".into(),
            dataset_path:    "examples/evals/qa.jsonl".into(),
            metric:          "exact_match".into(),
            aggregator:      "mean".into(),
            threshold:       0.85,
            aggregate_score: 0.92,
            pass:            true,
            cases: vec![CaseResult {
                id:         "q1".into(),
                input:      "hi".into(),
                expected:   "hello".into(),
                actual:     "hello".into(),
                score:      1.0,
                latency_ms: 312,
            }],
        };
        let s    = serde_json::to_string(&r).unwrap();
        let back: EvalReport = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }
}

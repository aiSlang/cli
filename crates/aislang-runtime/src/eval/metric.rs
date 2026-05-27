//! Per-case scoring: turn `(expected, actual)` into a `f64` in
//! `[0.0, 1.0]` according to the declared `EvalMetric`.
//!
//! v0.1 implements only `exact_match` and `f1` (Decision 15). The
//! other AST variants — `bleu`, `rouge_l`, `llm_judge` — return a
//! clean `Unimplemented` error with a v0.2 pointer instead of
//! panicking. Grammar accepts them today; runner refuses to
//! execute. Forward-compat: when v0.2 lands, source files using
//! these metrics start working without grammar changes.
//!
//! ## Algorithms
//!
//! - **exact_match.** Normalize both strings (trim + lowercase),
//!   then `==`. Returns 1.0 or 0.0. Lexically strict — `"4"` vs
//!   `"four"` is 0.0.
//!
//! - **f1.** Tokenize both via whitespace split (also lowercased
//!   + trimmed), treat as multisets (counts matter), compute
//!   common-token-count using the per-token `min(expected_count,
//!   actual_count)` rule, derive precision + recall, return the
//!   harmonic mean. Matches the SQuAD-style F1 used in QA evals.
//!   Empty-both edge case: 1.0 (vacuously matched). Empty-one
//!   edge case: 0.0.

use aislang_core::types::EvalMetric;
use thiserror::Error;

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Error)]
pub enum MetricError {
    /// The declared metric is in the AST grammar but not yet
    /// runner-implemented. v0.1 (Decision 15) reserves
    /// `bleu`, `rouge_l`, `llm_judge` for v0.2 — the parser accepts
    /// them, the runner refuses to execute. Source files using
    /// these metrics will start working without grammar changes
    /// once v0.2 lands.
    #[error("metric `{metric:?}` is not implemented in v0.1 — reserved for {v02_target}. \
             See PLAN-90-days.md week 8 Decision 15 + the eval explainer section.")]
    Unimplemented {
        metric:     EvalMetric,
        v02_target: &'static str,
    },
}

// ============================================================================
// Public API
// ============================================================================

/// Score one `(expected, actual)` pair according to the declared
/// metric. Returns `Ok(score)` in `[0.0, 1.0]` for implemented
/// metrics, `Err(Unimplemented)` otherwise.
pub fn score(
    metric:   EvalMetric,
    expected: &str,
    actual:   &str,
) -> Result<f64, MetricError> {
    match metric {
        EvalMetric::ExactMatch => Ok(exact_match(expected, actual)),
        EvalMetric::F1         => Ok(f1(expected, actual)),
        EvalMetric::Bleu       => Err(MetricError::Unimplemented {
            metric, v02_target: "v0.2 (needs locale-aware n-gram tokenization)",
        }),
        EvalMetric::RougeL     => Err(MetricError::Unimplemented {
            metric, v02_target: "v0.2 (needs LCS over n-grams)",
        }),
        EvalMetric::LlmJudge   => Err(MetricError::Unimplemented {
            metric, v02_target: "v0.2 (needs catalog-pinned judge model + judge-prompt + judge-cost integration)",
        }),
    }
}

// ============================================================================
// exact_match
// ============================================================================

/// Trim + lowercase both sides, compare. Returns 1.0 or 0.0.
fn exact_match(expected: &str, actual: &str) -> f64 {
    let e = normalize(expected);
    let a = normalize(actual);
    if e == a { 1.0 } else { 0.0 }
}

/// Shared normalization: trim outer whitespace, lowercase.
/// Matches how QA evals typically pre-process strings before
/// comparison. Locale-aware lowercasing is `str::to_lowercase`
/// (handles Unicode correctly).
fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

// ============================================================================
// f1 — bag-of-tokens (multiset) precision/recall harmonic mean
// ============================================================================

/// SQuAD-style token-bag F1.
///
/// Steps:
///   1. Normalize both strings (trim + lowercase).
///   2. Whitespace-split into token vectors.
///   3. Count common tokens via min-frequency rule (multiset
///      intersection — handles `"the the" / "the"` correctly).
///   4. precision = common / |actual_tokens|
///   5. recall    = common / |expected_tokens|
///   6. f1        = 2 * P * R / (P + R)
///
/// Edge cases:
///   - Both empty after tokenization → 1.0 (vacuously matches).
///   - One empty, one non-empty → 0.0 (no overlap possible).
///   - Both non-empty but disjoint → 0.0.
fn f1(expected: &str, actual: &str) -> f64 {
    // Own the strings (split_whitespace borrows from the normalized
    // form, which is a temporary — owning lets us build the count
    // maps without lifetime gymnastics).
    let exp: Vec<String> = normalize(expected).split_whitespace()
        .map(str::to_owned).collect();
    let act: Vec<String> = normalize(actual).split_whitespace()
        .map(str::to_owned).collect();

    if exp.is_empty() && act.is_empty() {
        return 1.0;
    }
    if exp.is_empty() || act.is_empty() {
        return 0.0;
    }

    // Multiset intersection: for each distinct token, count
    // min(expected_count, actual_count).
    use std::collections::HashMap;
    let mut exp_counts: HashMap<&str, usize> = HashMap::new();
    for t in &exp { *exp_counts.entry(t).or_insert(0) += 1; }
    let mut act_counts: HashMap<&str, usize> = HashMap::new();
    for t in &act { *act_counts.entry(t).or_insert(0) += 1; }

    let common: usize = exp_counts.iter()
        .map(|(token, exp_n)| {
            let act_n = act_counts.get(token).copied().unwrap_or(0);
            (*exp_n).min(act_n)
        })
        .sum();
    if common == 0 {
        return 0.0;
    }

    let precision = common as f64 / act.len() as f64;
    let recall    = common as f64 / exp.len() as f64;
    2.0 * precision * recall / (precision + recall)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Treat near-equality with a small epsilon so float arithmetic
    // doesn't fail tests on insignificant precision differences.
    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // ---- exact_match ----

    #[test] fn exact_match_identical_strings() {
        assert_eq!(score(EvalMetric::ExactMatch, "hello", "hello").unwrap(), 1.0);
    }

    #[test] fn exact_match_different_strings() {
        assert_eq!(score(EvalMetric::ExactMatch, "hello", "world").unwrap(), 0.0);
    }

    #[test] fn exact_match_case_insensitive() {
        // "Hello" vs "hello" normalizes to identical → 1.0.
        assert_eq!(score(EvalMetric::ExactMatch, "Hello", "HELLO").unwrap(), 1.0);
    }

    #[test] fn exact_match_trims_outer_whitespace() {
        // "  hello  " vs "hello" → 1.0 after trim.
        assert_eq!(score(EvalMetric::ExactMatch, "  hello  ", "hello").unwrap(), 1.0);
    }

    #[test] fn exact_match_preserves_inner_whitespace() {
        // "a b" vs "a  b" (two spaces) is NOT identical — outer
        // trim only; inner whitespace is part of the string.
        assert_eq!(score(EvalMetric::ExactMatch, "a b", "a  b").unwrap(), 0.0);
    }

    #[test] fn exact_match_both_empty() {
        assert_eq!(score(EvalMetric::ExactMatch, "", "").unwrap(), 1.0);
    }

    #[test] fn exact_match_one_empty() {
        assert_eq!(score(EvalMetric::ExactMatch, "x", "").unwrap(), 0.0);
        assert_eq!(score(EvalMetric::ExactMatch, "", "x").unwrap(), 0.0);
    }

    #[test] fn exact_match_unicode_lowercase() {
        // `str::to_lowercase` handles Unicode. `Ä` → `ä`.
        assert_eq!(score(EvalMetric::ExactMatch, "Ärger", "ärger").unwrap(), 1.0);
    }

    // ---- f1 ----

    #[test] fn f1_identical_one_token() {
        assert_eq!(score(EvalMetric::F1, "hello", "hello").unwrap(), 1.0);
    }

    #[test] fn f1_disjoint_token_sets() {
        // No overlap → 0.0.
        assert_eq!(score(EvalMetric::F1, "a b c", "x y z").unwrap(), 0.0);
    }

    #[test] fn f1_half_overlap() {
        // expected: "a b" (2 tokens), actual: "a c" (2 tokens).
        // common = 1 ("a" matched). precision = 1/2; recall = 1/2.
        // F1 = 2*(0.5*0.5)/(0.5+0.5) = 0.5.
        let s = score(EvalMetric::F1, "a b", "a c").unwrap();
        assert!(approx(s, 0.5), "expected 0.5, got {s}");
    }

    #[test] fn f1_actual_is_superset_of_expected() {
        // expected: "a b"; actual: "a b c d".
        // common=2; precision=2/4=0.5; recall=2/2=1.0.
        // F1 = 2*(0.5*1.0)/(0.5+1.0) = 1.0/1.5 ≈ 0.6667.
        let s = score(EvalMetric::F1, "a b", "a b c d").unwrap();
        assert!(approx(s, 2.0/3.0), "expected ~0.6667, got {s}");
    }

    #[test] fn f1_multiset_counts_repeated_tokens() {
        // expected: "the the"; actual: "the".
        // common = min(2,1) = 1; precision = 1/1 = 1.0;
        // recall = 1/2 = 0.5; F1 = 2*1*0.5/(1+0.5) = 0.6667.
        let s = score(EvalMetric::F1, "the the", "the").unwrap();
        assert!(approx(s, 2.0/3.0), "expected ~0.6667, got {s}");
    }

    #[test] fn f1_case_and_whitespace_normalized() {
        // Same as identical after normalization.
        let s = score(EvalMetric::F1, "  Hello World  ", "hello   world").unwrap();
        assert_eq!(s, 1.0);
    }

    #[test] fn f1_both_empty_is_one() {
        // Convention: vacuously matched.
        assert_eq!(score(EvalMetric::F1, "", "").unwrap(), 1.0);
        assert_eq!(score(EvalMetric::F1, "   ", "").unwrap(), 1.0);
    }

    #[test] fn f1_one_empty_is_zero() {
        assert_eq!(score(EvalMetric::F1, "hello", "").unwrap(), 0.0);
        assert_eq!(score(EvalMetric::F1, "", "hello").unwrap(), 0.0);
    }

    #[test] fn f1_semantic_paraphrase_scores_partial() {
        // "The capital of France is Paris" vs "Paris is the capital of France".
        // Same six tokens in different order → multiset identical
        // → F1 = 1.0. This is exactly the case where F1 outscores
        // exact_match: reordering doesn't penalize.
        let s = score(EvalMetric::F1,
            "The capital of France is Paris",
            "Paris is the capital of France").unwrap();
        assert_eq!(s, 1.0,
            "F1 multiset should ignore order; got {s}");
    }

    // ---- Unimplemented variants (Decision 15) ----

    #[test] fn bleu_returns_unimplemented_with_v02_hint() {
        let err = score(EvalMetric::Bleu, "x", "y").err().unwrap();
        let msg = err.to_string();
        assert!(msg.contains("not implemented in v0.1"));
        assert!(msg.contains("v0.2"));
        assert!(msg.contains("Bleu") || msg.contains("bleu"),
            "error must name the metric; got: {msg}");
    }

    #[test] fn rouge_l_returns_unimplemented() {
        let err = score(EvalMetric::RougeL, "x", "y").err().unwrap();
        assert!(err.to_string().contains("RougeL")
             || err.to_string().contains("rouge_l")
             || err.to_string().contains("Rouge"),
            "got: {err}");
    }

    #[test] fn llm_judge_returns_unimplemented() {
        let err = score(EvalMetric::LlmJudge, "x", "y").err().unwrap();
        let msg = err.to_string();
        assert!(msg.contains("not implemented in v0.1"));
        assert!(msg.contains("LlmJudge") || msg.contains("llm_judge")
             || msg.contains("Llm"),
            "got: {msg}");
        // Specifically: the v0.2 target mentions the judge-cost +
        // catalog-pinned semantics (so the user knows it's a real
        // design surface, not a stub).
        assert!(msg.contains("catalog-pinned") || msg.contains("judge"),
            "must hint at the v0.2 design surface; got: {msg}");
    }

    #[test] fn unimplemented_variants_match_decision_15_set() {
        // Pin the set of v0.2-deferred metrics. If we expand v0.1
        // to add (say) BLEU, this test fails — forcing us to
        // update the PLAN Decision 15 row at the same time.
        for m in [EvalMetric::Bleu, EvalMetric::RougeL, EvalMetric::LlmJudge] {
            assert!(score(m, "x", "y").is_err(),
                "v0.1 must NOT implement {m:?}");
        }
        for m in [EvalMetric::ExactMatch, EvalMetric::F1] {
            assert!(score(m, "x", "y").is_ok(),
                "v0.1 MUST implement {m:?}");
        }
    }
}

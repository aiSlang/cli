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
//! - **f1.** Tokenize both via SQuAD-style normalization (lowercase
//!   + every non-alphanumeric character is a separator — so markdown
//!   `**Berlin**` tokenizes to `["berlin"]` and `Visa,` tokenizes to
//!   `["visa"]`), treat as multisets (counts matter), compute the
//!   common-token-count using the per-token `min(expected_count,
//!   actual_count)` rule, derive precision + recall, return the
//!   harmonic mean. Matches the SQuAD-style F1 used in QA evals.
//!   Empty-both edge case: 1.0 (vacuously matched). Empty-one
//!   edge case: 0.0.
//!
//!   The week-10-Thu retroactive fix: earlier the tokenization was
//!   whitespace-split. Haiku evals scored F1 = 0 on factually-perfect
//!   answers because the model emitted markdown bold + natural
//!   punctuation that the raw expected strings didn't carry. The
//!   non-alphanumeric-as-separator rule fixes that without giving up
//!   the multiset semantics.

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
             See PLAN-90-days-v0_1.md week 8 Decision 15 + the eval explainer section.")]
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
/// Used by `exact_match` — strict comparison, so we keep this
/// minimal. F1 uses the more aggressive `tokenize_for_f1` below.
/// Locale-aware lowercasing is `str::to_lowercase` (handles Unicode
/// correctly).
fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

/// SQuAD-style normalization + tokenization for F1.
///
/// Every non-alphanumeric character is treated as a token separator
/// (and lowercased). This makes the bag-of-tokens comparison
/// punctuation- AND markdown-tolerant: `**Berlin, Germany**` becomes
/// `["berlin", "germany"]`, matching the bare expected `Berlin Germany`.
///
/// Without this normalization, week-9 Haiku evals produced F1 = 0
/// on factually-correct answers — the LLM emits markdown bold and
/// natural punctuation; the raw expected strings don't. See the
/// week-10 retrospective and the regression-pin tests below.
///
/// Approach mirrors the official SQuAD evaluation pipeline minus the
/// article-stripping step (we don't strip "a / an / the" — that's
/// dataset-specific and v0.1 is metric-only). Unicode letters /
/// numbers are preserved via `char::is_alphanumeric`, so non-English
/// answers (German umlauts, Cyrillic, CJK) tokenize correctly.
pub(crate) fn tokenize_for_f1(s: &str) -> Vec<String> {
    let mut current = String::new();
    let mut tokens  = Vec::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            // `to_lowercase()` returns an iterator (some chars
            // lowercase to multiple codepoints, e.g. `ß` → `ss` in
            // some locales; the iterator handles that correctly).
            for lc in c.to_lowercase() {
                current.push(lc);
            }
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

// ============================================================================
// f1 — bag-of-tokens (multiset) precision/recall harmonic mean
// ============================================================================

/// SQuAD-style token-bag F1.
///
/// Steps:
///   1. Tokenize both via `tokenize_for_f1` (lowercase + every
///      non-alphanumeric is a separator — handles markdown +
///      punctuation correctly).
///   2. Count common tokens via min-frequency rule (multiset
///      intersection — handles `"the the" / "the"` correctly).
///   3. precision = common / |actual_tokens|
///   4. recall    = common / |expected_tokens|
///   5. f1        = 2 * P * R / (P + R)
///
/// Edge cases:
///   - Both empty after tokenization → 1.0 (vacuously matches).
///   - One empty, one non-empty → 0.0 (no overlap possible).
///   - Both non-empty but disjoint → 0.0.
fn f1(expected: &str, actual: &str) -> f64 {
    let exp = tokenize_for_f1(expected);
    let act = tokenize_for_f1(actual);

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

    // ---- week 10 Thu hotfix: SQuAD-style tokenization ----
    //
    // Regression pins for the F1 = 0 false-fail behaviour we saw on
    // live Haiku evals. Each test name records the actual user-
    // facing failure mode the fix unblocks.

    #[test] fn tokenize_for_f1_strips_markdown_bold() {
        // Stand-alone tokenizer test — easier to debug than the
        // full F1 round-trip when this regresses.
        assert_eq!(tokenize_for_f1("**Berlin, Germany**"),
                   vec!["berlin", "germany"]);
    }

    #[test] fn tokenize_for_f1_treats_punctuation_as_separator() {
        // `Visa, Mastercard, ` becomes the same tokens whether the
        // commas are present or replaced with spaces.
        assert_eq!(tokenize_for_f1("Visa, Mastercard, American Express, PayPal"),
                   vec!["visa", "mastercard", "american", "express", "paypal"]);
    }

    #[test] fn tokenize_for_f1_keeps_alphanumeric_runs_together() {
        // Email-looking tokens should not be split on dots: SQuAD-
        // ish tokenizers split aggressively, but for our QA evals
        // `support@example.com` is one concept. Actually our impl
        // DOES split on `@` and `.` since they're non-alphanumeric;
        // pin the resulting shape so callers' expected strings can
        // match.
        let t = tokenize_for_f1("support@example.com or open a ticket");
        assert_eq!(t, vec!["support", "example", "com", "or", "open", "a", "ticket"]);
    }

    #[test] fn f1_haiku_headquarters_now_scores_high() {
        // Live failure from week-10-Thu eval run:
        //   expected: "Berlin Germany"
        //   actual:   "Acme Demo Company is headquartered in
        //              **Berlin, Germany**, with additional offices
        //              in San Francisco and Tokyo."
        // Old F1: 0.0 (markdown + comma kept tokens unmatched).
        // New F1: ~0.18 (both expected tokens found; precision drops
        //                because actual has many extra tokens).
        // The improvement: NON-ZERO. The aggregate now reflects the
        // factual correctness of the answer, even if conciseness
        // also matters and lands later in the score.
        let s = score(EvalMetric::F1,
            "Berlin Germany",
            "Acme Demo Company is headquartered in **Berlin, Germany**, \
             with additional offices in San Francisco and Tokyo.").unwrap();
        assert!(s > 0.15,
            "Haiku-style verbose answer must score > 0.15 \
             (factual coverage is 100%); got {s}");
    }

    #[test] fn f1_haiku_payment_methods_now_scores_high() {
        // Live failure: every expected token is present verbatim in
        // the actual, but wrapped in markdown bold + commas. Score
        // jumped from 0.0 to ≥0.45 with the new tokenizer — recall
        // is 1.0 and precision is reasonable.
        let s = score(EvalMetric::F1,
            "Visa Mastercard American Express PayPal",
            "We accept **Visa**, **Mastercard**, **American Express**, \
             and **PayPal**. For orders over $500, we also offer bank \
             transfer.").unwrap();
        assert!(s > 0.45,
            "All-tokens-present markdown answer must score > 0.45; got {s}");
    }

    #[test] fn f1_unicode_letters_survive_normalization() {
        // Critical for non-English answers — German, Cyrillic, CJK.
        // Punctuation strips off, but `Ä`/`äöü`/`Й` etc. stay as
        // letter tokens.
        let s = score(EvalMetric::F1,
            "Über München",
            "Wir sind über München!").unwrap();
        // Both expected tokens present.
        assert!(s > 0.5,
            "Unicode letters must tokenize cleanly; got {s}");
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

//! Cost arithmetic for chat completions.
//!
//! Translates LiteLLM's per-completion `UsageStats` (token counts) into
//! a `Money` value using the catalog's per-1k-token prices. Pure
//! computation — no I/O, no global state. Used by `Session::turn`
//! (Wednesday) to drive the per-run cap and by the spend tracker
//! (Thursday) to roll up daily totals.
//!
//! ## Semantics
//!
//! Catalog prices are `Money` per 1k tokens (the `cost_input_per_1k_tokens`
//! and `cost_output_per_1k_tokens` fields in `crates/aislang-catalog/catalog.json`,
//! deserialized via `Money::from_literal`). Total cost for one completion:
//!
//! ```text
//! cost = (prompt_tokens * cost_input_per_1k + completion_tokens * cost_output_per_1k) / 1000
//! ```
//!
//! ## `Money::Unlimited` propagation
//!
//! If EITHER `cost_input_per_1k` OR `cost_output_per_1k` is
//! `Money::Unlimited`, the result is `Money::Unlimited` (absorbing).
//! This matches the resolver's behavior — an Unlimited-cost candidate
//! is never selected against a `Capped` constraint. A `Capped`
//! budget cap will reject this completion at the per-run check.
//!
//! ## Zero-cost models
//!
//! `Capped(0)` propagates normally (`Capped(0) + Capped(0) = Capped(0)`).
//! Free local models — Ollama as configured in v0.1's bundled catalog —
//! contribute zero to the running spend.

use aislang_core::types::{Currency, Money};
use crate::litellm::UsageStats;

// ============================================================================
// Money formatting — used by Session for the per-run cap synthetic message
// ============================================================================

/// Render a `Money` value as a human-readable currency string. Used
/// in the "budget exceeded" synthetic message emitted by
/// `Session::turn` (week 7 Wed) so users see the actual amounts in
/// the failed-cap notice.
///
/// Format:
///   - `Money::Unlimited` → `"unlimited"`.
///   - `Money::Capped`:    `<symbol><decimal>`, e.g. `$0.0028`, `$50`,
///     `€100.5`. Trailing zeros + lone trailing decimal point are
///     trimmed for readability (`$50.000000` would be noise).
pub fn format_money(m: &Money) -> String {
    let (currency, micro_units) = match (m.currency(), m.micro_units()) {
        (Some(c), Some(mu)) => (c, mu),
        _                    => return "unlimited".to_string(),
    };
    let sym = match currency {
        Currency::Usd => "$",
        Currency::Eur => "€",
        Currency::Gbp => "£",
        Currency::Jpy => "¥",
    };
    // Format with 6 decimal places (the precision the Money parser
    // accepts), then trim trailing zeros so `$50.000000` reads as
    // `$50` and `$0.002800` reads as `$0.0028`.
    let abs       = micro_units.abs();
    let sign      = if micro_units < 0 { "-" } else { "" };
    let whole     = abs / 1_000_000;
    let frac      = abs % 1_000_000;
    let raw       = format!("{whole}.{:06}", frac);
    let trimmed   = raw.trim_end_matches('0').trim_end_matches('.');
    format!("{sign}{sym}{trimmed}")
}

// ============================================================================
// Public API
// ============================================================================

/// Compute the cost of one chat completion.
///
/// `cost_input_per_1k` / `cost_output_per_1k` come from the catalog
/// entry's `cost_input_per_1k_tokens` / `cost_output_per_1k_tokens`
/// fields (already parsed into `Money` at catalog load time).
///
/// Mixed currencies are NOT supported in v0.1 — both prices must
/// share the same `Currency` variant (or one of them be `Unlimited`).
/// We don't *enforce* this at the type level (Money carries currency
/// per-variant); the helper picks the input's currency for the
/// result and a mismatched-currency catalog entry would surface as a
/// confusing total. Catalog authoring practice: keep both fields in
/// the same currency. v0.2 may add a `cross_currency` error variant.
///
/// Integer arithmetic throughout — fractional micro-units are
/// rounded toward zero by the division-by-1000 step. At sub-cent
/// LLM pricing the rounding error is negligible (under a single
/// micro-USD per completion, summed across the running session).
pub fn cost_of_usage(
    cost_input_per_1k:  &Money,
    cost_output_per_1k: &Money,
    usage:              &UsageStats,
) -> Money {
    // Absorbing-Unlimited rule: if either price is unbounded, the
    // total is unbounded.
    let (in_mu, currency) = match (cost_input_per_1k.micro_units(),
                                    cost_input_per_1k.currency()) {
        (Some(mu), Some(c)) => (mu, c),
        _                    => return Money::Unlimited,
    };
    let out_mu = match cost_output_per_1k.micro_units() {
        Some(mu) => mu,
        None     => return Money::Unlimited,
    };

    // prompt_tokens / completion_tokens are u32; widen to i64 for
    // multiplication. Maximum realistic single-completion total
    // (1M tokens × $100/1k = $100,000 = 100,000,000,000 µ-USD) fits
    // i64 comfortably.
    let prompt_cost     = in_mu .saturating_mul(usage.prompt_tokens     as i64) / 1_000;
    let completion_cost = out_mu.saturating_mul(usage.completion_tokens as i64) / 1_000;
    let total_micro     = prompt_cost.saturating_add(completion_cost);

    Money::Capped { currency, micro_units: total_micro }
}

/// Sum two `Money` values. Used by `Session` (Wednesday) to roll up
/// per-call costs into a per-turn running total. Currency-mismatch
/// behavior matches `cost_of_usage`: the left operand's currency wins.
///
/// Unlimited propagation: `Unlimited + anything = Unlimited`. Matches
/// the absorbing-top semantics from the resolver.
pub fn sum(a: &Money, b: &Money) -> Money {
    if a.is_unlimited() || b.is_unlimited() {
        return Money::Unlimited;
    }
    let a_mu = a.micro_units().expect("not unlimited");
    let b_mu = b.micro_units().expect("not unlimited");
    let currency = a.currency()
        .or_else(|| b.currency())
        .unwrap_or(Currency::Usd);
    Money::Capped {
        currency,
        micro_units: a_mu.saturating_add(b_mu),
    }
}

// ============================================================================
// Tests — known prices + edge cases
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::litellm::UsageStats;

    fn usage(prompt: u32, completion: u32) -> UsageStats {
        UsageStats {
            prompt_tokens:     prompt,
            completion_tokens: completion,
            total_tokens:      prompt + completion,
        }
    }

    // ---------------------------------------------------------------
    // Known-price arithmetic against the v0.1 bundled catalog values
    // ---------------------------------------------------------------

    #[test] fn anthropic_haiku_typical_completion() {
        // claude-haiku-4-5 (v0.1 catalog): $0.0008/1k input, $0.004/1k output.
        // 1000 prompt + 500 completion tokens:
        //   input:  $0.0008 × 1000 / 1000 = $0.0008 =     800 µ-USD
        //   output: $0.004  ×  500 / 1000 = $0.002  =   2_000 µ-USD
        //   total                                   =   2_800 µ-USD
        let cost_in  = Money::usd(800);   // $0.0008/1k = 800 µ-USD/1k
        let cost_out = Money::usd(4_000); // $0.004/1k  = 4_000 µ-USD/1k
        let c = cost_of_usage(&cost_in, &cost_out, &usage(1000, 500));
        assert_eq!(c, Money::usd(2_800),
            "Haiku 1k prompt + 500 completion should be 2_800 µ-USD; got {c:?}");
    }

    #[test] fn gpt_4o_mini_typical_completion() {
        // gpt-4o-mini (v0.1 catalog): $0.00015/1k input, $0.0006/1k output.
        // 2000 prompt + 200 completion:
        //   input:  150  × 2000 / 1000 =   300 µ-USD
        //   output: 600  ×  200 / 1000 =   120 µ-USD
        //   total                       =   420 µ-USD
        let cost_in  = Money::usd(150);
        let cost_out = Money::usd(600);
        let c = cost_of_usage(&cost_in, &cost_out, &usage(2000, 200));
        assert_eq!(c, Money::usd(420));
    }

    #[test] fn ollama_free_local_is_zero() {
        // Ollama in the v0.1 catalog: $0/$0. Per-completion cost is
        // always zero, regardless of token count.
        let cost_in  = Money::usd(0);
        let cost_out = Money::usd(0);
        let c = cost_of_usage(&cost_in, &cost_out, &usage(5_000, 10_000));
        assert_eq!(c, Money::usd(0),
            "Ollama free-tier must be zero-cost; got {c:?}");
    }

    // ---------------------------------------------------------------
    // Unlimited propagation (Decision 10)
    // ---------------------------------------------------------------

    #[test] fn unlimited_input_cost_yields_unlimited_total() {
        let c = cost_of_usage(&Money::Unlimited, &Money::usd(100),
                              &usage(1000, 500));
        assert_eq!(c, Money::Unlimited);
    }

    #[test] fn unlimited_output_cost_yields_unlimited_total() {
        let c = cost_of_usage(&Money::usd(100), &Money::Unlimited,
                              &usage(1000, 500));
        assert_eq!(c, Money::Unlimited);
    }

    #[test] fn both_unlimited_yields_unlimited() {
        let c = cost_of_usage(&Money::Unlimited, &Money::Unlimited,
                              &usage(1000, 500));
        assert_eq!(c, Money::Unlimited);
    }

    #[test] fn unlimited_propagates_regardless_of_token_count() {
        // Even with zero tokens, an unlimited price stays unlimited —
        // we're not multiplying through to a concrete number, we're
        // saying "we don't know the cost ceiling."
        let c = cost_of_usage(&Money::Unlimited, &Money::usd(0),
                              &usage(0, 0));
        assert_eq!(c, Money::Unlimited);
    }

    // ---------------------------------------------------------------
    // Edge cases
    // ---------------------------------------------------------------

    #[test] fn zero_tokens_zero_cost() {
        // 0 tokens at any price = 0 cost.
        let c = cost_of_usage(&Money::usd(1_000_000), &Money::usd(1_000_000),
                              &usage(0, 0));
        assert_eq!(c, Money::usd(0));
    }

    #[test] fn fractional_microunits_round_toward_zero() {
        // 1 token at $0.001/1k = 1_000 / 1_000 = 1 µ-USD exactly.
        // But 1 token at $0.0001/1k = 100 / 1_000 = 0.1 → 0 µ-USD
        // (integer division rounds toward zero — negligible accumulated
        // error at LLM pricing scale).
        let c = cost_of_usage(&Money::usd(100), &Money::usd(100),
                              &usage(1, 0));
        assert_eq!(c, Money::usd(0),
            "1 token × 100 µ-USD/1k input rounds toward zero; got {c:?}");
    }

    #[test] fn large_token_counts_dont_overflow() {
        // 10M prompt tokens at $1/1k = 10_000_000 × 1_000_000 / 1_000
        //                            = 10_000_000_000 µ-USD = $10,000.
        // Well under i64::MAX, no overflow.
        let cost_in  = Money::usd(1_000_000); // $1.00/1k
        let cost_out = Money::usd(0);
        let c = cost_of_usage(&cost_in, &cost_out, &usage(10_000_000, 0));
        assert_eq!(c, Money::usd(10_000_000_000));
    }

    #[test] fn cost_uses_input_currency() {
        // The result inherits the INPUT price's currency. Both prices
        // are expected to use the same currency in practice — pin
        // the contract.
        let cost_in  = Money::Capped { currency: Currency::Eur, micro_units: 1_000 };
        let cost_out = Money::Capped { currency: Currency::Eur, micro_units: 5_000 };
        let c = cost_of_usage(&cost_in, &cost_out, &usage(1000, 100));
        assert_eq!(c.currency(), Some(Currency::Eur));
    }

    // ---------------------------------------------------------------
    // sum() helper
    // ---------------------------------------------------------------

    #[test] fn sum_two_capped_amounts() {
        let total = sum(&Money::usd(100), &Money::usd(250));
        assert_eq!(total, Money::usd(350));
    }

    #[test] fn sum_unlimited_lhs_is_unlimited() {
        let total = sum(&Money::Unlimited, &Money::usd(100));
        assert_eq!(total, Money::Unlimited);
    }

    #[test] fn sum_unlimited_rhs_is_unlimited() {
        let total = sum(&Money::usd(100), &Money::Unlimited);
        assert_eq!(total, Money::Unlimited);
    }

    #[test] fn sum_zero_is_identity() {
        let total = sum(&Money::usd(0), &Money::usd(42));
        assert_eq!(total, Money::usd(42));
    }

    #[test] fn sum_saturates_on_overflow() {
        // No realistic LLM run will hit i64::MAX µ-USD ($9 trillion),
        // but the contract is: saturate, don't panic. Pin it.
        let total = sum(&Money::usd(i64::MAX), &Money::usd(i64::MAX));
        assert_eq!(total, Money::usd(i64::MAX));
    }

    // ---------------------------------------------------------------
    // format_money
    // ---------------------------------------------------------------

    #[test] fn format_unlimited() {
        assert_eq!(format_money(&Money::Unlimited), "unlimited");
    }

    #[test] fn format_round_dollar_trims_decimals() {
        assert_eq!(format_money(&Money::usd(50_000_000)), "$50");
    }

    #[test] fn format_typical_sub_cent() {
        // $0.0028 — the Anthropic Haiku 1k+500-tok cost from the
        // cost_of_usage test. Pin the display.
        assert_eq!(format_money(&Money::usd(2_800)), "$0.0028");
    }

    #[test] fn format_zero() {
        assert_eq!(format_money(&Money::usd(0)), "$0");
    }

    #[test] fn format_one_decimal_kept() {
        assert_eq!(format_money(&Money::usd(50_500_000)), "$50.5");
    }

    #[test] fn format_eur_currency_symbol() {
        let m = Money::Capped { currency: Currency::Eur, micro_units: 100_000_000 };
        assert_eq!(format_money(&m), "€100");
    }
}

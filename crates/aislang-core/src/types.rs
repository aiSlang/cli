//! Domain types from LANGUAGE-SPEC-v0.md §3.2.
//!
//! Each type stores a normalized internal form independent of literal syntax.
//! The `from_literal` constructors are pure value parsers: they take the
//! exact source-text token form (no surrounding whitespace, no quoting) and
//! return the typed value or a descriptive [`LiteralParseError`]. The
//! chumsky grammar (week 2) composes these for token-to-value conversion.

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ============================================================================
// Literal parse errors
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid {kind} literal `{input}`: {reason}")]
pub struct LiteralParseError {
    pub kind:   &'static str,
    pub input:  String,
    pub reason: String,
}

impl LiteralParseError {
    fn new(kind: &'static str, input: &str, reason: impl Into<String>) -> Self {
        Self { kind, input: input.to_owned(), reason: reason.into() }
    }
}

// ============================================================================
// Money
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Currency {
    Usd,
    Eur,
    Gbp,
    Jpy,
}

/// Monetary amount stored as integer micro-units of the base currency.
/// `$1` → `1_000_000` micro-USD. Six fractional decimal digits are
/// representable exactly (sufficient for sub-cent LLM pricing like
/// `$0.000075`); more is rejected at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Money {
    pub currency:    Currency,
    pub micro_units: i64,
}

/// `10^6` — number of micro-units per one base currency unit.
const MICRO_PER_BASE: i64 = 1_000_000;

impl Money {
    /// Parse a `Money` literal of the form `<symbol><integer>[.<fraction>]`,
    /// where `<symbol>` is one of `$`, `€`, `£`, `¥`.
    ///
    /// Examples (valid):
    ///   `$50`, `$0.001`, `$0.000075`, `€100`, `£0.50`, `¥1000`
    ///
    /// Examples (rejected):
    ///   `"$50"` (a String, not a Money — kind error caught by the type-checker),
    ///   `50`, `$`, `$.50`, `$0.0000001` (>6 fractional digits)
    pub fn from_literal(s: &str) -> Result<Self, LiteralParseError> {
        let err = |reason: &str| LiteralParseError::new("Money", s, reason);

        let mut chars = s.chars();
        let sym = chars.next().ok_or_else(|| err("empty input"))?;
        let currency = match sym {
            '$' => Currency::Usd,
            '€' => Currency::Eur,
            '£' => Currency::Gbp,
            '¥' => Currency::Jpy,
            _ => return Err(err("must start with one of $, €, £, ¥")),
        };

        let rest = chars.as_str();
        if rest.is_empty() {
            return Err(err("missing numeric amount after currency symbol"));
        }

        let (int_part, frac_part) = match rest.split_once('.') {
            Some((i, f)) => (i, f),
            None         => (rest, ""),
        };

        if int_part.is_empty() {
            return Err(err("missing integer digits before decimal point"));
        }
        if !int_part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(err("integer part must be decimal digits"));
        }
        if !frac_part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(err("fractional part must be decimal digits"));
        }
        if frac_part.len() > 6 {
            return Err(err("fractional precision exceeds 6 decimal places"));
        }

        let int_value: i64 = int_part
            .parse()
            .map_err(|_| err("integer part out of range"))?;
        let micro_from_int = int_value
            .checked_mul(MICRO_PER_BASE)
            .ok_or_else(|| err("amount overflow"))?;

        let micro_from_frac: i64 = if frac_part.is_empty() {
            0
        } else {
            let frac_value: i64 = frac_part
                .parse()
                .map_err(|_| err("fractional part out of range"))?;
            let scale = 10_i64.pow((6 - frac_part.len()) as u32);
            frac_value * scale
        };

        let micro_units = micro_from_int
            .checked_add(micro_from_frac)
            .ok_or_else(|| err("amount overflow"))?;

        Ok(Money { currency, micro_units })
    }
}

// ============================================================================
// Duration
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Duration {
    pub nanos: i64,
}

impl Duration {
    /// Parse a `Duration` literal of the form `<integer><unit>`, where
    /// `<unit>` is one of `ns`, `us`, `ms`, `s`, `m`, `h`, `d`.
    ///
    /// Decimal durations (`1.5s`) and composite durations (`5h30m`) are
    /// rejected — split them at the source.
    pub fn from_literal(s: &str) -> Result<Self, LiteralParseError> {
        let err = |reason: &str| LiteralParseError::new("Duration", s, reason);

        let digit_end = s
            .find(|c: char| !c.is_ascii_digit())
            .ok_or_else(|| err("missing unit suffix (ns/us/ms/s/m/h/d)"))?;
        if digit_end == 0 {
            return Err(err("missing numeric value"));
        }

        let (num_part, unit_part) = s.split_at(digit_end);
        let value: i64 = num_part
            .parse()
            .map_err(|_| err("number out of range"))?;

        let nanos_per_unit: i64 = match unit_part {
            "ns" => 1,
            "us" => 1_000,
            "ms" => 1_000_000,
            "s"  => 1_000_000_000,
            "m"  => 60 * 1_000_000_000,
            "h"  => 3_600 * 1_000_000_000,
            "d"  => 86_400 * 1_000_000_000,
            other => {
                return Err(err(&format!(
                    "unknown unit `{other}` (expected ns/us/ms/s/m/h/d)"
                )))
            }
        };

        let nanos = value
            .checked_mul(nanos_per_unit)
            .ok_or_else(|| err("duration overflow"))?;
        Ok(Duration { nanos })
    }
}

// ============================================================================
// Size — bytes for storage, tokens-as-units for context windows
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Size {
    /// Value in base units. For `Size` used as token counts, this is tokens;
    /// for `Size` used as bytes, this is bytes. The suffix (`k`, `MB`, `GB`)
    /// determines the multiplier at parse time.
    pub units: u64,
}

impl Size {
    /// Parse a `Size` literal of the form `<integer>[<unit>]`, where
    /// `<unit>` is one of `k`, `K`, `KB`, `MB`, `GB` (all 1024-based per
    /// LANGUAGE-SPEC-v0.md §3.2). No suffix means base units.
    pub fn from_literal(s: &str) -> Result<Self, LiteralParseError> {
        let err = |reason: &str| LiteralParseError::new("Size", s, reason);

        let digit_end = s
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(s.len());
        if digit_end == 0 {
            return Err(err("missing numeric value"));
        }

        let (num_part, unit_part) = s.split_at(digit_end);
        let value: u64 = num_part
            .parse()
            .map_err(|_| err("number out of range"))?;

        let multiplier: u64 = match unit_part {
            ""              => 1,
            "k" | "K" | "KB" => 1024,
            "MB"            => 1024 * 1024,
            "GB"            => 1024 * 1024 * 1024,
            other => {
                return Err(err(&format!(
                    "unknown unit `{other}` (expected k/K/KB/MB/GB or empty)"
                )))
            }
        };

        let units = value
            .checked_mul(multiplier)
            .ok_or_else(|| err("size overflow"))?;
        Ok(Size { units })
    }
}

// ============================================================================
// Percentage
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Percentage {
    /// Always in [0.0, 1.0]. `85%` → 0.85.
    pub ratio: f64,
}

impl Percentage {
    /// Parse a `Percentage` literal of the form `<number>%`. The numeric
    /// part may be integer (`85%`) or decimal (`0.5%`). Result must be in
    /// `[0, 100]` percent; out-of-range values are rejected.
    pub fn from_literal(s: &str) -> Result<Self, LiteralParseError> {
        let err = |reason: &str| LiteralParseError::new("Percentage", s, reason);

        let stripped = s
            .strip_suffix('%')
            .ok_or_else(|| err("must end with `%`"))?;
        if stripped.is_empty() {
            return Err(err("missing numeric value before `%`"));
        }

        let raw: f64 = stripped
            .parse()
            .map_err(|_| err("invalid number"))?;
        if !raw.is_finite() {
            return Err(err("must be a finite number"));
        }
        if !(0.0..=100.0).contains(&raw) {
            return Err(err("must be in [0, 100]"));
        }

        Ok(Percentage { ratio: raw / 100.0 })
    }
}

// ============================================================================
// Rate — composition of two domain types via the `per` keyword
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rate<N, D> {
    pub numerator:   N,
    pub denominator: D,
}

// ============================================================================
// OciImage
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OciImage {
    pub registry:   String,
    pub repository: String,
    pub tag:        Option<String>,
    pub digest:     Option<String>,
}

// ============================================================================
// Closed enums (LANGUAGE-SPEC-v0.md §3.4)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Chat,
    Embedding,
    ToolUse,
    Vision,
    Audio,
    Reasoning,
    LargeContext,
    StructuredOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Residency {
    Us,
    Eu,
    Uk,
    Apac,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeployTarget {
    Compose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OptimizeFor {
    Cost,
    Latency,
    Quality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalMetric {
    ExactMatch,
    F1,
    Bleu,
    RougeL,
    LlmJudge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalAggregator {
    Mean,
    Min,
    Max,
    PercentileP95,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod money_tests {
    use super::*;

    fn ok(input: &str, currency: Currency, micro: i64) {
        let parsed = Money::from_literal(input)
            .unwrap_or_else(|e| panic!("expected `{input}` to parse, got: {e}"));
        assert_eq!(parsed, Money { currency, micro_units: micro });
    }

    fn rejected(input: &str, substring: &str) {
        let err = Money::from_literal(input)
            .err()
            .unwrap_or_else(|| panic!("expected `{input}` to be rejected"));
        assert!(
            err.reason.contains(substring),
            "expected reason for `{input}` to contain `{substring}`, got: {}",
            err.reason
        );
    }

    #[test] fn integer_usd()      { ok("$50",       Currency::Usd, 50_000_000); }
    #[test] fn zero_usd()         { ok("$0",        Currency::Usd, 0); }
    #[test] fn small_decimal()    { ok("$0.50",     Currency::Usd, 500_000); }
    #[test] fn sub_cent()         { ok("$0.001",    Currency::Usd, 1_000); }
    #[test] fn six_decimals_max() { ok("$0.000075", Currency::Usd, 75); }
    #[test] fn euro()             { ok("€100",      Currency::Eur, 100_000_000); }
    #[test] fn pound()            { ok("£0.50",     Currency::Gbp, 500_000); }
    #[test] fn yen()              { ok("¥1000",     Currency::Jpy, 1_000_000_000); }

    #[test] fn quoted_string_kind_mismatch_is_caller_problem() {
        // `"$50"` is a String literal — it's the type-checker's job to reject
        // its assignment to a Money field. The literal parser only sees raw
        // token text after the lexer strips quotes; we reject it here because
        // a leading `"` is not a currency symbol.
        rejected("\"$50\"", "must start with one of");
    }

    #[test] fn missing_currency()             { rejected("50",        "must start with one of"); }
    #[test] fn empty()                        { rejected("",          "empty input"); }
    #[test] fn currency_only()                { rejected("$",         "missing numeric amount"); }
    #[test] fn leading_decimal_rejected()     { rejected("$.50",      "missing integer digits"); }
    #[test] fn seven_decimals_rejected()      { rejected("$0.0000001","fractional precision"); }
    #[test] fn non_digit_in_integer_rejected(){ rejected("$5a",       "integer part must be decimal digits"); }
    #[test] fn negative_rejected()            { rejected("$-5",       "integer part must be decimal digits"); }
}

#[cfg(test)]
mod duration_tests {
    use super::*;

    fn ok(input: &str, nanos: i64) {
        let parsed = Duration::from_literal(input)
            .unwrap_or_else(|e| panic!("expected `{input}` to parse, got: {e}"));
        assert_eq!(parsed.nanos, nanos);
    }
    fn rejected(input: &str, substring: &str) {
        let err = Duration::from_literal(input)
            .err()
            .unwrap_or_else(|| panic!("expected `{input}` to be rejected"));
        assert!(err.reason.contains(substring),
            "reason `{}` did not contain `{substring}`", err.reason);
    }

    #[test] fn nanoseconds() { ok("5ns",  5); }
    #[test] fn microseconds(){ ok("5us",  5_000); }
    #[test] fn milliseconds(){ ok("500ms", 500_000_000); }
    #[test] fn seconds()     { ok("2s",   2_000_000_000); }
    #[test] fn minutes()     { ok("2m",   120_000_000_000); }
    #[test] fn hours()       { ok("1h",   3_600_000_000_000); }
    #[test] fn days()        { ok("1d",   86_400_000_000_000); }
    #[test] fn zero()        { ok("0s",   0); }

    #[test] fn no_unit()           { rejected("5",     "missing unit suffix"); }
    #[test] fn no_number()         { rejected("s",     "missing numeric value"); }
    #[test] fn unknown_unit()      { rejected("5y",    "unknown unit"); }
    #[test] fn decimal_rejected()  { rejected("1.5s",  "unknown unit"); }
    #[test] fn composite_rejected(){ rejected("5h30m", "unknown unit"); }
    #[test] fn empty()             { rejected("",      "missing unit suffix"); }
    #[test] fn space_rejected()    { rejected("5 s",   "unknown unit"); }
}

#[cfg(test)]
mod size_tests {
    use super::*;

    fn ok(input: &str, units: u64) {
        let parsed = Size::from_literal(input)
            .unwrap_or_else(|e| panic!("expected `{input}` to parse, got: {e}"));
        assert_eq!(parsed.units, units);
    }
    fn rejected(input: &str, substring: &str) {
        let err = Size::from_literal(input)
            .err()
            .unwrap_or_else(|| panic!("expected `{input}` to be rejected"));
        assert!(err.reason.contains(substring),
            "reason `{}` did not contain `{substring}`", err.reason);
    }

    #[test] fn no_suffix_is_base_unit() { ok("1024", 1024); }
    #[test] fn lower_k()                { ok("32k",  32 * 1024); }
    #[test] fn upper_k()                { ok("32K",  32 * 1024); }
    #[test] fn kb()                     { ok("4KB",  4 * 1024); }
    #[test] fn mb()                     { ok("1MB",  1024 * 1024); }
    #[test] fn gb()                     { ok("8GB",  8 * 1024 * 1024 * 1024); }
    #[test] fn zero()                   { ok("0",    0); }

    #[test] fn no_number()                 { rejected("MB",    "missing numeric value"); }
    #[test] fn unknown_unit()              { rejected("5XB",   "unknown unit"); }
    #[test] fn lowercase_mb_rejected()     { rejected("1mb",   "unknown unit"); }
    #[test] fn bare_m_rejected()           { rejected("1M",    "unknown unit"); }
    #[test] fn space_rejected()            { rejected("32 k",  "unknown unit"); }
}

#[cfg(test)]
mod percentage_tests {
    use super::*;

    fn ok(input: &str, ratio: f64) {
        let parsed = Percentage::from_literal(input)
            .unwrap_or_else(|e| panic!("expected `{input}` to parse, got: {e}"));
        // Tolerance for the canonical decimal round-trip.
        assert!((parsed.ratio - ratio).abs() < 1e-12,
            "ratio {} != expected {}", parsed.ratio, ratio);
    }
    fn rejected(input: &str, substring: &str) {
        let err = Percentage::from_literal(input)
            .err()
            .unwrap_or_else(|| panic!("expected `{input}` to be rejected"));
        assert!(err.reason.contains(substring),
            "reason `{}` did not contain `{substring}`", err.reason);
    }

    #[test] fn integer_percent()  { ok("85%",   0.85); }
    #[test] fn decimal_percent()  { ok("0.5%",  0.005); }
    #[test] fn full_percent()     { ok("100%",  1.0); }
    #[test] fn zero_percent()     { ok("0%",    0.0); }

    #[test] fn no_percent_sign()  { rejected("85",     "must end with"); }
    #[test] fn empty_number()     { rejected("%",      "missing numeric"); }
    #[test] fn over_100()         { rejected("150%",   "[0, 100]"); }
    #[test] fn negative()         { rejected("-1%",    "[0, 100]"); }
    #[test] fn non_numeric()      { rejected("abc%",   "invalid number"); }
    #[test] fn empty()            { rejected("",       "must end with"); }
}

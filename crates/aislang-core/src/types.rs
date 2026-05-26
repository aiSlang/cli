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
#[serde(rename_all = "UPPERCASE")]   // ISO 4217: USD/EUR/GBP/JPY
pub enum Currency {
    Usd,
    Eur,
    Gbp,
    Jpy,
}

/// Monetary amount. v0.1 has two shapes (Decision 10):
///
///   - **`Capped { currency, micro_units }`** — a finite amount stored
///     as integer micro-units of the base currency. `$1` →
///     `Capped { currency: Usd, micro_units: 1_000_000 }`. Six
///     fractional decimal digits representable exactly (sub-cent LLM
///     pricing like `$0.000075`); more rejected at parse time.
///   - **`Unlimited`** — the bare keyword `unlimited`. Used in
///     `budget` block fields to mean "no cap" — common for local-only
///     models (Ollama). Treated as the absorbing top in cost
///     comparisons: `Unlimited > Capped(any)`, `Unlimited == Unlimited`.
///
/// `Unlimited` is conceptually valid for catalog `cost_input_*` /
/// `cost_output_*` fields too ("model exists but cost is unbounded /
/// unknown") — the resolver handles it by excluding such candidates
/// from any `Capped` budget constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Money {
    Capped { currency: Currency, micro_units: i64 },
    Unlimited,
}

/// `10^6` — number of micro-units per one base currency unit.
const MICRO_PER_BASE: i64 = 1_000_000;

impl Money {
    /// `true` iff this is the `Unlimited` variant.
    pub fn is_unlimited(&self) -> bool {
        matches!(self, Money::Unlimited)
    }

    /// Extract `micro_units` if `Capped`, `None` if `Unlimited`. Use
    /// at sites where "Unlimited" doesn't make arithmetic sense (e.g.
    /// emitting env vars).
    pub fn micro_units(&self) -> Option<i64> {
        match self {
            Money::Capped { micro_units, .. } => Some(*micro_units),
            Money::Unlimited                  => None,
        }
    }

    /// Extract `currency` if `Capped`, `None` if `Unlimited`.
    pub fn currency(&self) -> Option<Currency> {
        match self {
            Money::Capped { currency, .. } => Some(*currency),
            Money::Unlimited               => None,
        }
    }

    /// Build a USD-`Capped` Money — used by tests + helpers.
    pub fn usd(micro_units: i64) -> Self {
        Money::Capped { currency: Currency::Usd, micro_units }
    }

    /// Parse a `Money` literal: `<symbol><integer>[.<fraction>]` for
    /// a finite amount, OR the bare keyword `unlimited` for the
    /// `Unlimited` variant.
    ///
    /// Examples (valid):
    ///   `$50`, `$0.001`, `$0.000075`, `€100`, `£0.50`, `¥1000`,
    ///   `unlimited`
    ///
    /// Examples (rejected):
    ///   `"$50"` (a String, not a Money — kind error caught by the type-checker),
    ///   `50`, `$`, `$.50`, `$0.0000001` (>6 fractional digits)
    pub fn from_literal(s: &str) -> Result<Self, LiteralParseError> {
        if s == "unlimited" {
            return Ok(Money::Unlimited);
        }
        let err = |reason: &str| LiteralParseError::new("Money", s, reason);

        let mut chars = s.chars();
        let sym = chars.next().ok_or_else(|| err("empty input"))?;
        let currency = match sym {
            '$' => Currency::Usd,
            '€' => Currency::Eur,
            '£' => Currency::Gbp,
            '¥' => Currency::Jpy,
            _ => return Err(err("must start with one of $, €, £, ¥, or be the bare keyword `unlimited`")),
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

        Ok(Money::Capped { currency, micro_units })
    }
}

// Cost-comparison ordering. v0.1 ignores currency mismatches — all
// catalog/budget values are USD in practice. Mixed-currency
// comparisons (rare) compare micro_units directly, matching what
// users likely expect.
impl PartialOrd for Money {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Money {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        match (self, other) {
            (Money::Unlimited,                 Money::Unlimited)                 => Equal,
            (Money::Unlimited,                 Money::Capped { .. })             => Greater,
            (Money::Capped { .. },             Money::Unlimited)                 => Less,
            (Money::Capped { micro_units: a, ..}, Money::Capped { micro_units: b, ..}) => a.cmp(b),
        }
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
// SemVer — exact-version domain type (strict semantics)
// ============================================================================

/// An exact semver 2.0 version. v0.1 deliberately *rejects* range operators
/// (`^`, `~`, `>=`, `=`, `<`) — a `prompt.version` or other pinned reference
/// must name one specific version. Range syntax is deferred to v0.2; see
/// LANGUAGE-SPEC-v0.md §3.2.
///
/// `#[serde(transparent)]` makes this serialize as a bare string `"1.2.3"`
/// rather than `{"0": "1.2.3"}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SemVer(pub semver::Version);

impl SemVer {
    /// Parse a `SemVer` literal — the textual semver 2.0 version, with no
    /// range operators.
    pub fn from_literal(s: &str) -> Result<Self, LiteralParseError> {
        let err = |reason: &str| LiteralParseError::new("SemVer", s, reason);

        if let Some(first) = s.chars().next() {
            if matches!(first, '^' | '~' | '>' | '<' | '=') {
                return Err(err(
                    "v0.1 requires an exact version (e.g. `\"1.0.0\"`); \
                     range operators (^, ~, >=, =, <) are deferred to v0.2",
                ));
            }
        } else {
            return Err(err("empty input"));
        }

        semver::Version::parse(s)
            .map(SemVer)
            .map_err(|e| err(&e.to_string()))
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

impl OciImage {
    /// Parse an `OciImage` literal of the form
    /// `<registry>/<repository>[:<tag>][@<algo>:<hex>]`.
    ///
    /// **v0.1 simplifications:**
    /// - Registry must look like a hostname (contain `.`). Docker Hub
    ///   shorthand (`nginx:latest`) and port-only addresses
    ///   (`localhost:5000/foo`) are rejected — defer to v0.2.
    /// - At least one of `tag` or `digest` is required. Implicit
    ///   `:latest` is rejected for reproducibility.
    /// - Digest is `<algorithm>:<hex>` with hex-only payload.
    pub fn from_literal(s: &str) -> Result<Self, LiteralParseError> {
        let err = |reason: &str| LiteralParseError::new("OciImage", s, reason);

        if s.is_empty() {
            return Err(err("empty input"));
        }

        // Split off digest at the rightmost `@` (registry/repository never
        // contain `@`, so rsplit is safe).
        let (before_digest, digest) = match s.rsplit_once('@') {
            Some((before, dig)) => (before, Some(dig.to_string())),
            None => (s, None),
        };

        if let Some(ref d) = digest {
            if d.is_empty() {
                return Err(err("digest after `@` is empty"));
            }
            let (algo, hex) = d.split_once(':').ok_or_else(|| {
                err("digest must be `<algorithm>:<hex>` (e.g. `sha256:abc…`)")
            })?;
            if algo.is_empty() || hex.is_empty() {
                return Err(err("digest algorithm or hex part is empty"));
            }
            if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(err("digest hex part must be hexadecimal"));
            }
        }

        // First `/` separates registry from repository.
        let first_slash = before_digest.find('/').ok_or_else(|| {
            err("missing `/` between registry and repository \
                 (e.g. `ghcr.io/owner/repo:tag`)")
        })?;
        let registry = &before_digest[..first_slash];
        let rest     = &before_digest[first_slash + 1..];

        if registry.is_empty() {
            return Err(err("registry segment before `/` is empty"));
        }
        if !registry.contains('.') {
            return Err(err(
                "registry must look like a hostname (contain `.`); \
                 Docker Hub shorthand and port-only registries deferred to v0.2",
            ));
        }

        // Last `:` in the post-registry segment separates repository from tag.
        // (OCI tags never contain `:`, so rsplit is safe.)
        let (repository, tag) = match rest.rsplit_once(':') {
            Some((repo, t)) => (repo.to_string(), Some(t.to_string())),
            None            => (rest.to_string(), None),
        };

        if repository.is_empty() {
            return Err(err("repository segment is empty"));
        }
        if let Some(ref t) = tag {
            if t.is_empty() {
                return Err(err("tag after `:` is empty"));
            }
        }
        if tag.is_none() && digest.is_none() {
            return Err(err(
                "must specify a tag (e.g. `:1.0.0`) or digest (e.g. `@sha256:…`); \
                 implicit `:latest` is rejected for reproducibility",
            ));
        }

        Ok(OciImage {
            registry: registry.to_string(),
            repository,
            tag,
            digest,
        })
    }
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
        assert_eq!(parsed, Money::Capped { currency, micro_units: micro });
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

    // ---- Money::Unlimited (Decision 10, week 7 Mon) ----

    #[test] fn unlimited_literal_parses() {
        // The bare keyword `unlimited` produces `Money::Unlimited`.
        let m = Money::from_literal("unlimited").expect("unlimited parses");
        assert_eq!(m, Money::Unlimited);
        assert!(m.is_unlimited());
        assert_eq!(m.micro_units(), None);
        assert_eq!(m.currency(),    None);
    }

    #[test] fn unlimited_is_case_sensitive() {
        // We reserve only the lowercase form. `UNLIMITED`, `Unlimited`,
        // etc. continue to fail the currency-symbol check.
        rejected("UNLIMITED", "must start with one of");
        rejected("Unlimited", "must start with one of");
    }

    #[test] fn capped_helpers_work() {
        let m = Money::Capped { currency: Currency::Usd, micro_units: 42 };
        assert!(!m.is_unlimited());
        assert_eq!(m.micro_units(), Some(42));
        assert_eq!(m.currency(),    Some(Currency::Usd));
    }

    #[test] fn usd_constructor_helper() {
        assert_eq!(
            Money::usd(1_000),
            Money::Capped { currency: Currency::Usd, micro_units: 1_000 },
        );
    }

    // ---- Money Ord: Unlimited as absorbing top ----

    #[test] fn unlimited_is_greater_than_any_capped() {
        assert!(Money::Unlimited > Money::usd(0));
        assert!(Money::Unlimited > Money::usd(i64::MAX));
    }

    #[test] fn capped_compares_by_micro_units() {
        assert!(Money::usd(1)   < Money::usd(2));
        assert!(Money::usd(100) > Money::usd(50));
        assert_eq!(Money::usd(7), Money::usd(7));
    }

    #[test] fn unlimited_equals_unlimited() {
        assert_eq!(Money::Unlimited, Money::Unlimited);
        // Ord: not less, not greater.
        assert_eq!(Money::Unlimited.cmp(&Money::Unlimited),
                   std::cmp::Ordering::Equal);
    }

    #[test] fn min_with_unlimited_picks_capped() {
        // Pinned for the resolver's `min_by` cost-optimization path
        // (week 4 Mon): Unlimited never wins "cheapest".
        let candidates = vec![Money::Unlimited, Money::usd(50), Money::usd(10)];
        let cheapest = candidates.iter().min().unwrap();
        assert_eq!(*cheapest, Money::usd(10));
    }

    // ---- JSON round-trip for both variants ----

    #[test] fn capped_json_round_trip() {
        let m = Money::usd(42);
        let s = serde_json::to_string(&m).unwrap();
        // Tagged enum: kind=capped + currency + micro_units.
        assert!(s.contains(r#""kind":"capped""#), "got: {s}");
        assert!(s.contains(r#""currency":"USD""#), "got: {s}");
        assert!(s.contains(r#""micro_units":42"#), "got: {s}");
        let back: Money = serde_json::from_str(&s).unwrap();
        assert_eq!(back, m);
    }

    #[test] fn unlimited_json_round_trip() {
        let m = Money::Unlimited;
        let s = serde_json::to_string(&m).unwrap();
        // The variant has no payload — just the tag.
        assert_eq!(s, r#"{"kind":"unlimited"}"#);
        let back: Money = serde_json::from_str(&s).unwrap();
        assert_eq!(back, Money::Unlimited);
    }
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
mod oci_image_tests {
    use super::*;

    fn ok(input: &str, registry: &str, repository: &str, tag: Option<&str>, digest: Option<&str>) {
        let parsed = OciImage::from_literal(input)
            .unwrap_or_else(|e| panic!("expected `{input}` to parse, got: {e}"));
        assert_eq!(parsed.registry,   registry);
        assert_eq!(parsed.repository, repository);
        assert_eq!(parsed.tag.as_deref(),    tag);
        assert_eq!(parsed.digest.as_deref(), digest);
    }
    fn rejected(input: &str, substring: &str) {
        let err = OciImage::from_literal(input)
            .err()
            .unwrap_or_else(|| panic!("expected `{input}` to be rejected"));
        assert!(err.reason.contains(substring),
            "reason `{}` did not contain `{substring}`", err.reason);
    }

    #[test] fn simple_tag() {
        ok("ghcr.io/example/mcp_filesystem:0.3.1",
           "ghcr.io", "example/mcp_filesystem", Some("0.3.1"), None);
    }
    #[test] fn digest_only() {
        ok("ghcr.io/foo/bar@sha256:abc123",
           "ghcr.io", "foo/bar", None, Some("sha256:abc123"));
    }
    #[test] fn tag_and_digest() {
        ok("ghcr.io/foo/bar:1.0@sha256:abcdef",
           "ghcr.io", "foo/bar", Some("1.0"), Some("sha256:abcdef"));
    }
    #[test] fn dockerhub_library_explicit() {
        ok("docker.io/library/nginx:1.21",
           "docker.io", "library/nginx", Some("1.21"), None);
    }
    #[test] fn deep_repository_path() {
        ok("registry.example.com/team/app/component:v2",
           "registry.example.com", "team/app/component", Some("v2"), None);
    }

    #[test] fn missing_registry_hostname_rejected() {
        // `localhost/foo:tag` has a `/` (so passes the slash check) but the
        // registry segment `localhost` has no `.` — fails the hostname check.
        rejected("localhost/foo:tag", "hostname");
    }
    #[test] fn dockerhub_shorthand_rejected() {
        // Docker Hub library shorthand (`nginx:latest`) has no `/` at all
        // and thus fails the slash check before getting to hostname validation.
        rejected("nginx:latest", "missing `/`");
    }
    #[test] fn missing_slash_rejected() {
        rejected("ghcr.io", "missing `/`");
    }
    #[test] fn missing_tag_and_digest_rejected() {
        rejected("ghcr.io/foo/bar", "must specify a tag");
    }
    #[test] fn empty_tag_rejected() {
        rejected("ghcr.io/foo/bar:", "tag after `:` is empty");
    }
    #[test] fn empty_digest_rejected() {
        rejected("ghcr.io/foo/bar@", "digest after `@` is empty");
    }
    #[test] fn malformed_digest_rejected() {
        rejected("ghcr.io/foo/bar@sha256-abcd", "digest must be");
    }
    #[test] fn non_hex_digest_rejected() {
        rejected("ghcr.io/foo/bar@sha256:xyzzy", "hexadecimal");
    }
    #[test] fn empty_repository_rejected() {
        rejected("ghcr.io/:tag", "repository segment is empty");
    }
    #[test] fn empty_input_rejected() {
        rejected("", "empty input");
    }
}

#[cfg(test)]
mod semver_tests {
    use super::*;

    fn ok(input: &str, major: u64, minor: u64, patch: u64) {
        let parsed = SemVer::from_literal(input)
            .unwrap_or_else(|e| panic!("expected `{input}` to parse, got: {e}"));
        assert_eq!(parsed.0.major, major);
        assert_eq!(parsed.0.minor, minor);
        assert_eq!(parsed.0.patch, patch);
    }
    fn rejected(input: &str, substring: &str) {
        let err = SemVer::from_literal(input)
            .err()
            .unwrap_or_else(|| panic!("expected `{input}` to be rejected"));
        assert!(err.reason.contains(substring),
            "reason `{}` did not contain `{substring}`", err.reason);
    }

    #[test] fn basic_version()    { ok("1.0.0",                1, 0, 0); }
    #[test] fn prerelease()       { ok("1.2.3-alpha",          1, 2, 3); }
    #[test] fn build_metadata()   { ok("2.0.0+build.123",      2, 0, 0); }
    #[test] fn prerelease_build() { ok("1.0.0-rc.1+sha.abcdef",1, 0, 0); }
    #[test] fn zero_version()     { ok("0.0.0",                0, 0, 0); }

    #[test] fn caret_rejected()           { rejected("^1.0.0",  "range operators"); }
    #[test] fn tilde_rejected()           { rejected("~1.2.0",  "range operators"); }
    #[test] fn gte_rejected()             { rejected(">=1.0.0", "range operators"); }
    #[test] fn equals_rejected()          { rejected("=1.0.0",  "range operators"); }
    #[test] fn empty()                    { rejected("",        "empty input"); }
    #[test] fn missing_patch_rejected()   { rejected("1.0",     "minor"); }    // semver crate error contains "minor"/"patch"
    #[test] fn non_numeric_rejected()     { rejected("abc",     "expected"); }
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

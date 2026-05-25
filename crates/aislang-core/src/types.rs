//! Domain types from LANGUAGE-SPEC-v0.md §3.2.
//!
//! Each type stores a normalized internal form independent of literal syntax.
//! Literal parsers (week 2) convert source-text literals into these values;
//! the type checker (week 3) then ensures assignment compatibility.

use serde::{Deserialize, Serialize};

// -------------------------------------------------------------------------- //
// Money
// -------------------------------------------------------------------------- //

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Currency {
    Usd,
    Eur,
    Gbp,
    Jpy,
}

/// Monetary amount stored as integer micro-units of the smallest currency
/// subdivision (e.g. micro-cents for USD/EUR/GBP, micro-yen for JPY).
/// Avoids floating-point rounding entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    pub currency: Currency,
    pub micro_units: i64,
}

// -------------------------------------------------------------------------- //
// Duration
// -------------------------------------------------------------------------- //

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Duration {
    pub nanos: i64,
}

// -------------------------------------------------------------------------- //
// Size — bytes for storage, tokens-as-units for context windows
// -------------------------------------------------------------------------- //

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    /// Value in base units. For `Size` used as token counts, this is tokens;
    /// for `Size` used as bytes, this is bytes. The suffix (`k`, `MB`, `GB`)
    /// determines the multiplier at parse time.
    pub units: u64,
}

// -------------------------------------------------------------------------- //
// Percentage
// -------------------------------------------------------------------------- //

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Percentage {
    /// Always in [0.0, 1.0]. `85%` → 0.85.
    pub ratio: f64,
}

// -------------------------------------------------------------------------- //
// Rate — composition of two domain types via the `per` keyword
// -------------------------------------------------------------------------- //

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rate<N, D> {
    pub numerator: N,
    pub denominator: D,
}

// -------------------------------------------------------------------------- //
// OciImage
// -------------------------------------------------------------------------- //

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciImage {
    pub registry:   String,
    pub repository: String,
    pub tag:        Option<String>,
    pub digest:     Option<String>,
}

// -------------------------------------------------------------------------- //
// Closed enums (LANGUAGE-SPEC-v0.md §3.4)
// -------------------------------------------------------------------------- //

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

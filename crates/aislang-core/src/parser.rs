//! Parser for the aiSlang v0 grammar.
//!
//! Architecture (kept stable across week-2 expansion):
//!
//! 1. **Leaf parsers** (top-level fns): one combinator per token shape
//!    — `ident_parser`, `string_lit_parser`, `money_parser`,
//!    `duration_parser`, `size_parser`, `percentage_parser`,
//!    `float_parser`, `int_parser`. Each is unit-testable in isolation.
//! 2. **`value_parser`** — choice over all literal kinds; produces
//!    [`RawValue`]. Padded with whitespace + comments.
//! 3. **`attribute_parser`** — `ident '=' value`.
//! 4. **`parser` (the public root)** — recursive `block` production over
//!    attributes and nested blocks; emits a [`RawBlock`] tree.
//! 5. **Converters** (`to_project`, `to_budget`, …) — translate the
//!    generic block tree into the typed [`crate::ast`] tree, carrying
//!    every semantic-layer restriction from spec §8.1. Week 2 grows
//!    this section with the six remaining resource converters.
//!
//! See LANGUAGE-SPEC-v0.md §8 for the EBNF this implements.

use crate::ast::*;
use crate::types::*;
use chumsky::prelude::*;
use serde::Serialize;

// ============================================================================
// Public entry point
// ============================================================================

/// Parse a source string into a typed [`Project`] AST.
pub fn parse(source: &str) -> Result<Project, String> {
    let raw = parse_raw_internal(source)?;
    to_project(raw)
}

/// Internal grammar-only parse — returns the RawBlock tree without
/// running semantic conversion. Used by `parse` (above) and by the
/// diagnostic helpers below. Keeps the chumsky parser construction in
/// one place.
fn parse_raw_internal(source: &str) -> Result<RawBlock, String> {
    parser()
        .parse(source)
        .into_result()
        .map_err(|errs| {
            errs.iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        })
}

// ============================================================================
// Diagnostic helpers (consumed by `aislang validate -v / -vv / -vvv` on
// the failure path — when a converter rejects something, the partial
// RawBlock tree is what got parsed before the rejection)
// ============================================================================

/// Brief structural summary of the parsed RawBlock tree. Used at
/// `validate -v` on conversion failure to show what the grammar saw.
pub fn parse_raw_summary(source: &str) -> Result<String, String> {
    let raw = parse_raw_internal(source)?;
    let mut out = String::new();
    use std::fmt::Write;
    let _ = writeln!(out, "  top-level block_type: `{}`", raw.block_type);
    let _ = writeln!(out, "  labels:               {:?}", raw.labels);
    let (attrs, blocks) = count_body_items(&raw);
    let _ = writeln!(out, "  body items:           {} attribute(s), {} nested block(s)", attrs, blocks);
    let block_types = body_block_types(&raw);
    if !block_types.is_empty() {
        let _ = writeln!(out, "  nested block_types:   {:?}", block_types);
    }
    Ok(out.trim_end().to_string())
}

/// Debug (Rust pretty-printed) form of the parsed RawBlock tree.
/// Used at `validate -vv` on conversion failure.
pub fn parse_raw_debug(source: &str) -> Result<String, String> {
    let raw = parse_raw_internal(source)?;
    Ok(format!("{raw:#?}"))
}

/// JSON form of the parsed RawBlock tree. Used at `validate -vvv` on
/// conversion failure. The shape is the *internal* RawBlock — not the
/// typed Project — so the JSON differs from `validate -vvv` on success.
pub fn parse_raw_json(source: &str) -> Result<String, String> {
    let raw = parse_raw_internal(source)?;
    serde_json::to_string_pretty(&raw)
        .map_err(|e| format!("JSON serialization failed: {e}"))
}

fn count_body_items(block: &RawBlock) -> (usize, usize) {
    let mut attrs = 0usize;
    let mut blocks = 0usize;
    for item in &block.body {
        match item {
            RawBodyItem::Attribute(_) => attrs  += 1,
            RawBodyItem::Block(_)     => blocks += 1,
        }
    }
    (attrs, blocks)
}

fn body_block_types(block: &RawBlock) -> Vec<String> {
    block
        .body
        .iter()
        .filter_map(|item| match item {
            RawBodyItem::Block(b) => Some(b.block_type.clone()),
            _ => None,
        })
        .collect()
}

// ============================================================================
// Generic raw block tree (private to the parser module)
// ============================================================================

/// Variants are constructed by the parser even when no converter consumes
/// them yet (the budget converter only needs Money / EnumMember). The
/// remaining variants get their converters across weeks 2 Wed–Fri; until
/// then `dead_code` is silenced on the whole enum.
///
/// `Serialize` is derived so the CLI's `validate -vvv` (on failure) can
/// dump the partial RawBlock tree as JSON — the type stays internal
/// (`pub(crate)`-equivalent: only reachable via the public helpers
/// `parse_raw_summary` / `parse_raw_debug` / `parse_raw_json`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[allow(dead_code)]
enum RawValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Money(Money),
    Duration(Duration),
    Size(Size),
    Percentage(Percentage),
    EnumMember(String),
    /// Dot-path identifier reference, e.g. `model.fast` (2 segments) or
    /// `anthropic.claude_haiku_4_5` (catalog ref, 2 segments) or
    /// `shared.models.fast` (3 segments, v0.3+ module path). Always at
    /// least 2 segments (one dot) — bare identifiers go to `EnumMember`
    /// or `Bool`. The converter interprets the path based on field type
    /// (resource ref → `ast::Ref`; catalog ref → `ast::ModelId`; etc).
    AttributeAccess(Vec<String>),
    /// Comma-separated list literal, `[a, b, c]`. Elements may be any
    /// `RawValue`, including nested lists/objects.
    List(Vec<RawValue>),
    /// Comma-separated attribute set inside braces, `{ k = v, k = v }`.
    /// Distinct from a `block` body which uses whitespace separation.
    /// Used for typed-map fields like `deploy.expose = { port = 8080 }`.
    Object(Vec<RawAttribute>),
    /// Composite value formed by the `per` infix keyword, e.g.
    /// `$0.001 per 1k_tokens`. Boxed because `RawRate` contains
    /// two `RawValue` fields and Rust requires indirection for the
    /// recursive size to be finite.
    Rate(Box<RawRate>),
}

/// Numerator + denominator. Per spec EBNF §8: numerator is one of
/// Money/Int/Float; denominator is one of Size/Duration/unit_quantity
/// (where `1k_tokens` parses to Size with the unit-name dropped).
#[derive(Debug, Clone, PartialEq, Serialize)]
struct RawRate {
    numerator:   RawValue,
    denominator: RawValue,
}

#[derive(Debug, Clone, Serialize)]
struct RawBlock {
    block_type: String,
    labels:     Vec<String>,
    body:       Vec<RawBodyItem>,
}

#[derive(Debug, Clone, Serialize)]
enum RawBodyItem {
    Attribute(RawAttribute),
    Block(RawBlock),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct RawAttribute {
    name:  String,
    value: RawValue,
}

// ============================================================================
// Leaf parsers — one combinator per token shape, individually testable
// ============================================================================

/// Whitespace + line-comment skipper. Line comments start with `#` or `//`
/// (HCL convention, spec §2) and extend to but not including the newline.
fn padding<'src>(
) -> impl Parser<'src, &'src str, (), extra::Err<Rich<'src, char>>> + Clone {
    let hash_comment  = just('#').ignore_then(none_of('\n').repeated().ignored());
    let slash_comment = just("//").ignore_then(none_of('\n').repeated().ignored());
    choice((
        text::whitespace().at_least(1).ignored(),
        hash_comment,
        slash_comment,
    ))
    .repeated()
    .ignored()
}

fn ident_parser<'src>(
) -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> + Clone {
    text::ascii::ident().map(|s: &str| s.to_string())
}

fn string_lit_parser<'src>(
) -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> + Clone {
    just('"')
        .ignore_then(none_of("\"\\\n").repeated().to_slice())
        .then_ignore(just('"'))
        .map(|s: &str| s.to_string())
}

fn money_parser<'src>(
) -> impl Parser<'src, &'src str, Money, extra::Err<Rich<'src, char>>> + Clone {
    // All four currency symbols supported by Money::from_literal; €/£/¥
    // are multi-byte UTF-8 but `one_of` matches at the char level so the
    // distinction is transparent.
    one_of("$€£¥")
        .then(text::int(10))
        .then(just('.').then(text::digits(10).to_slice()).or_not())
        .to_slice()
        .try_map(|s: &str, span| {
            Money::from_literal(s).map_err(|e| Rich::custom(span, e.reason))
        })
}

/// Duration units. Two-character variants must be tried before their
/// single-character prefixes (`ms`/`us`/`ns` before `s`).
fn duration_parser<'src>(
) -> impl Parser<'src, &'src str, Duration, extra::Err<Rich<'src, char>>> + Clone {
    let unit = choice((
        just("ns"), just("us"), just("ms"),
        just("s"),  just("m"),  just("h"), just("d"),
    ));
    text::int(10)
        .then(unit)
        .to_slice()
        .try_map(|s: &str, span| {
            Duration::from_literal(s).map_err(|e| Rich::custom(span, e.reason))
        })
}

/// Size with required suffix. Bare digits without a suffix are parsed as
/// `Int` by the value parser and converted to `Size` only when the
/// receiving field expects one — see the converter layer.
fn size_parser<'src>(
) -> impl Parser<'src, &'src str, Size, extra::Err<Rich<'src, char>>> + Clone {
    // Two-char suffixes before single-char (KB before K, MB/GB are unambiguous).
    let unit = choice((
        just("KB"), just("MB"), just("GB"),
        just("k"),  just("K"),
    ));
    text::int(10)
        .then(unit)
        .to_slice()
        .try_map(|s: &str, span| {
            Size::from_literal(s).map_err(|e| Rich::custom(span, e.reason))
        })
}

fn percentage_parser<'src>(
) -> impl Parser<'src, &'src str, Percentage, extra::Err<Rich<'src, char>>> + Clone {
    text::int(10)
        .then(just('.').then(text::digits(10).to_slice()).or_not())
        .then(just('%'))
        .to_slice()
        .try_map(|s: &str, span| {
            Percentage::from_literal(s).map_err(|e| Rich::custom(span, e.reason))
        })
}

/// Float requires a `.` — bare digits are `Int`. This split lets the
/// converter receive an unambiguous numeric type at parse time.
fn float_parser<'src>(
) -> impl Parser<'src, &'src str, f64, extra::Err<Rich<'src, char>>> + Clone {
    text::int(10)
        .then(just('.'))
        .then(text::digits(10).to_slice())
        .to_slice()
        .try_map(|s: &str, span| {
            s.parse::<f64>()
                .map_err(|e| Rich::custom(span, format!("invalid float: {e}")))
        })
}

fn int_parser<'src>(
) -> impl Parser<'src, &'src str, i64, extra::Err<Rich<'src, char>>> + Clone {
    text::int(10).try_map(|s: &str, span| {
        s.parse::<i64>()
            .map_err(|e| Rich::custom(span, format!("integer out of range: {e}")))
    })
}

// ============================================================================
// Value parser — choice over all literal kinds
// ============================================================================

/// **Choice order matters** — typed-suffix literals must be tried before
/// their unsuffixed numeric prefixes:
///   `85%`   → percentage (not 85 then leftover `%`)
///   `5s`    → duration   (not 5 then leftover `s`)
///   `32k`   → size       (not 32 then leftover `k`)
///   `0.85`  → float      (not 0 then leftover `.85`)
///   `8080`  → int        (terminal)
/// Money (`$€£¥…`) is unambiguous from numerics.
///
/// Bare identifiers: `true`/`false` route to `Bool`; `model.fast`-style
/// dot-paths route to `AttributeAccess`; everything else routes to
/// `EnumMember`. The three cases are collapsed into one combinator
/// (`ident_or_access`) so we never need chumsky to backtrack after
/// consuming the leading identifier.
///
/// `list` and `object` make this parser recursive — a list element is a
/// value, an object attribute's RHS is a value.
fn value_parser<'src>(
) -> impl Parser<'src, &'src str, RawValue, extra::Err<Rich<'src, char>>> + Clone {
    recursive(|value| {
        // Define `pad` inside the recursive closure so the closure is
        // self-contained (no captures from outer scope). Capturing an
        // 'src-lifetime parser from outside makes type inference push the
        // recursive return type toward `'static`, which then fails to
        // unify with `'src`. Internal definition avoids the issue.
        let pad = padding();

        // Attribute inside an object literal. Same shape as the top-level
        // `attribute_parser`, but separated by `,` instead of whitespace.
        let attribute_in_object = ident_parser()
            .padded_by(pad.clone())
            .then_ignore(just('=').padded_by(pad.clone()))
            .then(value.clone())
            .map(|(name, value)| RawAttribute { name, value });

        // List: `[` (value `,`)* value? `]`. Trailing comma allowed.
        let list = value
            .clone()
            .separated_by(just(',').padded_by(pad.clone()))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(
                just('[').padded_by(pad.clone()),
                just(']').padded_by(pad.clone()),
            )
            .map(RawValue::List);

        // Object: `{` (attr `,`)* attr? `}`. Trailing comma allowed.
        let object = attribute_in_object
            .separated_by(just(',').padded_by(pad.clone()))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(
                just('{').padded_by(pad.clone()),
                just('}').padded_by(pad.clone()),
            )
            .map(RawValue::Object);

        // -------------------------------------------------------------
        // Rate (the `per` infix keyword) — Wednesday's novel production
        // -------------------------------------------------------------
        //
        // Implementation note on `just("per")` vs `text::ascii::keyword`:
        // `text::ascii::keyword` would enforce word-boundary (rejecting
        // `permission` as accidental match), but combining it with our
        // custom padding inside `recursive(...)` triggers a chumsky-0.10
        // type-inference bug that demands `'src: 'static` on the parser's
        // closure body. `just("per")` composes cleanly. The v0.1 user
        // impact is bounded: the only rate use site is `cost_max = $X
        // per Y` where `Y` always starts with a digit, so an unintended
        // `per`-prefix identifier would fail loudly at the denominator
        // parse (no digit start) rather than silently succeed.
        // Revisit in v0.2 once chumsky lifetimes settle or boxed-parser
        // alternatives are explored.

        // `unit_quantity`: `<digits><size_unit>_<identifier>` — the
        // `1k_tokens` form. Only legal in rate-denominator position.
        // Emits the Size part; the unit name is descriptive metadata
        // dropped per spec EBNF §8.
        let size_unit_for_quantity = choice((
            just("KB"), just("MB"), just("GB"),
            just("k"),  just("K"),
        ));
        let unit_quantity = text::int(10)
            .then(size_unit_for_quantity)
            .then(just('_'))
            .then(text::ascii::ident())
            .to_slice()
            .try_map(|s: &str, span| {
                // Captured slice is e.g. `1k_tokens`; Size part is the
                // prefix up to the underscore.
                let underscore_pos = s.find('_').expect("matched pattern has `_`");
                let size_text = &s[..underscore_pos];
                Size::from_literal(size_text)
                    .map_err(|e| Rich::custom(span, e.reason))
            });

        // Rate denominator: try unit_quantity first (most specific —
        // requires an underscore segment), then fall back to bare size /
        // duration.
        let rate_denominator = choice((
            unit_quantity.map(RawValue::Size),
            size_parser().map(RawValue::Size),
            duration_parser().map(RawValue::Duration),
        ));

        // `per` + denominator with padding around the keyword. See note
        // above re: `just("per")` vs `text::ascii::keyword`.
        let per_clause = just("per")
            .padded_by(pad.clone())
            .ignore_then(rate_denominator);

        // Money is wrapped with optional per-clause. Per the EBNF, Int
        // and Float can also be numerators, but no v0.1 spec field uses
        // them, so they're left as plain literals to keep the type-
        // inference graph tractable for rustc + chumsky-0.10.
        let money_or_rate = money_parser()
            .then(per_clause.or_not())
            .map(|(m, denom)| match denom {
                Some(d) => RawValue::Rate(Box::new(RawRate {
                    numerator: RawValue::Money(m),
                    denominator: d,
                })),
                None => RawValue::Money(m),
            });

        // Combined ident-or-access. Parse an ident, then zero-or-more
        // `.ident` segments. If the tail is empty, dispatch on the first
        // ident's text (true / false / other-enum). If the tail is
        // non-empty, build an AttributeAccess. Avoids chumsky backtracking.
        let ident_or_access = ident_parser()
            .then(
                just('.')
                    .ignore_then(ident_parser())
                    .repeated()
                    .collect::<Vec<String>>(),
            )
            .map(|(first, rest): (String, Vec<String>)| {
                if rest.is_empty() {
                    match first.as_str() {
                        "true"  => RawValue::Bool(true),
                        "false" => RawValue::Bool(false),
                        _       => RawValue::EnumMember(first),
                    }
                } else {
                    let mut path = Vec::with_capacity(1 + rest.len());
                    path.push(first);
                    path.extend(rest);
                    RawValue::AttributeAccess(path)
                }
            });

        choice((
            money_or_rate,
            percentage_parser().map(RawValue::Percentage),
            duration_parser().map(RawValue::Duration),
            size_parser().map(RawValue::Size),
            float_parser().map(RawValue::Float),
            int_parser().map(RawValue::Int),
            string_lit_parser().map(RawValue::String),
            list,
            object,
            ident_or_access,
        ))
        .padded_by(pad.clone())
        // .boxed() collapses the nested impl-Parser type into a concrete
        // Boxed<...> with explicit 'src lifetime. Without it, the
        // compiler can't reconcile the inferred bounds from all the
        // try_map / map closures and the recursive call site, producing
        // a spurious "must outlive 'static" error.
        .boxed()
    })
}

// ============================================================================
// Attribute parser — ident '=' value
// ============================================================================

fn attribute_parser<'src>(
) -> impl Parser<'src, &'src str, RawAttribute, extra::Err<Rich<'src, char>>> + Clone {
    let pad = padding();
    ident_parser()
        .padded_by(pad.clone())
        .then_ignore(just('=').padded_by(pad))
        .then(value_parser())
        .map(|(name, value)| RawAttribute { name, value })
}

// ============================================================================
// Block parser — recursive; the public root
// ============================================================================

fn parser<'src>(
) -> impl Parser<'src, &'src str, RawBlock, extra::Err<Rich<'src, char>>> {
    let pad = padding();

    let block = recursive(|block| {
        let body_item = choice((
            block.map(RawBodyItem::Block),
            attribute_parser().map(RawBodyItem::Attribute),
        ));

        ident_parser()
            .padded_by(pad.clone())
            .then(
                string_lit_parser()
                    .padded_by(pad.clone())
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .then_ignore(just('{').padded_by(pad.clone()))
            .then(body_item.repeated().collect::<Vec<_>>())
            .then_ignore(just('}').padded_by(pad.clone()))
            .map(|((block_type, labels), body)| RawBlock {
                block_type,
                labels,
                body,
            })
    });

    block.padded_by(pad).then_ignore(end())
}

// ============================================================================
// Generic-to-typed conversion (semantic-layer restrictions from spec §8.1)
// ============================================================================

fn to_project(raw: RawBlock) -> Result<Project, String> {
    if raw.block_type != "project" {
        return Err(format!(
            "top-level block must be `project`; got `{}`",
            raw.block_type
        ));
    }
    if raw.labels.is_empty() {
        return Err(
            "project block requires a name label, e.g. `project \"foo\" { … }`"
                .into(),
        );
    }
    if raw.labels.len() > 1 {
        return Err(format!(
            "project block takes exactly one label; got {}",
            raw.labels.len()
        ));
    }
    let name = raw.labels.into_iter().next().unwrap();

    let mut project = Project { name, ..Project::default() };

    for item in raw.body {
        let block = match item {
            RawBodyItem::Block(b) => b,
            RawBodyItem::Attribute(a) => {
                return Err(format!(
                    "attribute `{}` not allowed inside `project` body; \
                     only resource blocks (budget, model, prompt, …) are valid",
                    a.name
                ));
            }
        };
        match block.block_type.as_str() {
            "budget" => {
                if project.budget.is_some() {
                    return Err(
                        "`budget` block declared more than once; \
                         budget is a singleton (spec §4.2)".into(),
                    );
                }
                project.budget = Some(to_budget(block)?);
            }
            // Week-2: model | prompt | mcp_server | agent | eval | secret | deploy
            other => {
                return Err(format!(
                    "unknown block_type `{other}` inside project; \
                     v0.1 week-1 skeleton supports: budget. \
                     Other resource types arrive in week 2."
                ));
            }
        }
    }

    if project.budget.is_none() {
        return Err(
            "project missing required `budget` block (spec §4.2)".into(),
        );
    }
    Ok(project)
}

fn to_budget(raw: RawBlock) -> Result<Budget, String> {
    if !raw.labels.is_empty() {
        return Err("`budget` block does not take labels".into());
    }

    let mut hard_cap_per_day: Option<Money> = None;
    let mut hard_cap_per_run: Option<Money> = None;
    let mut optimize_for = OptimizeFor::Cost;

    for item in raw.body {
        let attr = match item {
            RawBodyItem::Attribute(a) => a,
            RawBodyItem::Block(b) => {
                return Err(format!(
                    "nested block `{}` not allowed inside `budget` body \
                     (spec §8.1: resource bodies are attribute-only)",
                    b.block_type
                ));
            }
        };
        match attr.name.as_str() {
            "hard_cap_per_day" => {
                hard_cap_per_day = Some(expect_money(&attr.name, attr.value)?);
            }
            "hard_cap_per_run" => {
                hard_cap_per_run = Some(expect_money(&attr.name, attr.value)?);
            }
            "optimize_for" => {
                optimize_for = expect_optimize_for(attr.value)?;
            }
            other => {
                return Err(format!(
                    "unknown attribute `{other}` in budget block; \
                     allowed: hard_cap_per_day, hard_cap_per_run, optimize_for"
                ));
            }
        }
    }

    Ok(Budget {
        hard_cap_per_day: hard_cap_per_day.ok_or_else(|| {
            "budget missing required attribute `hard_cap_per_day`".to_string()
        })?,
        hard_cap_per_run: hard_cap_per_run.ok_or_else(|| {
            "budget missing required attribute `hard_cap_per_run`".to_string()
        })?,
        optimize_for,
    })
}

fn expect_money(field: &str, v: RawValue) -> Result<Money, String> {
    match v {
        RawValue::Money(m) => Ok(m),
        other => Err(format!(
            "`{field}` expects a Money literal (e.g. `$50`); got {other:?}"
        )),
    }
}

fn expect_optimize_for(v: RawValue) -> Result<OptimizeFor, String> {
    match v {
        RawValue::EnumMember(e) => match e.as_str() {
            "cost"    => Ok(OptimizeFor::Cost),
            "latency" => Ok(OptimizeFor::Latency),
            "quality" => Ok(OptimizeFor::Quality),
            other => Err(format!(
                "`optimize_for` must be one of cost / latency / quality; got `{other}`"
            )),
        },
        other => Err(format!(
            "`optimize_for` expects a bare enum member (cost / latency / quality); got {other:?}"
        )),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod value_tests {
    //! Unit tests for the value parser. Operate directly on the leaf
    //! `value_parser()` so we can exercise every literal kind without
    //! needing a containing block.
    use super::*;

    fn parse_value(src: &str) -> Result<RawValue, String> {
        let pad = padding();
        value_parser()
            .padded_by(pad)
            .then_ignore(end())
            .parse(src)
            .into_result()
            .map_err(|errs| {
                errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n")
            })
    }

    #[test] fn int_literal()         { assert_eq!(parse_value("8080").unwrap(),  RawValue::Int(8080)); }
    #[test] fn int_zero()            { assert_eq!(parse_value("0").unwrap(),     RawValue::Int(0)); }
    #[test] fn float_literal()       { assert_eq!(parse_value("0.5").unwrap(),   RawValue::Float(0.5)); }
    #[test] fn float_pi()            { assert_eq!(parse_value("3.14").unwrap(),  RawValue::Float(3.14)); }
    #[test] fn bool_true()           { assert_eq!(parse_value("true").unwrap(),  RawValue::Bool(true)); }
    #[test] fn bool_false()          { assert_eq!(parse_value("false").unwrap(), RawValue::Bool(false)); }
    #[test] fn enum_member()         { assert_eq!(parse_value("cost").unwrap(),  RawValue::EnumMember("cost".into())); }

    #[test] fn money_int()           { assert_eq!(parse_value("$50").unwrap(),
        RawValue::Money(Money { currency: Currency::Usd, micro_units: 50_000_000 })); }
    #[test] fn money_decimal()       { assert_eq!(parse_value("$0.001").unwrap(),
        RawValue::Money(Money { currency: Currency::Usd, micro_units: 1_000 })); }

    #[test] fn duration_seconds()    { assert_eq!(parse_value("5s").unwrap(),
        RawValue::Duration(Duration { nanos: 5_000_000_000 })); }
    #[test] fn duration_ms()         { assert_eq!(parse_value("500ms").unwrap(),
        RawValue::Duration(Duration { nanos: 500_000_000 })); }
    #[test] fn duration_hours()      { assert_eq!(parse_value("2h").unwrap(),
        RawValue::Duration(Duration { nanos: 7_200_000_000_000 })); }

    #[test] fn size_k()              { assert_eq!(parse_value("32k").unwrap(),
        RawValue::Size(Size { units: 32 * 1024 })); }
    #[test] fn size_mb()             { assert_eq!(parse_value("1MB").unwrap(),
        RawValue::Size(Size { units: 1024 * 1024 })); }
    #[test] fn size_gb()             { assert_eq!(parse_value("8GB").unwrap(),
        RawValue::Size(Size { units: 8 * 1024 * 1024 * 1024 })); }

    #[test] fn percentage_int()      {
        match parse_value("85%").unwrap() {
            RawValue::Percentage(Percentage { ratio }) =>
                assert!((ratio - 0.85).abs() < 1e-12),
            other => panic!("expected Percentage, got {other:?}"),
        }
    }
    #[test] fn percentage_fractional() {
        match parse_value("0.5%").unwrap() {
            RawValue::Percentage(Percentage { ratio }) =>
                assert!((ratio - 0.005).abs() < 1e-12),
            other => panic!("expected Percentage, got {other:?}"),
        }
    }

    #[test] fn string_literal() {
        assert_eq!(parse_value(r#""hello""#).unwrap(), RawValue::String("hello".into()));
    }

    // Disambiguation tests — make sure the choice-order is right.
    #[test] fn digits_alone_are_int_not_size() {
        // `1024` could conceivably be Size{1024} but parser emits Int; the
        // converter promotes Int to Size when field type demands it.
        assert_eq!(parse_value("1024").unwrap(), RawValue::Int(1024));
    }
    #[test] fn digits_with_dot_are_float_not_int() {
        assert_eq!(parse_value("0.5").unwrap(), RawValue::Float(0.5));
    }
    #[test] fn digits_with_percent_are_percentage_not_int() {
        match parse_value("85%").unwrap() {
            RawValue::Percentage(_) => {}
            other => panic!("expected Percentage; got {other:?}"),
        }
    }
    #[test] fn digits_with_unit_are_duration_not_int() {
        match parse_value("5s").unwrap() {
            RawValue::Duration(_) => {}
            other => panic!("expected Duration; got {other:?}"),
        }
    }
    #[test] fn ms_not_split_to_m() {
        // Critical: the duration_unit choice must try "ms" before "m";
        // otherwise `500ms` parses as Duration(500m) then a leftover `s`.
        match parse_value("500ms").unwrap() {
            RawValue::Duration(Duration { nanos }) => assert_eq!(nanos, 500_000_000),
            other => panic!("expected Duration(500ms); got {other:?}"),
        }
    }
    #[test] fn kb_not_split_to_k() {
        // Same disambiguation for size: `4KB` must be Size(4*1024), not
        // Size(4*1024) with leftover `B`.
        assert_eq!(parse_value("4KB").unwrap(), RawValue::Size(Size { units: 4 * 1024 }));
    }

    // ------------------------------------------------------------------
    // Compound expressions — Tuesday additions
    // ------------------------------------------------------------------

    #[test] fn empty_list() {
        assert_eq!(parse_value("[]").unwrap(), RawValue::List(vec![]));
    }
    #[test] fn list_of_ints() {
        assert_eq!(parse_value("[1, 2, 3]").unwrap(),
            RawValue::List(vec![RawValue::Int(1), RawValue::Int(2), RawValue::Int(3)]));
    }
    #[test] fn list_trailing_comma_allowed() {
        assert_eq!(parse_value("[1, 2,]").unwrap(),
            RawValue::List(vec![RawValue::Int(1), RawValue::Int(2)]));
    }
    #[test] fn list_with_multiline_whitespace() {
        let src = "[\n  1,\n  2,\n  3\n]";
        assert_eq!(parse_value(src).unwrap(),
            RawValue::List(vec![RawValue::Int(1), RawValue::Int(2), RawValue::Int(3)]));
    }
    #[test] fn list_of_mixed_literals() {
        // Each element is a different RawValue variant — exercises the
        // choice across the whole expression alphabet.
        let parsed = parse_value(r#"[$1, 5s, 32k, true, "hello"]"#).unwrap();
        let RawValue::List(items) = parsed else { panic!("expected List, got {parsed:?}") };
        assert_eq!(items.len(), 5);
        assert!(matches!(items[0], RawValue::Money(_)));
        assert!(matches!(items[1], RawValue::Duration(_)));
        assert!(matches!(items[2], RawValue::Size(_)));
        assert!(matches!(items[3], RawValue::Bool(true)));
        assert!(matches!(items[4], RawValue::String(_)));
    }
    #[test] fn nested_list() {
        let parsed = parse_value("[[1, 2], [3]]").unwrap();
        let RawValue::List(outer) = parsed else { panic!("expected List") };
        assert_eq!(outer.len(), 2);
        match &outer[0] {
            RawValue::List(inner) => assert_eq!(inner.len(), 2),
            other => panic!("expected nested List, got {other:?}"),
        }
    }
    #[test] fn empty_object() {
        assert_eq!(parse_value("{}").unwrap(), RawValue::Object(vec![]));
    }
    #[test] fn single_attr_object() {
        let parsed = parse_value("{ port = 8080 }").unwrap();
        assert_eq!(parsed, RawValue::Object(vec![
            RawAttribute { name: "port".into(), value: RawValue::Int(8080) },
        ]));
    }
    #[test] fn multi_attr_object() {
        let parsed = parse_value(r#"{ port = 8080, host = "localhost" }"#).unwrap();
        let RawValue::Object(attrs) = parsed else { panic!("expected Object") };
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs[0].name, "port");
        assert_eq!(attrs[1].name, "host");
    }
    #[test] fn object_trailing_comma_allowed() {
        let parsed = parse_value("{ a = 1, b = 2, }").unwrap();
        let RawValue::Object(attrs) = parsed else { panic!("expected Object") };
        assert_eq!(attrs.len(), 2);
    }
    #[test] fn attribute_access_two_segments() {
        assert_eq!(parse_value("model.fast").unwrap(),
            RawValue::AttributeAccess(vec!["model".into(), "fast".into()]));
    }
    #[test] fn attribute_access_catalog_ref() {
        assert_eq!(parse_value("anthropic.claude_haiku_4_5").unwrap(),
            RawValue::AttributeAccess(vec!["anthropic".into(), "claude_haiku_4_5".into()]));
    }
    #[test] fn attribute_access_three_segments() {
        // Forward-looking: v0.3+ module path syntax like `shared.models.fast`.
        // Parser already accepts arbitrary depth (per spec EBNF §8); only
        // the converter restricts it for v0.1.
        assert_eq!(parse_value("shared.models.fast").unwrap(),
            RawValue::AttributeAccess(vec!["shared".into(), "models".into(), "fast".into()]));
    }
    #[test] fn bare_ident_is_enum_member_not_access() {
        // Single ident, no dots → EnumMember (not AttributeAccess of length 1).
        assert_eq!(parse_value("cost").unwrap(),
            RawValue::EnumMember("cost".into()));
    }
    #[test] fn list_of_attribute_accesses() {
        // The shape used by `model.prefer` / `model.fallback` in the demo:
        // a list of catalog refs.
        let parsed = parse_value("[anthropic.claude_haiku_4_5, openai.gpt_4o_mini]").unwrap();
        let RawValue::List(items) = parsed else { panic!("expected List") };
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], RawValue::AttributeAccess(p) if p.len() == 2));
        assert!(matches!(&items[1], RawValue::AttributeAccess(p) if p.len() == 2));
    }

    // ------------------------------------------------------------------
    // Rate + unit_quantity — Wednesday additions
    // ------------------------------------------------------------------

    #[test] fn rate_money_per_unit_quantity() {
        // The single rate use case in support-agent.ais.
        let parsed = parse_value("$0.001 per 1k_tokens").unwrap();
        let RawValue::Rate(r) = parsed else { panic!("expected Rate, got {parsed:?}") };
        assert_eq!(r.numerator,
            RawValue::Money(Money { currency: Currency::Usd, micro_units: 1_000 }));
        assert_eq!(r.denominator,
            RawValue::Size(Size { units: 1024 }));
    }

    #[test] fn rate_money_per_bare_size() {
        // Rate denominator can also be a bare size (no unit-name).
        let parsed = parse_value("$0.01 per 1k").unwrap();
        let RawValue::Rate(r) = parsed else { panic!("expected Rate") };
        assert_eq!(r.denominator, RawValue::Size(Size { units: 1024 }));
    }

    #[test] fn rate_money_per_duration() {
        let parsed = parse_value("$5 per 1h").unwrap();
        let RawValue::Rate(r) = parsed else { panic!("expected Rate") };
        assert_eq!(r.denominator,
            RawValue::Duration(Duration { nanos: 3_600_000_000_000 }));
    }

    // NOTE: `rate_int_per_duration` (Int-numerator rate) and the
    // matching Float-numerator case are in the EBNF (spec §8) but
    // intentionally NOT wired up in the v0.1 parser. No v0.1 field uses
    // them; enabling them in `value_parser` doubles the type-inference
    // load on rustc + chumsky 0.10 past what currently compiles cleanly.
    // The grammar reserves the syntactic room — they're additive in
    // v0.2 when chumsky lifetime ergonomics improve.

    #[test] fn money_alone_is_not_rate() {
        // Crucial: bare Money without `per` must still parse as Money,
        // not as a malformed Rate. The optional per-clause via or_not()
        // is what enables this.
        let parsed = parse_value("$50").unwrap();
        assert_eq!(parsed,
            RawValue::Money(Money { currency: Currency::Usd, micro_units: 50_000_000 }));
    }

    #[test] fn int_alone_is_not_rate() {
        // Same invariant for Int — bare `8080` stays Int, not Rate(8080, _).
        assert_eq!(parse_value("8080").unwrap(), RawValue::Int(8080));
    }

    #[test] fn per_prefix_in_ident_fails_loudly_not_silently() {
        // `permission` starts with `per` but is one identifier. In v0.1,
        // `just("per")` (used instead of `text::ascii::keyword("per")` —
        // see implementation note in parser.rs) doesn't enforce a word
        // boundary. The protection comes from the *denominator* parser:
        // after consuming `per`, we need a Size/Duration/unit_quantity
        // (all start with digits). `mission` doesn't, so the parse
        // fails. The test pins this LOUD failure — the contract is "no
        // silent consumption", not "no `per` prefix in idents".
        let pad = padding();
        let result: Result<RawValue, String> = {
            let src = "$50 permission";
            value_parser()
                .padded_by(pad)
                .then_ignore(end())
                .parse(src)
                .into_result()
                .map_err(|errs| {
                    errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n")
                })
        };
        assert!(result.is_err(),
            "expected error on `$50 permission`, got Ok({result:?})");
    }
}

#[cfg(test)]
mod snippet_tests {
    //! Round-trip every `examples/snippets/*.ais` fragment through the
    //! value parser. Fragments are not full projects — they're single
    //! value expressions, paired with this test so the parser-side
    //! invariant for each literal kind has a human-readable source-of-
    //! truth file (see `examples/snippets/README.md`).
    //!
    //! Tests also assert no snippet file is left without a corresponding
    //! test case, so the two sides stay in sync when literal kinds are
    //! added in future phases.

    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn snippets_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..").join("..")
            .join("examples").join("snippets")
    }

    fn parse_snippet(filename: &str) -> RawValue {
        let path = snippets_dir().join(filename);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        // Bind the parsed result to a local before returning. The chumsky
        // parser temporary borrows `source`; if the parse chain is the
        // function's tail expression, the temporary outlives `source` at
        // function return. Binding to `result` forces the temp to drop at
        // the end of the let-statement, while `source` is still alive.
        let result: Result<RawValue, String> = {
            let pad = padding();
            value_parser()
                .padded_by(pad)
                .then_ignore(end())
                .parse(&source)
                .into_result()
                .map_err(|errs| {
                    errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n")
                })
        };
        result.unwrap_or_else(|e| panic!("parse {} failed:\n{e}", path.display()))
    }

    // Snippet → expected RawValue. Adding a new snippet file without
    // adding a row here trips `every_snippet_has_a_test` below.
    fn snippet_cases() -> Vec<(&'static str, RawValue)> {
        vec![
            ("money-50-usd.ais",
                RawValue::Money(Money { currency: Currency::Usd, micro_units: 50_000_000 })),
            ("money-001-usd.ais",
                RawValue::Money(Money { currency: Currency::Usd, micro_units: 1_000 })),
            ("money-100-eur.ais",
                RawValue::Money(Money { currency: Currency::Eur, micro_units: 100_000_000 })),
            ("duration-2s.ais",
                RawValue::Duration(Duration { nanos: 2_000_000_000 })),
            ("duration-500ms.ais",
                RawValue::Duration(Duration { nanos: 500_000_000 })),
            ("size-32k.ais",
                RawValue::Size(Size { units: 32 * 1024 })),
            ("size-1mb.ais",
                RawValue::Size(Size { units: 1024 * 1024 })),
            ("percentage-85.ais",
                RawValue::Percentage(Percentage { ratio: 0.85 })),
            ("int-8080.ais",
                RawValue::Int(8080)),
            ("float-half.ais",
                RawValue::Float(0.5)),
            ("bool-true.ais",
                RawValue::Bool(true)),
            ("string-hello.ais",
                RawValue::String("hello".to_string())),
            ("enum-cost.ais",
                RawValue::EnumMember("cost".to_string())),
            // Tuesday additions — compound expressions
            ("list-empty.ais",
                RawValue::List(vec![])),
            ("list-of-ints.ais",
                RawValue::List(vec![
                    RawValue::Int(1), RawValue::Int(2), RawValue::Int(3),
                ])),
            ("list-of-refs.ais",
                RawValue::List(vec![
                    RawValue::AttributeAccess(vec!["anthropic".into(), "claude_haiku_4_5".into()]),
                    RawValue::AttributeAccess(vec!["openai".into(),    "gpt_4o_mini".into()]),
                ])),
            ("object-single.ais",
                RawValue::Object(vec![
                    RawAttribute { name: "port".into(), value: RawValue::Int(8080) },
                ])),
            ("object-multi.ais",
                RawValue::Object(vec![
                    RawAttribute { name: "port".into(), value: RawValue::Int(8080) },
                    RawAttribute { name: "host".into(), value: RawValue::String("localhost".into()) },
                ])),
            ("attr-access-two.ais",
                RawValue::AttributeAccess(vec!["model".into(), "fast".into()])),
            ("attr-access-three.ais",
                RawValue::AttributeAccess(vec!["shared".into(), "models".into(), "fast".into()])),
            // Wednesday additions — Rate + unit_quantity
            ("rate-money-per-tokens.ais",
                RawValue::Rate(Box::new(RawRate {
                    numerator:   RawValue::Money(Money { currency: Currency::Usd, micro_units: 1_000 }),
                    denominator: RawValue::Size(Size { units: 1024 }),
                }))),
            ("rate-money-per-duration.ais",
                RawValue::Rate(Box::new(RawRate {
                    numerator:   RawValue::Money(Money { currency: Currency::Usd, micro_units: 5_000_000 }),
                    denominator: RawValue::Duration(Duration { nanos: 3_600_000_000_000 }),
                }))),
        ]
    }

    #[test]
    fn every_snippet_parses_to_expected_value() {
        for (filename, expected) in snippet_cases() {
            let parsed = parse_snippet(filename);
            // Special-case Percentage / Float — f64 PartialEq is exact but
            // brittle; the literal forms here are bit-exact so equality
            // works, but if a future snippet uses imprecise decimals this
            // assertion is the right place to add epsilon tolerance.
            assert_eq!(parsed, expected,
                "snippet {filename}: parsed {parsed:?}, expected {expected:?}");
        }
    }

    #[test]
    fn every_snippet_has_a_test() {
        // Scan the directory and assert every `.ais` file is named in
        // snippet_cases(). Forces the two sides to stay in sync when a
        // new literal kind lands.
        let dir = snippets_dir();
        let mut on_disk: Vec<String> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                name.ends_with(".ais").then_some(name)
            })
            .collect();
        on_disk.sort();

        let mut in_test: Vec<String> = snippet_cases()
            .into_iter()
            .map(|(n, _)| n.to_string())
            .collect();
        in_test.sort();

        assert_eq!(on_disk, in_test,
            "snippet files on disk and snippet_cases() are out of sync — \
             every `.ais` file in examples/snippets/ must have a matching \
             entry in snippet_cases(), and vice versa");
    }
}

#[cfg(test)]
mod project_tests {
    //! Existing Project-level tests from Friday. Exercise the full parse
    //! pipeline against synthetic source strings.
    use super::*;

    fn assert_parse_ok(src: &str) -> Project {
        parse(src).unwrap_or_else(|e| panic!("expected ok, got error:\n{e}"))
    }
    fn assert_parse_err_contains(src: &str, needle: &str) {
        let err = parse(src).err().unwrap_or_else(|| panic!("expected error"));
        assert!(
            err.contains(needle),
            "error did not contain `{needle}`; full error:\n{err}"
        );
    }

    #[test]
    fn minimal_project_parses() {
        let src = r#"
            project "minimal" {
              budget {
                hard_cap_per_day = $10
                hard_cap_per_run = $0.10
              }
            }
        "#;
        let p = assert_parse_ok(src);
        assert_eq!(p.name, "minimal");
        let b = p.budget.expect("budget present");
        assert_eq!(b.hard_cap_per_day,
            Money { currency: Currency::Usd, micro_units: 10_000_000 });
        assert_eq!(b.hard_cap_per_run,
            Money { currency: Currency::Usd, micro_units: 100_000 });
        assert_eq!(b.optimize_for, OptimizeFor::Cost);
    }

    #[test]
    fn comments_and_blank_lines_skipped() {
        let src = r#"
            # leading hash comment
            // leading slash comment
            project "x" {  # inline hash

              // comment inside body
              budget {
                hard_cap_per_day = $1   // trailing slash
                hard_cap_per_run = $0.01  # trailing hash
              }
            }
            // trailing comment
        "#;
        let p = assert_parse_ok(src);
        assert_eq!(p.name, "x");
    }

    #[test]
    fn optimize_for_attribute_accepted() {
        let src = r#"
            project "x" {
              budget {
                hard_cap_per_day = $1
                hard_cap_per_run = $0.01
                optimize_for     = latency
              }
            }
        "#;
        let p = assert_parse_ok(src);
        assert_eq!(p.budget.unwrap().optimize_for, OptimizeFor::Latency);
    }

    #[test] fn empty_input_fails() {
        assert!(parse("").is_err());
    }

    #[test] fn non_project_top_block_fails() {
        let src = r#"budget { hard_cap_per_day = $1 hard_cap_per_run = $0 }"#;
        assert_parse_err_contains(src, "top-level block must be `project`");
    }

    #[test] fn project_without_label_fails() {
        let src = r#"project { budget { hard_cap_per_day = $1 hard_cap_per_run = $0 } }"#;
        assert_parse_err_contains(src, "name label");
    }

    #[test] fn budget_required() {
        let src = r#"project "x" { }"#;
        assert_parse_err_contains(src, "required `budget`");
    }

    #[test] fn missing_required_attribute_fails() {
        let src = r#"
            project "x" {
              budget {
                hard_cap_per_day = $10
              }
            }
        "#;
        assert_parse_err_contains(src, "hard_cap_per_run");
    }

    #[test] fn unknown_block_type_fails() {
        let src = r#"
            project "x" {
              foobar {
                k = $1
              }
            }
        "#;
        assert_parse_err_contains(src, "foobar");
    }

    #[test] fn duplicate_budget_fails() {
        let src = r#"
            project "x" {
              budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
              budget { hard_cap_per_day = $2  hard_cap_per_run = $0.02 }
            }
        "#;
        assert_parse_err_contains(src, "singleton");
    }

    #[test] fn invalid_optimize_for_value_fails() {
        let src = r#"
            project "x" {
              budget {
                hard_cap_per_day = $1
                hard_cap_per_run = $0.01
                optimize_for     = chaos
              }
            }
        "#;
        assert_parse_err_contains(src, "chaos");
    }

    #[test] fn money_string_in_money_field_fails() {
        let src = r#"
            project "x" {
              budget {
                hard_cap_per_day = "$10"
                hard_cap_per_run = $0.01
              }
            }
        "#;
        assert_parse_err_contains(src, "Money literal");
    }
}

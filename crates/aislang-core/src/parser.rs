//! Parser for the aiSlang v0 grammar (week 1 Friday skeleton).
//!
//! Implements just enough of LANGUAGE-SPEC-v0.md §8 to make
//! `examples/minimal.ais` parse end-to-end. The chumsky grammar
//! emits a **generic block tree** (private `RawBlock` / `RawBodyItem`
//! / `RawAttribute` / `RawValue`); the per-resource converters
//! (`to_project`, `to_budget`) translate that into the typed
//! [`crate::ast`] tree, performing the semantic-layer restrictions
//! documented in spec §8.1 (block_type whitelist, label arity,
//! attribute presence, value type, etc.).
//!
//! Week-2 expansion plan: extend the converter with the remaining
//! seven resource types (`model`, `prompt`, `mcp_server`, `agent`,
//! `eval`, `secret`, `deploy`) and the remaining literal/expression
//! productions (`list`, `object`, `attribute_access`, `rate`, the
//! other typed literals). The generic block tree shape stays
//! unchanged; only `RawValue` grows new variants.

use crate::ast::*;
use crate::types::*;
use chumsky::prelude::*;

// ============================================================================
// Public entry point
// ============================================================================

/// Parse a source string into a typed [`Project`] AST.
pub fn parse(source: &str) -> Result<Project, String> {
    let raw = parser()
        .parse(source)
        .into_result()
        .map_err(|errs| {
            errs.iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        })?;
    to_project(raw)
}

// ============================================================================
// Generic raw block tree (private to the parser module)
// ============================================================================

#[derive(Debug, Clone)]
struct RawBlock {
    block_type: String,
    labels:     Vec<String>,
    body:       Vec<RawBodyItem>,
}

#[derive(Debug, Clone)]
enum RawBodyItem {
    Attribute(RawAttribute),
    Block(RawBlock),
}

#[derive(Debug, Clone)]
struct RawAttribute {
    name:  String,
    value: RawValue,
}

#[derive(Debug, Clone)]
enum RawValue {
    Money(Money),
    /// String values are constructed by the parser today but not yet
    /// destructured by any converter (the budget block has no string-
    /// typed attributes). Will be read by week 2 — `prompt.file`,
    /// `mcp_server.image`, `secret.source`, etc. Silenced for now.
    #[allow(dead_code)]
    String(String),
    EnumMember(String),
    // Week-2 additions: Int, Float, Bool, Duration, Size, Percentage,
    // SemVer (from string), AttributeAccess, List, Object, Rate
}

// ============================================================================
// Chumsky grammar
// ============================================================================

fn parser<'src>(
) -> impl Parser<'src, &'src str, RawBlock, extra::Err<Rich<'src, char>>> {
    let pad = padding();

    // identifier: ASCII snake_case (lexer-level lowercase enforcement
    // deferred to the type-checker — see spec §8 token rules).
    let ident = text::ascii::ident()
        .map(|s: &str| s.to_string());

    // string literal: "..." with no escape sequence support in v0.1
    // (escapes deferred to week 2; minimal.ais has no escapes).
    let string_lit = just('"')
        .ignore_then(none_of("\"\\\n").repeated().to_slice())
        .then_ignore(just('"'))
        .map(|s: &str| s.to_string());

    // money literal: $50, $0.001 — capture the whole source slice and
    // delegate validation to Money::from_literal so the grammar shape
    // can be permissive while the diagnostic is precise.
    let money = just('$')
        .then(text::int(10))
        .then(just('.').then(text::digits(10).to_slice()).or_not())
        .to_slice()
        .try_map(|s: &str, span| {
            Money::from_literal(s)
                .map_err(|e| Rich::custom(span, e.reason))
        });

    // value: the v0.1-Friday subset of expression. Week 2 adds the
    // other literal kinds, attribute_access, list, object, rate.
    let value = choice((
        money.map(RawValue::Money),
        string_lit.clone().map(RawValue::String),
        ident.clone().map(RawValue::EnumMember),
    ))
    .padded_by(pad.clone());

    // attribute: ident '=' value
    let attribute = ident
        .clone()
        .padded_by(pad.clone())
        .then_ignore(just('=').padded_by(pad.clone()))
        .then(value)
        .map(|(name, value)| RawAttribute { name, value });

    // block: recursively contains blocks and attributes
    let block = recursive(|block| {
        let body_item = choice((
            // try block first — distinguishable by the `{` or quoted label
            // that follows the block_type; attribute is `ident '='`.
            block.map(RawBodyItem::Block),
            attribute.map(RawBodyItem::Attribute),
        ));

        ident
            .clone()
            .padded_by(pad.clone())
            .then(
                string_lit
                    .clone()
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

    // file: exactly one top-level block in v0.1 (semantically must be
    // `project`; checked in to_project).
    block.padded_by(pad).then_ignore(end())
}

/// Whitespace + line-comment skipper. Line comments start with either
/// `#` or `//` (HCL convention, spec §2) and extend to — but do not
/// include — the next newline.
fn padding<'src>(
) -> impl Parser<'src, &'src str, (), extra::Err<Rich<'src, char>>> + Clone {
    let hash_comment = just('#')
        .ignore_then(none_of('\n').repeated().ignored());
    let slash_comment = just("//")
        .ignore_then(none_of('\n').repeated().ignored());
    choice((
        text::whitespace().at_least(1).ignored(),
        hash_comment,
        slash_comment,
    ))
    .repeated()
    .ignored()
}

// ============================================================================
// Generic-to-typed conversion (carries all the spec §8.1 semantic checks)
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

    let mut project = Project {
        name,
        ..Project::default()
    };

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
                         budget is a singleton (spec §4.2)"
                            .into(),
                    );
                }
                project.budget = Some(to_budget(block)?);
            }
            // Week-2: model | prompt | mcp_server | agent | eval | secret | deploy
            other => {
                return Err(format!(
                    "unknown block_type `{other}` inside project; \
                     v0.1 Friday skeleton supports: budget. \
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
                hard_cap_per_day =
                    Some(expect_money(&attr.name, attr.value)?);
            }
            "hard_cap_per_run" => {
                hard_cap_per_run =
                    Some(expect_money(&attr.name, attr.value)?);
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
mod tests {
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

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

/// Variants are constructed by the parser even when no converter consumes
/// them yet (the budget converter only needs Money / EnumMember). The
/// remaining variants get their converters across weeks 2 Wed–Fri; until
/// then `dead_code` is silenced on the whole enum.
#[derive(Debug, Clone, PartialEq)]
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
    // Week-2 Tuesday additions: AttributeAccess(Vec<String>), List, Object
    // Week-2 Wednesday addition:  Rate(Box<RawRate>)
}

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
/// Money (`$…`) is unambiguous from numerics. `true`/`false` are
/// recognized as booleans inside the ident branch — the EBNF treats them
/// as reserved tokens, but chumsky-side we route them through ident and
/// dispatch on the matched name.
fn value_parser<'src>(
) -> impl Parser<'src, &'src str, RawValue, extra::Err<Rich<'src, char>>> + Clone {
    let pad = padding();
    let ident_value = ident_parser().map(|s: String| match s.as_str() {
        "true"  => RawValue::Bool(true),
        "false" => RawValue::Bool(false),
        _       => RawValue::EnumMember(s),
    });

    choice((
        money_parser().map(RawValue::Money),
        percentage_parser().map(RawValue::Percentage),
        duration_parser().map(RawValue::Duration),
        size_parser().map(RawValue::Size),
        float_parser().map(RawValue::Float),
        int_parser().map(RawValue::Int),
        string_lit_parser().map(RawValue::String),
        ident_value,
    ))
    .padded_by(pad)
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

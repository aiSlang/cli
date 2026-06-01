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
use std::path::PathBuf;

// ============================================================================
// Parse-error type — preserves chumsky span info for source-aware
// rendering (week-3 Thursday). Converter errors (to_project / to_budget
// / …) have no AST-side span info today, so they carry `span = None`;
// the renderer falls back to plain message printing for those.
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub span:    Option<std::ops::Range<usize>>,
}

impl ParseError {
    pub fn unspanned(message: impl Into<String>) -> Self {
        Self { message: message.into(), span: None }
    }
}

// ============================================================================
// Public entry point
// ============================================================================

/// Parse a source string into a typed [`Project`] AST. Returns a list
/// of errors — grammar-level errors carry span info; converter-level
/// errors don't (yet). Single-error responses are common but not
/// guaranteed; chumsky may produce multiple grammar errors per parse.
pub fn parse(source: &str) -> Result<Project, Vec<ParseError>> {
    let raw = parse_raw_internal(source)?;
    to_project(raw).map_err(|msg| vec![ParseError::unspanned(msg)])
}

/// Internal grammar-only parse — returns the RawBlock tree without
/// running semantic conversion. Used by `parse` (above) and by the
/// diagnostic helpers below.
fn parse_raw_internal(source: &str) -> Result<RawBlock, Vec<ParseError>> {
    parser().parse(source).into_result().map_err(|errs| {
        errs.iter()
            .map(|e| {
                let span = e.span();
                ParseError {
                    message: e.to_string(),
                    span:    Some(span.start..span.end),
                }
            })
            .collect()
    })
}

/// Flatten a Vec<ParseError> into a single string. Used by the
/// diagnostic helpers (parse_raw_summary/debug/json) which keep their
/// String-error signature for CLI callers that don't need spans.
fn flatten_errors(errs: Vec<ParseError>) -> String {
    errs.into_iter()
        .map(|e| e.message)
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// Diagnostic helpers (consumed by `aislang validate -v / -vv / -vvv` on
// the failure path — when a converter rejects something, the partial
// RawBlock tree is what got parsed before the rejection)
// ============================================================================

/// Brief structural summary of the parsed RawBlock tree. Used at
/// `validate -v` on conversion failure to show what the grammar saw.
pub fn parse_raw_summary(source: &str) -> Result<String, String> {
    let raw = parse_raw_internal(source).map_err(flatten_errors)?;
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
    let raw = parse_raw_internal(source).map_err(flatten_errors)?;
    Ok(format!("{raw:#?}"))
}

/// JSON form of the parsed RawBlock tree. Used at `validate -vvv` on
/// conversion failure. The shape is the *internal* RawBlock — not the
/// typed Project — so the JSON differs from `validate -vvv` on success.
pub fn parse_raw_json(source: &str) -> Result<String, String> {
    let raw = parse_raw_internal(source).map_err(flatten_errors)?;
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
    /// v0.3 Week 16 Tue: cross-module reference parsed from
    /// `use <module>.<label>` syntax (Decision 44.1).  The `use`
    /// keyword disambiguates this from the same-module 2-segment
    /// `<kind>.<label>` form (`AttributeAccess`).  Kind is inferred
    /// from the attribute context at converter time (e.g.
    /// `system_prompt = use coder.foo` → `RefKind::Prompt`).
    ModuleRef { module: String, label: String },
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
    // Two shapes:
    //   - `<symbol><digits>[.<digits>]` for finite amounts (`$50`, `€100`, …).
    //   - bare keyword `unlimited` for the no-cap variant (Decision 10).
    // We bias the currency-symbol path first because it's the common case;
    // the keyword path uses `text::keyword` so it doesn't match prefixes of
    // longer identifiers (e.g. `unlimited_xyz`).
    let capped = one_of("$€£¥")
        .then(text::int(10))
        .then(just('.').then(text::digits(10).to_slice()).or_not())
        .to_slice()
        .try_map(|s: &str, span| {
            Money::from_literal(s).map_err(|e| Rich::custom(span, e.reason))
        });
    let unlimited = text::keyword("unlimited").to(Money::Unlimited);
    choice((capped, unlimited))
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

        // v0.3 Week 16 Tue: `use <module>.<label>` — cross-module ref.
        //
        // Placed in the choice() BEFORE ident_or_access so the `use`
        // keyword is consumed as a hard keyword.  If the trailing
        // `<ident>.<ident>` shape doesn't match, choice() backtracks
        // and ident_or_access re-parses "use" as an EnumMember
        // (preserves the "use as a bare identifier" escape hatch —
        // no v0.2 .ais file uses `use` that way, but the fallthrough
        // keeps the change non-breaking for hypothetical authors).
        //
        // Implementation note re: `just("use")` vs `text::ascii::keyword`:
        // mirrors the `just("per")` rationale at line ~406 — keyword()
        // triggers a chumsky-0.10 type-inference bug inside recursive(),
        // so we use just() + rely on choice-arm backtracking to handle
        // the no-keyword case.  The required `.` between module and
        // label gives word-boundary discipline: `useful.foo` would
        // start matching this arm but the `.` enforces module + label
        // separation, and `usewell` (no dot) would fail at the dot
        // expectation — both fall through to ident_or_access cleanly.
        let use_ref = just("use")
            .ignore_then(pad.clone())
            .ignore_then(ident_parser())
            .then_ignore(just('.'))
            .then(ident_parser())
            .map(|(module, label)| RawValue::ModuleRef { module, label });

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
            // v0.3 Week 16 Tue: cross-module ref MUST come before
            // ident_or_access — `use` would otherwise be eaten as
            // an EnumMember.
            use_ref,
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
            "model"      => project.models.push(to_model(block)?),
            "prompt"     => project.prompts.push(to_prompt(block)?),
            "mcp_server" => project.mcp_servers.push(to_mcp_server(block)?),
            "agent"      => project.agents.push(to_agent(block)?),
            "eval"       => project.evals.push(to_eval(block)?),
            "secret"     => project.secrets.push(to_secret(block)?),
            "deploy"     => project.deploys.push(to_deploy(block)?),
            // v0.2 Fri (planning-doc Track A architectural decision #7):
            // `main` block declares the user-facing LLM as first-class
            // topology.  Multiple `main` blocks are allowed per project
            // (different chat_clients → different entry points).
            "main"       => project.mains.push(to_main(block)?),
            // v0.3 Week 17 Mon (Decision 44.5): `pipeline` composition
            // primitive.  Sequential agent composition with one Vec<Ref>
            // step list + optional input/output descriptors.  Tue extends
            // to `loop` + `branch`; Wed-Fri wires the runtime dispatch.
            "pipeline"   => project.pipelines.push(to_pipeline(block)?),
            other => {
                return Err(format!(
                    "unknown block_type `{other}` inside project; \
                     v0.3 supports: budget, model, prompt, mcp_server, \
                     agent, eval, secret, deploy, main, pipeline"
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
// v0.2 Fri: enum helpers for `main` block + `model.source`.
// Closed-set membership checks at parse time; the type-checker (week 3
// pipeline) doesn't need to re-validate.
// ============================================================================

fn expect_pattern(field: &str, v: RawValue) -> Result<Pattern, String> {
    match v {
        RawValue::EnumMember(e) => match e.as_str() {
            "orchestrator"     => Ok(Pattern::Orchestrator),
            "supervisor"       => Ok(Pattern::Supervisor),
            "router"           => Ok(Pattern::Router),
            "reflector"        => Ok(Pattern::Reflector),
            "planner_executor" => Ok(Pattern::PlannerExecutor),
            "coordinator"      => Ok(Pattern::Coordinator),
            other => Err(format!(
                "`{field}` must be one of orchestrator / supervisor / router / \
                 reflector / planner_executor / coordinator; got `{other}`"
            )),
        },
        other => Err(format!(
            "`{field}` expects a bare enum member; got {other:?}"
        )),
    }
}

fn expect_chat_client(field: &str, v: RawValue) -> Result<ChatClient, String> {
    match v {
        RawValue::EnumMember(e) => match e.as_str() {
            "claude_desktop" => Ok(ChatClient::ClaudeDesktop),
            "cursor"         => Ok(ChatClient::Cursor),
            "chatgpt"        => Ok(ChatClient::Chatgpt),
            "open_webui"     => Ok(ChatClient::OpenWebui),
            "custom"         => Ok(ChatClient::Custom),
            other => Err(format!(
                "`{field}` must be one of claude_desktop / cursor / chatgpt / \
                 open_webui / custom; got `{other}`"
            )),
        },
        other => Err(format!(
            "`{field}` expects a bare enum member; got {other:?}"
        )),
    }
}

fn expect_model_source(field: &str, v: RawValue) -> Result<ModelSource, String> {
    match v {
        RawValue::EnumMember(e) => match e.as_str() {
            "api"          => Ok(ModelSource::Api),
            "subscription" => Ok(ModelSource::Subscription),
            other => Err(format!(
                "`{field}` must be one of api / subscription; got `{other}`"
            )),
        },
        other => Err(format!(
            "`{field}` expects a bare enum member (api or subscription); got {other:?}"
        )),
    }
}

// ============================================================================
// Thursday converters: prompt, mcp_server, deploy, eval, secret, agent
// (the "leaf" resources — no Rate, no Vec<ModelId>, no constraints). The
// complex `to_model` converter is Friday's work.
// ============================================================================

fn to_prompt(raw: RawBlock) -> Result<Prompt, String> {
    let label = require_single_label("prompt", raw.labels)?;
    let mut file:    Option<PathBuf> = None;
    let mut version: Option<SemVer>  = None;

    for item in raw.body {
        let attr = expect_attr_only("prompt", item)?;
        match attr.name.as_str() {
            "file"    => file    = Some(expect_path(&attr.name, attr.value)?),
            "version" => version = Some(expect_semver(&attr.name, attr.value)?),
            other => return Err(format!(
                "unknown attribute `{other}` in prompt block; allowed: file, version"
            )),
        }
    }

    Ok(Prompt {
        label,
        file: file.ok_or_else(||
            "prompt missing required attribute `file`".to_string())?,
        version: version.ok_or_else(||
            "prompt missing required attribute `version`".to_string())?,
    })
}

fn to_mcp_server(raw: RawBlock) -> Result<McpServer, String> {
    let label = require_single_label("mcp_server", raw.labels)?;
    let mut image:   Option<OciImage>             = None;
    let mut mount:   Option<PathBuf>              = None;
    let mut env:     Option<Vec<(String, String)>> = None;
    let mut secrets: Option<Vec<Ref>>             = None;

    for item in raw.body {
        let attr = expect_attr_only("mcp_server", item)?;
        match attr.name.as_str() {
            "image"   => image   = Some(expect_oci_image(&attr.name, attr.value)?),
            "mount"   => mount   = Some(expect_path(&attr.name, attr.value)?),
            "env"     => env     = Some(expect_env_map(&attr.name, attr.value)?),
            "secrets" => secrets = Some(expect_list_of_refs(
                &attr.name, RefKind::Secret, attr.value)?),
            other => return Err(format!(
                "unknown attribute `{other}` in mcp_server block; \
                 allowed: image, mount, env, secrets"
            )),
        }
    }

    Ok(McpServer {
        label,
        image: image.ok_or_else(||
            "mcp_server missing required attribute `image`".to_string())?,
        mount,
        env,
        secrets,
    })
}

fn to_deploy(raw: RawBlock) -> Result<Deploy, String> {
    let label = require_single_label("deploy", raw.labels)?;
    let mut target: Option<DeployTarget> = None;
    let mut expose: Option<Expose>       = None;

    // Mixed-item dispatch: deploy bodies accept both Attributes
    // (`target = …`) AND a single nested `expose { … }` Block (v0.2
    // Tue grammar break — see the migration error path on `expose =`
    // below).  The rest of the resource bodies are still
    // attribute-only per spec §8.1; deploy is the first nested-block
    // exception, with `main` likely following in v0.2 Wed.
    for item in raw.body {
        match item {
            RawBodyItem::Attribute(attr) => match attr.name.as_str() {
                "target" => target = Some(expect_deploy_target(&attr.name, attr.value)?),
                "expose" => {
                    // v0.1 used `expose = { port = 8080 }` (attribute,
                    // Map<String, Int> value).  v0.2 Tue promotes
                    // this to a typed nested block so `web_ui` can
                    // join `port` without forcing a heterogeneous
                    // Map.  Emit a clear migration message instead
                    // of silently dropping or interpreting the old
                    // form (which would mask user intent).
                    return Err(
                        "deploy `expose` is a nested block in v0.2, not an attribute. \
                         Replace `expose = { port = N }` with `expose { port = N }` \
                         (and add `web_ui = true` inside if you want Open WebUI). \
                         Track A2 — see LANGUAGE-SPEC §4.9.".to_string()
                    );
                }
                other => return Err(format!(
                    "unknown attribute `{other}` in deploy block; \
                     allowed: target (attr), expose (nested block)"
                )),
            },
            RawBodyItem::Block(block) => match block.block_type.as_str() {
                "expose" => {
                    if expose.is_some() {
                        return Err(
                            "duplicate `expose { … }` block in deploy; at most one allowed"
                            .to_string()
                        );
                    }
                    expose = Some(to_expose(block)?);
                }
                other => return Err(format!(
                    "unknown nested block `{other}` in deploy body; \
                     allowed: expose (only)"
                )),
            },
        }
    }

    Ok(Deploy {
        label,
        target: target.ok_or_else(||
            "deploy missing required attribute `target`".to_string())?,
        expose,
    })
}

/// Parse the inner body of `expose { … }` — v0.2 Tue (Track A2) +
/// Thu (Track B Decision 43.6).
///
/// All three fields are optional; an empty `expose {}` is allowed
/// and emits the same `Expose::default()` (no web UI, no
/// informational port, no Ingress).  Unknown attribute names are
/// an error so typos surface immediately.
fn to_expose(raw: RawBlock) -> Result<Expose, String> {
    if !raw.labels.is_empty() {
        return Err(format!(
            "`expose` block takes no label; got {} label(s)", raw.labels.len()
        ));
    }
    let mut port:    Option<u16>    = None;
    let mut web_ui:  bool           = false;
    let mut ingress: Option<String> = None;

    for item in raw.body {
        let attr = expect_attr_only("expose", item)?;
        match attr.name.as_str() {
            "port"    => port    = Some(expect_port_u16(&attr.name, attr.value)?),
            "web_ui"  => web_ui  = expect_bool(&attr.name, attr.value)?,
            // Thu (Decision 43.6): hostname string for the optional
            // K8s Ingress that fronts the Open WebUI Service.  Empty
            // string is rejected at the typed-value level
            // (`expect_string` accepts any String, but a hostname-less
            // Ingress isn't useful — we'd rather catch the typo than
            // silently emit a broken Ingress).
            "ingress" => {
                let host = expect_string(&attr.name, attr.value)?;
                if host.trim().is_empty() {
                    return Err(format!(
                        "`ingress` host must be non-empty; got an empty string. \
                         Drop the attribute entirely if you don't need an \
                         Ingress (Decision 43.6 — Ingress is opt-in)."
                    ));
                }
                ingress = Some(host);
            }
            other => return Err(format!(
                "unknown attribute `{other}` in expose block; \
                 allowed: port, web_ui, ingress"
            )),
        }
    }

    Ok(Expose { port, web_ui, ingress })
}

/// Single-port extractor used by `to_expose`.  Reuses the same
/// 1..=65535 range check as the v0.1 `expect_port_map`.
fn expect_port_u16(field: &str, v: RawValue) -> Result<u16, String> {
    match v {
        RawValue::Int(i) if (1..=65535).contains(&i) => Ok(i as u16),
        RawValue::Int(i) => Err(format!(
            "`{field}` expects a TCP port in 1..=65535; got {i}"
        )),
        other => Err(format!(
            "`{field}` expects an integer port; got {other:?}"
        )),
    }
}

fn to_eval(raw: RawBlock) -> Result<Eval, String> {
    let label = require_single_label("eval", raw.labels)?;
    let mut dataset:    Option<PathBuf>        = None;
    let mut metric:     Option<EvalMetric>     = None;
    let mut threshold:  Option<Percentage>     = None;
    let mut aggregator: EvalAggregator         = EvalAggregator::Mean;   // default
    let mut agent:      Option<Ref>            = None;

    for item in raw.body {
        let attr = expect_attr_only("eval", item)?;
        match attr.name.as_str() {
            "dataset"    => dataset    = Some(expect_path(&attr.name, attr.value)?),
            "metric"     => metric     = Some(expect_eval_metric(&attr.name, attr.value)?),
            "threshold"  => threshold  = Some(expect_percentage(&attr.name, attr.value)?),
            "aggregator" => aggregator = expect_eval_aggregator(&attr.name, attr.value)?,
            "agent"      => agent      = Some(expect_ref(
                &attr.name, RefKind::Agent, attr.value)?),
            other => return Err(format!(
                "unknown attribute `{other}` in eval block; \
                 allowed: dataset, metric, threshold, aggregator, agent"
            )),
        }
    }

    Ok(Eval {
        label,
        dataset: dataset.ok_or_else(||
            "eval missing required attribute `dataset`".to_string())?,
        metric: metric.ok_or_else(||
            "eval missing required attribute `metric`".to_string())?,
        threshold: threshold.ok_or_else(||
            "eval missing required attribute `threshold`".to_string())?,
        aggregator,
        agent,
    })
}

fn to_secret(raw: RawBlock) -> Result<Secret, String> {
    let label = require_single_label("secret", raw.labels)?;
    let mut source: Option<String>   = None;
    let mut scope:  Option<Vec<Ref>> = None;

    for item in raw.body {
        let attr = expect_attr_only("secret", item)?;
        match attr.name.as_str() {
            "source" => source = Some(expect_string(&attr.name, attr.value)?),
            "scope"  => scope  = Some(expect_scope_list(&attr.name, attr.value)?),
            other => return Err(format!(
                "unknown attribute `{other}` in secret block; allowed: source, scope"
            )),
        }
    }

    let source = source.ok_or_else(||
        "secret missing required attribute `source`".to_string())?;

    // v0.1 (Decision 8): accept only `env://` and `file://` URI
    // schemes. Other schemes (`vault://`, `aws-sm://`, `gcp-secret://`,
    // …) are reserved for v0.2's `secret_provider` resource type —
    // see LANGUAGE-SPEC §5.7. Reject at parse time with a pointer
    // there so users know it's a v0.2 promise, not a typo.
    validate_secret_uri_scheme(&label, &source)?;

    Ok(Secret {
        label,
        source,
        scope:  scope.ok_or_else(||
            "secret missing required attribute `scope`".to_string())?,
    })
}

/// Reject any secret-source URI scheme that isn't `env://` or
/// `file://`. v0.1 (Decision 8) restricts the surface to these two
/// zero-dep schemes; enterprise vaults / cloud secret stores land
/// in v0.2 via the `secret_provider` resource type (LANGUAGE-SPEC §5.7).
fn validate_secret_uri_scheme(label: &str, source: &str) -> Result<(), String> {
    const SUPPORTED: &[&str] = &["env://", "file://"];
    if SUPPORTED.iter().any(|prefix| source.starts_with(prefix)) {
        return Ok(());
    }
    // Extract just the scheme part for the error message (everything
    // up to the first colon, then strip the `://` suffix).
    let scheme = source.split(':').next().unwrap_or(source);
    Err(format!(
        "secret `{label}`: source URI scheme `{scheme}://` is not supported in \
         v0.1. Supported v0.1 schemes: `env://VAR` (read from process env) and \
         `file://path` (read file contents). Enterprise secret stores \
         (vault, aws-sm, gcp-sm, azure-kv, k8s-secret) are reserved for v0.2 \
         via the `secret_provider` resource type — see LANGUAGE-SPEC §5.7."
    ))
}

fn to_agent(raw: RawBlock) -> Result<Agent, String> {
    let label = require_single_label("agent", raw.labels)?;
    let mut model:         Option<Ref>      = None;
    let mut system_prompt: Option<Ref>      = None;
    let mut tools:         Option<Vec<Ref>> = None;

    for item in raw.body {
        let attr = expect_attr_only("agent", item)?;
        match attr.name.as_str() {
            "model"         => model         = Some(expect_ref(
                &attr.name, RefKind::Model, attr.value)?),
            "system_prompt" => system_prompt = Some(expect_ref(
                &attr.name, RefKind::Prompt, attr.value)?),
            "tools"         => tools         = Some(expect_list_of_refs(
                &attr.name, RefKind::McpServer, attr.value)?),
            other => return Err(format!(
                "unknown attribute `{other}` in agent block; \
                 allowed: model, system_prompt, tools"
            )),
        }
    }

    Ok(Agent {
        label,
        model: model.ok_or_else(||
            "agent missing required attribute `model`".to_string())?,
        system_prompt: system_prompt.ok_or_else(||
            "agent missing required attribute `system_prompt`".to_string())?,
        // tools defaults to empty if absent — an agent with no tools is a
        // valid pure-chat agent.
        tools: tools.unwrap_or_default(),
    })
}

// ============================================================================
// Friday converter: model (the complex one — Rate, multi-list, residency,
// ModelId resolution against the catalog).
// ============================================================================

fn to_model(raw: RawBlock) -> Result<Model, String> {
    let label = require_single_label("model", raw.labels)?;
    let mut requires:    Option<Vec<Capability>>     = None;
    let mut context_min: Option<Size>                = None;
    let mut cost_max:    Option<Rate<Money, Size>>   = None;
    let mut latency_p95: Option<Duration>            = None;
    let mut residency:   Option<Vec<Residency>>      = None;
    let mut prefer:      Option<Vec<ModelId>>        = None;
    let mut fallback:    Option<Vec<ModelId>>        = None;
    let mut pin:         Option<ModelId>             = None;
    let mut source:      Option<ModelSource>         = None;  // v0.2 Fri

    for item in raw.body {
        let attr = expect_attr_only("model", item)?;
        match attr.name.as_str() {
            "requires"    => requires    = Some(expect_capability_list(&attr.name, attr.value)?),
            "context_min" => context_min = Some(expect_size(&attr.name, attr.value)?),
            "cost_max"    => cost_max    = Some(expect_rate_money_per_size(&attr.name, attr.value)?),
            "latency_p95" => latency_p95 = Some(expect_duration(&attr.name, attr.value)?),
            "residency"   => residency   = Some(expect_residency_list(&attr.name, attr.value)?),
            "prefer"      => prefer      = Some(expect_model_id_list(&attr.name, attr.value)?),
            "fallback"    => fallback    = Some(expect_model_id_list(&attr.name, attr.value)?),
            "pin"         => pin         = Some(expect_model_id(&attr.name, attr.value)?),
            // v0.2 Fri (Decision 42): optional `source = subscription | api`.
            // Absence → ModelSource::default() = Api (v0.1 behaviour, hard
            // budget enforcement through LiteLLM).  Subscription disables
            // emit-side LiteLLM config + budget hard-cap enforcement; the
            // tracker still records observed tokens.
            "source"      => source      = Some(expect_model_source(&attr.name, attr.value)?),
            other => return Err(format!(
                "unknown attribute `{other}` in model block; allowed: \
                 requires, context_min, cost_max, latency_p95, residency, \
                 prefer, fallback, pin, source"
            )),
        }
    }

    Ok(Model {
        label,
        requires: requires.ok_or_else(||
            "model missing required attribute `requires` (list of capabilities)".to_string())?,
        context_min: context_min.ok_or_else(||
            "model missing required attribute `context_min` (e.g. `32k`)".to_string())?,
        cost_max,
        latency_p95,
        residency,
        // prefer/fallback are Vec<ModelId> in the AST (not Option), so
        // absence means "empty list". The model resolver (week 4) is the
        // one that errors if BOTH are empty — at the parser layer we stay
        // permissive.
        prefer:   prefer.unwrap_or_default(),
        fallback: fallback.unwrap_or_default(),
        pin,
        source:   source.unwrap_or_default(),
    })
}

// ============================================================================
// v0.2 Fri: `main` block converter.
// ============================================================================
//
// Structurally identical to `to_agent` — labeled block with model +
// system_prompt refs.  Adds: `chat_client` enum + `pattern` enum.
// Does NOT have `tools` (main blocks don't dispatch deployed-agent
// tools directly; the host LLM client uses MCP tools instead).

fn to_main(raw: RawBlock) -> Result<Main, String> {
    let label = require_single_label("main", raw.labels)?;
    let mut model:         Option<Ref>          = None;
    let mut system_prompt: Option<Ref>          = None;
    let mut chat_client:   Option<ChatClient>   = None;
    let mut pattern:       Option<Pattern>      = None;

    for item in raw.body {
        let attr = expect_attr_only("main", item)?;
        match attr.name.as_str() {
            "model"         => model         = Some(expect_ref(
                &attr.name, RefKind::Model, attr.value)?),
            "system_prompt" => system_prompt = Some(expect_ref(
                &attr.name, RefKind::Prompt, attr.value)?),
            "chat_client"   => chat_client   = Some(expect_chat_client(
                &attr.name, attr.value)?),
            "pattern"       => pattern       = Some(expect_pattern(
                &attr.name, attr.value)?),
            other => return Err(format!(
                "unknown attribute `{other}` in main block; \
                 allowed: model, system_prompt, chat_client, pattern"
            )),
        }
    }

    Ok(Main {
        label,
        model: model.ok_or_else(||
            "main block missing required attribute `model`".to_string())?,
        system_prompt: system_prompt.ok_or_else(||
            "main block missing required attribute `system_prompt`".to_string())?,
        chat_client: chat_client.ok_or_else(||
            "main block missing required attribute `chat_client` \
             (one of: claude_desktop, cursor, chatgpt, open_webui, custom)".to_string())?,
        pattern: pattern.ok_or_else(||
            "main block missing required attribute `pattern` \
             (one of: orchestrator, supervisor, router, reflector, \
             planner_executor, coordinator)".to_string())?,
    })
}

// ============================================================================
// `pipeline` block converter — v0.3 Week 17 Mon (Decision 44.5)
// ============================================================================
//
// Sequential composition primitive.  steps[] is a Vec<Ref> with
// RefKind::Agent — same Ref shape as agent.tools but constrained
// to agents (not mcp_servers).  input + output are descriptive
// String metadata in v0.3; structured types are v0.5+.

fn to_pipeline(raw: RawBlock) -> Result<Pipeline, String> {
    let label = require_single_label("pipeline", raw.labels)?;
    let mut steps:  Option<Vec<Ref>> = None;
    let mut input:  Option<String>   = None;
    let mut output: Option<String>   = None;

    for item in raw.body {
        let attr = expect_attr_only("pipeline", item)?;
        match attr.name.as_str() {
            "steps"  => steps  = Some(expect_list_of_refs(
                &attr.name, RefKind::Agent, attr.value)?),
            "input"  => input  = Some(expect_string(&attr.name, attr.value)?),
            "output" => output = Some(expect_string(&attr.name, attr.value)?),
            other => return Err(format!(
                "unknown attribute `{other}` in pipeline block; \
                 allowed: steps, input, output"
            )),
        }
    }

    let steps = steps.ok_or_else(||
        "pipeline block missing required attribute `steps` \
         (a non-empty list of agent references, e.g. \
         `steps = [agent.architect, agent.coder]`)".to_string())?;
    if steps.is_empty() {
        return Err(
            "pipeline `steps` must contain at least one agent reference; \
             got an empty list".to_string());
    }

    Ok(Pipeline { label, steps, input, output })
}

// ============================================================================
// Model-specific value helpers
// ============================================================================

fn expect_size(field: &str, v: RawValue) -> Result<Size, String> {
    match v {
        RawValue::Size(s) => Ok(s),
        // Bare digits without a suffix are emitted as Int by the parser
        // (Monday's design); the converter promotes them to Size here.
        // `context_min = 1024` and `context_min = 1k` both yield Size{1024}.
        RawValue::Int(i) => {
            if i < 0 {
                return Err(format!(
                    "`{field}` Size value must be non-negative; got {i}"
                ));
            }
            Ok(Size { units: i as u64 })
        }
        other => Err(format!(
            "`{field}` expects a Size literal (e.g. `32k`, `1MB`) or bare \
             non-negative integer; got {other:?}"
        )),
    }
}

fn expect_duration(field: &str, v: RawValue) -> Result<Duration, String> {
    match v {
        RawValue::Duration(d) => Ok(d),
        other => Err(format!(
            "`{field}` expects a Duration literal (e.g. `2s`, `500ms`); got {other:?}"
        )),
    }
}

fn expect_rate_money_per_size(field: &str, v: RawValue)
    -> Result<Rate<Money, Size>, String>
{
    let rate = match v {
        RawValue::Rate(r) => r,
        other => return Err(format!(
            "`{field}` expects a Rate literal (e.g. `$0.001 per 1k_tokens`); got {other:?}"
        )),
    };
    let numerator = match rate.numerator {
        RawValue::Money(m) => m,
        other => return Err(format!(
            "`{field}` Rate numerator must be Money; got {other:?}"
        )),
    };
    let denominator = match rate.denominator {
        RawValue::Size(s) => s,
        other => return Err(format!(
            "`{field}` Rate denominator must be Size (e.g. `1k_tokens`, `1k`); got {other:?}"
        )),
    };
    Ok(Rate { numerator, denominator })
}

fn expect_capability_list(field: &str, v: RawValue)
    -> Result<Vec<Capability>, String>
{
    let items = match v {
        RawValue::List(items) => items,
        other => return Err(format!(
            "`{field}` expects a list of capability identifiers \
             (e.g. `[chat, tool_use]`); got {other:?}"
        )),
    };
    items.into_iter().enumerate().map(|(i, item)| {
        let s = match item {
            RawValue::EnumMember(s) => s,
            other => return Err(format!(
                "`{field}[{i}]` expects a bare capability identifier; got {other:?}"
            )),
        };
        match s.as_str() {
            "chat"              => Ok(Capability::Chat),
            "embedding"         => Ok(Capability::Embedding),
            "tool_use"          => Ok(Capability::ToolUse),
            "vision"            => Ok(Capability::Vision),
            "audio"             => Ok(Capability::Audio),
            "reasoning"         => Ok(Capability::Reasoning),
            "large_context"     => Ok(Capability::LargeContext),
            "structured_output" => Ok(Capability::StructuredOutput),
            other => Err(format!(
                "`{field}[{i}]` unknown capability `{other}`; allowed: \
                 chat, embedding, tool_use, vision, audio, reasoning, \
                 large_context, structured_output"
            )),
        }
    }).collect()
}

fn expect_residency_list(field: &str, v: RawValue)
    -> Result<Vec<Residency>, String>
{
    let items = match v {
        RawValue::List(items) => items,
        other => return Err(format!(
            "`{field}` expects a list of residency identifiers \
             (e.g. `[us, eu]`); got {other:?}"
        )),
    };
    items.into_iter().enumerate().map(|(i, item)| {
        let s = match item {
            RawValue::EnumMember(s) => s,
            other => return Err(format!(
                "`{field}[{i}]` expects a bare residency identifier; got {other:?}"
            )),
        };
        match s.as_str() {
            "us"   => Ok(Residency::Us),
            "eu"   => Ok(Residency::Eu),
            "uk"   => Ok(Residency::Uk),
            "apac" => Ok(Residency::Apac),
            other => Err(format!(
                "`{field}[{i}]` unknown residency `{other}`; allowed: us, eu, uk, apac"
            )),
        }
    }).collect()
}

/// Catalog model identifier — `provider.model_name`. Distinct from
/// `expect_ref`: ModelId's "provider" is an open string (any catalog
/// entry's provider), not a closed RefKind enum. Validation that the
/// ModelId actually exists in the catalog is the resolver's job (week 4),
/// not the parser's.
fn expect_model_id(field: &str, v: RawValue) -> Result<ModelId, String> {
    let path = match v {
        RawValue::AttributeAccess(p) => p,
        other => return Err(format!(
            "`{field}` expects a catalog model ID like \
             `provider.model_name`; got {other:?}"
        )),
    };
    if path.len() != 2 {
        return Err(format!(
            "`{field}` expects a 2-segment ID like `provider.model_name`; \
             got `{}` ({} segment(s))",
            path.join("."),
            path.len()
        ));
    }
    Ok(ModelId {
        provider: path[0].clone(),
        name:     path[1].clone(),
    })
}

fn expect_model_id_list(field: &str, v: RawValue) -> Result<Vec<ModelId>, String> {
    let items = match v {
        RawValue::List(items) => items,
        other => return Err(format!(
            "`{field}` expects a list of catalog model IDs \
             (e.g. `[anthropic.claude_haiku_4_5]`); got {other:?}"
        )),
    };
    items.into_iter().enumerate().map(|(i, item)| {
        let indexed = format!("{field}[{i}]");
        expect_model_id(&indexed, item)
    }).collect()
}

// ============================================================================
// Shared converter helpers
// ============================================================================

fn require_single_label(block_name: &str, labels: Vec<String>)
    -> Result<String, String>
{
    match labels.len() {
        0 => Err(format!(
            "`{block_name}` block requires a name label, \
             e.g. `{block_name} \"foo\" {{ … }}`"
        )),
        1 => Ok(labels.into_iter().next().unwrap()),
        n => Err(format!(
            "`{block_name}` block takes exactly one label; got {n}"
        )),
    }
}

fn expect_attr_only(block_name: &str, item: RawBodyItem)
    -> Result<RawAttribute, String>
{
    match item {
        RawBodyItem::Attribute(a) => Ok(a),
        RawBodyItem::Block(b) => Err(format!(
            "nested block `{}` not allowed inside `{block_name}` body \
             (spec §8.1: resource bodies are attribute-only)",
            b.block_type
        )),
    }
}

fn expect_string(field: &str, v: RawValue) -> Result<String, String> {
    match v {
        RawValue::String(s) => Ok(s),
        other => Err(format!(
            "`{field}` expects a quoted string; got {other:?}"
        )),
    }
}

/// Bare-bool extractor.  v0.2 Tue: first added for `expose.web_ui`,
/// promotes here so future bool attributes share one error message
/// shape.
fn expect_bool(field: &str, v: RawValue) -> Result<bool, String> {
    match v {
        RawValue::Bool(b) => Ok(b),
        other => Err(format!(
            "`{field}` expects a bare `true` or `false`; got {other:?}"
        )),
    }
}

fn expect_path(field: &str, v: RawValue) -> Result<PathBuf, String> {
    match v {
        RawValue::String(s) => Ok(PathBuf::from(s)),
        other => Err(format!(
            "`{field}` expects a Path (quoted string like `\"./file\"`); got {other:?}"
        )),
    }
}

fn expect_semver(field: &str, v: RawValue) -> Result<SemVer, String> {
    match v {
        RawValue::String(s) => SemVer::from_literal(&s)
            .map_err(|e| format!("`{field}` SemVer parse failed: {}", e.reason)),
        other => Err(format!(
            "`{field}` expects a SemVer string (quoted, e.g. `\"1.0.0\"`); got {other:?}"
        )),
    }
}

fn expect_oci_image(field: &str, v: RawValue) -> Result<OciImage, String> {
    match v {
        RawValue::String(s) => OciImage::from_literal(&s)
            .map_err(|e| format!("`{field}` OciImage parse failed: {}", e.reason)),
        other => Err(format!(
            "`{field}` expects an OciImage (quoted string like \
             `\"ghcr.io/owner/repo:tag\"`); got {other:?}"
        )),
    }
}

fn expect_percentage(field: &str, v: RawValue) -> Result<Percentage, String> {
    match v {
        RawValue::Percentage(p) => Ok(p),
        other => Err(format!(
            "`{field}` expects a Percentage literal (e.g. `85%`); got {other:?}"
        )),
    }
}

fn expect_eval_metric(field: &str, v: RawValue) -> Result<EvalMetric, String> {
    match v {
        RawValue::EnumMember(e) => match e.as_str() {
            "exact_match" => Ok(EvalMetric::ExactMatch),
            "f1"          => Ok(EvalMetric::F1),
            "bleu"        => Ok(EvalMetric::Bleu),
            "rouge_l"     => Ok(EvalMetric::RougeL),
            "llm_judge"   => Ok(EvalMetric::LlmJudge),
            other => Err(format!(
                "`{field}` unknown metric `{other}`; \
                 allowed: exact_match, f1, bleu, rouge_l, llm_judge"
            )),
        },
        other => Err(format!(
            "`{field}` expects a bare EvalMetric identifier; got {other:?}"
        )),
    }
}

fn expect_eval_aggregator(field: &str, v: RawValue) -> Result<EvalAggregator, String> {
    match v {
        RawValue::EnumMember(e) => match e.as_str() {
            "mean"           => Ok(EvalAggregator::Mean),
            "min"            => Ok(EvalAggregator::Min),
            "max"            => Ok(EvalAggregator::Max),
            "percentile_p95" => Ok(EvalAggregator::PercentileP95),
            other => Err(format!(
                "`{field}` unknown aggregator `{other}`; \
                 allowed: mean, min, max, percentile_p95"
            )),
        },
        other => Err(format!(
            "`{field}` expects a bare EvalAggregator identifier; got {other:?}"
        )),
    }
}

fn expect_deploy_target(field: &str, v: RawValue) -> Result<DeployTarget, String> {
    match v {
        RawValue::EnumMember(e) => match e.as_str() {
            // v0.1 baseline:
            "compose"    => Ok(DeployTarget::Compose),
            // v0.2 Phase 2 Week 14 Mon (Track B):
            "kubernetes" => Ok(DeployTarget::Kubernetes),
            other => Err(format!(
                "`{field}` must be `compose` or `kubernetes`; got `{other}` \
                 (ecs / nomad / databricks targets are v0.3+ — see LANGUAGE-\
                 SPEC §7.3)"
            )),
        },
        other => Err(format!(
            "`{field}` expects a bare DeployTarget identifier; got {other:?}"
        )),
    }
}

fn parse_ref_from_access(path: &[String]) -> Result<Ref, String> {
    if path.len() != 2 {
        return Err(format!(
            "expected a 2-segment reference like `kind.label`; got `{}` ({} segment(s))",
            path.join("."),
            path.len()
        ));
    }
    let kind = match path[0].as_str() {
        "model"      => RefKind::Model,
        "prompt"     => RefKind::Prompt,
        "mcp_server" => RefKind::McpServer,
        "agent"      => RefKind::Agent,
        "secret"     => RefKind::Secret,
        other => return Err(format!(
            "unknown resource kind `{other}` in reference `{}`; \
             expected one of: model, prompt, mcp_server, agent, secret",
            path.join(".")
        )),
    };
    // module: None — same-module ref (v0.2 baseline).  Cross-module
    // refs go through a separate parse path (the `use` keyword, see
    // `value_parser` for the RawValue::ModuleRef variant).
    Ok(Ref { kind, label: path[1].clone(), module: None })
}

fn ref_kind_str(k: RefKind) -> &'static str {
    match k {
        RefKind::Model     => "model",
        RefKind::Prompt    => "prompt",
        RefKind::McpServer => "mcp_server",
        RefKind::Agent     => "agent",
        RefKind::Secret    => "secret",
    }
}

fn expect_ref(field: &str, expected_kind: RefKind, v: RawValue) -> Result<Ref, String> {
    let prefix = ref_kind_str(expected_kind);
    match v {
        // v0.2 same-module form: `<kind>.<label>`.
        RawValue::AttributeAccess(path) => {
            let r = parse_ref_from_access(&path)
                .map_err(|e| format!("`{field}`: {e}"))?;
            if r.kind != expected_kind {
                return Err(format!(
                    "`{field}` expects a `{prefix}` reference; got a `{}` reference (`{}`)",
                    ref_kind_str(r.kind), path.join(".")
                ));
            }
            Ok(r)
        }
        // v0.3 Week 16 Tue cross-module form: `use <module>.<label>`.
        // Kind is inferred from the attribute context (`expected_kind`)
        // — the surface syntax intentionally omits it for brevity,
        // matching PLAN-v0_3.md Decision 44.1's `use coder.system_prompt`
        // example.  Cross-module ref resolution (does the module exist?
        // does it export the label?) lives in the checker's
        // `check_multi` entry point.
        RawValue::ModuleRef { module, label } => Ok(Ref {
            kind:   expected_kind,
            label,
            module: Some(module),
        }),
        other => Err(format!(
            "`{field}` expects a {prefix} reference (e.g. `{prefix}.label` \
             or `use <module>.<label>`); got {other:?}"
        )),
    }
}

fn expect_list_of_refs(field: &str, expected_kind: RefKind, v: RawValue)
    -> Result<Vec<Ref>, String>
{
    let items = match v {
        RawValue::List(items) => items,
        other => return Err(format!(
            "`{field}` expects a list of {} references; got {other:?}",
            ref_kind_str(expected_kind)
        )),
    };
    items.into_iter().enumerate().map(|(i, item)| {
        let indexed = format!("{field}[{i}]");
        expect_ref(&indexed, expected_kind, item)
    }).collect()
}

/// `secret.scope` allows refs to *either* `agent` or `mcp_server` — the two
/// runtime carriers that can read a secret. This is the only union-kind
/// list in v0.1; everything else uses `expect_list_of_refs` with a single
/// expected kind.
fn expect_scope_list(field: &str, v: RawValue) -> Result<Vec<Ref>, String> {
    let items = match v {
        RawValue::List(items) => items,
        other => return Err(format!(
            "`{field}` expects a list of `agent` or `mcp_server` references; got {other:?}"
        )),
    };
    items.into_iter().enumerate().map(|(i, item)| {
        let path = match item {
            RawValue::AttributeAccess(p) => p,
            other => return Err(format!(
                "`{field}[{i}]` expects a resource reference; got {other:?}"
            )),
        };
        let r = parse_ref_from_access(&path)
            .map_err(|e| format!("`{field}[{i}]`: {e}"))?;
        if !matches!(r.kind, RefKind::Agent | RefKind::McpServer) {
            return Err(format!(
                "`{field}[{i}]` must reference an `agent` or `mcp_server`; \
                 got a `{}` reference (`{}`)",
                ref_kind_str(r.kind), path.join(".")
            ));
        }
        Ok(r)
    }).collect()
}

fn expect_env_map(field: &str, v: RawValue) -> Result<Vec<(String, String)>, String> {
    let attrs = match v {
        RawValue::Object(a) => a,
        other => return Err(format!(
            "`{field}` expects an object literal like `{{ KEY = \"value\" }}`; got {other:?}"
        )),
    };
    attrs.into_iter().map(|attr| {
        let val = match attr.value {
            RawValue::String(s) => s,
            other => return Err(format!(
                "`{field}.{}` must be a quoted string; got {other:?}", attr.name
            )),
        };
        Ok((attr.name, val))
    }).collect()
}

// `expect_port_map` was removed in v0.2 Tue alongside the `expose`
// grammar promotion from attribute (`= { port = N }`) to nested
// block (`expose { port = N  web_ui = true }`).  Single-port extraction
// moved to `expect_port_u16` near `to_expose`.

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
        RawValue::Money(Money::Capped { currency: Currency::Usd, micro_units: 50_000_000 })); }
    #[test] fn money_decimal()       { assert_eq!(parse_value("$0.001").unwrap(),
        RawValue::Money(Money::Capped { currency: Currency::Usd, micro_units: 1_000 })); }
    #[test] fn money_unlimited()     { assert_eq!(parse_value("unlimited").unwrap(),
        RawValue::Money(Money::Unlimited)); }
    #[test] fn unlimited_keyword_does_not_match_prefix() {
        // `text::keyword` is word-boundary aware: `unlimited_x` must lex as
        // an enum-member identifier, not as `Money::Unlimited` followed by
        // trailing junk. Pins the boundary contract.
        let v = parse_value("unlimited_x").unwrap();
        assert_eq!(v, RawValue::EnumMember("unlimited_x".into()));
    }

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
            RawValue::Money(Money::Capped { currency: Currency::Usd, micro_units: 1_000 }));
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
            RawValue::Money(Money::Capped { currency: Currency::Usd, micro_units: 50_000_000 }));
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
                RawValue::Money(Money::Capped { currency: Currency::Usd, micro_units: 50_000_000 })),
            ("money-001-usd.ais",
                RawValue::Money(Money::Capped { currency: Currency::Usd, micro_units: 1_000 })),
            ("money-100-eur.ais",
                RawValue::Money(Money::Capped { currency: Currency::Eur, micro_units: 100_000_000 })),
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
                    numerator:   RawValue::Money(Money::Capped { currency: Currency::Usd, micro_units: 1_000 }),
                    denominator: RawValue::Size(Size { units: 1024 }),
                }))),
            ("rate-money-per-duration.ais",
                RawValue::Rate(Box::new(RawRate {
                    numerator:   RawValue::Money(Money::Capped { currency: Currency::Usd, micro_units: 5_000_000 }),
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
        parse(src).unwrap_or_else(|errs| {
            let joined = errs.iter().map(|e| e.message.as_str())
                .collect::<Vec<_>>().join("\n");
            panic!("expected ok, got errors:\n{joined}")
        })
    }
    fn assert_parse_err_contains(src: &str, needle: &str) {
        let errs = parse(src).err().unwrap_or_else(|| panic!("expected error"));
        let joined = errs.iter().map(|e| e.message.as_str())
            .collect::<Vec<_>>().join("\n");
        assert!(
            joined.contains(needle),
            "error did not contain `{needle}`; full error:\n{joined}"
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
            Money::Capped { currency: Currency::Usd, micro_units: 10_000_000 });
        assert_eq!(b.hard_cap_per_run,
            Money::Capped { currency: Currency::Usd, micro_units: 100_000 });
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

    // ------------------------------------------------------------------
    // Six new resource types — Thursday: end-to-end full-parse tests
    // ------------------------------------------------------------------

    fn project_with(body: &str) -> String {
        format!(
            r#"project "p" {{
                budget {{ hard_cap_per_day = $1   hard_cap_per_run = $0.01 }}
                {body}
            }}"#
        )
    }

    #[test] fn prompt_block_parses() {
        let src = project_with(r#"prompt "system" { file = "./p.md"  version = "1.0.0" }"#);
        let p = assert_parse_ok(&src);
        assert_eq!(p.prompts.len(), 1);
        assert_eq!(p.prompts[0].label, "system");
        assert_eq!(p.prompts[0].file.to_str().unwrap(), "./p.md");
    }

    #[test] fn prompt_missing_file_fails() {
        let src = project_with(r#"prompt "p" { version = "1.0.0" }"#);
        assert_parse_err_contains(&src, "missing required attribute `file`");
    }

    #[test] fn prompt_with_range_version_fails() {
        // Strict SemVer — ^/~/>= rejected by SemVer::from_literal
        let src = project_with(r#"prompt "p" { file = "./p.md"  version = "^1.0.0" }"#);
        assert_parse_err_contains(&src, "range operators");
    }

    #[test] fn mcp_server_block_parses() {
        let src = project_with(
            r#"mcp_server "docs" { image = "ghcr.io/x/y:1.0"  mount = "./k/" }"#);
        let p = assert_parse_ok(&src);
        assert_eq!(p.mcp_servers.len(), 1);
        let m = &p.mcp_servers[0];
        assert_eq!(m.label, "docs");
        assert_eq!(m.image.registry,   "ghcr.io");
        assert_eq!(m.image.repository, "x/y");
        assert_eq!(m.image.tag.as_deref(), Some("1.0"));
        assert_eq!(m.mount.as_ref().unwrap().to_str().unwrap(), "./k/");
    }

    #[test] fn mcp_server_unquoted_image_fails() {
        // OciImage MUST be quoted (spec §3.2). The bare `ghcr.io/x/y:1.0`
        // would not even parse as a single value at the grammar level.
        let src = project_with(
            r#"mcp_server "docs" { image = ghcr.io/x/y:1.0 }"#);
        assert!(parse(&src).is_err());
    }

    #[test] fn mcp_server_bad_image_fails_with_oci_diagnostic() {
        let src = project_with(
            r#"mcp_server "docs" { image = "nginx:latest" }"#);
        assert_parse_err_contains(&src, "OciImage");
    }

    #[test] fn deploy_block_parses() {
        // v0.2 Tue: `expose` is a nested block, not an attribute.
        let src = project_with(
            r#"deploy "local" { target = compose  expose { port = 8080 } }"#);
        let p = assert_parse_ok(&src);
        assert_eq!(p.deploys.len(), 1);
        let d = &p.deploys[0];
        assert_eq!(d.target, DeployTarget::Compose);
        let expose = d.expose.as_ref().unwrap();
        assert_eq!(expose.port, Some(8080));
        assert!(!expose.web_ui);
    }

    #[test] fn deploy_expose_with_web_ui() {
        // v0.2 Tue: full Open WebUI opt-in shape.
        let src = project_with(
            r#"deploy "local" { target = compose  expose { web_ui = true } }"#);
        let p = assert_parse_ok(&src);
        let expose = p.deploys[0].expose.as_ref().unwrap();
        assert_eq!(expose.port, None);
        assert!(expose.web_ui);
    }

    #[test] fn deploy_expose_port_and_web_ui_together() {
        let src = project_with(
            r#"deploy "local" {
                 target = compose
                 expose {
                   port   = 8080
                   web_ui = true
                 }
               }"#);
        let p = assert_parse_ok(&src);
        let expose = p.deploys[0].expose.as_ref().unwrap();
        assert_eq!(expose.port, Some(8080));
        assert!(expose.web_ui);
    }

    #[test] fn deploy_expose_empty_block_ok() {
        // `expose {}` is a no-op but valid — default Expose (no
        // port, no web_ui).  Mirrors the v0.1 forward-compat
        // posture (an `Expose::default()`-equivalent should round-
        // trip as `{}` rather than refuse to parse).
        let src = project_with(
            r#"deploy "local" { target = compose  expose {} }"#);
        let p = assert_parse_ok(&src);
        let expose = p.deploys[0].expose.as_ref().unwrap();
        assert_eq!(expose.port, None);
        assert!(!expose.web_ui);
        assert_eq!(expose.ingress, None);
    }

    #[test] fn deploy_port_out_of_range_fails() {
        let src = project_with(
            r#"deploy "local" { target = compose  expose { port = 99999 } }"#);
        assert_parse_err_contains(&src, "1..=65535");
    }

    #[test] fn deploy_expose_legacy_attribute_form_errors_with_migration_hint() {
        // v0.1 shape `expose = { port = 8080 }` MUST surface a
        // migration message, not silently parse as something else.
        let src = project_with(
            r#"deploy "local" { target = compose  expose = { port = 8080 } }"#);
        assert_parse_err_contains(&src, "nested block in v0.2");
    }

    #[test] fn deploy_expose_unknown_attr_fails() {
        let src = project_with(
            r#"deploy "local" { target = compose  expose { metrics = true } }"#);
        assert_parse_err_contains(&src, "metrics");
    }

    #[test] fn deploy_expose_duplicate_block_fails() {
        let src = project_with(
            r#"deploy "local" {
                 target = compose
                 expose { port   = 8080 }
                 expose { web_ui = true }
               }"#);
        assert_parse_err_contains(&src, "duplicate");
    }

    #[test] fn deploy_expose_web_ui_non_bool_fails() {
        let src = project_with(
            r#"deploy "local" { target = compose  expose { web_ui = "yes" } }"#);
        assert_parse_err_contains(&src, "`true` or `false`");
    }

    // ── expose.ingress (Phase 2 Week 14 Thu — Decision 43.6) ──

    #[test] fn deploy_expose_with_ingress_host_parses() {
        // Happy path: `ingress = "example.com"` lands as
        // `Some("example.com")` in the AST.
        let src = project_with(
            r#"deploy "prod" {
                 target = kubernetes
                 expose { web_ui = true  ingress = "support.example.com" }
               }"#);
        let p = assert_parse_ok(&src);
        let expose = p.deploys[0].expose.as_ref().unwrap();
        assert_eq!(expose.ingress.as_deref(), Some("support.example.com"));
        assert!(expose.web_ui);
    }

    #[test] fn deploy_expose_ingress_without_web_ui_parses_warns_at_emit() {
        // Grammar-level: ingress without web_ui IS parseable (no
        // cross-field validation at the parser layer).  The K8s
        // emitter ignores it with a warning (tracked in the emitter
        // tests).  Keeps the parser's job to "shape only" + reserves
        // semantic complaints to the emit-side.
        let src = project_with(
            r#"deploy "prod" {
                 target = kubernetes
                 expose { ingress = "ui.example.com" }
               }"#);
        let p = assert_parse_ok(&src);
        let expose = p.deploys[0].expose.as_ref().unwrap();
        assert_eq!(expose.ingress.as_deref(), Some("ui.example.com"));
        assert!(!expose.web_ui);
    }

    #[test] fn deploy_expose_empty_ingress_string_fails() {
        // Empty string is rejected — the user almost certainly meant
        // to drop the attribute rather than emit a broken Ingress
        // with an empty `spec.rules[0].host`.
        let src = project_with(
            r#"deploy "prod" {
                 target = kubernetes
                 expose { ingress = "" }
               }"#);
        assert_parse_err_contains(&src, "non-empty");
    }

    #[test] fn deploy_expose_ingress_non_string_fails() {
        let src = project_with(
            r#"deploy "prod" {
                 target = kubernetes
                 expose { ingress = true }
               }"#);
        assert_parse_err_contains(&src, "ingress");
    }

    #[test] fn deploy_unknown_target_fails() {
        // `k8s` is the SHORT form — not accepted; the spec says
        // `kubernetes`.  Surfaces an error pointing at the LANGUAGE-
        // SPEC §7.3 backlog for the rest (ecs / nomad / databricks).
        let src = project_with(r#"deploy "x" { target = k8s }"#);
        assert_parse_err_contains(&src, "k8s");
    }

    #[test] fn deploy_kubernetes_target_parses() {
        // Phase 2 Week 14 Mon (Track B) — `target = kubernetes` joins
        // `target = compose` as an accepted DeployTarget variant.
        // Emission lives in the new `aislang-k8s` crate (raw manifests
        // + Helm chart wrapper); this test only pins the grammar +
        // typed-AST round-trip.
        let src = project_with(r#"deploy "prod" { target = kubernetes }"#);
        let p   = assert_parse_ok(&src);
        assert_eq!(p.deploys.len(), 1);
        assert_eq!(p.deploys[0].label, "prod");
        assert_eq!(p.deploys[0].target, DeployTarget::Kubernetes);
    }

    #[test] fn eval_block_parses() {
        let src = project_with(
            r#"eval "acc" { dataset = "./q.jsonl"  metric = exact_match  threshold = 85% }"#);
        let p = assert_parse_ok(&src);
        assert_eq!(p.evals.len(), 1);
        let e = &p.evals[0];
        assert_eq!(e.label, "acc");
        assert_eq!(e.metric, EvalMetric::ExactMatch);
        assert_eq!(e.aggregator, EvalAggregator::Mean);   // default applied
        assert!((e.threshold.ratio - 0.85).abs() < 1e-12);
    }

    #[test] fn eval_with_explicit_aggregator() {
        let src = project_with(
            r#"eval "acc" { dataset = "./q.jsonl"  metric = f1  threshold = 90%  aggregator = percentile_p95 }"#);
        let p = assert_parse_ok(&src);
        assert_eq!(p.evals[0].aggregator, EvalAggregator::PercentileP95);
    }

    #[test] fn eval_threshold_must_be_percentage_not_float() {
        // `0.85` parses as Float, but eval.threshold requires Percentage.
        // Converter rejects with a useful error pointing at the type.
        let src = project_with(
            r#"eval "x" { dataset = "./q"  metric = exact_match  threshold = 0.85 }"#);
        assert_parse_err_contains(&src, "Percentage");
    }

    #[test] fn secret_block_parses() {
        let src = project_with(
            r#"secret "key" { source = "env://OPENAI_API_KEY"  scope = [agent.support] }"#);
        let p = assert_parse_ok(&src);
        assert_eq!(p.secrets.len(), 1);
        let s = &p.secrets[0];
        assert_eq!(s.source, "env://OPENAI_API_KEY");
        assert_eq!(s.scope.len(), 1);
        assert_eq!(s.scope[0].kind, RefKind::Agent);
        assert_eq!(s.scope[0].label, "support");
    }

    #[test] fn secret_scope_accepts_mcp_server_refs() {
        let src = project_with(
            r#"secret "k" { source = "env://X"  scope = [agent.a, mcp_server.b] }"#);
        let p = assert_parse_ok(&src);
        assert_eq!(p.secrets[0].scope[0].kind, RefKind::Agent);
        assert_eq!(p.secrets[0].scope[1].kind, RefKind::McpServer);
    }

    #[test] fn secret_scope_rejects_model_refs() {
        // model.X is a valid AttributeAccess at parse time, but secret.scope
        // only allows agent/mcp_server kinds — the converter rejects.
        let src = project_with(
            r#"secret "k" { source = "env://X"  scope = [model.fast] }"#);
        assert_parse_err_contains(&src, "agent` or `mcp_server");
    }

    // ---- URI scheme validation (week 7 Fri, Decision 8) ----

    #[test] fn secret_file_uri_accepted() {
        // `file://path` is the second v0.1 supported scheme — read
        // file contents as the secret value.
        let src = project_with(
            r#"secret "db" { source = "file://./.secrets/db.token"  scope = [mcp_server.docs] }"#);
        let p = assert_parse_ok(&src);
        assert_eq!(p.secrets[0].source, "file://./.secrets/db.token");
    }

    #[test] fn secret_vault_uri_rejected_with_spec_pointer() {
        // `vault://` is reserved for v0.2's `secret_provider` work —
        // see LANGUAGE-SPEC §5.7. v0.1 rejects at parse time so the
        // user gets a clean diagnostic pointing at the spec section.
        let src = project_with(
            r#"secret "k" { source = "vault://secret/foo"  scope = [agent.support] }"#);
        assert_parse_err_contains(&src, "LANGUAGE-SPEC §5.7");
        assert_parse_err_contains(&src, "vault://");
    }

    #[test] fn secret_aws_sm_uri_rejected() {
        // Every non-env / non-file scheme rejects the same way.
        let src = project_with(
            r#"secret "k" { source = "aws-sm://arn:..."  scope = [agent.support] }"#);
        assert_parse_err_contains(&src, "is not supported in v0.1");
        assert_parse_err_contains(&src, "secret_provider");
    }

    #[test] fn secret_bare_value_rejected() {
        // `source = "MY_KEY_VALUE_HERE"` (no scheme prefix) is also
        // wrong. v0.1 requires a URI form so the user is explicit
        // about WHERE the secret comes from. Pin the rejection.
        let src = project_with(
            r#"secret "k" { source = "MY_KEY_VALUE"  scope = [agent.support] }"#);
        assert_parse_err_contains(&src, "not supported in v0.1");
    }

    #[test] fn agent_block_parses() {
        let src = project_with(
            r#"agent "support" { model = model.fast  system_prompt = prompt.system  tools = [mcp_server.docs] }"#);
        let p = assert_parse_ok(&src);
        assert_eq!(p.agents.len(), 1);
        let a = &p.agents[0];
        assert_eq!(a.label, "support");
        assert_eq!(a.model.kind, RefKind::Model);
        assert_eq!(a.model.label, "fast");
        assert_eq!(a.system_prompt.kind, RefKind::Prompt);
        assert_eq!(a.tools.len(), 1);
        assert_eq!(a.tools[0].kind, RefKind::McpServer);
    }

    #[test] fn agent_with_no_tools_defaults_to_empty() {
        let src = project_with(
            r#"agent "support" { model = model.fast  system_prompt = prompt.system }"#);
        let p = assert_parse_ok(&src);
        assert_eq!(p.agents[0].tools.len(), 0);
    }

    #[test] fn agent_wrong_ref_kind_fails() {
        // `agent.model` expects a model.X reference; giving prompt.X is wrong
        let src = project_with(
            r#"agent "a" { model = prompt.system  system_prompt = prompt.system }"#);
        assert_parse_err_contains(&src, "expects a `model` reference");
    }

    #[test] fn agent_tools_must_be_mcp_server_refs() {
        let src = project_with(
            r#"agent "a" { model = model.fast  system_prompt = prompt.system  tools = [agent.other] }"#);
        assert_parse_err_contains(&src, "expects a `mcp_server` reference");
    }

    #[test] fn unknown_attribute_in_resource_block_fails() {
        let src = project_with(r#"prompt "p" { file = "./x"  version = "1.0.0"  flavor = vanilla }"#);
        assert_parse_err_contains(&src, "unknown attribute `flavor`");
    }

    #[test] fn nested_block_in_resource_body_rejected() {
        // spec §8.1: resource bodies are attribute-only in v0.1
        let src = project_with(r#"prompt "p" { file = "./x"  version = "1.0.0"  budget { } }"#);
        assert_parse_err_contains(&src, "nested block");
    }

    // ------------------------------------------------------------------
    // to_model — Friday: the complex converter
    // ------------------------------------------------------------------

    #[test] fn model_block_minimal_parses() {
        let src = project_with(
            r#"model "fast" {
                 requires    = [chat]
                 context_min = 32k
            }"#);
        let p = assert_parse_ok(&src);
        assert_eq!(p.models.len(), 1);
        let m = &p.models[0];
        assert_eq!(m.label, "fast");
        assert_eq!(m.requires, vec![Capability::Chat]);
        assert_eq!(m.context_min, Size { units: 32 * 1024 });
        assert_eq!(m.cost_max,    None);
        assert_eq!(m.latency_p95, None);
        assert_eq!(m.residency,   None);
        assert_eq!(m.prefer.len(),   0);
        assert_eq!(m.fallback.len(), 0);
        assert_eq!(m.pin, None);
    }

    #[test] fn model_block_full_parses() {
        // Mirror the support-agent.ais model block — every field exercised.
        let src = project_with(
            r#"model "fast" {
                 requires    = [chat, tool_use]
                 context_min = 32k
                 cost_max    = $0.001 per 1k_tokens
                 latency_p95 = 2s
                 residency   = [eu, us]
                 prefer      = [anthropic.claude_haiku_4_5, openai.gpt_4o_mini]
                 fallback    = [ollama.llama_3_1_8b]
            }"#);
        let p = assert_parse_ok(&src);
        let m = &p.models[0];
        assert_eq!(m.requires, vec![Capability::Chat, Capability::ToolUse]);
        assert_eq!(m.context_min, Size { units: 32 * 1024 });
        let cost = m.cost_max.as_ref().expect("cost_max set");
        assert_eq!(cost.numerator,
            Money::Capped { currency: Currency::Usd, micro_units: 1_000 });
        assert_eq!(cost.denominator, Size { units: 1024 });
        assert_eq!(m.latency_p95, Some(Duration { nanos: 2_000_000_000 }));
        assert_eq!(m.residency, Some(vec![Residency::Eu, Residency::Us]));
        assert_eq!(m.prefer, vec![
            ModelId { provider: "anthropic".into(), name: "claude_haiku_4_5".into() },
            ModelId { provider: "openai".into(),    name: "gpt_4o_mini".into() },
        ]);
        assert_eq!(m.fallback, vec![
            ModelId { provider: "ollama".into(), name: "llama_3_1_8b".into() },
        ]);
    }

    #[test] fn model_with_pin_parses() {
        let src = project_with(
            r#"model "compliance" {
                 requires    = [chat]
                 context_min = 200k
                 pin         = anthropic.claude_sonnet_4_6
            }"#);
        let p = assert_parse_ok(&src);
        assert_eq!(p.models[0].pin,
            Some(ModelId { provider: "anthropic".into(), name: "claude_sonnet_4_6".into() }));
    }

    #[test] fn model_context_min_accepts_bare_int() {
        // Spec §3.2: bare digits are valid Size literals (interpreted as
        // base units). Parser emits Int; converter promotes to Size.
        let src = project_with(
            r#"model "fast" { requires = [chat]  context_min = 1024 }"#);
        let p = assert_parse_ok(&src);
        assert_eq!(p.models[0].context_min, Size { units: 1024 });
    }

    #[test] fn model_missing_requires_fails() {
        let src = project_with(r#"model "fast" { context_min = 32k }"#);
        assert_parse_err_contains(&src, "required attribute `requires`");
    }

    #[test] fn model_missing_context_min_fails() {
        let src = project_with(r#"model "fast" { requires = [chat] }"#);
        assert_parse_err_contains(&src, "required attribute `context_min`");
    }

    #[test] fn model_unknown_capability_fails() {
        let src = project_with(
            r#"model "fast" { requires = [chat, telepathy]  context_min = 32k }"#);
        assert_parse_err_contains(&src, "unknown capability `telepathy`");
    }

    #[test] fn model_unknown_residency_fails() {
        let src = project_with(
            r#"model "f" { requires = [chat]  context_min = 32k  residency = [mars] }"#);
        assert_parse_err_contains(&src, "unknown residency `mars`");
    }

    #[test] fn model_cost_max_must_be_rate_not_bare_money() {
        // Bare Money is valid as a value but not for cost_max which
        // expects a Rate.
        let src = project_with(
            r#"model "f" { requires = [chat]  context_min = 32k  cost_max = $0.001 }"#);
        assert_parse_err_contains(&src, "Rate");
    }

    #[test] fn model_prefer_must_be_two_segment_attr_access() {
        // `prefer = [anthropic]` — bare identifier, no provider.name shape
        let src = project_with(
            r#"model "f" { requires = [chat]  context_min = 32k  prefer = [anthropic] }"#);
        assert_parse_err_contains(&src, "catalog model ID");
    }

    // ------------------------------------------------------------------
    // Original Friday tests follow
    // ------------------------------------------------------------------

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

    // ────────────────────────────────────────────────────────────────
    // v0.2 Week-12 Fri: `main` block + `model.source` grammar
    // additions (planning-doc Track A architectural decision #7).
    // ────────────────────────────────────────────────────────────────

    /// Shorthand for building a project that already has the
    /// prereq `model "primary"` + `prompt "main_brief"` blocks
    /// that a `main` block references.  Saves repeating ~10 lines
    /// per test.
    fn project_with_main_prereqs(main_body: &str) -> String {
        format!(r#"
            project "demo" {{
              budget {{
                hard_cap_per_day = $1
                hard_cap_per_run = $0.01
              }}
              model "primary" {{
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_sonnet_4_6]
              }}
              prompt "main_brief" {{
                file    = "./main_brief.md"
                version = "1.0.0"
              }}
              main "user_facing" {{
                {main_body}
              }}
            }}
        "#)
    }

    // ── `main` block: parse happy paths ──────────────────────────

    #[test] fn main_block_parses_with_orchestrator_pattern() {
        let src = project_with_main_prereqs(r#"
            model         = model.primary
            chat_client   = claude_desktop
            pattern       = orchestrator
            system_prompt = prompt.main_brief
        "#);
        let project = assert_parse_ok(&src);
        assert_eq!(project.mains.len(), 1);
        let m = &project.mains[0];
        assert_eq!(m.label, "user_facing");
        assert_eq!(m.pattern, Pattern::Orchestrator);
        assert_eq!(m.chat_client, ChatClient::ClaudeDesktop);
    }

    #[test] fn main_block_accepts_all_six_patterns() {
        for (name, expected) in [
            ("orchestrator",     Pattern::Orchestrator),
            ("supervisor",       Pattern::Supervisor),
            ("router",           Pattern::Router),
            ("reflector",        Pattern::Reflector),
            ("planner_executor", Pattern::PlannerExecutor),
            ("coordinator",      Pattern::Coordinator),
        ] {
            let src = project_with_main_prereqs(&format!(r#"
                model         = model.primary
                chat_client   = claude_desktop
                pattern       = {name}
                system_prompt = prompt.main_brief
            "#));
            let project = assert_parse_ok(&src);
            assert_eq!(project.mains[0].pattern, expected,
                "pattern `{name}` should parse to {expected:?}");
        }
    }

    #[test] fn main_block_accepts_all_five_chat_clients() {
        for (name, expected) in [
            ("claude_desktop", ChatClient::ClaudeDesktop),
            ("cursor",         ChatClient::Cursor),
            ("chatgpt",        ChatClient::Chatgpt),
            ("open_webui",     ChatClient::OpenWebui),
            ("custom",         ChatClient::Custom),
        ] {
            let src = project_with_main_prereqs(&format!(r#"
                model         = model.primary
                chat_client   = {name}
                pattern       = orchestrator
                system_prompt = prompt.main_brief
            "#));
            let project = assert_parse_ok(&src);
            assert_eq!(project.mains[0].chat_client, expected);
        }
    }

    #[test] fn multiple_main_blocks_per_project_allowed() {
        // Customer-facing + admin entry, both legal.  Validated by
        // ref resolution (model + system_prompt exist) but otherwise
        // the parser doesn't constrain main-block count.
        let src = r#"
            project "two-mains" {
              budget {
                hard_cap_per_day = $1
                hard_cap_per_run = $0.01
              }
              model "primary" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_sonnet_4_6]
              }
              prompt "user_brief"  { file = "./u.md" version = "1.0.0" }
              prompt "admin_brief" { file = "./a.md" version = "1.0.0" }
              main "user" {
                model         = model.primary
                chat_client   = claude_desktop
                pattern       = orchestrator
                system_prompt = prompt.user_brief
              }
              main "admin" {
                model         = model.primary
                chat_client   = cursor
                pattern       = supervisor
                system_prompt = prompt.admin_brief
              }
            }
        "#;
        let project = assert_parse_ok(src);
        assert_eq!(project.mains.len(), 2);
        assert_eq!(project.mains[0].label, "user");
        assert_eq!(project.mains[1].label, "admin");
    }

    // ── `main` block: error paths ────────────────────────────────

    #[test] fn main_block_unknown_pattern_value_errors() {
        let src = project_with_main_prereqs(r#"
            model         = model.primary
            chat_client   = claude_desktop
            pattern       = unknown_pattern_value
            system_prompt = prompt.main_brief
        "#);
        assert_parse_err_contains(&src,
            "`pattern` must be one of orchestrator");
    }

    #[test] fn main_block_unknown_chat_client_errors() {
        let src = project_with_main_prereqs(r#"
            model         = model.primary
            chat_client   = some_unknown_client
            pattern       = orchestrator
            system_prompt = prompt.main_brief
        "#);
        assert_parse_err_contains(&src,
            "`chat_client` must be one of claude_desktop");
    }

    #[test] fn main_block_missing_model_errors() {
        let src = project_with_main_prereqs(r#"
            chat_client   = claude_desktop
            pattern       = orchestrator
            system_prompt = prompt.main_brief
        "#);
        assert_parse_err_contains(&src,
            "main block missing required attribute `model`");
    }

    #[test] fn main_block_missing_system_prompt_errors() {
        let src = project_with_main_prereqs(r#"
            model       = model.primary
            chat_client = claude_desktop
            pattern     = orchestrator
        "#);
        assert_parse_err_contains(&src,
            "main block missing required attribute `system_prompt`");
    }

    #[test] fn main_block_missing_pattern_errors() {
        let src = project_with_main_prereqs(r#"
            model         = model.primary
            chat_client   = claude_desktop
            system_prompt = prompt.main_brief
        "#);
        assert_parse_err_contains(&src,
            "main block missing required attribute `pattern`");
    }

    #[test] fn main_block_missing_chat_client_errors() {
        let src = project_with_main_prereqs(r#"
            model         = model.primary
            pattern       = orchestrator
            system_prompt = prompt.main_brief
        "#);
        assert_parse_err_contains(&src,
            "main block missing required attribute `chat_client`");
    }

    #[test] fn main_block_unknown_attribute_errors() {
        let src = project_with_main_prereqs(r#"
            model         = model.primary
            chat_client   = claude_desktop
            pattern       = orchestrator
            system_prompt = prompt.main_brief
            tools         = [mcp_server.docs]
        "#);
        assert_parse_err_contains(&src,
            "unknown attribute `tools` in main block");
    }

    // ── `model.source` enum: parse paths ─────────────────────────

    #[test] fn model_source_subscription_parses() {
        let src = r#"
            project "demo" {
              budget {
                hard_cap_per_day = $1
                hard_cap_per_run = $0.01
              }
              model "primary" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_sonnet_4_6]
                source      = subscription
              }
            }
        "#;
        let project = assert_parse_ok(src);
        assert_eq!(project.models[0].source, ModelSource::Subscription);
    }

    #[test] fn model_source_api_explicit_parses() {
        let src = r#"
            project "demo" {
              budget {
                hard_cap_per_day = $1
                hard_cap_per_run = $0.01
              }
              model "primary" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_sonnet_4_6]
                source      = api
              }
            }
        "#;
        let project = assert_parse_ok(src);
        assert_eq!(project.models[0].source, ModelSource::Api);
    }

    #[test] fn model_source_defaults_to_api_when_absent() {
        // Forward-compat invariant: v0.1 model blocks (no `source`
        // field declared) continue to validate AND default to Api.
        let src = r#"
            project "demo" {
              budget {
                hard_cap_per_day = $1
                hard_cap_per_run = $0.01
              }
              model "primary" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_sonnet_4_6]
              }
            }
        "#;
        let project = assert_parse_ok(src);
        assert_eq!(project.models[0].source, ModelSource::Api);
    }

    #[test] fn model_source_unknown_value_errors() {
        let src = r#"
            project "demo" {
              budget {
                hard_cap_per_day = $1
                hard_cap_per_run = $0.01
              }
              model "primary" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_sonnet_4_6]
                source      = local_inference
              }
            }
        "#;
        assert_parse_err_contains(src,
            "`source` must be one of api / subscription");
    }

    // ── JSON round-trip: forward-compat invariants ───────────────

    #[test] fn project_without_mains_omits_field_in_json() {
        // Forward-compat: serializing a v0.1-shape project (no main
        // blocks declared) must not emit `"mains": []` — that would
        // break byte-stable round-trip against existing
        // examples/*.expected-ast.json fixtures.
        let project = Project {
            name: "demo".into(),
            ..Project::default()
        };
        let json = serde_json::to_string(&project).unwrap();
        assert!(!json.contains("mains"),
            "empty mains Vec must not serialize; got: {json}");
    }

    #[test] fn project_with_mains_includes_field_in_json() {
        // The complement check: when there IS a main, it serializes.
        let project = Project {
            name: "demo".into(),
            mains: vec![Main {
                label:         "u".into(),
                model:         Ref { kind: RefKind::Model,  label: "primary".into(), module: None },
                system_prompt: Ref { kind: RefKind::Prompt, label: "b".into(),       module: None },
                chat_client:   ChatClient::ClaudeDesktop,
                pattern:       Pattern::Orchestrator,
            }],
            ..Project::default()
        };
        let json = serde_json::to_string(&project).unwrap();
        assert!(json.contains("\"mains\""),
            "populated mains must serialize");
        assert!(json.contains("\"orchestrator\""));
    }

    #[test] fn model_with_default_source_omits_field_in_json() {
        // Forward-compat: serializing a v0.1-shape model (no `source`
        // declared, defaults to Api) must NOT emit `"source": "api"`.
        let model = Model {
            label:       "f".into(),
            requires:    vec![Capability::Chat],
            context_min: Size { units: 32 * 1024 },
            cost_max:    None,
            latency_p95: None,
            residency:   None,
            prefer:      vec![],
            fallback:    vec![],
            pin:         None,
            source:      ModelSource::Api,
        };
        let json = serde_json::to_string(&model).unwrap();
        assert!(!json.contains("source"),
            "default source must not serialize; got: {json}");
    }

    #[test] fn model_with_subscription_source_includes_field_in_json() {
        let model = Model {
            label:       "f".into(),
            requires:    vec![Capability::Chat],
            context_min: Size { units: 32 * 1024 },
            cost_max:    None,
            latency_p95: None,
            residency:   None,
            prefer:      vec![],
            fallback:    vec![],
            pin:         None,
            source:      ModelSource::Subscription,
        };
        let json = serde_json::to_string(&model).unwrap();
        assert!(json.contains("\"source\":\"subscription\""),
            "subscription source must serialize; got: {json}");
    }

    // ---------------------------------------------------------------- //
    // v0.3 Week 16 Tue — cross-module ref parsing via `use` keyword.
    // ---------------------------------------------------------------- //

    /// Helper: parse + extract the first agent's resolved Refs.  Every
    /// cross-module test below builds a minimal project with `use`-form
    /// refs and asserts the AST shape.
    fn parse_first_agent(src: &str) -> Agent {
        let p = assert_parse_ok(src);
        p.agents.into_iter().next().expect("at least one agent")
    }

    #[test] fn use_keyword_parses_cross_module_ref_for_model_attribute() {
        // `use coder.fast` in agent.model position — module = "coder",
        // label = "fast", kind inferred from attribute (= Model).
        let src = r#"project "p" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          prompt "system" { file = "./s.md"  version = "1.0.0" }
          agent "a" {
            model         = use coder.fast
            system_prompt = prompt.system
            tools         = []
          }
        }"#;
        let a = parse_first_agent(src);
        assert_eq!(a.model.module, Some("coder".to_string()));
        assert_eq!(a.model.label,  "fast");
        assert_eq!(a.model.kind,   RefKind::Model);
    }

    #[test] fn use_keyword_parses_cross_module_ref_for_prompt_attribute() {
        // Same syntax, different attribute → kind dispatches to Prompt.
        let src = r#"project "p" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          model "m" {
            requires    = [chat]
            context_min = 32k
            prefer      = [ollama.llama_3_1_8b]
            pin         = ollama.llama_3_1_8b
          }
          agent "a" {
            model         = model.m
            system_prompt = use coder.system
            tools         = []
          }
        }"#;
        let a = parse_first_agent(src);
        assert_eq!(a.system_prompt.module, Some("coder".to_string()));
        assert_eq!(a.system_prompt.label,  "system");
        assert_eq!(a.system_prompt.kind,   RefKind::Prompt);
    }

    #[test] fn use_keyword_parses_cross_module_ref_inside_tools_list() {
        // `tools = [use coder.docs, mcp_server.local]` — mixed list of
        // cross-module + same-module refs.  Pins both code paths
        // share the same list element parser.
        let src = r#"project "p" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          model "m" {
            requires    = [chat]
            context_min = 32k
            prefer      = [ollama.llama_3_1_8b]
            pin         = ollama.llama_3_1_8b
          }
          prompt "system" { file = "./s.md"  version = "1.0.0" }
          mcp_server "local" {
            image = "docker.io/x/y:1"
          }
          agent "a" {
            model         = model.m
            system_prompt = prompt.system
            tools         = [use coder.docs, mcp_server.local]
          }
        }"#;
        let a = parse_first_agent(src);
        assert_eq!(a.tools.len(), 2);
        assert_eq!(a.tools[0].module, Some("coder".to_string()));
        assert_eq!(a.tools[0].label,  "docs");
        assert_eq!(a.tools[0].kind,   RefKind::McpServer);
        assert_eq!(a.tools[1].module, None);
        assert_eq!(a.tools[1].label,  "local");
    }

    #[test] fn use_keyword_round_trips_through_json() {
        // Forward-compat pin: a Ref with module=Some serializes WITH
        // the field; a Ref with module=None serializes WITHOUT it.
        let with_module = Ref {
            kind: RefKind::Prompt, label: "x".into(),
            module: Some("coder".into()),
        };
        let without = Ref {
            kind: RefKind::Prompt, label: "x".into(), module: None,
        };
        let json_with = serde_json::to_string(&with_module).unwrap();
        let json_without = serde_json::to_string(&without).unwrap();
        assert!(json_with.contains("\"module\":\"coder\""),
            "module=Some must serialize the field; got: {json_with}");
        assert!(!json_without.contains("module"),
            "module=None must NOT serialize the field (v0.2 forward-compat); \
             got: {json_without}");
        // Round-trip: deserializing the without-module JSON yields
        // module=None (defaulted).
        let parsed: Ref = serde_json::from_str(&json_without).unwrap();
        assert_eq!(parsed.module, None);
    }

    #[test] fn bare_use_word_without_dot_is_parse_error() {
        // `use coder` (no dot) — the use-ref arm starts matching but
        // fails at the dot expectation; choice() backtracks; ident_or_
        // access tries to parse `use` as an ident → `use` becomes
        // `EnumMember("use")` which is the WRONG type for agent.model
        // → converter rejects with a "expects model reference" error.
        // What we pin: SOMETHING fails (not silently accepted).
        let src = r#"project "p" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          prompt "system" { file = "./s.md"  version = "1.0.0" }
          agent "a" {
            model         = use coder
            system_prompt = prompt.system
            tools         = []
          }
        }"#;
        // Either the parser fails outright OR the converter fails.
        // Both are acceptable "rejected" outcomes.
        let result = parse(src);
        assert!(result.is_err(),
            "`use coder` (no dot) must be rejected; got Ok");
    }

    #[test] fn use_form_carries_correct_kind_via_attribute_context() {
        // Same `use coder.thing` syntax in three different attribute
        // positions → three different kinds.  Pin the
        // expected_kind-from-context inference.
        let src_model = r#"project "p" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          prompt "system" { file = "./s.md"  version = "1.0.0" }
          agent "a" {
            model         = use coder.thing
            system_prompt = prompt.system
            tools         = []
          }
        }"#;
        let m_kind = parse_first_agent(src_model).model.kind;
        assert_eq!(m_kind, RefKind::Model);

        let src_prompt = r#"project "p" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          model "m" {
            requires    = [chat]
            context_min = 32k
            prefer      = [ollama.llama_3_1_8b]
            pin         = ollama.llama_3_1_8b
          }
          agent "a" {
            model         = model.m
            system_prompt = use coder.thing
            tools         = []
          }
        }"#;
        let p_kind = parse_first_agent(src_prompt).system_prompt.kind;
        assert_eq!(p_kind, RefKind::Prompt);
    }

    // ---------------------------------------------------------------- //
    // v0.3 Week 17 Mon — `pipeline { … }` composition block.
    // ---------------------------------------------------------------- //

    /// Helper: parse a project containing one or more agents + a
    /// pipeline that chains them.  Returns the parsed Project.
    fn project_with_pipeline(pipeline_body: &str) -> Project {
        let src = format!(r#"project "p" {{
          budget {{ hard_cap_per_day = $1  hard_cap_per_run = $0.01 }}
          model "m" {{
            requires    = [chat]
            context_min = 32k
            prefer      = [ollama.llama_3_1_8b]
            pin         = ollama.llama_3_1_8b
          }}
          prompt "sys" {{ file = "./s.md"  version = "1.0.0" }}
          agent "a" {{
            model         = model.m
            system_prompt = prompt.sys
            tools         = []
          }}
          agent "b" {{
            model         = model.m
            system_prompt = prompt.sys
            tools         = []
          }}
          {pipeline_body}
        }}"#);
        assert_parse_ok(&src)
    }

    #[test] fn pipeline_block_parses_with_required_steps_only() {
        let p = project_with_pipeline(r#"
          pipeline "two_step" {
            steps = [agent.a, agent.b]
          }
        "#);
        assert_eq!(p.pipelines.len(), 1);
        let pl = &p.pipelines[0];
        assert_eq!(pl.label, "two_step");
        assert_eq!(pl.steps.len(), 2);
        assert_eq!(pl.steps[0].kind,  RefKind::Agent);
        assert_eq!(pl.steps[0].label, "a");
        assert_eq!(pl.steps[1].label, "b");
        assert_eq!(pl.input,  None);
        assert_eq!(pl.output, None);
    }

    #[test] fn pipeline_block_parses_with_optional_input_output_descriptors() {
        let p = project_with_pipeline(r#"
          pipeline "annotated" {
            steps  = [agent.a]
            input  = "user request"
            output = "final reply"
          }
        "#);
        let pl = &p.pipelines[0];
        assert_eq!(pl.input,  Some("user request".to_string()));
        assert_eq!(pl.output, Some("final reply".to_string()));
    }

    #[test] fn pipeline_block_rejects_empty_steps_list() {
        // PLAN requires the pipeline body to actually compose
        // SOMETHING — an empty steps[] is a user mistake we catch at
        // parse time so the runtime never has to.
        assert_parse_err_contains(
            r#"project "p" {
              budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
              pipeline "empty" {
                steps = []
              }
            }"#,
            "at least one agent reference",
        );
    }

    #[test] fn pipeline_block_rejects_missing_steps() {
        assert_parse_err_contains(
            r#"project "p" {
              budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
              pipeline "no_steps" {
                input = "x"
              }
            }"#,
            "missing required attribute `steps`",
        );
    }

    #[test] fn pipeline_block_rejects_non_agent_refs_in_steps() {
        // steps[] MUST hold agent.* refs only.  A `prompt.sys` ref in
        // steps[] is a category error at parse time (kind mismatch
        // from `expect_list_of_refs`).
        assert_parse_err_contains(
            r#"project "p" {
              budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
              prompt "sys" { file = "./s.md"  version = "1.0.0" }
              pipeline "bad" {
                steps = [prompt.sys]
              }
            }"#,
            "expects a `agent` reference",
        );
    }

    #[test] fn pipeline_block_rejects_unknown_attribute() {
        assert_parse_err_contains(
            r#"project "p" {
              budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
              pipeline "weird" {
                steps        = [agent.a]
                made_up_attr = "x"
              }
            }"#,
            "unknown attribute `made_up_attr`",
        );
    }

    #[test] fn pipeline_block_accepts_cross_module_step_refs() {
        // The `use <module>.<label>` form (v0.3 Tue Week 16) inside
        // steps[] parses to a Ref with module=Some.  Resolution
        // against the multi-module symbol table is the checker's
        // job; the parser just shapes the AST.
        let p = project_with_pipeline(r#"
          pipeline "x_module" {
            steps = [agent.a, use coder.advanced]
          }
        "#);
        let pl = &p.pipelines[0];
        assert_eq!(pl.steps.len(), 2);
        assert_eq!(pl.steps[0].module, None);
        assert_eq!(pl.steps[1].module, Some("coder".to_string()));
        assert_eq!(pl.steps[1].label,  "advanced");
        assert_eq!(pl.steps[1].kind,   RefKind::Agent);
    }

    #[test] fn pipelines_field_omitted_from_json_when_empty() {
        // Forward-compat pin: a Project with no pipelines must
        // serialize WITHOUT the `pipelines` field — same discipline
        // as `mains` (v0.2 Fri).  Keeps v0.2 expected-AST JSON
        // fixtures byte-stable.
        let p: Project = Project::default();
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("\"pipelines\""),
            "empty pipelines must NOT serialize; got: {json}");
    }
}

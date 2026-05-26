//! aiSlang core: parser, AST, type system, type-checker, resolver.
//!
//! See LANGUAGE-SPEC-v0.md for the authoritative reference.
//!
//! Week-by-week plan (PLAN-90-days.md):
//!   week 2: parser + AST
//!   week 3: type checker + resource graph
//!   week 4: model resolver + lockfile

pub mod ast;
pub mod checker;
pub mod diagnostic;
pub mod parser;
pub mod types;

pub use checker::Warning;
pub use parser::ParseError;

/// The error type returned by [`validate`].
///
/// `Parse(Vec<ParseError>)` carries the structured errors from chumsky
/// (grammar-level errors include byte-offset spans, which the CLI uses
/// to render source-snippet diagnostics). Converter-level errors
/// (to_project / to_budget / …) are wrapped without spans for now —
/// adding per-AST-node span tracking is a future-week enhancement.
///
/// Manual `Display` impl rather than `thiserror` so the Parse variant
/// can format the contained Vec of messages on multiple lines without
/// fighting attribute-macro syntax.
#[derive(Debug)]
pub enum ValidateError {
    Parse(Vec<ParseError>),
    Type(String),
    UnresolvedRef(String),
}

impl std::error::Error for ValidateError {}

impl std::fmt::Display for ValidateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(errs) => {
                write!(f, "parse error")?;
                if errs.len() > 1 { write!(f, " ({} issues)", errs.len())?; }
                writeln!(f, ":")?;
                for e in errs {
                    writeln!(f, "  {}", e.message)?;
                }
                Ok(())
            }
            Self::Type(s)          => write!(f, "type error: {s}"),
            Self::UnresolvedRef(s) => write!(f, "unresolved reference: {s}"),
        }
    }
}

/// Parse + type-check the source string.
///
/// Pipeline (week 3):
///   1. `parser::parse`  — chumsky grammar + per-resource converters
///   2. `checker::check` — duplicate-label detection (Mon),
///                         cross-resource ref resolution (Tue)
///
/// Returns `(Project, Vec<Warning>)` on success. Warnings include
/// future in-core findings (deprecated grammar form, unused resource);
/// today always empty. Catalog ID validation lives in the sibling
/// `aislang-resolver` crate (avoids the core ↔ catalog dep cycle).
pub fn validate(source: &str) -> Result<(ast::Project, Vec<Warning>), ValidateError> {
    let project  = parser::parse(source).map_err(ValidateError::Parse)?;
    let warnings = checker::check(&project).map_err(ValidateError::Type)?;
    Ok((project, warnings))
}

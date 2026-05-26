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
pub mod parser;
pub mod types;

pub use checker::Warning;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ValidateError {
    #[error("parse error:\n{0}")]
    Parse(String),
    #[error("type error: {0}")]
    Type(String),
    #[error("unresolved reference: {0}")]
    UnresolvedRef(String),
}

/// Parse + type-check the source string.
///
/// Pipeline (week 3):
///   1. `parser::parse`  — chumsky grammar + per-resource converters
///   2. `checker::check` — duplicate-label detection (Mon),
///                         cross-resource ref resolution (Tue),
///                         catalog ID validation (Wed) — returns the
///                         list of non-fatal warnings as the success
///                         value
///
/// Returns `(Project, Vec<Warning>)` on success. Warnings include
/// references to catalog entries marked `deprecated: true` — those
/// references are honoured (the compliance-audit-trail use case
/// requires it; see PLAN §Locked design decisions) but surfaced to
/// the user.
pub fn validate(source: &str) -> Result<(ast::Project, Vec<Warning>), ValidateError> {
    let project  = parser::parse(source).map_err(ValidateError::Parse)?;
    let warnings = checker::check(&project).map_err(ValidateError::Type)?;
    Ok((project, warnings))
}

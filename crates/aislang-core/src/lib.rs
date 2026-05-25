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
/// Pipeline (week 3 day 1 — duplicate-label detection lives in the
/// checker; ref resolution and catalog validation arrive Tue/Wed):
///   1. `parser::parse`  — chumsky grammar + per-resource converters
///   2. `checker::check` — duplicate-label detection (Mon),
///                         cross-resource ref resolution (Tue),
///                         catalog ID validation (Wed)
///
/// Returns the validated [`ast::Project`] on success. Wednesday's
/// warning infrastructure will change the return shape to
/// `Result<(Project, Vec<Warning>), ValidateError>`.
pub fn validate(source: &str) -> Result<ast::Project, ValidateError> {
    let project = parser::parse(source).map_err(ValidateError::Parse)?;
    checker::check(&project).map_err(ValidateError::Type)?;
    Ok(project)
}

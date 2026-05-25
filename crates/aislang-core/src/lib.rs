//! aiSlang core: parser, AST, type system, type-checker, resolver.
//!
//! See LANGUAGE-SPEC-v0.md for the authoritative reference.
//!
//! Week-by-week plan (PLAN-90-days.md):
//!   week 2: parser + AST
//!   week 3: type checker + resource graph
//!   week 4: model resolver + lockfile

pub mod ast;
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

/// Parse + type-check + resolve references in a source string.
///
/// Returns the validated [`ast::Project`] on success.
pub fn validate(source: &str) -> Result<ast::Project, ValidateError> {
    let project = parser::parse(source).map_err(ValidateError::Parse)?;
    // TODO week 3: typecheck(&project)?;
    // TODO week 3: resolve_refs(&project)?;
    Ok(project)
}

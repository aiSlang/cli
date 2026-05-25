//! Parser for the aiSlang v0 grammar.
//!
//! TODO week 2: implement with `chumsky` 0.10 against the EBNF in
//! LANGUAGE-SPEC-v0.md §8. Until then, this module exposes the
//! [`parse`] signature so the CLI compiles end-to-end.
//!
//! Implementation notes carried over from Navcola VM:
//!   - use `chumsky::Parser` with `extra::Err<Rich<'src, char>>` for
//!     spanned error reporting
//!   - keep the literal parsers (`Money`, `Duration`, `Size`,
//!     `Percentage`) as standalone combinators so the type-checker can
//!     reuse them on Tue of week 1
//!   - parse into the [`crate::ast`] tree directly — no intermediate IR

use crate::ast;

/// Parse a source string into an [`ast::Project`] tree.
///
/// Returns a human-readable error string suitable for printing to stderr.
pub fn parse(_source: &str) -> Result<ast::Project, String> {
    Err(
        "parser not implemented yet — scheduled for week 2 \
         (see PLAN-90-days.md and LANGUAGE-SPEC-v0.md §8)"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_stub_returns_error_with_helpful_message() {
        let err = parse("").unwrap_err();
        assert!(err.contains("week 2"));
    }
}

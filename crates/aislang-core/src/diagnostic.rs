//! Source-aware diagnostic rendering — manual snippet construction per
//! the week-3 locked design decision (no `ariadne` dep; ~120 LOC keeps
//! 90% of the value with zero new SemVer surface).
//!
//! Output format (rustc-flavoured):
//!
//! ```text
//! error at line 5, column 22: expected ',' or ']'
//!   4 |   model "fast" {
//!   5 |     requires = [chat tool_use]
//!     |                      ^^^^^^^^^
//!   6 |     context_min = 32k
//! ```

use std::fmt::Write;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error   => "error",
            Self::Warning => "warning",
        }
    }
}

/// Render a single diagnostic with source-snippet context. Shows one
/// line before + the offending line (with caret) + one line after.
/// At the file's edges, the context is clipped accordingly.
///
/// `span` is byte offsets in `source`. The caret pointer is sized to
/// the span width (clipped to the line) but at least one character.
pub fn render_with_source(
    source:   &str,
    span:     Range<usize>,
    severity: Severity,
    message:  &str,
) -> String {
    let (line, col) = line_col_from_offset(source, span.start);
    let lines: Vec<&str> = source.split('\n').collect();
    if lines.is_empty() {
        return format!("{} at line {line}, column {col}: {message}\n",
                       severity.as_str());
    }

    let line_idx_zero = line.saturating_sub(1).min(lines.len() - 1);
    let context_start = line_idx_zero.saturating_sub(1);
    let context_end   = (line_idx_zero + 1).min(lines.len() - 1);

    // Gutter width is the digit-width of the largest line number we'll print.
    let gutter_width = (context_end + 1).to_string().len();

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} at line {line}, column {col}: {message}",
        severity.as_str(),
    );

    for i in context_start..=context_end {
        let line_num = i + 1;
        let line_str = lines[i];
        let _ = writeln!(out, "  {line_num:>gutter_width$} | {line_str}");

        if i == line_idx_zero {
            // Caret-pointer line. Spaces before the caret match the
            // 1-based column; caret width is the span length clipped
            // to what fits on the remaining line.
            let caret_col = col.saturating_sub(1);
            let span_len  = span.end.saturating_sub(span.start).max(1);
            let line_chars = line_str.chars().count();
            let remaining  = line_chars.saturating_sub(caret_col);
            let caret_len  = span_len.min(remaining).max(1);

            let pad    = " ".repeat(caret_col);
            let carets = "^".repeat(caret_len);
            let _ = writeln!(out, "  {:>gutter_width$} | {pad}{carets}", "");
        }
    }

    out
}

/// Convert a byte offset into a (line, column) pair, both 1-based.
/// Column is character (not byte) position within the line — keeps the
/// caret aligned with multi-byte UTF-8 source.
pub fn line_col_from_offset(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col  = 1usize;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            return (line, col);
        }
        if ch == '\n' {
            line += 1;
            col   = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn line_col_basics() {
        assert_eq!(line_col_from_offset("abc", 0),         (1, 1));
        assert_eq!(line_col_from_offset("abc", 1),         (1, 2));
        assert_eq!(line_col_from_offset("ab\ncd", 3),      (2, 1));
        assert_eq!(line_col_from_offset("ab\ncd\nef", 6),  (3, 1));
    }

    #[test] fn line_col_handles_multibyte() {
        // `€` is 3 bytes in UTF-8; the column should still advance by 1.
        let src = "x€y";
        assert_eq!(line_col_from_offset(src, 0), (1, 1));
        // Offset 4 is right after €y; column should be 3 (after x, €).
        assert_eq!(line_col_from_offset(src, 4), (1, 3));
    }

    #[test] fn render_single_line_basics() {
        let src = "project \"p\" { budget { } }";
        let out = render_with_source(src, 22..23, Severity::Error,
            "missing required attribute `hard_cap_per_day`");
        assert!(out.contains("error at line 1, column 23"));
        assert!(out.contains("missing required attribute"));
        assert!(out.contains("project \"p\" { budget { } }"));
        assert!(out.contains('^'),
            "expected a caret in:\n{out}");
    }

    #[test] fn render_with_context_lines() {
        let src = "\
line one
line two
LINE THREE
line four";
        // Span covers part of "LINE THREE"
        let line3_start = src.find("LINE").unwrap();
        let out = render_with_source(
            src,
            line3_start..line3_start + 4,
            Severity::Error,
            "all caps",
        );
        // Context shows lines 2, 3, 4.
        assert!(out.contains("line two"));
        assert!(out.contains("LINE THREE"));
        assert!(out.contains("line four"));
        // Caret on line 3.
        assert!(out.contains("^^^^"));
        // Line numbers in the gutter.
        assert!(out.contains("  2 |"));
        assert!(out.contains("  3 |"));
        assert!(out.contains("  4 |"));
    }

    #[test] fn render_first_line_no_pre_context() {
        let src = "first line\nsecond";
        let out = render_with_source(src, 0..5, Severity::Error, "x");
        // No line 0; just lines 1 and 2.
        assert!(out.contains("  1 |"));
        assert!(out.contains("  2 |"));
        assert!(!out.contains("  0 |"));
    }

    #[test] fn render_last_line_no_post_context() {
        let src = "first\nlast";
        let last_start = src.find("last").unwrap();
        let out = render_with_source(
            src, last_start..last_start + 4,
            Severity::Error, "tail",
        );
        // No line 3 (doesn't exist).
        assert!(out.contains("  1 |"));
        assert!(out.contains("  2 |"));
        assert!(!out.contains("  3 |"));
    }

    #[test] fn render_warning_severity_used_in_header() {
        let src = "anything";
        let out = render_with_source(src, 0..1, Severity::Warning, "heads up");
        assert!(out.contains("warning at line 1, column 1"));
    }

    #[test] fn render_zero_width_span_still_has_caret() {
        // Zero-width spans happen for chumsky "expected X but found EOF"
        // errors. Caret width clamps to ≥1.
        let src = "abc";
        let out = render_with_source(src, 3..3, Severity::Error, "unexpected EOF");
        assert!(out.contains('^'));
    }
}

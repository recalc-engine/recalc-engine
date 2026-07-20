//! Parse and lex errors.
//!
//! Per the project's "never silently wrong" principle
//! (`implementation-plan.md` §0), the front-end either parses a construct
//! correctly, rejects it with a precise [`ParseError`], or represents it as an
//! explicit [`crate::ExprKind::Unsupported`] node. A wrong AST is never
//! produced on purpose.

use crate::span::Span;
use core::fmt;

/// The category of a [`ParseError`], with enough detail for a diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseErrorKind {
    /// A string literal was opened with `"` but never closed.
    UnterminatedString,
    /// A quoted sheet name (`'...'`) was opened but never closed.
    UnterminatedSheetName,
    /// A structured-reference bracket group (`[...]`) was never closed.
    UnterminatedBracket,
    /// A numeric literal could not be parsed as a finite `f64`.
    InvalidNumber,
    /// An A1 reference names a cell outside the sheet grid (column > `XFD`
    /// or row > 1048576).
    RefOutOfRange,
    /// A character was encountered that cannot begin any token.
    UnexpectedChar(char),
    /// A token appeared where it is not grammatically valid.
    UnexpectedToken,
    /// Input ended while a larger construct was still expected.
    UnexpectedEof,
    /// An operand (expression) was expected but something else was found.
    ExpectedExpr,
    /// A closing delimiter (`)`, `}`) was expected.
    ExpectedClose(char),
    /// An array literal `{...}` contained a non-constant element (only
    /// numbers, strings, booleans, and error literals are permitted per
    /// ECMA-376 §18.17.2).
    NonConstantInArray,
    /// An array literal `{...}` had rows of differing lengths.
    RaggedArray,
    /// A sheet prefix (`Sheet1!`) was not followed by a reference or name.
    ExpectedSheetReference,
    /// The formula nests expressions deeper than the parser's fixed recursion
    /// limit ([`crate::MAX_NESTING_DEPTH`], 200 — comfortably above Excel's
    /// own 64-level function-nesting cap). The bound keeps stack usage finite
    /// on untrusted input; without it a deeply parenthesized 32,767-character
    /// formula could overflow the stack and abort the process.
    TooDeeplyNested,
    /// A quoted sheet name was found in a position where it is not a prefix.
    UnexpectedQuotedName,
    /// A name is separated from a following `(` by whitespace, as in
    /// `SUM (1)`. This is ambiguous between a function call and an
    /// intersection of the name with a parenthesized expression; Excel's
    /// reading is not pinned by the public docs, so it is rejected pending
    /// oracle probe OXP-052 (`docs/oracle-experiments.md`) rather than
    /// silently picking either interpretation.
    AmbiguousSpaceBeforeParen,
    /// A range whose right endpoint carries its own sheet qualifier, in a
    /// shape that is **not** the accepted redundant same-sheet form. Oracle
    /// probe OXP-053 (RUN-2026-07-11-oracle01) pinned Excel's reading:
    /// `Sheet1!A1:Sheet1!B2` is exactly the range `Sheet1!A1:B2` and is now
    /// **accepted** (RFC-0004 slice-1 — the parser normalizes it), with
    /// whitespace around `:` inert. This error remains for the **deferred
    /// slice-2** shapes, whose evaluator error-mapping is still
    /// under-determined: a *different*-sheet right endpoint
    /// (`Sheet1!A1:Sheet2!B2` → #VALUE!), a *bare* left endpoint
    /// (`A1:Sheet1!B2`), an unresolvable sheet (`M:m!M` → #NAME?), or a
    /// non-reference right operand (parenthesized/`@`-wrapped/structured). It
    /// is a precise reject rather than a guess, gated by
    /// `rfcs/0004-sheet-qualified-range-endpoints.md`.
    SheetQualifiedRangeEndpoints,
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseErrorKind::UnterminatedString => f.write_str("unterminated string literal"),
            ParseErrorKind::UnterminatedSheetName => f.write_str("unterminated quoted sheet name"),
            ParseErrorKind::UnterminatedBracket => {
                f.write_str("unterminated structured-reference bracket")
            }
            ParseErrorKind::InvalidNumber => f.write_str("invalid numeric literal"),
            ParseErrorKind::RefOutOfRange => {
                f.write_str("cell reference outside the sheet grid (max column XFD, row 1048576)")
            }
            ParseErrorKind::UnexpectedChar(c) => write!(f, "unexpected character {c:?}"),
            ParseErrorKind::UnexpectedToken => f.write_str("unexpected token"),
            ParseErrorKind::UnexpectedEof => f.write_str("unexpected end of formula"),
            ParseErrorKind::ExpectedExpr => f.write_str("expected an expression"),
            ParseErrorKind::ExpectedClose(c) => write!(f, "expected closing {c:?}"),
            ParseErrorKind::NonConstantInArray => {
                f.write_str("array literals may contain only scalar constants")
            }
            ParseErrorKind::RaggedArray => f.write_str("array literal rows have unequal lengths"),
            ParseErrorKind::ExpectedSheetReference => {
                f.write_str("sheet prefix must be followed by a reference or name")
            }
            ParseErrorKind::TooDeeplyNested => f.write_str(
                "formula nests expressions deeper than the supported limit of 200 levels \
                 (Excel itself allows at most 64 nested function levels)",
            ),
            ParseErrorKind::UnexpectedQuotedName => {
                f.write_str("quoted name is only valid as a sheet prefix")
            }
            ParseErrorKind::AmbiguousSpaceBeforeParen => f.write_str(
                "whitespace between a name and '(' is ambiguous (function call vs \
                 intersection with a parenthesized expression); rejected pending oracle \
                 probe OXP-052 — remove the space for a call",
            ),
            ParseErrorKind::SheetQualifiedRangeEndpoints => f.write_str(
                "a range's right endpoint carries a sheet qualifier for a different or bare \
                 left endpoint (e.g. 'Sheet1!A1:Sheet2!B2' or 'A1: Sheet1!B2'); not supported \
                 yet — Excel accepts it (OXP-053), but the evaluator mapping is deferred to \
                 RFC 0004 slice-2. The redundant same-sheet form 'Sheet1!A1:Sheet1!B2' is \
                 accepted; write 'Sheet1!A1:B2'",
            ),
        }
    }
}

/// An error produced by the lexer or parser, with the source [`Span`] it
/// covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// What went wrong.
    pub kind: ParseErrorKind,
    /// The byte range in the source formula the error refers to.
    pub span: Span,
}

impl ParseError {
    /// Construct a parse error.
    #[must_use]
    pub const fn new(kind: ParseErrorKind, span: Span) -> Self {
        Self { kind, span }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (at bytes {}..{})",
            self.kind, self.span.start, self.span.end
        )
    }
}

impl std::error::Error for ParseError {}

/// Convenience alias for fallible front-end operations.
pub type Result<T> = core::result::Result<T, ParseError>;

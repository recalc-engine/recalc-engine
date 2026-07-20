//! `xl-ast` — Excel formula grammar for Recalc.
//!
//! This crate owns the formula **lexer** and **Pratt parser**: A1 and R1C1
//! references, mixed (absolute/relative) refs, 3-D sheet references,
//! structured table references, union (`,`) and intersection (space)
//! operators, array literals (`{...}`), function calls (including `LET` /
//! `LAMBDA`, which parse as ordinary calls), implicit-intersection `@`, the
//! spill-range postfix `#`, and the `_xlfn`/`_xlws` future-function prefixes.
//!
//! # Provenance
//! The grammar is reconstructed **clean-room** from public specifications
//! only: ECMA-376 (ISO/IEC 29500) §18.17 "Formulas" for the token grammar, and
//! Microsoft Learn **"Calculation operators and precedence in Excel"** for
//! operator precedence and associativity (the full precedence table, with
//! citation, is reproduced in the source of the private `parser` module). GPL
//! spreadsheet source (LibreOffice / Gnumeric) is forbidden
//! reading (`implementation-plan.md` §9).
//!
//! # Cardinal principle — never silently mis-parse
//! Every construct is either (a) parsed correctly per citation, (b) rejected
//! with a precise [`ParseError`], or (c) represented as an explicit
//! [`ExprKind::Unsupported`] node that preserves the raw source. A wrong AST is
//! never produced on purpose (`implementation-plan.md` §0, "Never silently
//! wrong").
//!
//! ## What is `Unsupported` vs a `ParseError` in v0
//! - **`Unsupported`**: structured/table references (`Table1[Col]`,
//!   `Sheet1!T[[#Data],[Col]]`) and bracket-delimited external groups. These
//!   are lexed in full and carried through verbatim, pending a later task.
//! - **`ParseError`**: genuinely malformed input — unterminated strings /
//!   sheet names / brackets, out-of-grid references in either dialect
//!   (`$XFE$1`, `R0C0`, `R[999999999999]C`), invalid numbers, ragged or
//!   non-constant array literals, and stray tokens — plus two constructs
//!   whose Excel reading is not yet pinned from public docs and is therefore
//!   **rejected rather than guessed**: whitespace between a name and `(`
//!   (`SUM (1)` — call or intersection? probe OXP-052) and the deferred
//!   *slice-2* per-endpoint sheet-qualified ranges — a different-sheet or
//!   bare-left endpoint (`Sheet1!A1:Sheet2!B2`, `A1:Sheet1!B2`; probe
//!   OXP-053). The redundant *same-sheet* form `Sheet1!A1:Sheet1!B2` is
//!   **accepted** and normalized to the left-qualified `Sheet1!A1:B2`
//!   (RFC-0004 slice-1). See `docs/oracle-experiments.md`.
//!
//! # Textual round-trip
//! [`Expr`] implements [`std::fmt::Display`] to emit a **canonical** formula
//! string (uppercase function names and column letters, canonical `$`
//! markers, grouping parentheses preserved). [`Expr::to_sexpr`] emits a
//! span-free structural dump used by the golden tests. Numeric literals are
//! rendered with Rust's shortest round-trip `f64` formatting, which can
//! differ **cosmetically** from Excel's rendering at extreme magnitudes
//! (e.g. `1.5E+308` prints as its long decimal expansion); the value is
//! identical and re-parses exactly.
//!
//! # Example
//! ```
//! use xl_ast::{parse, ExprKind, BinaryOp};
//!
//! let e = parse("=-2^2").unwrap();
//! // Unary minus binds tighter than `^` in Excel: (-2)^2, not -(2^2).
//! assert_eq!(e.to_sexpr(), "(^ (u- 2) 2)");
//! assert!(matches!(e.kind, ExprKind::Binary { op: BinaryOp::Pow, .. }));
//! ```

#![forbid(unsafe_code)]

mod ast;
mod error;
mod lexer;
mod parser;
mod span;
mod token;

pub use ast::{
    Axis, BinaryOp, Expr, ExprKind, FuncName, NameRef, PostfixOp, R1C1Axis, R1C1Ref, RefKind,
    Reference, SheetRef, UnaryOp,
};
pub use error::{ParseError, ParseErrorKind};
pub use lexer::{ParseMode, lex};
pub use parser::{MAX_NESTING_DEPTH, parse, parse_with_mode};
pub use span::Span;
pub use token::{Token, TokenKind};

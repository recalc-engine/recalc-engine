//! The Pratt (top-down operator-precedence) parser.
//!
//! ## Provenance — operator precedence
//! Precedence and associativity follow Microsoft Learn, **"Calculation
//! operators and precedence in Excel"** (the "Operator precedence in Excel
//! formulas" table), reproduced here from tightest- to loosest-binding:
//!
//! | Level | Operators | Notes |
//! |------:|-----------|-------|
//! | 1 | `:` (range), ` ` (intersection), `,` (union) | reference operators |
//! | 2 | unary `-`, `+` | negation binds **tighter** than `^` |
//! | 3 | `%` | postfix percent |
//! | 4 | `^` | exponentiation, **left-associative** in Excel |
//! | 5 | `*` `/` | |
//! | 6 | `+` `-` | binary add/subtract |
//! | 7 | `&` | text concatenation |
//! | 8 | `= < > <= >= <>` | comparison (loosest) |
//!
//! Two documented Excel quirks fall directly out of this table and are pinned
//! by golden tests:
//! - **`-2^2 = 4`**: unary minus (level 2) binds tighter than `^` (level 4),
//!   so it parses as `(-2)^2`, not `-(2^2)`.
//! - **`2^3^2 = 64`**: `^` is left-associative, so it parses as `(2^3)^2`.
//!
//! The Microsoft table groups the three reference operators on a single row
//! ("Reference operators") without ranking them against each other. Their
//! internal precedence — `:` tighter than ` ` (intersection) tighter than `,`
//! (union) — was **confirmed** by oracle probe **OXP-050**
//! (`RUN-2026-07-11-oracle01`, `docs/oracle-experiments.md`): `=SUM(A1:A3
//! A2:A4)` intersects the two ranges, `=SUM((A1,A2):(B1,B2))` ranges over both
//! grouped unions, and `=SUM(A1:A3,B1:B3 C1:C3)` intersects inside an
//! argument. Pinned by the `reference_operator_precedence_oxp050` golden test.
//!
//! ## Union vs argument separator
//! `,` is the union operator only inside a *grouping* parenthesis
//! (`(A1,B1)`); inside a function argument list it separates arguments. The
//! parser tracks this with an `allow_union` flag that is set when entering a
//! grouping `(...)` and cleared when entering each argument. See
//! [`ExprKind::Binary`] with [`BinaryOp::Union`].
//!
//! ## `LET`/`LAMBDA`
//! These parse as ordinary function calls; their binding semantics are an
//! evaluation concern handled downstream, not a grammar concern.
//!
//! ## Sheet-qualified range endpoints (RFC-0004 slice-1, OXP-053)
//! A range may carry a sheet qualifier on its *right* endpoint. Oracle probe
//! **OXP-053** (`RUN-2026-07-11-oracle01`, `docs/oracle-experiments.md`) pinned
//! Excel's reading: a redundant *same-sheet* double qualification
//! `Sheet1!A1:Sheet1!B2` is exactly the left-qualified range `Sheet1!A1:B2`
//! (`SUM(Sheet1!A1:Sheet1!B2)` → `10.0`), and whitespace around `:` does not
//! change the reading. RFC-0004 **slice-1** (ratified) normalizes that form by
//! dropping the redundant qualifier (see `normalize_same_sheet_range`); the
//! lexer backs off a cell-shaped 3-D-span first part so the contiguous and
//! spaced spellings share one token stream (closing fuzz FINDING-001). The
//! cross-sheet / bare-left shapes (`Sheet1!A1:Sheet2!B2` → `#VALUE!`, `M:m!M`
//! → `#NAME?`) are **slice-2**, deferred as under-determined, and stay a
//! precise [`ParseErrorKind::SheetQualifiedRangeEndpoints`].

use crate::ast::{
    Axis, BinaryOp, Expr, ExprKind, FuncName, NameRef, PostfixOp, RefKind, Reference, SheetRef,
    UnaryOp, col_to_num,
};
use crate::error::{ParseError, ParseErrorKind, Result};
use crate::lexer::{ParseMode, lex};
use crate::span::Span;
use crate::token::{Token, TokenKind};

const REASON_STRUCTURED: &str =
    "structured (table) reference — parsed as Unsupported in v0, flagged for a later task";
/// Distinct reason for *sheet-qualified* structured refs (`Sheet1!T[...]`,
/// `F![]`): `leading_sheet_qualifier` matches on it so a range like
/// `S :F![]` is rejected the same as `S: F!A1` (FINDING-001 / OXP-053) —
/// the qualification is not otherwise visible on an `Unsupported` node.
const REASON_STRUCTURED_SHEET: &str = "sheet-qualified structured (table) reference — parsed as \
     Unsupported in v0, flagged for a later task";

/// Parse a formula string in A1 mode.
///
/// A single optional leading `=` is stripped for convenience (stored `.xlsx`
/// formulas omit it); byte spans in the returned tree and in any error are
/// relative to the formula body after that `=`.
///
/// # Errors
/// Returns a [`ParseError`] for any malformed formula.
pub fn parse(src: &str) -> Result<Expr> {
    parse_with_mode(src, ParseMode::A1)
}

/// Parse a formula string in the given [`ParseMode`].
///
/// # Errors
/// Returns a [`ParseError`] for any malformed formula.
pub fn parse_with_mode(src: &str, mode: ParseMode) -> Result<Expr> {
    let body = src.strip_prefix('=').unwrap_or(src);
    let tokens = lex(body, mode)?;
    let mut p = Parser {
        src: body,
        tokens,
        pos: 0,
        depth: 0,
    };
    if p.tokens.is_empty() {
        return Err(ParseError::new(
            ParseErrorKind::ExpectedExpr,
            Span::point(0),
        ));
    }
    let expr = p.parse_expr(0, false)?;
    if p.pos < p.tokens.len() {
        return Err(ParseError::new(
            ParseErrorKind::UnexpectedToken,
            p.tokens[p.pos].span,
        ));
    }
    Ok(expr)
}

/// Maximum expression-nesting depth the parser will recurse into.
///
/// Excel itself caps function nesting at **64 levels** (Microsoft Learn,
/// "Excel specifications and limits", calculation section). 200 leaves ample
/// slack for additional parenthesis/unary-operator nesting around those 64
/// call levels while keeping the parser's stack usage small and bounded —
/// formulas can legally be 32,767 characters, so an *unbounded* recursive
/// descent over untrusted workbook content could otherwise overflow the
/// stack and abort the host process (uncatchably). Exceeding the limit is a
/// precise [`ParseErrorKind::TooDeeplyNested`], never a crash.
///
/// Stack budget: the deepest recursion cycle (nested calls) measures ≈3.5 KiB
/// per level in an unoptimized debug build (macOS aarch64, Rust 1.96), so the
/// full 200 levels use ≈0.7 MiB — comfortably inside the 2 MiB default stack
/// of Rust test threads and any typical 8 MiB main thread. The parser's
/// recursion-cycle functions are deliberately kept as small frames; see the
/// note on `parse_expr_bp` before adding locals to them.
pub const MAX_NESTING_DEPTH: usize = 200;

// ---------------------------------------------------------------------------
// Binding powers, tightest- to loosest-binding, mirroring the module-header
// precedence table (levels 1-8 cite Microsoft Learn "Calculation operators
// and precedence in Excel"; `#`, `@` are OXP-051 assumptions).
// ---------------------------------------------------------------------------
/// `:` range operator (level 1, tightest of the reference operators).
const BP_RANGE: u8 = 100;
/// `#` spill-range postfix (undocumented in the table; probe OXP-051).
const BP_SPILL: u8 = 95;
/// `@` implicit-intersection prefix (undocumented; probe OXP-051) — binds
/// looser than `:` so `@A1:A10` covers the whole range.
const BP_AT: u8 = 95;
/// Space intersection operator (level 1, middle reference operator).
const BP_INTERSECT: u8 = 90;
/// `,` union operator (level 1, loosest reference operator; order confirmed by
/// OXP-050 / RUN-2026-07-11-oracle01 — looser than `:` and ` `).
const BP_UNION: u8 = 80;
/// Unary `-`/`+` (level 2 — tighter than `^`, hence `-2^2 = 4`).
const BP_UNARY: u8 = 70;
/// `%` postfix (level 3).
const BP_PERCENT: u8 = 60;
/// `^` (level 4, left-associative).
const BP_POW: u8 = 50;
/// `*` `/` (level 5).
const BP_MUL_DIV: u8 = 40;
/// Binary `+` `-` (level 6).
const BP_ADD_SUB: u8 = 30;
/// `&` concatenation (level 7).
const BP_CONCAT: u8 = 20;
/// Comparisons `= < > <= >= <>` (level 8, loosest).
const BP_COMPARE: u8 = 10;

struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    /// Current expression-nesting depth, bounded by [`MAX_NESTING_DEPTH`].
    depth: usize,
}

enum InfixKind {
    /// A binary operator; `bool` is whether it has a token to consume (space
    /// intersection has none).
    Bin(BinaryOp, bool),
    Post(PostfixOp),
}

struct Infix {
    lbp: u8,
    rbp: u8,
    kind: InfixKind,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn peek_kind(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos).map(|t| &t.kind)
    }

    fn bump(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        self.pos += 1;
        t
    }

    fn eof_span(&self) -> Span {
        Span::point(self.src.len())
    }

    fn raw(&self, span: Span) -> String {
        self.src[span.start..span.end].to_string()
    }

    // -- Pratt loop ---------------------------------------------------------

    /// Depth-guarded entry to the Pratt loop.
    ///
    /// **Every** recursive cycle in this parser passes through `parse_expr`
    /// (audited: `parse_prefix`'s `(`, unary, and `@` branches, the infix
    /// right-hand side, and `finish_call → parse_args → parse_arg` all call
    /// back into `parse_expr`; `parse_array → parse_array_const` accepts only
    /// scalar constants and never recurses), so this single guard bounds the
    /// whole parser's stack use at [`MAX_NESTING_DEPTH`] frames.
    fn parse_expr(&mut self, min_bp: u8, allow_union: bool) -> Result<Expr> {
        if self.depth >= MAX_NESTING_DEPTH {
            return Err(self.too_deep());
        }
        self.depth += 1;
        let result = self.parse_expr_bp(min_bp, allow_union);
        self.depth -= 1;
        result
    }

    #[cold]
    fn too_deep(&self) -> ParseError {
        let span = self.peek().map_or_else(|| self.eof_span(), |t| t.span);
        ParseError::new(ParseErrorKind::TooDeeplyNested, span)
    }

    // NOTE on function granularity here and in `parse_prefix`: the bodies of
    // the recursive-cycle functions are deliberately split into small helper
    // functions. Debug builds give every match arm's temporaries their own
    // stack slots for the *whole* enclosing frame, which made each nesting
    // level cost ~12 KiB and overflow a default 2 MiB thread well before
    // [`MAX_NESTING_DEPTH`]; splitting keeps each frame on the recursion
    // cycle small so the depth limit is the only stack bound that matters.
    fn parse_expr_bp(&mut self, min_bp: u8, allow_union: bool) -> Result<Expr> {
        let mut lhs = self.parse_prefix(allow_union)?;
        while let Some(inf) = self.next_infix(allow_union) {
            if inf.lbp <= min_bp {
                break;
            }
            lhs = match inf.kind {
                InfixKind::Post(op) => self.apply_postfix(op, lhs),
                InfixKind::Bin(op, consumes) => {
                    self.apply_infix(op, consumes, inf.rbp, allow_union, lhs)?
                }
            };
        }
        Ok(lhs)
    }

    fn apply_postfix(&mut self, op: PostfixOp, lhs: Expr) -> Expr {
        let optok = self.bump();
        let span = lhs.span.to(optok.span);
        Expr::new(
            ExprKind::Postfix {
                op,
                expr: Box::new(lhs),
            },
            span,
        )
    }

    fn apply_infix(
        &mut self,
        op: BinaryOp,
        consumes: bool,
        rbp: u8,
        allow_union: bool,
        lhs: Expr,
    ) -> Result<Expr> {
        if consumes {
            self.bump();
        }
        let rhs = self.parse_expr(rbp, allow_union)?;
        let span = lhs.span.to(rhs.span);
        let (lhs_n, mut rhs_n) = if op == BinaryOp::Range {
            (promote_range_operand(lhs), promote_range_operand(rhs))
        } else {
            (lhs, rhs)
        };
        // A range whose *right* operand carries its own sheet qualifier
        // (`Sheet1!A1:Sheet1!B2`, `A1: Sheet1!B2`, `m: m!M`) reaches this
        // binary-`:` path. Whitespace around `:` is inert: the contiguous
        // spelling now produces the same tokens as the spaced one (the lexer
        // backs off a cell-shaped 3-D-span first part — see `try_sheet_prefix`),
        // so both spellings converge here. OXP-053 (RUN-2026-07-11-oracle01)
        // pinned Excel's reading, and RFC-0004 slice-1 is ratified for the
        // low-risk slice: a redundant *same-sheet* double qualification is
        // exactly the left-qualified range (`Sheet1!A1:Sheet1!B2` ==
        // `Sheet1!A1:B2` → 10.0), so accept it by dropping the right endpoint's
        // redundant qualifier. Every other shape — a different sheet
        // (`Sheet1!A1:Sheet2!B2` → #VALUE!), a bare left endpoint
        // (`A1:Sheet1!B2`), or an unresolvable sheet (`M:m!M` → #NAME?) — is
        // slice-2, whose evaluator error-mapping is still under-determined and
        // gated by `rfcs/0004-sheet-qualified-range-endpoints.md`; it stays a
        // loud, precise reject rather than a guess.
        if op == BinaryOp::Range && leading_sheet_qualifier(&rhs_n) {
            match normalize_same_sheet_range(&lhs_n, &rhs_n) {
                Some(normalized) => rhs_n = normalized,
                None => {
                    return Err(ParseError::new(
                        ParseErrorKind::SheetQualifiedRangeEndpoints,
                        span,
                    ));
                }
            }
        }
        Ok(Expr::new(
            ExprKind::Binary {
                op,
                lhs: Box::new(lhs_n),
                rhs: Box::new(rhs_n),
            },
            span,
        ))
    }

    fn next_infix(&self, allow_union: bool) -> Option<Infix> {
        let tok = self.peek()?;
        let bin = |op, l, r| {
            Some(Infix {
                lbp: l,
                rbp: r,
                kind: InfixKind::Bin(op, true),
            })
        };
        match &tok.kind {
            TokenKind::Caret => bin(BinaryOp::Pow, BP_POW, BP_POW),
            TokenKind::Star => bin(BinaryOp::Mul, BP_MUL_DIV, BP_MUL_DIV),
            TokenKind::Slash => bin(BinaryOp::Div, BP_MUL_DIV, BP_MUL_DIV),
            TokenKind::Plus => bin(BinaryOp::Add, BP_ADD_SUB, BP_ADD_SUB),
            TokenKind::Minus => bin(BinaryOp::Sub, BP_ADD_SUB, BP_ADD_SUB),
            TokenKind::Amp => bin(BinaryOp::Concat, BP_CONCAT, BP_CONCAT),
            TokenKind::Eq => bin(BinaryOp::Eq, BP_COMPARE, BP_COMPARE),
            TokenKind::Ne => bin(BinaryOp::Ne, BP_COMPARE, BP_COMPARE),
            TokenKind::Lt => bin(BinaryOp::Lt, BP_COMPARE, BP_COMPARE),
            TokenKind::Le => bin(BinaryOp::Le, BP_COMPARE, BP_COMPARE),
            TokenKind::Gt => bin(BinaryOp::Gt, BP_COMPARE, BP_COMPARE),
            TokenKind::Ge => bin(BinaryOp::Ge, BP_COMPARE, BP_COMPARE),
            TokenKind::Colon => bin(BinaryOp::Range, BP_RANGE, BP_RANGE),
            TokenKind::Comma if allow_union => bin(BinaryOp::Union, BP_UNION, BP_UNION),
            TokenKind::Percent => Some(Infix {
                lbp: BP_PERCENT,
                rbp: 0,
                kind: InfixKind::Post(PostfixOp::Percent),
            }),
            TokenKind::Hash => Some(Infix {
                lbp: BP_SPILL,
                rbp: 0,
                kind: InfixKind::Post(PostfixOp::SpillRange),
            }),
            other => {
                // Space between two operands is the intersection operator.
                if tok.space_before && other.starts_atom() {
                    Some(Infix {
                        lbp: BP_INTERSECT,
                        rbp: BP_INTERSECT,
                        kind: InfixKind::Bin(BinaryOp::Intersect, false),
                    })
                } else {
                    None
                }
            }
        }
    }

    // -- prefix / atoms (nud) ----------------------------------------------
    fn parse_prefix(&mut self, allow_union: bool) -> Result<Expr> {
        let Some(tok) = self.peek() else {
            return Err(ParseError::new(
                ParseErrorKind::UnexpectedEof,
                self.eof_span(),
            ));
        };
        let span = tok.span;
        match &tok.kind {
            TokenKind::Number(_)
            | TokenKind::Str(_)
            | TokenKind::Error(_)
            | TokenKind::Ref(_)
            | TokenKind::Brackets(_) => self.parse_simple_atom(span),
            TokenKind::Sheet(_) => self.parse_sheet_qualified(),
            // The glued-call fast path lives directly in this frame so the
            // nested-call recursion cycle skips `parse_ident_atom` entirely
            // (stack-size note on `parse_expr_bp`).
            TokenKind::Ident(_) if self.ident_is_glued_call() => {
                let t = self.bump();
                let TokenKind::Ident(name) = t.kind else {
                    unreachable!("guard checked the token is an identifier");
                };
                self.finish_call(parse_func_name(&name), t.span)
            }
            TokenKind::Ident(_) => self.parse_ident_atom(),
            TokenKind::Minus => self.parse_unary(UnaryOp::Neg, allow_union),
            TokenKind::Plus => self.parse_unary(UnaryOp::Plus, allow_union),
            TokenKind::At => self.parse_at(span, allow_union),
            TokenKind::LParen => self.parse_paren(span),
            TokenKind::LBrace => self.parse_array(span),
            _ => Err(ParseError::new(ParseErrorKind::ExpectedExpr, span)),
        }
    }

    /// Single-token atoms (kept out of `parse_prefix` to keep that frame
    /// small; see the note on `parse_expr_bp`).
    fn parse_simple_atom(&mut self, span: Span) -> Result<Expr> {
        let t = self.bump();
        let kind = match t.kind {
            TokenKind::Number(n) => ExprKind::Number(n),
            TokenKind::Str(s) => ExprKind::Text(s),
            TokenKind::Error(e) => ExprKind::Error(e),
            TokenKind::Ref(kind) => ExprKind::Ref(Reference { sheet: None, kind }),
            TokenKind::Brackets(_) => ExprKind::Unsupported {
                raw: self.raw(t.span),
                reason: REASON_STRUCTURED.to_string(),
            },
            _ => unreachable!("parse_simple_atom entered on a non-atom token"),
        };
        Ok(Expr::new(kind, span))
    }

    fn parse_at(&mut self, span: Span, allow_union: bool) -> Result<Expr> {
        self.bump();
        let inner = self.parse_expr(BP_AT, allow_union)?;
        let full = span.to(inner.span);
        Ok(Expr::new(
            ExprKind::ImplicitIntersection(Box::new(inner)),
            full,
        ))
    }

    fn parse_paren(&mut self, span: Span) -> Result<Expr> {
        self.bump();
        let inner = self.parse_expr(0, true)?;
        let close = self.expect(TokenKind::RParen, ')')?;
        let full = span.to(close.span);
        Ok(Expr::new(ExprKind::Paren(Box::new(inner)), full))
    }

    fn parse_unary(&mut self, op: UnaryOp, allow_union: bool) -> Result<Expr> {
        let t = self.bump();
        let operand = self.parse_expr(BP_UNARY, allow_union)?;
        let span = t.span.to(operand.span);
        Ok(Expr::new(
            ExprKind::Unary {
                op,
                expr: Box::new(operand),
            },
            span,
        ))
    }

    /// Non-call identifier atoms. The glued-call case (`NAME(`) is
    /// intercepted by `parse_prefix` before this is reached.
    fn parse_ident_atom(&mut self) -> Result<Expr> {
        let t = self.bump();
        let TokenKind::Ident(name) = t.kind else {
            unreachable!("parse_ident_atom entered without an identifier");
        };
        let start = t.span;

        // `NAME (` *with* a space is ambiguous — a call, or an intersection of
        // the name with a parenthesized expression. Microsoft's docs do not
        // pin which reading Excel takes, so per the no-guess rule this is a
        // hard error pending oracle probe OXP-052, never a silent choice.
        self.reject_spaced_lparen(start)?;
        // Structured reference: `NAME[...]` with no intervening space.
        if let Some(bspan) = self.next_is_brackets_glued() {
            self.bump();
            let full = start.to(bspan);
            return Ok(Expr::new(
                ExprKind::Unsupported {
                    raw: self.raw(full),
                    reason: REASON_STRUCTURED.to_string(),
                },
                full,
            ));
        }
        // Boolean literal.
        if name.eq_ignore_ascii_case("TRUE") {
            return Ok(Expr::new(ExprKind::Bool(true), start));
        }
        if name.eq_ignore_ascii_case("FALSE") {
            return Ok(Expr::new(ExprKind::Bool(false), start));
        }
        // Plain defined name.
        Ok(Expr::new(
            ExprKind::Name(NameRef { sheet: None, name }),
            start,
        ))
    }

    fn parse_sheet_qualified(&mut self) -> Result<Expr> {
        let t = self.bump();
        let TokenKind::Sheet(sheet) = t.kind else {
            unreachable!("parse_sheet_qualified entered without a sheet prefix");
        };
        let start = t.span;

        let Some(next) = self.peek() else {
            return Err(ParseError::new(
                ParseErrorKind::ExpectedSheetReference,
                self.eof_span(),
            ));
        };
        match &next.kind {
            TokenKind::Ref(kind) => {
                let kind = *kind;
                let nt = self.bump();
                let full = start.to(nt.span);
                Ok(Expr::new(
                    ExprKind::Ref(Reference {
                        sheet: Some(sheet),
                        kind,
                    }),
                    full,
                ))
            }
            TokenKind::Ident(name) => {
                let name = name.clone();
                let nt = self.bump();
                // Sheet-qualified structured reference `Sheet1!Table[...]`.
                if let Some(bspan) = self.next_is_brackets_glued() {
                    self.bump();
                    let full = start.to(bspan);
                    return Ok(Expr::new(
                        ExprKind::Unsupported {
                            raw: self.raw(full),
                            reason: REASON_STRUCTURED_SHEET.to_string(),
                        },
                        full,
                    ));
                }
                // Same OXP-052 ambiguity for sheet-qualified names.
                self.reject_spaced_lparen(start)?;
                let full = start.to(nt.span);
                Ok(Expr::new(
                    ExprKind::Name(NameRef {
                        sheet: Some(sheet),
                        name,
                    }),
                    full,
                ))
            }
            TokenKind::Brackets(_) => {
                let nt = self.bump();
                let full = start.to(nt.span);
                Ok(Expr::new(
                    ExprKind::Unsupported {
                        raw: self.raw(full),
                        reason: REASON_STRUCTURED_SHEET.to_string(),
                    },
                    full,
                ))
            }
            // Two sheet prefixes in a row (`Sheet1!Sheet2!A1`, or the
            // `Foo:Sheet1!` 3-D-span tail of `Sheet1!Foo:Sheet1!B2` where the
            // left part is not a cell). The clean same-sheet cell range
            // `Sheet1!A1:Sheet1!B2` no longer reaches here — RFC-0004 slice-1
            // (OXP-053) has the lexer back off its cell-shaped 3-D-span first
            // part, so it arrives as an ordinary `ref : sheet-qualified-ref`
            // and is normalized in `apply_infix`. What remains here is the
            // deferred slice-2 family (per-endpoint qualifiers on non-cell or
            // cross-sheet operands), still a precise reject rather than a guess
            // — see `rfcs/0004-sheet-qualified-range-endpoints.md`.
            TokenKind::Sheet(_) => Err(ParseError::new(
                ParseErrorKind::SheetQualifiedRangeEndpoints,
                start.to(next.span),
            )),
            _ => Err(ParseError::new(
                ParseErrorKind::ExpectedSheetReference,
                next.span,
            )),
        }
    }

    /// Reject `NAME (` — see [`ParseErrorKind::AmbiguousSpaceBeforeParen`].
    fn reject_spaced_lparen(&self, start: Span) -> Result<()> {
        match self.peek() {
            Some(t) if t.kind == TokenKind::LParen && t.space_before => Err(ParseError::new(
                ParseErrorKind::AmbiguousSpaceBeforeParen,
                start.to(t.span),
            )),
            _ => Ok(()),
        }
    }

    /// Whether the *current* token is an identifier immediately followed
    /// (no whitespace) by `(` — i.e. an unambiguous function call.
    fn ident_is_glued_call(&self) -> bool {
        matches!(
            self.tokens.get(self.pos + 1),
            Some(t) if t.kind == TokenKind::LParen && !t.space_before
        )
    }

    fn next_is_brackets_glued(&self) -> Option<Span> {
        match self.peek() {
            Some(t) if !t.space_before => match &t.kind {
                TokenKind::Brackets(_) => Some(t.span),
                _ => None,
            },
            _ => None,
        }
    }

    /// Parse `(args...)` after a function name. The argument loop lives
    /// directly in this frame (rather than in `parse_args`/`parse_arg`
    /// helpers) to keep the nested-call recursion cycle short — see the
    /// stack-size note on `parse_expr_bp`.
    fn finish_call(&mut self, name: FuncName, start: Span) -> Result<Expr> {
        self.bump(); // '('
        let mut args = Vec::new();
        if !matches!(self.peek_kind(), Some(TokenKind::RParen)) {
            loop {
                // An omitted argument (`IF(A1,,2)`) is an explicit Missing
                // node; `,` inside an argument list separates arguments, so
                // union is disabled for the sub-expression.
                let arg = match self.peek_kind() {
                    Some(TokenKind::Comma | TokenKind::RParen) | None => {
                        let sp = self
                            .peek()
                            .map_or_else(|| self.eof_span(), |t| Span::point(t.span.start));
                        Expr::new(ExprKind::Missing, sp)
                    }
                    _ => self.parse_expr(0, false)?,
                };
                args.push(arg);
                if matches!(self.peek_kind(), Some(TokenKind::Comma)) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        let close = self.expect(TokenKind::RParen, ')')?;
        let full = start.to(close.span);
        Ok(Expr::new(ExprKind::Call { name, args }, full))
    }

    fn parse_array(&mut self, open: Span) -> Result<Expr> {
        self.bump(); // '{'
        let mut rows: Vec<Vec<Expr>> = Vec::new();
        let mut row: Vec<Expr> = Vec::new();
        loop {
            row.push(self.parse_array_const()?);
            match self.peek_kind() {
                Some(TokenKind::Comma) => {
                    self.bump();
                }
                Some(TokenKind::Semicolon) => {
                    self.bump();
                    rows.push(std::mem::take(&mut row));
                }
                Some(TokenKind::RBrace) => {
                    rows.push(std::mem::take(&mut row));
                    break;
                }
                Some(_) => {
                    return Err(ParseError::new(
                        ParseErrorKind::NonConstantInArray,
                        self.peek().map_or(open, |t| t.span),
                    ));
                }
                None => {
                    return Err(ParseError::new(
                        ParseErrorKind::ExpectedClose('}'),
                        self.eof_span(),
                    ));
                }
            }
        }
        let close = self.expect(TokenKind::RBrace, '}')?;
        // Rectangularity check.
        let width = rows[0].len();
        if rows.iter().any(|r| r.len() != width) {
            return Err(ParseError::new(
                ParseErrorKind::RaggedArray,
                open.to(close.span),
            ));
        }
        Ok(Expr::new(ExprKind::Array(rows), open.to(close.span)))
    }

    /// Parse a single array-literal element: a scalar constant only (optional
    /// sign before a number, a string, a boolean, or an error literal).
    fn parse_array_const(&mut self) -> Result<Expr> {
        let start_span = self.peek().map_or_else(|| self.eof_span(), |t| t.span);
        let mut negate = false;
        let mut signed = false;
        match self.peek_kind() {
            Some(TokenKind::Minus) => {
                self.bump();
                negate = true;
                signed = true;
            }
            Some(TokenKind::Plus) => {
                self.bump();
                signed = true;
            }
            _ => {}
        }
        let Some(tok) = self.peek() else {
            return Err(ParseError::new(
                ParseErrorKind::NonConstantInArray,
                self.eof_span(),
            ));
        };
        let tspan = tok.span;
        let full = start_span.to(tspan);
        match &tok.kind {
            TokenKind::Number(n) => {
                let v = if negate { -*n } else { *n };
                self.bump();
                Ok(Expr::new(ExprKind::Number(v), full))
            }
            _ if signed => Err(ParseError::new(ParseErrorKind::NonConstantInArray, full)),
            TokenKind::Str(s) => {
                let s = s.clone();
                self.bump();
                Ok(Expr::new(ExprKind::Text(s), tspan))
            }
            TokenKind::Error(e) => {
                let e = *e;
                self.bump();
                Ok(Expr::new(ExprKind::Error(e), tspan))
            }
            TokenKind::Ident(name) if name.eq_ignore_ascii_case("TRUE") => {
                self.bump();
                Ok(Expr::new(ExprKind::Bool(true), tspan))
            }
            TokenKind::Ident(name) if name.eq_ignore_ascii_case("FALSE") => {
                self.bump();
                Ok(Expr::new(ExprKind::Bool(false), tspan))
            }
            _ => Err(ParseError::new(ParseErrorKind::NonConstantInArray, tspan)),
        }
    }

    fn expect(&mut self, kind: TokenKind, close: char) -> Result<Token> {
        match self.peek() {
            Some(t) if t.kind == kind => Ok(self.bump()),
            Some(t) => Err(ParseError::new(
                ParseErrorKind::ExpectedClose(close),
                t.span,
            )),
            None => Err(ParseError::new(
                ParseErrorKind::ExpectedClose(close),
                self.eof_span(),
            )),
        }
    }
}

/// Strip `_xlfn.` / `_xlws.` prefixes and uppercase the bare function name.
fn parse_func_name(raw: &str) -> FuncName {
    let mut xlfn = false;
    let mut xlws = false;
    let mut rest = raw;
    loop {
        if let Some(r) = rest.strip_prefix("_xlfn.") {
            xlfn = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("_xlws.") {
            xlws = true;
            rest = r;
        } else {
            break;
        }
    }
    FuncName {
        canonical: rest.to_ascii_uppercase(),
        xlfn,
        xlws,
    }
}

/// When an operand of the range operator `:` is a bare name that spells a
/// column (`A`, `AA`) or a bare integer that names a row (`1`), promote it to a
/// whole-column / whole-row reference (`A:A`, `1:1`), preserving any sheet
/// qualification.
fn promote_range_operand(e: Expr) -> Expr {
    let span = e.span;
    match e.kind {
        ExprKind::Name(NameRef { sheet, name }) => {
            if let Some(col) = col_to_num(&name) {
                Expr::new(
                    ExprKind::Ref(Reference {
                        sheet,
                        kind: RefKind::Col(Axis {
                            index: col,
                            absolute: false,
                        }),
                    }),
                    span,
                )
            } else {
                Expr::new(ExprKind::Name(NameRef { sheet, name }), span)
            }
        }
        ExprKind::Number(n) if is_row_integer(n) => Expr::new(
            ExprKind::Ref(Reference {
                sheet: None,
                kind: RefKind::Row(Axis {
                    index: n as u32,
                    absolute: false,
                }),
            }),
            span,
        ),
        other => Expr::new(other, span),
    }
}

fn is_row_integer(n: f64) -> bool {
    n.fract() == 0.0 && n >= 1.0 && n <= f64::from(crate::ast::MAX_ROW)
}

/// Whether the leftmost atom of a range operand carries its own sheet (or
/// 3-D, or workbook-global) qualifier — the shape that, as the *right* operand
/// of `:`, triggers RFC-0004's same-sheet normalization / slice-2 rejection in
/// `apply_infix` (probe OXP-053).
///
/// Looks through grouping and operator wrappers to the leftmost leaf so
/// spellings like `A1:(Sheet1!B2)` or `A1:@Sheet1!B2` cannot smuggle the
/// (still-deferred) per-endpoint-qualifier production past the check. Recursion
/// is bounded by the parser's own [`MAX_NESTING_DEPTH`].
fn leading_sheet_qualifier(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Ref(r) => r.sheet.is_some(),
        ExprKind::Name(n) => n.sheet.is_some(),
        // Sheet-qualified structured refs are Unsupported nodes; their
        // qualification is only visible via the dedicated reason constant.
        ExprKind::Unsupported { reason, .. } => reason == REASON_STRUCTURED_SHEET,
        ExprKind::Paren(inner) | ExprKind::ImplicitIntersection(inner) => {
            leading_sheet_qualifier(inner)
        }
        ExprKind::Unary { expr, .. } | ExprKind::Postfix { expr, .. } => {
            leading_sheet_qualifier(expr)
        }
        ExprKind::Binary { lhs, .. } => leading_sheet_qualifier(lhs),
        _ => false,
    }
}

/// RFC-0004 slice-1 (OXP-053, RUN-2026-07-11-oracle01): normalize a range whose
/// two endpoints are both plain single-sheet-qualified references naming the
/// *same* sheet. `Sheet1!A1:Sheet1!B2` is exactly the already-supported
/// left-qualified range `Sheet1!A1:B2` in Excel (`SUM(Sheet1!A1:Sheet1!B2)` →
/// 10.0), so the right endpoint's qualifier is redundant; this returns that
/// right endpoint with the qualifier dropped.
///
/// Returns `None` for every other shape — a different sheet, a 3-D or
/// workbook-global qualifier on either side, a bare left endpoint, or a
/// non-`Ref` operand (parenthesized/`@`-wrapped/structured/name) — leaving
/// those to the deferred slice-2 rejection. Both operands are the
/// already-`promote_range_operand`-normalized forms, so a whole-column/-row
/// endpoint written as `Sheet1!A:Sheet1!A` compares by its promoted `Ref`.
fn normalize_same_sheet_range(lhs: &Expr, rhs: &Expr) -> Option<Expr> {
    let (ExprKind::Ref(l), ExprKind::Ref(r)) = (&lhs.kind, &rhs.kind) else {
        return None;
    };
    let (Some(ls), Some(rs)) = (&l.sheet, &r.sheet) else {
        return None;
    };
    if !same_single_sheet(ls, rs) {
        return None;
    }
    Some(Expr::new(
        ExprKind::Ref(Reference {
            sheet: None,
            kind: r.kind,
        }),
        rhs.span,
    ))
}

/// Whether two sheet qualifiers denote the same single worksheet: neither is a
/// 3-D span (`last` set) or the workbook-global scope, and their names match
/// case-insensitively (Excel sheet names fold case; en-US/ASCII only per the
/// v1 locale non-goal).
fn same_single_sheet(a: &SheetRef, b: &SheetRef) -> bool {
    !a.workbook_global
        && !b.workbook_global
        && a.last.is_none()
        && b.last.is_none()
        && a.first.eq_ignore_ascii_case(&b.first)
}

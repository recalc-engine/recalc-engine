//! The formula abstract syntax tree.
//!
//! Every node is spanned ([`Expr::span`]) so diagnostics can point back into
//! the source. Owned `String`s are used throughout for v0 simplicity; string
//! interning (sharing storage with `xl-value`'s [`xl_value::Text`]) is a later
//! optimization gated behind an RFC.
//!
//! Two textual renderings are provided:
//! - [`Expr`]'s [`std::fmt::Display`] emits a **canonical formula string**
//!   (uppercase function names and column letters, canonical `$` markers).
//!   Because grouping parentheses are preserved as [`ExprKind::Paren`] nodes,
//!   Display round-trips the structure of any parsed formula.
//! - [`Expr::to_sexpr`] emits a span-free, fully-parenthesized structural dump
//!   used by the golden AST tests to pin down operator precedence and
//!   associativity.

use crate::span::Span;
use core::fmt;
use xl_value::ErrorKind;

/// A spanned expression node — the universal AST element.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    /// The node payload.
    pub kind: ExprKind,
    /// Byte range in the source formula this node was parsed from.
    pub span: Span,
}

impl Expr {
    /// Construct a spanned expression.
    #[must_use]
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// Render a span-free, fully-parenthesized structural dump.
    ///
    /// This is the golden-test oracle for tree shape: it makes precedence and
    /// associativity explicit (e.g. `-2^2` → `(^ (u- 2) 2)`).
    #[must_use]
    pub fn to_sexpr(&self) -> String {
        let mut s = String::new();
        self.write_sexpr(&mut s);
        s
    }

    fn write_sexpr(&self, out: &mut String) {
        match &self.kind {
            ExprKind::Number(n) => out.push_str(&fmt_num(*n)),
            ExprKind::Text(t) => {
                out.push('"');
                out.push_str(t);
                out.push('"');
            }
            ExprKind::Bool(b) => out.push_str(if *b { "TRUE" } else { "FALSE" }),
            ExprKind::Error(e) => out.push_str(e.as_str()),
            ExprKind::Missing => out.push_str("<empty>"),
            ExprKind::Ref(r) => {
                out.push_str("(ref ");
                out.push_str(&r.to_string());
                out.push(')');
            }
            ExprKind::Name(n) => {
                out.push_str("(name ");
                out.push_str(&n.to_string());
                out.push(')');
            }
            ExprKind::Unary { op, expr } => {
                out.push('(');
                out.push_str(op.sexpr_tag());
                out.push(' ');
                expr.write_sexpr(out);
                out.push(')');
            }
            ExprKind::Postfix { op, expr } => {
                out.push('(');
                out.push_str(op.sexpr_tag());
                out.push(' ');
                expr.write_sexpr(out);
                out.push(')');
            }
            ExprKind::Binary { op, lhs, rhs } => {
                out.push('(');
                out.push_str(op.sexpr_tag());
                out.push(' ');
                lhs.write_sexpr(out);
                out.push(' ');
                rhs.write_sexpr(out);
                out.push(')');
            }
            ExprKind::Call { name, args } => {
                out.push_str("(call ");
                out.push_str(&name.canonical);
                if name.xlfn {
                    out.push_str(" :xlfn");
                }
                if name.xlws {
                    out.push_str(" :xlws");
                }
                for a in args {
                    out.push(' ');
                    a.write_sexpr(out);
                }
                out.push(')');
            }
            ExprKind::Array(rows) => {
                out.push_str("(array");
                for row in rows {
                    out.push_str(" (row");
                    for e in row {
                        out.push(' ');
                        e.write_sexpr(out);
                    }
                    out.push(')');
                }
                out.push(')');
            }
            ExprKind::Paren(inner) => {
                out.push_str("(paren ");
                inner.write_sexpr(out);
                out.push(')');
            }
            ExprKind::ImplicitIntersection(inner) => {
                out.push_str("(@ ");
                inner.write_sexpr(out);
                out.push(')');
            }
            ExprKind::Unsupported { raw, reason } => {
                out.push_str("(unsupported ");
                out.push_str(&format!("{raw:?} {reason:?}"));
                out.push(')');
            }
        }
    }
}

/// The payload of an [`Expr`].
#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    /// A numeric literal (decimal or scientific). Never carries a sign — a
    /// leading `-`/`+` is a [`UnaryOp`].
    Number(f64),
    /// A string literal (with `""` un-doubled to a single quote).
    Text(String),
    /// A boolean literal `TRUE`/`FALSE`.
    Bool(bool),
    /// An error literal such as `#DIV/0!`.
    Error(ErrorKind),
    /// A missing/empty function argument, as in `IF(A1,,2)`.
    Missing,
    /// A reference (cell, whole column/row, or R1C1), optionally
    /// sheet-qualified or 3-D.
    Ref(Reference),
    /// A defined name (optionally sheet-qualified).
    Name(NameRef),
    /// A prefix unary operator applied to an operand.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        expr: Box<Expr>,
    },
    /// A postfix operator applied to an operand (`%` or spill `#`).
    Postfix {
        /// The operator.
        op: PostfixOp,
        /// The operand.
        expr: Box<Expr>,
    },
    /// A binary operator applied to two operands.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
    },
    /// A function call `NAME(args...)`.
    Call {
        /// The (canonicalized) function name.
        name: FuncName,
        /// The argument list; a [`ExprKind::Missing`] element is an omitted
        /// argument.
        args: Vec<Expr>,
    },
    /// An array literal `{1,2;3,4}` — a vector of rows, each a vector of
    /// scalar-constant expressions.
    Array(Vec<Vec<Expr>>),
    /// A parenthesized grouping. Retained so [`Display`](std::fmt::Display)
    /// can faithfully reproduce the source and so union `,` context is
    /// unambiguous.
    Paren(Box<Expr>),
    /// An implicit-intersection prefix `@expr`.
    ImplicitIntersection(Box<Expr>),
    /// A construct that is lexed correctly but intentionally not parsed to a
    /// structured node in v0 (e.g. structured table references). Preserves the
    /// raw source so nothing is silently dropped.
    Unsupported {
        /// The exact source text of the unsupported construct.
        raw: String,
        /// Why it is unsupported (and, where relevant, the follow-up task).
        reason: String,
    },
}

/// Prefix unary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    /// Arithmetic negation `-x`.
    Neg,
    /// Unary plus `+x` (identity; preserved for round-tripping).
    Plus,
}

impl UnaryOp {
    const fn sexpr_tag(self) -> &'static str {
        match self {
            UnaryOp::Neg => "u-",
            UnaryOp::Plus => "u+",
        }
    }
    const fn symbol(self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Plus => "+",
        }
    }
}

/// Postfix operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostfixOp {
    /// Percent `x%` (divides by 100).
    Percent,
    /// Spill-range `ref#` (the whole dynamic-array region anchored at `ref`).
    SpillRange,
}

impl PostfixOp {
    const fn sexpr_tag(self) -> &'static str {
        match self {
            PostfixOp::Percent => "%",
            PostfixOp::SpillRange => "spill",
        }
    }
    const fn symbol(self) -> &'static str {
        match self {
            PostfixOp::Percent => "%",
            PostfixOp::SpillRange => "#",
        }
    }
}

/// Binary operators, including the reference operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    /// Range `a:b`.
    Range,
    /// Intersection (a single space between references).
    Intersect,
    /// Union `a,b` (only inside a parenthesized reference expression).
    Union,
    /// Exponentiation `a^b` (left-associative in Excel).
    Pow,
    /// Multiplication `a*b`.
    Mul,
    /// Division `a/b`.
    Div,
    /// Addition `a+b`.
    Add,
    /// Subtraction `a-b`.
    Sub,
    /// Text concatenation `a&b`.
    Concat,
    /// Equal `a=b`.
    Eq,
    /// Not equal `a<>b`.
    Ne,
    /// Less than `a<b`.
    Lt,
    /// Less than or equal `a<=b`.
    Le,
    /// Greater than `a>b`.
    Gt,
    /// Greater than or equal `a>=b`.
    Ge,
}

impl BinaryOp {
    const fn sexpr_tag(self) -> &'static str {
        match self {
            BinaryOp::Range => ":",
            BinaryOp::Intersect => "isect",
            BinaryOp::Union => "union",
            BinaryOp::Pow => "^",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Concat => "&",
            BinaryOp::Eq => "=",
            BinaryOp::Ne => "<>",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
        }
    }
    /// The infix symbol used when rendering canonical formula text. Intersect
    /// renders as a space and is handled specially by [`Display`].
    const fn symbol(self) -> &'static str {
        match self {
            BinaryOp::Range => ":",
            BinaryOp::Intersect => " ",
            BinaryOp::Union => ",",
            BinaryOp::Pow => "^",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Concat => "&",
            BinaryOp::Eq => "=",
            BinaryOp::Ne => "<>",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
        }
    }
}

/// A canonicalized function name plus the future-function prefixes Excel writes
/// into the stored formula.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuncName {
    /// The uppercased bare name, e.g. `XLOOKUP`.
    pub canonical: String,
    /// The name had an `_xlfn.` prefix (a function newer than the file format
    /// baseline). Semantically identical to `canonical`; the flag preserves
    /// the fact for faithful round-tripping.
    pub xlfn: bool,
    /// The name had an `_xlws.` prefix (a worksheet-only future function).
    pub xlws: bool,
}

impl fmt::Display for FuncName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.xlfn {
            f.write_str("_xlfn.")?;
        }
        if self.xlws {
            f.write_str("_xlws.")?;
        }
        f.write_str(&self.canonical)
    }
}

/// One axis (column or row) of an A1 reference: a 1-based index and whether it
/// is `$`-anchored (absolute).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Axis {
    /// 1-based index (column A = 1, row 1 = 1).
    pub index: u32,
    /// Whether the axis is absolute (had a `$`).
    pub absolute: bool,
}

/// An R1C1 axis component: relative (bracketed offset, or bare `R`/`C` = 0) or
/// absolute (a bare number).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum R1C1Axis {
    /// A relative offset, e.g. `R[2]` → `Relative(2)`, bare `R` → `Relative(0)`.
    Relative(i32),
    /// An absolute 1-based index, e.g. `R2` → `Absolute(2)`.
    Absolute(u32),
}

/// An R1C1 reference. Either axis may be absent to denote a whole row (`R5`,
/// column absent) or whole column (`C5`, row absent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct R1C1Ref {
    /// The row component, if present.
    pub row: Option<R1C1Axis>,
    /// The column component, if present.
    pub col: Option<R1C1Axis>,
}

/// The grid-addressing part of a reference (sheet qualification is separate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    /// A single A1 cell, e.g. `$A$1`.
    Cell(Axis, Axis),
    /// A whole A1 column, e.g. `A` (only as an operand of `:`, as in `A:A`).
    Col(Axis),
    /// A whole A1 row, e.g. `1` (only as an operand of `:`, as in `1:1`).
    Row(Axis),
    /// An R1C1 reference.
    R1C1(R1C1Ref),
}

/// A sheet qualification: a single sheet (`Sheet1!`), a 3-D span
/// (`Sheet1:Sheet3!`), or the workbook-global scope (`!Name`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetRef {
    /// The first (or only) sheet name, unescaped. Empty for workbook-global.
    pub first: String,
    /// The last sheet name for a 3-D span, unescaped.
    pub last: Option<String>,
    /// Whether the name(s) were quoted in the source (needed special chars).
    pub quoted: bool,
    /// Whether this is the workbook-global scope prefix (`!Name`).
    pub workbook_global: bool,
}

impl fmt::Display for SheetRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.workbook_global && self.first.is_empty() && self.last.is_none() {
            return f.write_str("!");
        }
        write_sheet_name(f, &self.first, self.quoted)?;
        if let Some(last) = &self.last {
            f.write_str(":")?;
            write_sheet_name(f, last, self.quoted)?;
        }
        f.write_str("!")
    }
}

fn write_sheet_name(f: &mut fmt::Formatter<'_>, name: &str, quoted: bool) -> fmt::Result {
    if quoted {
        f.write_str("'")?;
        for c in name.chars() {
            if c == '\'' {
                f.write_str("''")?;
            } else {
                f.write_fmt(format_args!("{c}"))?;
            }
        }
        f.write_str("'")
    } else {
        f.write_str(name)
    }
}

/// A reference: sheet qualification plus a grid-addressing [`RefKind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    /// Optional sheet / 3-D / workbook-global qualification.
    pub sheet: Option<SheetRef>,
    /// The cell / column / row / R1C1 address.
    pub kind: RefKind,
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(sheet) = &self.sheet {
            write!(f, "{sheet}")?;
        }
        match self.kind {
            RefKind::Cell(col, row) => {
                write_a1_col(f, col)?;
                write_a1_row(f, row)
            }
            RefKind::Col(col) => write_a1_col(f, col),
            RefKind::Row(row) => write_a1_row(f, row),
            RefKind::R1C1(r) => write_r1c1(f, r),
        }
    }
}

fn write_a1_col(f: &mut fmt::Formatter<'_>, col: Axis) -> fmt::Result {
    if col.absolute {
        f.write_str("$")?;
    }
    f.write_str(&num_to_col(col.index))
}

fn write_a1_row(f: &mut fmt::Formatter<'_>, row: Axis) -> fmt::Result {
    if row.absolute {
        f.write_str("$")?;
    }
    write!(f, "{}", row.index)
}

fn write_r1c1(f: &mut fmt::Formatter<'_>, r: R1C1Ref) -> fmt::Result {
    if let Some(row) = r.row {
        f.write_str("R")?;
        write_r1c1_axis(f, row)?;
    }
    if let Some(col) = r.col {
        f.write_str("C")?;
        write_r1c1_axis(f, col)?;
    }
    Ok(())
}

fn write_r1c1_axis(f: &mut fmt::Formatter<'_>, a: R1C1Axis) -> fmt::Result {
    match a {
        R1C1Axis::Relative(0) => Ok(()),
        R1C1Axis::Relative(n) => write!(f, "[{n}]"),
        R1C1Axis::Absolute(n) => write!(f, "{n}"),
    }
}

/// A defined name, optionally sheet-qualified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameRef {
    /// Optional sheet / workbook-global scope.
    pub sheet: Option<SheetRef>,
    /// The name text as written (case preserved).
    pub name: String,
}

impl fmt::Display for NameRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(sheet) = &self.sheet {
            write!(f, "{sheet}")?;
        }
        f.write_str(&self.name)
    }
}

// ---------------------------------------------------------------------------
// Canonical Display for the whole tree.
// ---------------------------------------------------------------------------

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ExprKind::Number(n) => f.write_str(&fmt_num(*n)),
            ExprKind::Text(t) => {
                f.write_str("\"")?;
                for c in t.chars() {
                    if c == '"' {
                        f.write_str("\"\"")?;
                    } else {
                        f.write_fmt(format_args!("{c}"))?;
                    }
                }
                f.write_str("\"")
            }
            ExprKind::Bool(b) => f.write_str(if *b { "TRUE" } else { "FALSE" }),
            ExprKind::Error(e) => f.write_str(e.as_str()),
            ExprKind::Missing => Ok(()),
            ExprKind::Ref(r) => write!(f, "{r}"),
            ExprKind::Name(n) => write!(f, "{n}"),
            ExprKind::Unary { op, expr } => write!(f, "{}{expr}", op.symbol()),
            ExprKind::Postfix { op, expr } => write!(f, "{expr}{}", op.symbol()),
            ExprKind::Binary { op, lhs, rhs } => {
                // Intersection renders as the single space it is spelled with;
                // every other operator renders with its symbol and no padding
                // (Excel's canonical stored form).
                write!(f, "{lhs}{}{rhs}", op.symbol())
            }
            ExprKind::Call { name, args } => {
                write!(f, "{name}(")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{a}")?;
                }
                f.write_str(")")
            }
            ExprKind::Array(rows) => {
                f.write_str("{")?;
                for (ri, row) in rows.iter().enumerate() {
                    if ri > 0 {
                        f.write_str(";")?;
                    }
                    for (ci, e) in row.iter().enumerate() {
                        if ci > 0 {
                            f.write_str(",")?;
                        }
                        write!(f, "{e}")?;
                    }
                }
                f.write_str("}")
            }
            ExprKind::Paren(inner) => write!(f, "({inner})"),
            ExprKind::ImplicitIntersection(inner) => write!(f, "@{inner}"),
            ExprKind::Unsupported { raw, .. } => f.write_str(raw),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared column/number helpers (also used by the lexer).
// ---------------------------------------------------------------------------

/// Maximum column index (`XFD`).
pub(crate) const MAX_COL: u32 = 16_384;
/// Maximum row index.
pub(crate) const MAX_ROW: u32 = 1_048_576;

/// Convert A1 column letters to a 1-based index, validating `≤ XFD`.
/// Returns `None` for non-letters or out-of-range columns.
pub(crate) fn col_to_num(s: &str) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut n: u32 = 0;
    for c in s.chars() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        let d = (c.to_ascii_uppercase() as u32) - ('A' as u32) + 1;
        n = n.checked_mul(26)?.checked_add(d)?;
        if n > MAX_COL {
            return None;
        }
    }
    Some(n)
}

/// Convert a 1-based column index to its letters (`1` → `A`, `27` → `AA`).
pub(crate) fn num_to_col(mut n: u32) -> String {
    let mut buf = Vec::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        buf.push(b'A' + rem as u8);
        n = (n - 1) / 26;
    }
    buf.reverse();
    // Safe: all bytes are ASCII uppercase letters.
    String::from_utf8(buf).unwrap_or_default()
}

/// Format an `f64` literal the way canonical formula text spells it (no
/// trailing `.0`, shortest round-trip form).
///
/// Note: this is Rust's shortest round-trip formatting, which can differ
/// *cosmetically* from Excel's own rendering at extreme magnitudes (e.g.
/// `1.5E+308` prints as its long decimal expansion). The value is identical
/// and re-parses exactly; Excel-fidelity *display* formatting is `to_text`'s
/// job in `xl-value`, not the AST serializer's.
fn fmt_num(n: f64) -> String {
    format!("{n}")
}

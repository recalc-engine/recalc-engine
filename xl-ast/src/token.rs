//! Lexer tokens.
//!
//! Whitespace is **not** emitted as its own token; instead each token records
//! whether whitespace preceded it ([`Token::space_before`]). The parser uses
//! that flag to decide when a space is the intersection operator (between two
//! reference operands) versus insignificant padding — see the module docs on
//! `parser`.

use crate::ast::{RefKind, SheetRef};
use crate::span::Span;
use xl_value::ErrorKind;

/// A lexical token with its source span and preceding-whitespace flag.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    /// The token payload.
    pub kind: TokenKind,
    /// Byte range in the source.
    pub span: Span,
    /// Whether one or more whitespace characters immediately preceded this
    /// token (significant only as the intersection operator).
    pub space_before: bool,
}

/// The kinds of lexical token the formula lexer produces.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    /// A numeric literal (unsigned; a leading sign is a separate operator).
    Number(f64),
    /// A string literal, with `""` already un-doubled to `"`.
    Str(String),
    /// An error literal such as `#DIV/0!`.
    Error(ErrorKind),
    /// A concrete reference (A1 cell / whole col / whole row, or R1C1).
    Ref(RefKind),
    /// A sheet (or 3-D, or workbook-global) prefix, `!` already consumed.
    Sheet(SheetRef),
    /// An identifier: a defined name, function name, or a bare column/row
    /// word (`TRUE`/`FALSE` are recognized from identifiers by the parser).
    Ident(String),
    /// A balanced structured-reference bracket group, raw source incl. `[]`.
    Brackets(String),

    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `^`
    Caret,
    /// `%`
    Percent,
    /// `&`
    Amp,
    /// `=`
    Eq,
    /// `<>`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `:`
    Colon,
    /// `,`
    Comma,
    /// `;`
    Semicolon,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `@`
    At,
    /// `#` used as the spill-range postfix (not part of an error literal).
    Hash,
}

impl TokenKind {
    /// Whether this token can begin an operand (an atom in the grammar). Used
    /// by the parser to detect space-as-intersection and adjacency errors.
    #[must_use]
    pub fn starts_atom(&self) -> bool {
        matches!(
            self,
            TokenKind::Number(_)
                | TokenKind::Str(_)
                | TokenKind::Error(_)
                | TokenKind::Ref(_)
                | TokenKind::Sheet(_)
                | TokenKind::Ident(_)
                | TokenKind::Brackets(_)
                | TokenKind::LParen
                | TokenKind::LBrace
                | TokenKind::At
                | TokenKind::Minus
                | TokenKind::Plus
        )
    }
}

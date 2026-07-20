//! The formula lexer.
//!
//! ## Provenance
//! Token shapes follow ECMA-376 (ISO/IEC 29500) §18.17.2 "Syntax and
//! grammar" for the formula language, cross-checked against Microsoft Learn
//! "Calculation operators and precedence in Excel". Behavior that cannot be
//! pinned from those documents is never guessed — it is either rejected or
//! carried through as [`crate::ExprKind::Unsupported`]. GPL spreadsheet source
//! is forbidden reading (`implementation-plan.md` §9).
//!
//! ## Whitespace
//! Whitespace is significant in Excel **only** as the intersection operator
//! between two reference-yielding operands. The lexer therefore skips it but
//! records [`Token::space_before`]; the parser decides whether a given space
//! is an operator.
//!
//! ## Reference disambiguation
//! `A1` lexes as a cell reference, but a bare `A` or `1` (needed for `A:A` /
//! `1:1`) is ambiguous with a name / number and is left as [`TokenKind::Ident`]
//! / [`TokenKind::Number`]; the parser promotes them to whole-column / -row
//! references when they are operands of `:`. A `$` makes a partial reference
//! (`$A`, `$1`) unambiguous, so those are lexed as references directly. A word
//! that is followed by `!` (optionally through a `:name` 3-D span) is a sheet
//! prefix.

use crate::ast::{Axis, R1C1Axis, R1C1Ref, RefKind, SheetRef, col_to_num};
use crate::error::{ParseError, ParseErrorKind, Result};
use crate::span::Span;
use crate::token::{Token, TokenKind};
use xl_value::ErrorKind;

/// Which reference dialect the formula uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseMode {
    /// A1-style references (default).
    A1,
    /// R1C1-style references.
    R1C1,
}

/// Error literals recognized by the lexer, longest-first so that e.g. `#N/A`
/// is not shadowed by a shorter prefix.
const ERROR_LITERALS: &[(&str, ErrorKind)] = &[
    ("#GETTING_DATA", ErrorKind::GettingData),
    ("#DIV/0!", ErrorKind::Div0),
    ("#VALUE!", ErrorKind::Value),
    ("#SPILL!", ErrorKind::Spill),
    ("#NAME?", ErrorKind::Name),
    ("#NULL!", ErrorKind::Null),
    ("#CALC!", ErrorKind::Calc),
    ("#REF!", ErrorKind::Ref),
    ("#NUM!", ErrorKind::Num),
    ("#N/A", ErrorKind::Na),
];

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    mode: ParseMode,
    out: Vec<Token>,
}

/// A scanned-but-unvalidated R1C1 axis component (digits kept as text so
/// overflow is detected during validation instead of being clamped).
struct RawR1c1Axis {
    /// The digit run (empty for a bare `R`/`C`).
    digits: String,
    /// Whether a `-` sign was present inside brackets.
    neg: bool,
    /// Whether the component was bracketed (relative) or bare (absolute).
    bracketed: bool,
    /// Span of the component (excluding the `R`/`C` letter).
    span: Span,
}

/// Tokenize a formula string in the given [`ParseMode`].
///
/// # Errors
/// Returns a [`ParseError`] on any malformed token (unterminated string /
/// sheet name / bracket, out-of-range reference, invalid number, or an
/// unexpected character).
pub fn lex(src: &str, mode: ParseMode) -> Result<Vec<Token>> {
    let mut lx = Lexer {
        src,
        pos: 0,
        mode,
        out: Vec::new(),
    };
    lx.run()?;
    Ok(lx.out)
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_at(&self, byte_off: usize) -> Option<char> {
        self.src[self.pos + byte_off..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) -> bool {
        let mut seen = false;
        while let Some(c) = self.peek() {
            if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
                self.pos += c.len_utf8();
                seen = true;
            } else {
                break;
            }
        }
        seen
    }

    fn run(&mut self) -> Result<()> {
        loop {
            let space_before = self.skip_ws();
            let start = self.pos;
            let Some(c) = self.peek() else { break };
            let kind = self.lex_one(c, start)?;
            let span = Span::new(start, self.pos);
            self.out.push(Token {
                kind,
                span,
                space_before,
            });
        }
        Ok(())
    }

    fn lex_one(&mut self, c: char, start: usize) -> Result<TokenKind> {
        match c {
            '"' => self.lex_string(start),
            '\'' => self.lex_quoted_sheet(start),
            '!' => {
                self.bump();
                Ok(TokenKind::Sheet(SheetRef {
                    first: String::new(),
                    last: None,
                    quoted: false,
                    workbook_global: true,
                }))
            }
            '#' => self.lex_hash(start),
            '[' => self.lex_brackets(start),
            '{' => {
                self.bump();
                Ok(TokenKind::LBrace)
            }
            '}' => {
                self.bump();
                Ok(TokenKind::RBrace)
            }
            '(' => {
                self.bump();
                Ok(TokenKind::LParen)
            }
            ')' => {
                self.bump();
                Ok(TokenKind::RParen)
            }
            ',' => {
                self.bump();
                Ok(TokenKind::Comma)
            }
            ';' => {
                self.bump();
                Ok(TokenKind::Semicolon)
            }
            ':' => {
                self.bump();
                Ok(TokenKind::Colon)
            }
            '+' => {
                self.bump();
                Ok(TokenKind::Plus)
            }
            '-' => {
                self.bump();
                Ok(TokenKind::Minus)
            }
            '*' => {
                self.bump();
                Ok(TokenKind::Star)
            }
            '/' => {
                self.bump();
                Ok(TokenKind::Slash)
            }
            '^' => {
                self.bump();
                Ok(TokenKind::Caret)
            }
            '%' => {
                self.bump();
                Ok(TokenKind::Percent)
            }
            '&' => {
                self.bump();
                Ok(TokenKind::Amp)
            }
            '@' => {
                self.bump();
                Ok(TokenKind::At)
            }
            '=' => {
                self.bump();
                Ok(TokenKind::Eq)
            }
            '<' => {
                self.bump();
                if self.eat('=') {
                    Ok(TokenKind::Le)
                } else if self.eat('>') {
                    Ok(TokenKind::Ne)
                } else {
                    Ok(TokenKind::Lt)
                }
            }
            '>' => {
                self.bump();
                if self.eat('=') {
                    Ok(TokenKind::Ge)
                } else {
                    Ok(TokenKind::Gt)
                }
            }
            '$' => self.lex_reference_or_error(start),
            '0'..='9' => self.lex_number(start),
            '.' if matches!(self.peek_at(1), Some('0'..='9')) => self.lex_number(start),
            // Word start: ASCII letter / `_`, OR a non-ASCII **letter**. The
            // leading-non-ASCII-letter case was pinned ACCEPT by OXP-211's COM
            // Name-Manager probe (RUN 2026-07-16; leading `í`/`Ω`/`中` each
            // define+resolve a name), so it is admitted here via [`is_name_start`].
            // A leading non-letter (symbol `°`, combining mark) is NOT admitted
            // and falls through to the loud `UnexpectedChar` below — Excel
            // rejected a leading combining mark, and precise symbol/mark classes
            // need Unicode-category data we cannot take a dep on. See `read_ident_run`.
            c if is_name_start(c) => self.lex_word_like(start),
            other => {
                self.bump();
                Err(ParseError::new(
                    ParseErrorKind::UnexpectedChar(other),
                    Span::new(start, self.pos),
                ))
            }
        }
    }

    // --- string literal ----------------------------------------------------
    fn lex_string(&mut self, start: usize) -> Result<TokenKind> {
        self.bump(); // opening quote
        let mut s = String::new();
        loop {
            match self.bump() {
                None => {
                    return Err(ParseError::new(
                        ParseErrorKind::UnterminatedString,
                        Span::new(start, self.pos),
                    ));
                }
                Some('"') => {
                    if self.peek() == Some('"') {
                        self.bump();
                        s.push('"');
                    } else {
                        return Ok(TokenKind::Str(s));
                    }
                }
                Some(c) => s.push(c),
            }
        }
    }

    // --- quoted sheet prefix ('Sheet Name'! or 'A':'B'!) -------------------
    fn lex_quoted_sheet(&mut self, start: usize) -> Result<TokenKind> {
        match self.try_sheet_prefix()? {
            Some(sheet) => Ok(TokenKind::Sheet(sheet)),
            None => Err(ParseError::new(
                ParseErrorKind::UnexpectedQuotedName,
                Span::new(start, self.pos),
            )),
        }
    }

    // --- `#` : error literal or spill postfix ------------------------------
    fn lex_hash(&mut self, start: usize) -> Result<TokenKind> {
        // Compare on bytes: error literals are pure ASCII, so this both
        // avoids char-boundary panics on multibyte input and is case-folding.
        let rest = &self.src.as_bytes()[start..];
        for (lit, kind) in ERROR_LITERALS {
            if let Some(head) = rest.get(..lit.len())
                && head.eq_ignore_ascii_case(lit.as_bytes())
            {
                self.pos = start + lit.len();
                return Ok(TokenKind::Error(*kind));
            }
        }
        // Not an error literal: the spill-range postfix `#`.
        self.bump();
        Ok(TokenKind::Hash)
    }

    // --- structured-reference bracket group --------------------------------
    fn lex_brackets(&mut self, start: usize) -> Result<TokenKind> {
        let mut depth = 0u32;
        while let Some(c) = self.bump() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(TokenKind::Brackets(self.src[start..self.pos].to_string()));
                    }
                }
                _ => {}
            }
        }
        Err(ParseError::new(
            ParseErrorKind::UnterminatedBracket,
            Span::new(start, self.pos),
        ))
    }

    // --- number ------------------------------------------------------------
    fn lex_number(&mut self, start: usize) -> Result<TokenKind> {
        while matches!(self.peek(), Some('0'..='9')) {
            self.bump();
        }
        if self.peek() == Some('.') {
            self.bump();
            while matches!(self.peek(), Some('0'..='9')) {
                self.bump();
            }
        }
        // Exponent, only if well-formed.
        if matches!(self.peek(), Some('e' | 'E')) {
            let sign_len = matches!(self.peek_at(1), Some('+' | '-')) as usize;
            let after = self.peek_at(1 + sign_len);
            if matches!(after, Some('0'..='9')) {
                self.bump(); // e/E
                if sign_len == 1 {
                    self.bump();
                }
                while matches!(self.peek(), Some('0'..='9')) {
                    self.bump();
                }
            }
        }
        let text = &self.src[start..self.pos];
        match text.parse::<f64>() {
            Ok(n) if n.is_finite() => Ok(TokenKind::Number(n)),
            _ => Err(ParseError::new(
                ParseErrorKind::InvalidNumber,
                Span::new(start, self.pos),
            )),
        }
    }

    // --- `$`-anchored reference (unambiguous) ------------------------------
    fn lex_reference_or_error(&mut self, start: usize) -> Result<TokenKind> {
        match self.try_a1_ref()? {
            Some(kind) => Ok(TokenKind::Ref(kind)),
            None => {
                // A lone `$` (or `$` not forming a ref) is not valid.
                self.bump();
                Err(ParseError::new(
                    ParseErrorKind::UnexpectedChar('$'),
                    Span::new(start, self.pos),
                ))
            }
        }
    }

    // --- letter/underscore starts: sheet prefix, reference, or identifier --
    fn lex_word_like(&mut self, start: usize) -> Result<TokenKind> {
        // 1. Sheet prefix (`Sheet1!`, `Sheet1:Sheet3!`).
        if let Some(sheet) = self.try_sheet_prefix()? {
            return Ok(TokenKind::Sheet(sheet));
        }
        // 2. R1C1 reference (only in R1C1 mode).
        if self.mode == ParseMode::R1C1
            && let Some(r) = self.try_r1c1_ref()?
        {
            return Ok(TokenKind::Ref(RefKind::R1C1(r)));
        }
        // 3. A1 cell reference (e.g. `A1`, `A$1`).
        if self.mode == ParseMode::A1
            && let Some(kind) = self.try_a1_ref()?
        {
            return Ok(TokenKind::Ref(kind));
        }
        // 4. Plain identifier / name / function name / bool word.
        let word = self.read_ident_run(start);
        Ok(TokenKind::Ident(word))
    }

    /// Consume the identifier/name run after a word start.
    ///
    /// The **continue** class is ASCII `[A-Za-z0-9_.]` plus any non-ASCII
    /// Unicode *letter*, per [`is_name_continue`]. OXP-211 (RUN 2026-07-16,
    /// Excel 16.0) pinned the previously-unknown class: a defined name with a
    /// non-leading non-ASCII letter parses **and resolves** in Excel for every
    /// letter probed across scripts — Latin `í`/`º`/`ª`/`ñ`/`ü` (Ll/Lo), Greek
    /// `Ω` (Lu), and CJK `中` (Lo) — decisively "any Unicode letter", not the
    /// Latin-only class we could not assume. This unblocks the FUSE Spanish
    /// energy-audit template family (`NºdeMejoras_Actual`,
    /// `AhorroEnergíaAnual_kWh`; ~97 shared-formula follow-on cells whose group
    /// masters reference such names).
    ///
    /// Widened to **letters** in both positions: the *continue* class here and
    /// the *start* gate in [`Self::lex_one`] via [`is_name_start`] (a leading
    /// non-ASCII letter was pinned ACCEPT by OXP-211's follow-up COM
    /// Name-Manager probe, RUN 2026-07-16 — leading `í`/`Ω`/`中` each
    /// define+resolve). Held narrow (never-guess): only **letters** are
    /// admitted. Non-letter non-ASCII (symbols `°`/`§`, combining marks) is NOT
    /// admitted even though that same COM probe found Excel *accepts* it in a
    /// name — because precisely matching Excel's So/Mn classes needs Unicode
    /// general-category data this zero-dependency crate cannot pull, and a crude
    /// "any non-ASCII" widening would over-admit unprobed categories (a guess).
    /// A leading combining mark was pinned REJECT, consistent with the
    /// letters-only start gate. So those stay a loud
    /// [`ParseErrorKind::UnexpectedChar`], never a silent misread. Cited:
    /// docs/oracle-experiments.md (OXP-211),
    /// docs/shared-residual-classification.md.
    fn read_ident_run(&mut self, start: usize) -> String {
        while let Some(c) = self.peek() {
            if is_name_continue(c) {
                self.bump();
            } else {
                break;
            }
        }
        self.src[start..self.pos].to_string()
    }

    /// Attempt to lex an A1 reference (cell, or `$`-anchored partial col/row)
    /// starting at the current position. Restores position and returns `None`
    /// if the text is not a clean reference (e.g. a bare word, or a cell name
    /// glued to further identifier characters).
    fn try_a1_ref(&mut self) -> Result<Option<RefKind>> {
        let save = self.pos;
        let col_abs = self.eat('$');
        let letters = self.read_ascii_letters();
        let row_abs = self.eat('$');
        let digits = self.read_ascii_digits();

        // Reject if immediately followed by more identifier characters
        // (`A1B`, `A1_x`, `A1ñ` → those are names, not references). Uses the
        // same widened continue class as the name run (OXP-211) so a cell-ref-
        // shaped prefix glued to a non-ASCII letter stays one name token.
        if matches!(self.peek(), Some(c) if is_name_continue(c)) {
            self.pos = save;
            return Ok(None);
        }

        let result = match (letters.is_empty(), digits.is_empty()) {
            // letters + digits → a cell.
            (false, false) => {
                let Some(col) = col_to_num(&letters) else {
                    // Letters present but out of column range: only an error
                    // when unambiguously a reference ($-anchored); otherwise a
                    // name.
                    if col_abs || row_abs {
                        self.pos = save;
                        return Err(ParseError::new(
                            ParseErrorKind::RefOutOfRange,
                            Span::new(save, self.pos_after_probe(save)),
                        ));
                    }
                    self.pos = save;
                    return Ok(None);
                };
                let Some(row) = parse_row(&digits) else {
                    if col_abs || row_abs {
                        self.pos = save;
                        return Err(ParseError::new(
                            ParseErrorKind::RefOutOfRange,
                            Span::new(save, self.pos_after_probe(save)),
                        ));
                    }
                    self.pos = save;
                    return Ok(None);
                };
                Some(RefKind::Cell(
                    Axis {
                        index: col,
                        absolute: col_abs,
                    },
                    Axis {
                        index: row,
                        absolute: row_abs,
                    },
                ))
            }
            // letters only, `$`-anchored column → whole-column partial (`$A`).
            (false, true) if col_abs && !row_abs => match col_to_num(&letters) {
                Some(col) => Some(RefKind::Col(Axis {
                    index: col,
                    absolute: true,
                })),
                None => {
                    self.pos = save;
                    return Err(ParseError::new(
                        ParseErrorKind::RefOutOfRange,
                        Span::new(save, self.pos_after_probe(save)),
                    ));
                }
            },
            // digits only, `$`-anchored row → whole-row partial (`$1`). The
            // leading `$` was consumed as `col_abs`.
            (true, false) if col_abs && !row_abs => match parse_row(&digits) {
                Some(row) => Some(RefKind::Row(Axis {
                    index: row,
                    absolute: true,
                })),
                None => {
                    self.pos = save;
                    return Err(ParseError::new(
                        ParseErrorKind::RefOutOfRange,
                        Span::new(save, self.pos_after_probe(save)),
                    ));
                }
            },
            _ => None,
        };

        if result.is_none() {
            self.pos = save;
        }
        Ok(result)
    }

    /// Length of the raw probe text (for error spans) without leaving `pos`
    /// advanced.
    fn pos_after_probe(&self, save: usize) -> usize {
        let mut p = save;
        let bytes = self.src.as_bytes();
        while p < self.src.len() {
            let c = bytes[p];
            if c.is_ascii_alphanumeric() || c == b'$' || c == b'_' || c == b'.' {
                p += 1;
            } else {
                break;
            }
        }
        p
    }

    fn read_ascii_letters(&mut self) -> String {
        let s = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphabetic()) {
            self.bump();
        }
        self.src[s..self.pos].to_string()
    }

    fn read_ascii_digits(&mut self) -> String {
        let s = self.pos;
        while matches!(self.peek(), Some('0'..='9')) {
            self.bump();
        }
        self.src[s..self.pos].to_string()
    }

    /// Attempt to lex an R1C1 reference at the current position.
    ///
    /// Returns `Ok(None)` (restoring position) if the text is not shaped like
    /// an R1C1 reference, so that words like `RAND` fall through to
    /// identifiers and `R[Col]` falls through to the structured-reference
    /// path. Once the text **is** unambiguously R1C1-shaped (R/C with a
    /// bracketed signed-integer offset or bare digits, not glued to further
    /// identifier characters), any overflow or out-of-grid axis is a hard
    /// [`ParseErrorKind::RefOutOfRange`] — never a silently clamped value.
    fn try_r1c1_ref(&mut self) -> Result<Option<R1C1Ref>> {
        let save = self.pos;
        let Some(first) = self.peek().map(|c| c.to_ascii_uppercase()) else {
            return Ok(None);
        };
        if first != 'R' && first != 'C' {
            return Ok(None);
        }

        let mut row = None;
        let mut col = None;

        if first == 'R' {
            self.bump();
            let Some(axis) = self.scan_r1c1_axis() else {
                self.pos = save;
                return Ok(None);
            };
            row = Some(axis);
            if matches!(self.peek(), Some('c' | 'C')) {
                self.bump();
                let Some(axis) = self.scan_r1c1_axis() else {
                    self.pos = save;
                    return Ok(None);
                };
                col = Some(axis);
            }
        } else {
            // starts with C → whole column
            self.bump();
            let Some(axis) = self.scan_r1c1_axis() else {
                self.pos = save;
                return Ok(None);
            };
            col = Some(axis);
        }

        // Must not be followed by identifier characters (`RANDBETWEEN`,
        // `R0C0D`, `R1Cñ` as a defined name). Checked *before* range validation
        // so such words stay identifiers rather than becoming errors. Uses the
        // same widened continue class as the name run (OXP-211).
        if matches!(self.peek(), Some(c) if is_name_continue(c)) {
            self.pos = save;
            return Ok(None);
        }

        // Committed to an R1C1 reference: validate both axes against the
        // grid (rows ≤ 1048576, columns ≤ 16384; relative offsets within
        // ±(max−1)). Overflow or out-of-grid is a precise error, mirroring
        // the A1 `$`-anchored path — never `unwrap_or(0)`.
        let row = row
            .map(|r| self.check_r1c1_axis(r, crate::ast::MAX_ROW, save))
            .transpose()?;
        let col = col
            .map(|c| self.check_r1c1_axis(c, crate::ast::MAX_COL, save))
            .transpose()?;

        Ok(Some(R1C1Ref { row, col }))
    }

    /// Scan one raw R1C1 axis component right after its `R`/`C` letter.
    ///
    /// Returns `None` when the component is not R1C1-shaped (a bracket group
    /// whose contents are not a plain optionally-signed integer, e.g. the
    /// structured reference `R[Col]`); the caller then restores and falls
    /// through to identifier / structured-reference lexing. No numeric
    /// conversion happens here — see `check_r1c1_axis`.
    fn scan_r1c1_axis(&mut self) -> Option<RawR1c1Axis> {
        let start = self.pos;
        if self.peek() == Some('[') {
            self.bump();
            let neg = self.eat('-');
            if !neg {
                self.eat('+');
            }
            let digits = self.read_ascii_digits();
            // Contents must be a non-empty integer immediately closed by `]`;
            // anything else is not an R1C1 offset.
            if digits.is_empty() || !self.eat(']') {
                return None;
            }
            Some(RawR1c1Axis {
                digits,
                neg,
                bracketed: true,
                span: Span::new(start, self.pos),
            })
        } else if matches!(self.peek(), Some('0'..='9')) {
            let digits = self.read_ascii_digits();
            Some(RawR1c1Axis {
                digits,
                neg: false,
                bracketed: false,
                span: Span::new(start, self.pos),
            })
        } else {
            // bare `R`/`C` → relative offset 0 (current row/column).
            Some(RawR1c1Axis {
                digits: String::new(),
                neg: false,
                bracketed: false,
                span: Span::new(start, self.pos),
            })
        }
    }

    /// Convert a scanned raw axis to an [`R1C1Axis`], enforcing grid bounds:
    /// absolute indices in `1..=max`, relative offsets in `±(max − 1)`.
    /// `max` is [`crate::ast::MAX_ROW`] or [`crate::ast::MAX_COL`].
    fn check_r1c1_axis(&self, raw: RawR1c1Axis, max: u32, ref_start: usize) -> Result<R1C1Axis> {
        let err = || {
            ParseError::new(
                ParseErrorKind::RefOutOfRange,
                Span::new(ref_start, raw.span.end),
            )
        };
        if raw.digits.is_empty() {
            // bare `R`/`C` → offset 0.
            return Ok(R1C1Axis::Relative(0));
        }
        if raw.bracketed {
            let mag: i64 = raw.digits.parse().map_err(|_| err())?;
            let mag = if raw.neg { -mag } else { mag };
            let limit = i64::from(max) - 1;
            if (-limit..=limit).contains(&mag) {
                #[allow(clippy::cast_possible_truncation)]
                Ok(R1C1Axis::Relative(mag as i32))
            } else {
                Err(err())
            }
        } else {
            let n: u32 = raw.digits.parse().map_err(|_| err())?;
            if (1..=max).contains(&n) {
                Ok(R1C1Axis::Absolute(n))
            } else {
                Err(err())
            }
        }
    }

    /// Attempt to lex a sheet prefix (`Sheet1!`, `'a b'!`, `S1:S3!`) at the
    /// current position, consuming the trailing `!` on success. Restores
    /// position and returns `None` if there is no such prefix.
    fn try_sheet_prefix(&mut self) -> Result<Option<SheetRef>> {
        let save = self.pos;
        let Some((first, q1)) = self.read_sheet_name_part()? else {
            self.pos = save;
            return Ok(None);
        };

        // 3-D span: `first:last!` — only if the `:name` is actually followed
        // by `!` (otherwise the `:` is the range operator).
        if self.peek() == Some(':') {
            let before_colon = self.pos;
            self.bump();
            if let Some((last, q2)) = self.read_sheet_name_part()?
                && self.eat('!')
            {
                // If the *unquoted* first part spells an in-grid cell reference
                // (`A1:Sheet1!B2`, or the `A1:Sheet1!` tail of
                // `Sheet1!A1:Sheet1!B2`), this is not a 3-D sheet span whose
                // first name happens to be `A1` — it is a range whose right
                // endpoint carries its own sheet qualifier. Don't consume it as
                // a sheet prefix, and don't reject here: back off so the cell
                // re-lexes as a cell, the `:` as the range operator, and the
                // trailing `Sheet1!` as its own prefix — the identical token
                // stream the whitespace-spaced spelling produces. Whitespace
                // around `:` is therefore inert (OXP-053,
                // RUN-2026-07-11-oracle01), and the parser becomes the single
                // place that decides the range's fate: RFC-0004 slice-1 accepts
                // the redundant same-sheet form `Sheet1!A1:Sheet1!B2`
                // (normalizing it to `Sheet1!A1:B2`) and still rejects the
                // deferred slice-2 cross-sheet / bare-left shapes. This also
                // closes fuzz FINDING-001's lexer/`Display` asymmetry at its
                // root — the two spellings now share one token stream.
                if !q1 && spells_a1_cell(&first) {
                    self.pos = save;
                    return Ok(None);
                }
                return Ok(Some(SheetRef {
                    first,
                    last: Some(last),
                    quoted: q1 || q2,
                    workbook_global: false,
                }));
            }
            self.pos = before_colon;
        }

        if self.eat('!') {
            return Ok(Some(SheetRef {
                first,
                last: None,
                quoted: q1,
                workbook_global: false,
            }));
        }

        self.pos = save;
        Ok(None)
    }

    /// Read one sheet-name part: a quoted `'...'` name or an unquoted word.
    /// Returns `(name, was_quoted)`.
    fn read_sheet_name_part(&mut self) -> Result<Option<(String, bool)>> {
        match self.peek() {
            Some('\'') => {
                let start = self.pos;
                self.bump(); // opening '
                let mut s = String::new();
                loop {
                    match self.bump() {
                        None => {
                            return Err(ParseError::new(
                                ParseErrorKind::UnterminatedSheetName,
                                Span::new(start, self.pos),
                            ));
                        }
                        Some('\'') => {
                            if self.peek() == Some('\'') {
                                self.bump();
                                s.push('\'');
                            } else {
                                return Ok(Some((s, true)));
                            }
                        }
                        Some(c) => s.push(c),
                    }
                }
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let start = self.pos;
                while let Some(c) = self.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                        self.bump();
                    } else {
                        break;
                    }
                }
                Ok(Some((self.src[start..self.pos].to_string(), false)))
            }
            _ => Ok(None),
        }
    }
}

/// Characters admitted in a defined-name / identifier token **after** the
/// first character (the "continue" class): ASCII `[A-Za-z0-9_.]` plus any
/// non-ASCII Unicode *letter* (general category L).
///
/// The non-ASCII-letter widening is pinned by **OXP-211** (RUN 2026-07-16,
/// Excel 16.0, `RUN-2026-07-16-oracle01`): a defined name embedding one
/// non-leading non-ASCII char between ASCII anchors parses **and resolves** in
/// Excel for every letter probed across scripts — Latin `í`/`º`/`ª`/`ñ`/`ü`
/// (Ll/Lo), Greek `Ω` (Lu), and CJK `中` (Lo) all returned the referenced
/// cell's value — decisively "any Unicode letter", not a Latin-only class.
/// Non-letter non-ASCII — a symbol (`°`/`§`, cat So), a combining mark (Mn),
/// or a non-ASCII digit — is **not** admitted here. OXP-211's follow-up COM
/// Name-Manager probe (RUN 2026-07-16) found Excel actually *accepts* a symbol
/// or combining mark in a non-leading name position, but matching its exact
/// So/Mn classes needs Unicode general-category data this zero-dependency crate
/// cannot pull, and a blanket "any non-ASCII" rule would over-admit unprobed
/// categories (a guess). So they stay a loud `UnexpectedChar`, never a silent
/// misread. (`µ` U+00B5 is category Ll — a *letter* — so it is already admitted
/// by the letter arm, despite looking symbol-like.) See
/// docs/oracle-experiments.md (OXP-211).
fn is_name_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.' || (!c.is_ascii() && c.is_alphabetic())
}

/// Characters admitted as the **first** character of a defined-name /
/// identifier token: ASCII letter or `_`, plus any non-ASCII Unicode *letter*
/// (general category L). The non-ASCII-letter start is pinned by **OXP-211**'s
/// follow-up COM Name-Manager probe (RUN 2026-07-16, Excel 16.0): a name whose
/// leading character is `í` (Ll), `Ω` (Lu), or `中` (Lo) both defines via
/// `Names.Add` and resolves when referenced in a formula. A leading non-letter
/// (a symbol `°`, or a combining mark — the latter pinned REJECT by the same
/// probe) is **not** admitted, so it falls through to a loud `UnexpectedChar`.
/// Mirrors the letters-only continue class in [`is_name_continue`].
fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || (!c.is_ascii() && c.is_alphabetic())
}

/// Whether a word is letters-then-digits spelling an in-grid A1 cell
/// (`A1`, `xfd1048576`) — used so a cell reference on the left of `:` is not
/// consumed as an unquoted 3-D sheet name (`A1:Sheet1!B2`). When it matches,
/// `try_sheet_prefix` backs off so the range re-lexes with the cell, the range
/// `:`, and the trailing sheet prefix as separate tokens (RFC-0004 slice-1 /
/// OXP-053), letting the parser normalize or reject it uniformly.
fn spells_a1_cell(word: &str) -> bool {
    let letters_len = word.chars().take_while(char::is_ascii_alphabetic).count();
    if letters_len == 0 || letters_len == word.len() {
        return false;
    }
    let (letters, digits) = word.split_at(letters_len);
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    col_to_num(letters).is_some() && parse_row(digits).is_some()
}

/// Parse a decimal row number, validating `1 ≤ row ≤ 1048576`.
fn parse_row(s: &str) -> Option<u32> {
    let n: u32 = s.parse().ok()?;
    if (1..=crate::ast::MAX_ROW).contains(&n) {
        Some(n)
    } else {
        None
    }
}

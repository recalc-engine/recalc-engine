//! `ADDRESS` — build a cell-reference **string** from a row and column number.
//!
//! # Provenance
//! Behavior contract: `docs/specs/ADDRESS.md` (Microsoft Learn "ADDRESS
//! function" — <https://support.microsoft.com/en-us/office/address-function-d0c26c0d-3991-446b-8de4-ab46431d4f89>,
//! verified by WebFetch on 2026-07-11). Clean-room: behavior is taken only from
//! that public page (and, where the page defers, deferred to the oracle rather
//! than guessed — Recalc Principle 2). No GPL source consulted.
//!
//! `ADDRESS` is **pure text construction**: it turns a `(row, column)` pair into
//! the *text* of a reference (`"$C$2"`), it does **not** resolve or read a cell,
//! so it needs no grid access. Coercion is deferred to `xl-value`
//! ([`to_number`]/[`to_bool`]/[`to_text`]).
//!
//! # Semantics implemented
//! `ADDRESS(row_num, column_num, [abs_num=1], [a1=TRUE], [sheet_text])`
//! - `row_num` (arg 0) / `column_num` (arg 1): required, scalar-numeric-coerced
//!   via [`to_number`], then **truncated toward zero** (the codebase numeric-arg
//!   convention the sibling `LEFT`/`DATE` args follow) before use. A value `< 1`
//!   is out of the addressable domain → `#VALUE!`.
//! - `abs_num` (arg 2): optional, default `1`. Selects absolute/relative parts:
//!   `1`→`$C$2`, `2`→`C$2` (abs row, rel col), `3`→`$C2` (rel row, abs col),
//!   `4`→`C2` (both relative). Truncated toward zero; a truncated value outside
//!   `1..=4` → `#VALUE!`.
//! - `a1` (arg 3): optional, default `TRUE`. `TRUE`→A1 style; `FALSE`→R1C1 style.
//!   Coerced via [`to_bool`] (so `0`→R1C1, any non-zero number→A1).
//! - `sheet_text` (arg 4): optional. When supplied, prefixes `sheet!` (see the
//!   quoting buckets below). Coerced via [`to_text`].
//! - Errors in any argument propagate leftmost-first (arguments are forced in
//!   left-to-right order).
//!
//! An **omitted** optional argument (an absent trailing position *or* an elided
//! `ADDRESS(2,3,,FALSE)` slot — [`ArgShape::Omitted`]) takes its documented
//! default; only a *present, non-omitted* argument is coerced.
//!
//! # Documented anchors
//! The MS Learn examples table pins: `ADDRESS(2,3)`=`$C$2`, `ADDRESS(2,3,2)`=
//! `C$2`, `ADDRESS(2,3,2,FALSE)`=`R2C[3]`, `ADDRESS(2,3,1,FALSE,"EXCEL SHEET")`=
//! `'EXCEL SHEET'!R2C3`. The `R2C[3]` example documents the R1C1 bracket
//! notation (absolute axis → bare number `R2`; relative axis → bracketed
//! `C[3]`); combined with the documented `abs_num`→(row abs, col abs) mapping,
//! the R1C1 forms for `abs_num` 1/3/4 apply that same documented notation to the
//! other axis (`R2C3`, `R[2]C3`, `R[2]C[3]`).
//!
//! # Deliberately deferred (Principle 2 — `#UNSUPPORTED!`, never guessed)
//! - **Out-of-grid row/column.** A `row_num`/`column_num` past Excel's grid
//!   (row `> 1048576`, column `> 16384`/`XFD`) is undocumented: the page does
//!   not say whether Excel returns `#VALUE!` or formats an out-of-grid string.
//!   Rather than emit a possibly-wrong address, defer.
//!   // OXP (unassigned): ADDRESS(1,16385) / ADDRESS(1048577,1) — #VALUE! or a
//!   // formatted out-of-grid reference string?
//! - **Fractional `abs_num`.** `abs_num` is truncated toward zero to match the
//!   sibling `row_num`/`column_num` handling in this very function, then
//!   range-checked; whether Excel truncates `abs_num` specifically (vs erroring
//!   on a non-integer) is unobserved.
//!   // OXP (unassigned): ADDRESS(2,3,2.9) — "C$2" (truncate) or #VALUE!?
//! - **Exotic `sheet_text`.** Excel's full sheet-name quoting rules are not on
//!   the ADDRESS page (only the space case `'EXCEL SHEET'` is shown). Only two
//!   unambiguous buckets are emitted: a *plain* name (ASCII letters/digits/`_`,
//!   not a leading digit, not shaped like a cell/R1C1 reference) unquoted, and a
//!   *space-containing* otherwise-safe name single-quoted. Everything else — a
//!   name with an embedded `'`, brackets/punctuation, non-ASCII characters, an
//!   external-workbook `[Book1]Sheet1` reference, a reference-shaped name like
//!   `A1`, or an empty name — is deferred.
//!   // OXP (unassigned): full ADDRESS sheet-name quoting/escaping table
//!   // (interior "''" escaping, ref-shaped names, external [Book]Sheet refs).

use xl_value::{ErrorKind, Value, to_bool, to_number, to_text};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Excel's grid bounds (the same caps `xl-io`'s `cellref` enforces): the last
/// addressable cell is column `XFD` (16384), row `1048576`.
const MAX_ROW: u32 = 1_048_576;
const MAX_COL: u32 = 16_384;

/// Evaluate an `ADDRESS(row_num, column_num, [abs_num], [a1], [sheet_text])`
/// call. See the module docs for the behavior contract and deferrals.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // row_num (arg 0) / column_num (arg 1): required, coerced + truncated,
    // forced left-to-right so the leftmost argument error propagates first.
    let row = match coerce_index(&args.eval_scalar(0), MAX_ROW) {
        Ok(r) => r,
        Err(e) => return Value::Error(e),
    };
    let col = match coerce_index(&args.eval_scalar(1), MAX_COL) {
        Ok(c) => c,
        Err(e) => return Value::Error(e),
    };

    // abs_num (arg 2): default 1 when omitted; else truncate + range-check 1..=4.
    let abs_num = if args.shape(2) == ArgShape::Omitted {
        1
    } else {
        let raw = match to_number(&args.eval_scalar(2)) {
            Ok(n) => n,
            Err(e) => return Value::Error(e),
        };
        let t = raw.trunc();
        if !(1.0..=4.0).contains(&t) {
            return Value::Error(ErrorKind::Value);
        }
        t as u8
    };

    // a1 (arg 3): default TRUE when omitted; else coerce to bool (0 → R1C1).
    let a1_style = if args.shape(3) == ArgShape::Omitted {
        true
    } else {
        match to_bool(&args.eval_scalar(3)) {
            Ok(b) => b,
            Err(e) => return Value::Error(e),
        }
    };

    // sheet_text (arg 4): default none; else coerce to text and build the prefix
    // (deferring exotic names to #UNSUPPORTED).
    let prefix = if args.shape(4) == ArgShape::Omitted {
        None
    } else {
        let name = match to_text(&args.eval_scalar(4)) {
            Ok(t) => t,
            Err(e) => return Value::Error(e),
        };
        match sheet_prefix(name.as_str()) {
            Some(p) => Some(p),
            None => return Value::Error(ErrorKind::Unsupported),
        }
    };

    let (row_abs, col_abs) = abs_flags(abs_num);
    let body = if a1_style {
        format_a1(col, row, col_abs, row_abs)
    } else {
        format_r1c1(row, col, row_abs, col_abs)
    };
    match prefix {
        Some(p) => Value::text(&format!("{p}{body}")),
        None => Value::text(&body),
    }
}

/// Coerce a `row_num`/`column_num` argument: [`to_number`], truncate toward
/// zero, then require `1..=max`. `< 1` → `#VALUE!` (out of the addressable
/// domain); `> max` → `#UNSUPPORTED!` (out-of-grid behavior is unobserved).
fn coerce_index(v: &Value, max: u32) -> Result<u32, ErrorKind> {
    // `to_number` yields a finite value (xl-value invariant) or propagates an
    // error / `#VALUE!` on non-numeric text; `trunc` then truncates toward zero.
    let t = to_number(v)?.trunc();
    if t < 1.0 {
        return Err(ErrorKind::Value);
    }
    if t > f64::from(max) {
        // OXP (unassigned): out-of-grid ADDRESS — #VALUE! or formatted string?
        return Err(ErrorKind::Unsupported);
    }
    // `t` is finite, `>= 1.0`, and `<= max`, so the cast is exact.
    Ok(t as u32)
}

/// Map `abs_num` (1..=4) to `(row_absolute, col_absolute)`:
/// `1`→both absolute, `2`→abs row/rel col, `3`→rel row/abs col, `4`→both
/// relative (MS Learn `abs_num` table).
fn abs_flags(abs_num: u8) -> (bool, bool) {
    match abs_num {
        1 => (true, true),
        2 => (true, false),
        3 => (false, true),
        _ => (false, false), // 4 (the only remaining range-checked value)
    }
}

/// Format an A1-style reference: `[$]<letters>[$]<row>` (e.g. `$C$2`, `C$2`).
fn format_a1(col: u32, row: u32, col_abs: bool, row_abs: bool) -> String {
    let cd = if col_abs { "$" } else { "" };
    let rd = if row_abs { "$" } else { "" };
    let letters = num_to_col(col);
    format!("{cd}{letters}{rd}{row}")
}

/// Format an R1C1-style reference: an absolute axis is a bare number (`R2`,
/// `C3`), a relative axis is bracketed (`R[2]`, `C[3]`). The `R2C[3]` MS Learn
/// example documents this notation; the row axis applies it identically.
fn format_r1c1(row: u32, col: u32, row_abs: bool, col_abs: bool) -> String {
    let r = if row_abs {
        format!("R{row}")
    } else {
        format!("R[{row}]")
    };
    let c = if col_abs {
        format!("C{col}")
    } else {
        format!("C[{col}]")
    };
    format!("{r}{c}")
}

/// Convert a 1-based column index to its letters (`1`→`A`, `26`→`Z`, `27`→`AA`,
/// `16384`→`XFD`) — bijective base-26. `n` is always `1..=MAX_COL` here.
fn num_to_col(mut n: u32) -> String {
    let mut buf = Vec::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        buf.push(b'A' + rem as u8);
        n = (n - 1) / 26;
    }
    buf.reverse();
    // Every pushed byte is an ASCII uppercase letter.
    String::from_utf8(buf).expect("column letters are ASCII A-Z")
}

/// Build the `sheet!` prefix for a supplied `sheet_text`, or `None` to defer to
/// `#UNSUPPORTED!` when the name is outside the two unambiguous, documented
/// buckets (see the module-level deferral note).
fn sheet_prefix(name: &str) -> Option<String> {
    if is_plain_sheet_name(name) {
        Some(format!("{name}!"))
    } else if needs_simple_quote(name) {
        Some(format!("'{name}'!"))
    } else {
        None
    }
}

/// A name safe to emit **unquoted**: non-empty, only ASCII letters/digits/`_`,
/// a non-digit first char, and not shaped like an A1/R1C1 reference (e.g. the
/// documented `"Sheet2"`). Conservative: anything else falls through to the
/// quoting/deferral paths rather than risk a silently-wrong bare emission.
fn is_plain_sheet_name(name: &str) -> bool {
    let Some(first) = name.chars().next() else {
        return false; // empty
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    // A reference-shaped name (e.g. `A1`, `R1C1`) may require quoting; that rule
    // is not on the ADDRESS page, so defer instead of guessing.
    !looks_like_reference(name)
}

/// A name that needs the **simple single-quote** wrapping the MS Learn
/// `'EXCEL SHEET'` example documents: contains a space, is otherwise composed of
/// ASCII letters/digits/`_`/spaces only (no `'`, no other punctuation), and has
/// no leading/trailing space. Anything requiring interior `''` escaping or
/// exotic characters is *not* matched (it defers).
fn needs_simple_quote(name: &str) -> bool {
    name.contains(' ')
        && !name.starts_with(' ')
        && !name.ends_with(' ')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ' ')
}

/// Whether `name` (already known to be ASCII alphanumeric/`_`) looks like a cell
/// or R1C1 reference: `1..=3` letters then `1+` digits (A1-style `A1`/`XFD9`),
/// or `R<digits?>C<digits?>` (R1C1-style `R1C1`/`RC`).
fn looks_like_reference(name: &str) -> bool {
    let letters = name.chars().take_while(|c| c.is_ascii_alphabetic()).count();
    let rest = &name[letters..];
    let a1_shaped =
        (1..=3).contains(&letters) && !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit());
    a1_shaped || is_r1c1_shaped(name)
}

/// Whether `name` matches the R1C1 skeleton `R<digits?>C<digits?>` (case-folded),
/// e.g. `RC`, `R1C1`, `R12C`.
fn is_r1c1_shaped(name: &str) -> bool {
    let bytes = name.as_bytes();
    let mut i = 0;
    if bytes.first().map(|b| b.to_ascii_uppercase()) != Some(b'R') {
        return false;
    }
    i += 1;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if bytes.get(i).map(|b| b.to_ascii_uppercase()) != Some(b'C') {
        return false;
    }
    i += 1;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    i == bytes.len()
}

#[cfg(test)]
mod tests {
    use xl_value::{ErrorKind, Value};

    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    fn addr(args: Vec<crate::test_support::TestArg>) -> Value {
        eval_direct(super::eval, args)
    }

    fn b(v: bool) -> crate::test_support::TestArg {
        Scalar(Value::Bool(v))
    }

    // ---- A1 core: the documented abs_num 1..4 forms --------------------------

    #[test]
    fn default_is_fully_absolute() {
        // MS Learn: ADDRESS(2,3) = $C$2.
        assert_eq!(addr(vec![Scalar(num(2.0)), Scalar(num(3.0))]), txt("$C$2"));
    }

    #[test]
    fn abs_num_two_is_abs_row_rel_col() {
        // MS Learn: ADDRESS(2,3,2) = C$2.
        assert_eq!(
            addr(vec![Scalar(num(2.0)), Scalar(num(3.0)), Scalar(num(2.0))]),
            txt("C$2")
        );
    }

    #[test]
    fn abs_num_three_is_rel_row_abs_col() {
        assert_eq!(
            addr(vec![Scalar(num(2.0)), Scalar(num(3.0)), Scalar(num(3.0))]),
            txt("$C2")
        );
    }

    #[test]
    fn abs_num_four_is_fully_relative() {
        assert_eq!(
            addr(vec![Scalar(num(2.0)), Scalar(num(3.0)), Scalar(num(4.0))]),
            txt("C2")
        );
    }

    // ---- R1C1 style (a1 = FALSE) --------------------------------------------

    #[test]
    fn r1c1_absolute() {
        assert_eq!(
            addr(vec![
                Scalar(num(2.0)),
                Scalar(num(3.0)),
                Scalar(num(1.0)),
                b(false)
            ]),
            txt("R2C3")
        );
    }

    #[test]
    fn r1c1_abs_row_rel_col_matches_ms_learn_example() {
        // MS Learn: ADDRESS(2,3,2,FALSE) = R2C[3].
        assert_eq!(
            addr(vec![
                Scalar(num(2.0)),
                Scalar(num(3.0)),
                Scalar(num(2.0)),
                b(false)
            ]),
            txt("R2C[3]")
        );
    }

    #[test]
    fn r1c1_rel_row_abs_col() {
        assert_eq!(
            addr(vec![
                Scalar(num(2.0)),
                Scalar(num(3.0)),
                Scalar(num(3.0)),
                b(false)
            ]),
            txt("R[2]C3")
        );
    }

    #[test]
    fn r1c1_fully_relative() {
        assert_eq!(
            addr(vec![
                Scalar(num(2.0)),
                Scalar(num(3.0)),
                Scalar(num(4.0)),
                b(false)
            ]),
            txt("R[2]C[3]")
        );
    }

    #[test]
    fn a1_flag_as_number_zero_selects_r1c1() {
        // `a1` is bool-coerced: 0 → R1C1, non-zero → A1.
        assert_eq!(
            addr(vec![
                Scalar(num(2.0)),
                Scalar(num(3.0)),
                Scalar(num(1.0)),
                Scalar(num(0.0))
            ]),
            txt("R2C3")
        );
        assert_eq!(
            addr(vec![
                Scalar(num(2.0)),
                Scalar(num(3.0)),
                Scalar(num(1.0)),
                Scalar(num(1.0))
            ]),
            txt("$C$2")
        );
    }

    // ---- sheet_text ----------------------------------------------------------

    #[test]
    fn plain_sheet_name_is_unquoted() {
        assert_eq!(
            addr(vec![
                Scalar(num(2.0)),
                Scalar(num(3.0)),
                Scalar(num(1.0)),
                b(true),
                Scalar(txt("Sheet1")),
            ]),
            txt("Sheet1!$C$2")
        );
    }

    #[test]
    fn spaced_sheet_name_is_single_quoted() {
        // MS Learn: ADDRESS(2,3,1,FALSE,"EXCEL SHEET") = 'EXCEL SHEET'!R2C3.
        assert_eq!(
            addr(vec![
                Scalar(num(2.0)),
                Scalar(num(3.0)),
                Scalar(num(1.0)),
                b(false),
                Scalar(txt("EXCEL SHEET")),
            ]),
            txt("'EXCEL SHEET'!R2C3")
        );
    }

    #[test]
    fn reference_shaped_sheet_name_is_deferred() {
        // "A1" may need quoting in Excel (undocumented on the ADDRESS page) →
        // defer rather than emit a possibly-wrong bare `A1!...`.
        assert_eq!(
            addr(vec![
                Scalar(num(2.0)),
                Scalar(num(3.0)),
                Scalar(num(1.0)),
                b(true),
                Scalar(txt("A1")),
            ]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn r1c1_shaped_sheet_name_is_deferred() {
        assert_eq!(
            addr(vec![
                Scalar(num(1.0)),
                Scalar(num(1.0)),
                Scalar(num(1.0)),
                b(true),
                Scalar(txt("R1C1")),
            ]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn exotic_sheet_names_are_deferred() {
        // Embedded apostrophe (needs "''" escaping), external-workbook brackets,
        // and an empty name are all beyond the documented buckets.
        for exotic in ["a'b", "[Book1]Sheet1", ""] {
            assert_eq!(
                addr(vec![
                    Scalar(num(1.0)),
                    Scalar(num(1.0)),
                    Scalar(num(1.0)),
                    b(true),
                    Scalar(txt(exotic)),
                ]),
                Value::Error(ErrorKind::Unsupported),
                "sheet name {exotic:?} should defer"
            );
        }
    }

    // ---- abs_num validation --------------------------------------------------

    #[test]
    fn abs_num_out_of_range_is_value_error() {
        for bad in [0.0, 5.0, -1.0, 100.0] {
            assert_eq!(
                addr(vec![Scalar(num(2.0)), Scalar(num(3.0)), Scalar(num(bad))]),
                Value::Error(ErrorKind::Value),
                "abs_num {bad} should be #VALUE!"
            );
        }
    }

    #[test]
    fn abs_num_truncates_toward_zero() {
        // Convention-based (OXP note): 4.9 → 4 → fully relative; 0.9 → 0 → #VALUE!.
        assert_eq!(
            addr(vec![Scalar(num(2.0)), Scalar(num(3.0)), Scalar(num(4.9))]),
            txt("C2")
        );
        assert_eq!(
            addr(vec![Scalar(num(2.0)), Scalar(num(3.0)), Scalar(num(0.9))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // ---- row / column domain -------------------------------------------------

    #[test]
    fn row_or_column_below_one_is_value_error() {
        assert_eq!(
            addr(vec![Scalar(num(0.0)), Scalar(num(1.0))]),
            Value::Error(ErrorKind::Value)
        );
        assert_eq!(
            addr(vec![Scalar(num(1.0)), Scalar(num(0.0))]),
            Value::Error(ErrorKind::Value)
        );
        assert_eq!(
            addr(vec![Scalar(num(-1.0)), Scalar(num(1.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn row_and_column_truncate_toward_zero() {
        // 2.9 → row 2, 3.9 → col 3.
        assert_eq!(addr(vec![Scalar(num(2.9)), Scalar(num(3.9))]), txt("$C$2"));
    }

    #[test]
    fn out_of_grid_row_or_column_is_deferred() {
        // OXP (unassigned): out-of-grid behavior unobserved → #UNSUPPORTED.
        assert_eq!(
            addr(vec![Scalar(num(1.0)), Scalar(num(16385.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
        assert_eq!(
            addr(vec![Scalar(num(1_048_577.0)), Scalar(num(1.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // ---- column → letters (base-26) -----------------------------------------

    #[test]
    fn column_letters_base26() {
        // 26→Z, 27→AA, 28→AB, 702→ZZ, 703→AAA, 16384→XFD (grid max).
        assert_eq!(
            addr(vec![Scalar(num(1.0)), Scalar(num(26.0)), Scalar(num(4.0))]),
            txt("Z1")
        );
        assert_eq!(
            addr(vec![Scalar(num(1.0)), Scalar(num(27.0)), Scalar(num(4.0))]),
            txt("AA1")
        );
        assert_eq!(
            addr(vec![Scalar(num(1.0)), Scalar(num(28.0)), Scalar(num(4.0))]),
            txt("AB1")
        );
        assert_eq!(
            addr(vec![Scalar(num(1.0)), Scalar(num(702.0)), Scalar(num(4.0))]),
            txt("ZZ1")
        );
        assert_eq!(
            addr(vec![Scalar(num(1.0)), Scalar(num(703.0)), Scalar(num(4.0))]),
            txt("AAA1")
        );
        assert_eq!(
            addr(vec![Scalar(num(1_048_576.0)), Scalar(num(16_384.0))]),
            txt("$XFD$1048576")
        );
    }

    // ---- defaults for omitted / elided optional args -------------------------

    #[test]
    fn omitted_abs_num_defaults_to_absolute() {
        // Fully absent (count < 3) and elided (present-but-omitted) both default.
        assert_eq!(addr(vec![Scalar(num(2.0)), Scalar(num(3.0))]), txt("$C$2"));
        assert_eq!(
            addr(vec![Scalar(num(2.0)), Scalar(num(3.0)), Omitted, b(false)]),
            txt("R2C3")
        );
    }

    #[test]
    fn omitted_a1_defaults_to_a1_style() {
        assert_eq!(
            addr(vec![
                Scalar(num(2.0)),
                Scalar(num(3.0)),
                Scalar(num(1.0)),
                Omitted
            ]),
            txt("$C$2")
        );
    }

    // ---- coercion & error propagation ---------------------------------------

    #[test]
    fn numeric_text_args_coerce() {
        assert_eq!(addr(vec![Scalar(txt("2")), Scalar(txt("3"))]), txt("$C$2"));
    }

    #[test]
    fn error_in_row_propagates() {
        assert_eq!(
            addr(vec![
                Scalar(Value::Error(ErrorKind::Div0)),
                Scalar(num(3.0))
            ]),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn error_in_column_propagates() {
        assert_eq!(
            addr(vec![Scalar(num(2.0)), Scalar(Value::Error(ErrorKind::Ref))]),
            Value::Error(ErrorKind::Ref)
        );
    }

    #[test]
    fn non_numeric_text_row_is_value_error() {
        assert_eq!(
            addr(vec![Scalar(txt("abc")), Scalar(num(3.0))]),
            Value::Error(ErrorKind::Value)
        );
    }
}

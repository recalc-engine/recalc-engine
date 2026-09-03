//! `TEXTJOIN` — join the text of every item with a delimiter, optionally
//! skipping empty cells.
//!
//! # Provenance
//! Behavior contract: Microsoft support "TEXTJOIN function"
//! (<https://support.microsoft.com/en-us/office/textjoin-function-357b449a-ec91-49d0-80c3-0e8fc845691c>,
//! verified by WebFetch 2026-07-15). No `docs/specs/TEXTJOIN.md` exists in this
//! pass. Per-value text coercion is deferred entirely to `xl-value`'s frozen
//! [`to_text`], exactly as [`crate::func_concat`] / [`crate::func_concatenate`]
//! do, so the three agree cell-for-cell on scalar inputs. Boolean coercion of
//! `ignore_empty` is the frozen [`to_bool`] contract.
//!
//! # Behavior contract (one line)
//! `TEXTJOIN(delimiter, ignore_empty, text1, …)` = the text of every item
//! joined by `delimiter`; when `ignore_empty=TRUE`, empty cells are omitted.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - `delimiter` (arg 0): "A text string … or a reference to a valid text
//!   string. If a number is supplied, it will be treated as text." A scalar
//!   (incl. 1×1) delimiter is coerced via [`to_text`] (`Blank` → `""`). A
//!   genuine multi-cell/array delimiter is **refused** loudly — see below.
//! - `ignore_empty` (arg 1): coerced with [`to_bool`]. "TRUE" omits empty
//!   cells; "FALSE" includes them (producing consecutive delimiters).
//! - `text1, …`: each item's text is joined with `delimiter` in order. Numbers
//!   use "General" number-to-text via [`to_text`]; booleans → `"TRUE"`/
//!   `"FALSE"`. An item (scalar, or any cell of a range/array item) evaluating
//!   to an error makes that error the result.
//! - **32767-character cap**: "If the resulting string exceeds 32767 characters
//!   (cell limit), TEXTJOIN returns the #VALUE! error." Enforced on the joined
//!   result (character count).
//!
//! # Refused edges (loud), pending probes — see `docs/plans/2026-07-15-lane5-probe-needed.md`
//! - **Multi-cell / array delimiter** (L5-4): the cycling semantics of an array
//!   of delimiters are undocumented on this page → `#UNSUPPORTED!`.
//! - **`ignore_empty=TRUE` with an explicit empty string `Text("")`** (L5-2):
//!   the page documents omitting empty *cells* but "doesn't explicitly
//!   differentiate empty string text values from empty cells" — whether `""`
//!   is skipped or emitted as a zero-length segment is unpinned → the item
//!   `#UNSUPPORTED!`. (Genuinely blank cells in a *range* are elided upstream
//!   by the cell stream and correctly skipped; a scalar `Blank` under
//!   `ignore_empty=TRUE` is an empty cell and skipped per the page.)
//! - **`ignore_empty=FALSE` with a range/array text item** (L5-3): every empty
//!   cell must emit a delimiter, which needs blanks surfaced *positionally*
//!   (their count matters), but the cell stream elides blanks → `#UNSUPPORTED!`
//!   for that item. Scalar items under `ignore_empty=FALSE` are supported
//!   (`Blank`/`""` → an empty segment).
//!
//! # Array-position arguments (M2 lane 6 follow-up, 2026-09-04)
//! An argument in a range/array position is evaluated under the consumed-array
//! gate (RFC-0011; `docs/plans/2026-07-14-consumed-array-eval-spec.md` §2).
//! A materialized multi-cell array reaching this function is **refused** with a
//! loud `#UNSUPPORTED!` plus an engine diagnostic (spec §4, born-refusing
//! boundary): only the SUM/SUMPRODUCT consumers are oracle-pinned (OXP-201), and
//! the legacy alternative — a silent, host-row-dependent implicit intersection —
//! is a "never silently wrong" violation. Plain ranges are unchanged.

use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value, to_bool, to_text};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Excel's per-cell character limit; a TEXTJOIN result longer than this is
/// `#VALUE!` (MS page).
const CELL_CHAR_LIMIT: usize = 32767;

/// Evaluate a `TEXTJOIN(delimiter, ignore_empty, text1, …)` call. See the
/// module docs for the semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // --- delimiter (arg 0) ------------------------------------------------
    // A genuine multi-cell/array delimiter refuses loudly (L5-4). A scalar,
    // omitted, or 1×1 delimiter is coerced to text.
    match args.shape(0) {
        ArgShape::Range | ArgShape::Array => return Value::Error(ErrorKind::Unsupported),
        ArgShape::Scalar | ArgShape::Omitted => {}
    }
    let delimiter = match to_text(&args.eval_scalar(0)) {
        Ok(t) => t.as_str().to_owned(),
        Err(k) => return Value::Error(k),
    };

    // --- ignore_empty (arg 1) --------------------------------------------
    let ignore_empty = match to_bool(&args.eval_scalar(1)) {
        Ok(b) => b,
        Err(k) => return Value::Error(k),
    };

    // --- text items (args 2..) -------------------------------------------
    let mut segments: Vec<String> = Vec::new();
    for i in 2..args.count() {
        match args.shape(i) {
            ArgShape::Scalar | ArgShape::Omitted => {
                // Array position: evaluate under the array-context gate, so an operator
                // expression over a range materializes (and the scalar coercion below
                // refuses it loudly — unpinned for this function) instead of being
                // implicit-intersected into a silent host-row-dependent value.
                let v = args.eval_scalar_array_arg(i);
                match classify_scalar(&v, ignore_empty) {
                    ScalarItem::Skip => {}
                    ScalarItem::Refuse => return Value::Error(ErrorKind::Unsupported),
                    ScalarItem::Text => match to_text(&v) {
                        Ok(t) => segments.push(t.as_str().to_owned()),
                        Err(k) => return Value::Error(k),
                    },
                }
            }
            ArgShape::Range | ArgShape::Array => {
                // ignore_empty=FALSE over a range needs positional blank
                // surfacing the cell stream cannot give → refuse loudly (L5-3).
                if !ignore_empty {
                    return Value::Error(ErrorKind::Unsupported);
                }
                let mut err: Option<ErrorKind> = None;
                let mut defer = false;
                let segs = &mut segments;
                args.for_each_cell(i, &mut |v| match v {
                    Value::Error(k) => {
                        err = Some(*k);
                        ControlFlow::Break(())
                    }
                    // ignore_empty=TRUE + explicit empty string: unpinned (L5-2).
                    Value::Text(t) if t.as_str().is_empty() => {
                        defer = true;
                        ControlFlow::Break(())
                    }
                    // A blank cell is empty → skipped (the stream already elides
                    // genuinely-absent cells; a computed Blank is skipped here).
                    Value::Blank => ControlFlow::Continue(()),
                    _ => match to_text(v) {
                        Ok(t) => {
                            segs.push(t.as_str().to_owned());
                            ControlFlow::Continue(())
                        }
                        Err(k) => {
                            err = Some(k);
                            ControlFlow::Break(())
                        }
                    },
                });
                if let Some(k) = err {
                    return Value::Error(k);
                }
                if defer {
                    return Value::Error(ErrorKind::Unsupported);
                }
            }
        }
    }

    let joined = segments.join(&delimiter);
    // MS page: a result over the 32767-character cell limit → #VALUE!.
    if joined.chars().count() > CELL_CHAR_LIMIT {
        return Value::Error(ErrorKind::Value);
    }
    Value::text(&joined)
}

/// How a scalar text item is handled under the current `ignore_empty` flag.
enum ScalarItem {
    /// Contribute the value's text form as a segment.
    Text,
    /// Skip the item entirely (an empty cell under `ignore_empty=TRUE`).
    Skip,
    /// Refuse loudly (an explicit `""` under `ignore_empty=TRUE`, L5-2).
    Refuse,
}

/// Classify a scalar text item. An error value is handled by the caller (it
/// coerces via [`to_text`], which propagates it), so it maps to
/// [`ScalarItem::Text`] here and errors at coercion time.
fn classify_scalar(v: &Value, ignore_empty: bool) -> ScalarItem {
    match v {
        // A scalar Blank is an empty cell: skipped when ignoring empties,
        // otherwise an empty segment ("").
        Value::Blank if ignore_empty => ScalarItem::Skip,
        // An explicit empty string under ignore_empty=TRUE is the unpinned
        // ""-vs-blank edge (L5-2). Under ignore_empty=FALSE it is an
        // unambiguous empty segment, handled by to_text.
        Value::Text(t) if ignore_empty && t.as_str().is_empty() => ScalarItem::Refuse,
        _ => ScalarItem::Text,
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // Basic scalar join with a delimiter, ignore_empty=TRUE.
    #[test]
    fn scalar_join_with_delimiter() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt(", ")),
                    Scalar(Value::Bool(true)),
                    Scalar(txt("a")),
                    Scalar(txt("b")),
                    Scalar(txt("c")),
                ]
            ),
            txt("a, b, c")
        );
    }

    // A range item is flattened; cells joined with the delimiter.
    #[test]
    fn range_item_flattened() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("-")),
                    Scalar(Value::Bool(true)),
                    Range(vec![txt("x"), txt("y"), txt("z")]),
                ]
            ),
            txt("x-y-z")
        );
    }

    // ignore_empty=TRUE skips a scalar Blank (an empty cell).
    #[test]
    fn ignore_empty_true_skips_scalar_blank() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt(",")),
                    Scalar(Value::Bool(true)),
                    Scalar(txt("a")),
                    Scalar(Value::Blank),
                    Scalar(txt("b")),
                ]
            ),
            txt("a,b")
        );
    }

    // ignore_empty=TRUE over a range with genuinely-blank cells: blanks are
    // elided by the cell stream and skipped (Range mock omits absent cells);
    // a Blank passed explicitly is skipped too.
    #[test]
    fn ignore_empty_true_range_skips_blanks() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt(",")),
                    Scalar(Value::Bool(true)),
                    Range(vec![txt("a"), Value::Blank, txt("b")]),
                ]
            ),
            txt("a,b")
        );
    }

    // Numbers coerce via General text form.
    #[test]
    fn number_coercion() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("|")),
                    Scalar(Value::Bool(true)),
                    Scalar(num(12.0)),
                    Scalar(num(3.5)),
                ]
            ),
            txt("12|3.5")
        );
    }

    // Empty-string delimiter concatenates with no separator.
    #[test]
    fn empty_delimiter_concatenates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("")),
                    Scalar(Value::Bool(true)),
                    Scalar(txt("a")),
                    Scalar(txt("b"))
                ]
            ),
            txt("ab")
        );
    }

    // A number delimiter is treated as text ("If a number is supplied, it will
    // be treated as text").
    #[test]
    fn numeric_delimiter_treated_as_text() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.0)),
                    Scalar(Value::Bool(true)),
                    Scalar(txt("a")),
                    Scalar(txt("b"))
                ]
            ),
            txt("a0b")
        );
    }

    // ignore_empty=FALSE with SCALAR items includes empties as "" segments.
    // TEXTJOIN(",", FALSE, "a", Blank, "b") → "a,,b".
    #[test]
    fn ignore_empty_false_scalars_include_empty_segments() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt(",")),
                    Scalar(Value::Bool(false)),
                    Scalar(txt("a")),
                    Scalar(Value::Blank),
                    Scalar(txt("b")),
                ]
            ),
            txt("a,,b")
        );
        // A scalar "" under ignore_empty=FALSE is likewise an empty segment.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt(",")),
                    Scalar(Value::Bool(false)),
                    Scalar(txt("a")),
                    Scalar(txt("")),
                    Scalar(txt("b")),
                ]
            ),
            txt("a,,b")
        );
    }

    // An error in a scalar item propagates.
    #[test]
    fn scalar_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt(",")),
                    Scalar(Value::Bool(true)),
                    Scalar(txt("a")),
                    Scalar(Value::Error(ErrorKind::Div0)),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    // An error in any cell of a range item propagates.
    #[test]
    fn range_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt(",")),
                    Scalar(Value::Bool(true)),
                    Range(vec![txt("a"), Value::Error(ErrorKind::Na), txt("b")]),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // An error in the delimiter propagates.
    #[test]
    fn delimiter_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Ref)),
                    Scalar(Value::Bool(true)),
                    Scalar(txt("a"))
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    // An error in ignore_empty (non-coercible) propagates as #VALUE!.
    #[test]
    fn ignore_empty_non_boolean_is_value_error() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt(",")), Scalar(txt("banana")), Scalar(txt("a"))]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // ---- Refused edges (loud) ------------------------------------------

    // L5-4: a multi-cell/array delimiter refuses loudly.
    #[test]
    fn array_delimiter_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![txt(","), txt(";")]),
                    Scalar(Value::Bool(true)),
                    Scalar(txt("a")),
                    Scalar(txt("b")),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // L5-2: ignore_empty=TRUE with an explicit "" scalar refuses loudly.
    #[test]
    fn empty_string_under_ignore_empty_true_refused_scalar() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt(",")),
                    Scalar(Value::Bool(true)),
                    Scalar(txt("a")),
                    Scalar(txt("")),
                    Scalar(txt("b")),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // L5-2: an explicit "" cell within a range under ignore_empty=TRUE also
    // refuses loudly.
    #[test]
    fn empty_string_under_ignore_empty_true_refused_range() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt(",")),
                    Scalar(Value::Bool(true)),
                    Range(vec![txt("a"), txt(""), txt("b")]),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // L5-3: ignore_empty=FALSE with a range item refuses loudly (positional
    // blanks unavailable via the cell stream).
    #[test]
    fn ignore_empty_false_range_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt(",")),
                    Scalar(Value::Bool(false)),
                    Range(vec![txt("a"), txt("b")]),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // 32767-character result cap → #VALUE!. Two 20000-char segments joined
    // exceed the limit.
    #[test]
    fn over_char_limit_is_value_error() {
        let big = "x".repeat(20000);
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("")),
                    Scalar(Value::Bool(true)),
                    Scalar(txt(&big)),
                    Scalar(txt(&big)),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // Just under the cap succeeds (sanity that the check is a cap, not a
    // blanket refusal): a single 32767-char segment is fine.
    #[test]
    fn at_char_limit_succeeds() {
        let big = "y".repeat(32767);
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("")),
                    Scalar(Value::Bool(true)),
                    Scalar(txt(&big))
                ]
            ),
            txt(&big)
        );
    }
}

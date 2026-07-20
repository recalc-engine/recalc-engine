//! `CONCAT` — joins the text representation of every value across all
//! arguments, with no separator, into one string. Unlike its older sibling
//! [`CONCATENATE`](crate::func_concatenate), a range/array argument is
//! **flattened**: every cell contributes, not just one.
//!
//! # Provenance
//! Behavior contract: `docs/specs/CONCAT.md` (which cites the Microsoft
//! CONCAT function page). Per-value text coercion is deferred entirely to
//! `xl-value`'s [`to_text`], exactly as `func_concatenate` does, so the two
//! functions agree cell-for-cell on scalar inputs (CONCAT.md §Coercion).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Every argument's text form is appended in argument order, no separator
//!   (CONCAT.md §1). Numbers use "General" number-to-text formatting via
//!   `to_text` (not a cell's display format); booleans → `"TRUE"`/`"FALSE"`;
//!   blank → `""`; text passes through unchanged (CONCAT.md §Coercion).
//! - **The headline difference from `CONCATENATE`:** a range/array argument
//!   is flattened and *every* cell is concatenated (CONCAT.md §2), whereas
//!   `CONCATENATE` takes a single scalar from such an argument. A scalar
//!   argument (including a 1×1 range/array, which the engine classifies as
//!   [`ArgShape::Scalar`]) goes through [`CallArgs::eval_scalar`]; a genuine
//!   multi-cell [`ArgShape::Range`]/[`ArgShape::Array`] argument is streamed
//!   cell-by-cell through [`CallArgs::for_each_cell`].
//! - Flatten order is **row-major** (left-to-right, then top-to-bottom):
//!   `for_each_cell`'s documented contract streams a range/array's cells in
//!   row-major order, which is the order CONCAT.md §3 documents as the
//!   expected convention. For single-row and single-column ranges the order
//!   is unambiguous; for a genuine multi-row-multi-column range we rely on
//!   the engine's row-major streaming.
//!   // OXP (unassigned): confirm row-major flatten order on the pinned Excel
//!   // build for a genuine 2-D (multi-row, multi-column) range argument
//!   // (CONCAT.md §"Oracle experiments needed"). Single-row/column shapes are
//!   // unambiguous and covered by the tests below.
//! - Blank cells contribute nothing (`""`): `for_each_cell` elides
//!   genuinely-empty cells upstream, and a formula-computed `Blank` coerces
//!   to `""` via `to_text` all the same, so either way a blank adds no
//!   characters (CONCAT.md §Hit-list, `""` vs Blank).
//! - **Error propagation.** Any argument — a scalar arg, or *any* cell within
//!   a flattened range/array argument — evaluating to an error makes that
//!   error CONCAT's result (CONCAT.md §Error behavior). The range scan
//!   short-circuits at the first error via [`ControlFlow::Break`], returning
//!   the first error encountered in the engine's row-major stream.
//! - The documented ~253-argument cap (vs. `CONCATENATE`'s 255) is a
//!   call-arity concern enforced upstream at parse time, not a runtime edge
//!   this evaluator decides; like `func_concatenate`, `eval` simply processes
//!   whatever argument list it is handed and enforces no cap of its own.
//!   // OXP (unassigned): confirm the exact 253-argument cap and the
//!   // at/over-boundary behavior on the pinned Excel build
//!   // (CONCAT.md §"Oracle experiments needed").

use std::ops::ControlFlow;

use xl_value::{Value, to_text};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Evaluate a `CONCAT(text1, [text2], ...)` call. See the module docs for the
/// semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut out = String::new();

    for i in 0..args.count() {
        match args.shape(i) {
            // A scalar arg (or 1×1 range/array, or an omitted slot that reads
            // as `Blank`) contributes exactly its scalar text form — the same
            // path CONCATENATE takes, so the two agree on scalar inputs.
            ArgShape::Scalar | ArgShape::Omitted => match to_text(&args.eval_scalar(i)) {
                Ok(t) => out.push_str(t.as_str()),
                Err(k) => return Value::Error(k),
            },
            // A genuine multi-cell range/array is flattened: every cell's text
            // form is appended in row-major order. Short-circuit and propagate
            // on the first erroring cell.
            ArgShape::Range | ArgShape::Array => {
                let mut err = None;
                args.for_each_cell(i, &mut |v| match to_text(v) {
                    Ok(t) => {
                        out.push_str(t.as_str());
                        ControlFlow::Continue(())
                    }
                    Err(k) => {
                        err = Some(k);
                        ControlFlow::Break(())
                    }
                });
                if let Some(k) = err {
                    return Value::Error(k);
                }
            }
        }
    }

    Value::text(&out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xl_value::ErrorKind;

    use crate::test_support::{TestArg, eval_direct, num, txt};

    #[test]
    fn scalars_concat_like_concatenate() {
        // Plain scalar join with no separator — matches CONCATENATE.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(txt("Hello")),
                    TestArg::Scalar(txt(", ")),
                    TestArg::Scalar(txt("World")),
                ]
            ),
            txt("Hello, World")
        );
    }

    #[test]
    fn range_argument_flattened_in_order() {
        // CONCAT's headline behavior: every cell of a range is concatenated,
        // in row-major order — unlike CONCATENATE, which would take one cell.
        assert_eq!(
            eval_direct(
                eval,
                vec![TestArg::Range(vec![txt("a"), txt("b"), txt("c")])]
            ),
            txt("abc")
        );
    }

    #[test]
    fn rect_range_flattened_row_major() {
        // A genuine 2-D (2×2) range flattens left-to-right, then top-to-bottom.
        assert_eq!(
            eval_direct(
                eval,
                vec![TestArg::Rect {
                    rows: 2,
                    cols: 2,
                    data: vec![txt("a"), txt("b"), txt("c"), txt("d")],
                }]
            ),
            txt("abcd")
        );
    }

    #[test]
    fn mixed_scalar_and_range() {
        // Scalars and a flattened range interleave in argument order.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(txt("[")),
                    TestArg::Range(vec![txt("x"), txt("y")]),
                    TestArg::Scalar(txt("]")),
                ]
            ),
            txt("[xy]")
        );
    }

    #[test]
    fn number_and_bool_coercion() {
        // Numbers use General text form; booleans literalize to TRUE/FALSE,
        // both for scalar args and for cells inside a flattened range.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(num(12.0)),
                    TestArg::Scalar(Value::Bool(true)),
                    TestArg::Range(vec![num(3.5), Value::Bool(false)]),
                ]
            ),
            txt("12TRUE3.5FALSE")
        );
    }

    #[test]
    fn blank_cells_contribute_nothing() {
        // A blank scalar arg and blank cells within a range add no characters
        // (Blank → ""), leaving only the surrounding text.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(txt("a")),
                    TestArg::Scalar(Value::Blank),
                    TestArg::Range(vec![txt("b"), Value::Blank, txt("c")]),
                ]
            ),
            txt("abc")
        );
    }

    #[test]
    fn omitted_argument_contributes_nothing() {
        // An elided argument slot reads as Blank → "".
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(txt("a")),
                    TestArg::Omitted,
                    TestArg::Scalar(txt("b")),
                ]
            ),
            txt("ab")
        );
    }

    #[test]
    fn scalar_error_propagates() {
        // An erroring scalar argument becomes the whole result.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(txt("a")),
                    TestArg::Scalar(Value::Error(ErrorKind::Div0)),
                    TestArg::Scalar(txt("b")),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn error_within_range_propagates() {
        // An error in *any* cell of a flattened range propagates — CONCAT must
        // scan every cell, unlike CONCATENATE's single-cell take.
        assert_eq!(
            eval_direct(
                eval,
                vec![TestArg::Range(vec![
                    txt("a"),
                    Value::Error(ErrorKind::Value),
                    txt("b"),
                ])]
            ),
            Value::Error(ErrorKind::Value)
        );
    }
}

//! `ROWS` — return the number of rows in a reference or array.
//!
//! # Provenance
//! Behavior contract: `docs/specs/ROWS.md` (which cites the Microsoft Learn
//! `ROWS` function page
//! `https://support.microsoft.com/en-us/office/rows-function-b592593e-3fc2-47f2-bec1-bda493811597`,
//! verified 2026-07-11). The page states only: "Returns the number of rows in
//! a reference or array," with `array` documented as "An array, an array
//! formula, or a reference to a range of cells for which you want the number
//! of rows," and two worked examples (`C1:E4` → 4, `{1,2,3;4,5,6}` → 2). It
//! does **not** state single-cell or whole-column/row behavior explicitly.
//!
//! # Semantics implemented
//! - A bounded rectangle — a range, an array constant/expression, or a
//!   materialized 1×1 range/array literal — reports its row count straight
//!   from [`CallArgs::dims`] (the doc-verified worked-example behavior above).
//! - A **whole-column/row reference** — `ROWS(A:A)`, `ROWS(1:1)` — reports the
//!   full sheet-axis extent through the RFC-0005 [`CallArgs::arg_ref_extent`]
//!   channel (see below).
//! - A **true scalar expression** (`ArgShape::Scalar` with `dims() == None` and
//!   no ref extent — a literal, a single-cell reference, or a computed
//!   sub-expression) is a 1×1 array → `1`. This mirrors [`CallArgs::dims`]'s own
//!   documented precision note that "a 1×1 range or array literal still reports
//!   `Some((1, 1))`" for the bounded case; the analogous true-scalar case is
//!   likewise one row.
//!
//! # Whole-column/row range — resolved via `arg_ref_extent` (OXP-116)
//! `ROWS(A:A)` in real Excel returns the worksheet's fixed row count
//! (**1,048,576** in current Excel versions) and `ROWS(1:1)` returns `1`
//! (RUN-2026-07-11-oracle01, OXP-116). [`CallArgs::dims`] deliberately returns
//! `None` for the unbounded shape (its documented engineering guardrail against
//! a 1M-row dense walk), but the RFC-0005 [`CallArgs::arg_ref_extent`] channel
//! surfaces the *geometry-resolved* full extent — the engine computes it from
//! the AST reference's shape (`A:A` spans rows `0..=1_048_575`), so no constant
//! is hard-coded in `xl-fn`. `ROWS` reads `rect.height`. A reference the engine
//! cannot resolve to a rectangle at all (a 3-D span, an unresolvable name) still
//! yields no extent → `#UNSUPPORTED!` (never a guessed number; Principle 2).

use xl_value::{ErrorKind, Value};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Evaluate a `ROWS(array)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Bounded rectangle (range, array constant/expression, or a 1×1
    // range/array literal): dims() reports the row count directly. Bounded
    // behavior is unchanged from before RFC 0005.
    if let Some((rows, _cols)) = args.dims(0) {
        return Value::number(f64::from(rows));
    }
    // Unbounded whole-column/row reference: dims() refuses it, but the RFC-0005
    // reference-position channel surfaces the geometry-resolved full extent, so
    // ROWS(A:A) = 1,048,576 and ROWS(1:1) = 1 (OXP-116, RUN-2026-07-11-oracle01).
    if let Some(rect) = args.arg_ref_extent(0) {
        return Value::number(f64::from(rect.height));
    }
    match args.shape(0) {
        // A scalar-shaped expression: evaluate it under the array-context gate
        // first (`eval_scalar_array_arg`, the SUM/SUMPRODUCT gate). An operator
        // expression over a multi-cell range (`ROWS(B1:B7*1)`) or a
        // function-produced array is a computed array whose row count is not
        // oracle-pinned for this function — refuse loudly rather than report
        // the silent 1 that scalar-context implicit intersection produced. A
        // Recalc refusal sentinel from inside the argument propagates; any
        // other value is a true scalar → a 1×1 array → 1 row (unchanged).
        ArgShape::Scalar => match args.eval_scalar_array_arg(0) {
            Value::Array(a) if a.as_scalar().is_none() => Value::Error(ErrorKind::Unsupported),
            Value::Error(k) if k.is_recalc_sentinel() => Value::Error(k),
            _ => Value::number(1.0),
        },
        // A non-scalar with neither bounded dims nor a resolvable ref extent (a
        // 3-D span, an unresolvable name): no row count without guessing.
        ArgShape::Range | ArgShape::Array | ArgShape::Omitted => {
            Value::Error(ErrorKind::Unsupported)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::args::RefRect;
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::{ErrorKind, Value};

    /// Excel's fixed worksheet row count (1,048,576) as a 0-based max + 1.
    const SHEET_ROWS: u32 = 1_048_576;
    const SHEET_COLS: u32 = 16_384;

    #[test]
    fn range_reports_row_count() {
        // ROWS(A1:A3) — a 3×1 column range -> 3.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(1.0), num(2.0), num(3.0)])]),
            num(3.0)
        );
    }

    #[test]
    fn array_row_literal_is_one_row() {
        // ROWS({1,2}) — a 1×2 array constant (a single row) -> 1.
        assert_eq!(
            eval_direct(eval, vec![Array(vec![num(1.0), num(2.0)])]),
            num(1.0)
        );
    }

    #[test]
    fn rect_reports_row_count() {
        // ROWS of a 2×3 rectangle -> 2 (matches the MS Learn C1:E4 -> 4
        // worked example's shape-counting behavior, scaled down).
        assert_eq!(
            eval_direct(
                eval,
                vec![Rect {
                    rows: 2,
                    cols: 3,
                    data: vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0), num(6.0)],
                }]
            ),
            num(2.0)
        );
    }

    #[test]
    fn scalar_is_one_row() {
        // ROWS(5) — a true scalar expression is a 1×1 array -> 1.
        assert_eq!(eval_direct(eval, vec![Scalar(num(5.0))]), num(1.0));
    }

    #[test]
    fn whole_column_reports_full_row_extent() {
        // ROWS(A:A) — a whole-column reference reports the sheet's fixed row
        // count via arg_ref_extent (OXP-116, RUN-2026-07-11-oracle01: 1048576).
        assert_eq!(
            eval_direct(
                eval,
                vec![Reference(RefRect {
                    row: 0,
                    col: 0,
                    height: SHEET_ROWS,
                    width: 1,
                })]
            ),
            num(1_048_576.0)
        );
    }

    #[test]
    fn whole_row_reports_one_row() {
        // ROWS(1:1) — a whole-row reference is one row tall (OXP-116,
        // RUN-2026-07-11-oracle01: 1).
        assert_eq!(
            eval_direct(
                eval,
                vec![Reference(RefRect {
                    row: 0,
                    col: 0,
                    height: 1,
                    width: SHEET_COLS,
                })]
            ),
            num(1.0)
        );
    }

    #[test]
    fn unresolvable_reference_is_unsupported() {
        // A range shape with neither bounded dims nor a resolvable ref extent
        // (e.g. a 3-D span) still defers rather than guessing a row count.
        assert_eq!(
            eval_direct(eval, vec![Unbounded(vec![num(1.0), num(2.0), num(3.0)])]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

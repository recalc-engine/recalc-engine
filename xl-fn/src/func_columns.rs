//! `COLUMNS` — return the number of columns in a reference or array.
//!
//! # Provenance
//! Behavior contract: `docs/specs/COLUMNS.md` (which cites the Microsoft Learn
//! `COLUMNS` function page
//! `https://support.microsoft.com/en-us/office/columns-function-4e8e7b4e-e603-43e8-b177-956088fa48ca`).
//! The spec documents `COLUMNS` as the exact transpose of `ROWS`: "Returns the
//! number of columns in `array`, as a plain integer — a shape query, not a
//! value aggregation," working identically for a real range reference or an
//! in-memory array constant/expression, with a single-cell reference/scalar →
//! `1`. It does **not** state whole-column/row behavior explicitly.
//!
//! # ROWS symmetry
//! This is the column-axis mirror of [`crate::func_rows`]: `ROWS` reads the
//! row count (`rect.height`), `COLUMNS` reads the column count (`rect.width`).
//! Every shape decision below matches `ROWS`, transposed onto the column
//! dimension — including the RFC-0005 whole-column/row handling.
//!
//! # Semantics implemented
//! - A bounded rectangle — a range, an array constant/expression, or a
//!   materialized 1×1 range/array literal — reports its column count straight
//!   from [`CallArgs::dims`] (the doc-verified worked-example behavior).
//! - A **whole-column/row reference** — `COLUMNS(A:A)`, `COLUMNS(1:1)` — reports
//!   the full sheet-axis extent through the RFC-0005
//!   [`CallArgs::arg_ref_extent`] channel (see below).
//! - A **true scalar expression** (`ArgShape::Scalar` with `dims() == None` and
//!   no ref extent — a literal, a single-cell reference, or a computed
//!   sub-expression) is a 1×1 array → `1`, mirroring `ROWS`'s scalar case on the
//!   column axis.
//!
//! # Whole-column/row range — resolved via `arg_ref_extent` (OXP-116)
//! `COLUMNS(1:1)` in real Excel returns the worksheet's fixed column count
//! (**16,384** in current Excel versions) and `COLUMNS(A:A)` returns `1`
//! (RUN-2026-07-11-oracle01, OXP-116). As with `ROWS`, [`CallArgs::dims`]
//! refuses the unbounded shape (`None`) but the RFC-0005
//! [`CallArgs::arg_ref_extent`] channel surfaces the geometry-resolved full
//! extent (`1:1` spans columns `0..=16_383`), so no constant is hard-coded in
//! `xl-fn`. `COLUMNS` reads `rect.width`. A reference the engine cannot resolve
//! to a rectangle (a 3-D span, an unresolvable name) yields no extent →
//! `#UNSUPPORTED!` (Principle 2), the same conservative choice `ROWS` makes.

use xl_value::{ErrorKind, Value};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Evaluate a `COLUMNS(array)` call. See the module docs for the semantics and
/// their spec provenance. Column-axis transpose of [`crate::func_rows::eval`].
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Bounded rectangle (range, array constant/expression, or a 1×1
    // range/array literal): dims() reports the column count directly (second
    // field — ROWS reads the first). Bounded behavior is unchanged from before
    // RFC 0005.
    if let Some((_rows, cols)) = args.dims(0) {
        return Value::number(f64::from(cols));
    }
    // Unbounded whole-column/row reference: dims() refuses it, but the RFC-0005
    // reference-position channel surfaces the geometry-resolved full extent, so
    // COLUMNS(1:1) = 16,384 and COLUMNS(A:A) = 1 (OXP-116, RUN-2026-07-11-oracle01).
    if let Some(rect) = args.arg_ref_extent(0) {
        return Value::number(f64::from(rect.width));
    }
    match args.shape(0) {
        // A true scalar expression is a 1×1 array → 1 column.
        ArgShape::Scalar => Value::number(1.0),
        // A non-scalar with neither bounded dims nor a resolvable ref extent (a
        // 3-D span, an unresolvable name): no column count without guessing.
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

    const SHEET_ROWS: u32 = 1_048_576;
    const SHEET_COLS: u32 = 16_384;

    #[test]
    fn column_range_is_one_column() {
        // COLUMNS(A1:A3) — a 3×1 column range -> 1 column.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(1.0), num(2.0), num(3.0)])]),
            num(1.0)
        );
    }

    #[test]
    fn array_row_literal_reports_column_count() {
        // COLUMNS({1,2}) — a 1×2 array constant (a single row) -> 2 columns.
        assert_eq!(
            eval_direct(eval, vec![Array(vec![num(1.0), num(2.0)])]),
            num(2.0)
        );
    }

    #[test]
    fn rect_reports_column_count() {
        // COLUMNS of a 2×3 rectangle -> 3 (the transpose of the ROWS rect test,
        // which reports 2 rows for the same shape).
        assert_eq!(
            eval_direct(
                eval,
                vec![Rect {
                    rows: 2,
                    cols: 3,
                    data: vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0), num(6.0)],
                }]
            ),
            num(3.0)
        );
    }

    #[test]
    fn scalar_is_one_column() {
        // COLUMNS(5) — a true scalar expression is a 1×1 array -> 1.
        assert_eq!(eval_direct(eval, vec![Scalar(num(5.0))]), num(1.0));
    }

    #[test]
    fn whole_row_reports_full_column_extent() {
        // COLUMNS(1:1) — a whole-row reference reports the sheet's fixed column
        // count via arg_ref_extent (OXP-116, RUN-2026-07-11-oracle01: 16384).
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
            num(16_384.0)
        );
    }

    #[test]
    fn whole_column_reports_one_column() {
        // COLUMNS(A:A) — a whole-column reference is one column wide (OXP-116,
        // RUN-2026-07-11-oracle01: 1).
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
            num(1.0)
        );
    }

    #[test]
    fn unresolvable_reference_is_unsupported() {
        // A range shape with neither bounded dims nor a resolvable ref extent
        // (e.g. a 3-D span) still defers rather than guessing a column count.
        assert_eq!(
            eval_direct(eval, vec![Unbounded(vec![num(1.0), num(2.0), num(3.0)])]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

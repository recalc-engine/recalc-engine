//! `TAKE` — return contiguous rows/columns from the start or end of an array.
//!
//! # Provenance
//! Behavior contract: Microsoft support "TAKE function"
//! (<https://support.microsoft.com/en-us/office/take-function-25382ff1-5da1-4f78-ab43-f33bd2e4e003>,
//! verified by WebFetch 2026-07-15). No `docs/specs/TAKE.md` exists; this is a
//! clean-room implementation of the documented behavior only. The `_xlfn.`
//! authoring prefix is stripped by the parser
//! (`xl-ast::parser::parse_func_name`), so the registry name is bare `TAKE`.
//!
//! # Behavior contract (one line)
//! `TAKE(array, rows, [columns])` returns the first (or, for a negative count,
//! the last) `rows`/`columns` of `array`.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - `rows` (required): "The number of rows to take. A negative value takes from
//!   the end of the array." Positive → from the start; negative → from the end.
//! - `columns` (optional): same sign rule; omitted → all columns.
//! - `rows`/`columns == 0` → `#CALC!`: "Excel returns a #CALC! error to indicate
//!   an empty array when either rows or columns is 0."
//!
//! # Refused / assumed edges — see `docs/plans/2026-07-15-lane3a-probe-needed.md`
//! - **Unbounded whole-column/row / over-cap input** — `arrayshape` (L3A-CAP).
//! - **Elided `rows` (`TAKE(a,,3)`)** (L3A-TAKEROWS): `rows` is required and its
//!   elided default is unpinned → refused.
//! - **Non-integer / array-valued `rows`/`columns`** (L3A-FRAC / L3A-ARRIDX):
//!   refused.
//! - **`|count|` greater than the axis length** (L3A-CLAMP): clamped to the whole
//!   axis (assumed — the only sensible reading, but not textually documented).

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::arrayshape::{IntArg, Materialized, int_arg, materialize, subrect};
use crate::context::EvalContext;

/// Evaluate a `TAKE(array, rows, [columns])` call. See the module docs for the
/// semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let grid = match materialize(args, 0) {
        Materialized::Grid(g) => g,
        Materialized::Omitted => return Value::Error(ErrorKind::Unsupported),
        Materialized::Refused(k) => return Value::Error(k),
    };

    // rows (arg 1, required).
    let rows_n = match int_arg(args, 1) {
        IntArg::Value(n) => n,
        // Elided rows is unpinned; fractional / array-valued undocumented.
        IntArg::Omitted | IntArg::NonInteger | IntArg::NonScalar => {
            return Value::Error(ErrorKind::Unsupported);
        }
        IntArg::Err(k) => return Value::Error(k),
    };

    // columns (arg 2, optional): omitted → all columns.
    let cols_opt = match int_arg(args, 2) {
        IntArg::Value(n) => Some(n),
        IntArg::Omitted => None,
        IntArg::NonInteger | IntArg::NonScalar => return Value::Error(ErrorKind::Unsupported),
        IntArg::Err(k) => return Value::Error(k),
    };

    let (r0, r1) = match take_axis(rows_n, grid.rows) {
        Ok(rng) => rng,
        Err(v) => return v,
    };
    let (c0, c1) = match cols_opt {
        Some(nc) => match take_axis(nc, grid.cols) {
            Ok(rng) => rng,
            Err(v) => return v,
        },
        None => (0, grid.cols),
    };
    subrect(&grid, r0, r1, c0, c1)
}

/// The half-open `[start, end)` window `TAKE` keeps on one axis of length `dim`.
/// `#CALC!` for `n == 0` (documented empty-array); `|n|` clamps to `dim`.
fn take_axis(n: i64, dim: usize) -> Result<(usize, usize), Value> {
    if n == 0 {
        return Err(Value::Error(ErrorKind::Calc));
    }
    let k = n.unsigned_abs().min(dim as u64) as usize;
    if n > 0 {
        Ok((0, k))
    } else {
        Ok((dim - k, dim))
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg, eval_direct, num};
    use xl_value::{Array, ErrorKind, Value};

    use TestArg::*;

    fn rect() -> TestArg {
        Rect {
            rows: 3,
            cols: 3,
            data: vec![
                num(1.0),
                num(2.0),
                num(3.0),
                num(4.0),
                num(5.0),
                num(6.0),
                num(7.0),
                num(8.0),
                num(9.0),
            ],
        }
    }

    fn arr(rows: usize, cols: usize, data: Vec<Value>) -> Value {
        Value::Array(Array::new(rows, cols, data).unwrap())
    }

    // TAKE(rect, 2) → first 2 rows, all columns.
    #[test]
    fn take_first_rows_all_cols() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(2.0))]),
            arr(
                2,
                3,
                vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0), num(6.0)]
            )
        );
    }

    // TAKE(rect, -1) → the last row.
    #[test]
    fn take_last_row_negative() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(-1.0))]),
            arr(1, 3, vec![num(7.0), num(8.0), num(9.0)])
        );
    }

    // TAKE(rect, 2, 2) → top-left 2×2 block.
    #[test]
    fn take_rows_and_cols() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(2.0)), Scalar(num(2.0))]),
            arr(2, 2, vec![num(1.0), num(2.0), num(4.0), num(5.0)])
        );
    }

    // TAKE(rect, 2, -1) → first 2 rows, last column.
    #[test]
    fn take_negative_cols() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(2.0)), Scalar(num(-1.0))]),
            arr(2, 1, vec![num(3.0), num(6.0)])
        );
    }

    // rows == 0 → #CALC! (documented empty array).
    #[test]
    fn zero_rows_is_calc() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(0.0))]),
            Value::Error(ErrorKind::Calc)
        );
    }

    // L3A-CLAMP: |count| exceeding the axis clamps to the whole axis.
    #[test]
    fn overlarge_count_clamps() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(99.0))]),
            arr(
                3,
                3,
                vec![
                    num(1.0),
                    num(2.0),
                    num(3.0),
                    num(4.0),
                    num(5.0),
                    num(6.0),
                    num(7.0),
                    num(8.0),
                    num(9.0)
                ]
            )
        );
    }

    // L3A-TAKEROWS: an elided rows argument refuses.
    #[test]
    fn elided_rows_refused() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Omitted, Scalar(num(2.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // L3A-FRAC: a fractional count refuses.
    #[test]
    fn fractional_count_refused() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(1.5))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

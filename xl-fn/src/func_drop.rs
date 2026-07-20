//! `DROP` — exclude rows/columns from the start or end of an array.
//!
//! # Provenance
//! Behavior contract: Microsoft support "DROP function"
//! (<https://support.microsoft.com/en-us/office/drop-function-1cb4e151-9e17-4838-abe5-9ba48d8c6a34>,
//! verified by WebFetch 2026-07-15). No `docs/specs/DROP.md` exists; this is a
//! clean-room implementation of the documented behavior only. The `_xlfn.`
//! authoring prefix is stripped by the parser
//! (`xl-ast::parser::parse_func_name`), so the registry name is bare `DROP`.
//!
//! # Behavior contract (one line)
//! `DROP(array, rows, [columns])` returns `array` with the first (or, for a
//! negative count, the last) `rows`/`columns` removed.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - `rows` (required): "The number of rows to drop. A negative value drops from
//!   the end of the array." Positive → from the start; negative → from the end.
//! - `columns` (optional): same sign rule; omitted → drop no columns.
//!
//! # Refused / assumed edges — see `docs/plans/2026-07-15-lane3a-probe-needed.md`
//! - **`rows`/`columns == 0`** (L3A-DROP0): the page states "#CALC! … when rows
//!   or columns is 0", which **contradicts** the logical no-op reading (dropping
//!   0 should leave the array unchanged) and reads like text copied from `TAKE`.
//!   Unresolved → refused (`#UNSUPPORTED!`) rather than encode a probable doc
//!   error. One farm probe settles it.
//! - **Elided `rows` (`DROP(a,,3)`)** (L3A-DROPROWS): `rows` is required; its
//!   elided meaning is unpinned → refused.
//! - **Non-integer / array-valued `rows`/`columns`** (L3A-FRAC / L3A-ARRIDX):
//!   refused.
//! - **Dropping the whole axis (`|count|` ≥ axis length)** (L3A-DROPALL,
//!   *assumed*): the result is empty → `#CALC!`. This is **not** documented on
//!   the DROP page — its only `#CALC!` sentence is the count==0 one refused
//!   above. The `#CALC!` here is *extrapolated* from the empty-array rationale
//!   and TAKE's parallel, so it is an assumption pending a probe, not a
//!   documented path.
//! - **Unbounded whole-column/row / over-cap input** — `arrayshape` (L3A-CAP).

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::arrayshape::{IntArg, Materialized, int_arg, materialize, subrect};
use crate::context::EvalContext;

/// Evaluate a `DROP(array, rows, [columns])` call. See the module docs for the
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
        IntArg::Omitted | IntArg::NonInteger | IntArg::NonScalar => {
            return Value::Error(ErrorKind::Unsupported);
        }
        IntArg::Err(k) => return Value::Error(k),
    };

    // columns (arg 2, optional): omitted → drop no columns.
    let cols_opt = match int_arg(args, 2) {
        IntArg::Value(n) => Some(n),
        IntArg::Omitted => None,
        IntArg::NonInteger | IntArg::NonScalar => return Value::Error(ErrorKind::Unsupported),
        IntArg::Err(k) => return Value::Error(k),
    };

    let (r0, r1) = match drop_axis(rows_n, grid.rows) {
        Ok(rng) => rng,
        Err(v) => return v,
    };
    let (c0, c1) = match cols_opt {
        Some(nc) => match drop_axis(nc, grid.cols) {
            Ok(rng) => rng,
            Err(v) => return v,
        },
        None => (0, grid.cols),
    };
    subrect(&grid, r0, r1, c0, c1)
}

/// The half-open `[start, end)` window `DROP` keeps on one axis of length `dim`.
/// `n == 0` is refused (`#UNSUPPORTED!`, doc-vs-logic contradiction, L3A-DROP0);
/// dropping the whole axis empties the result → `#CALC!` (*assumed*, not on the
/// DROP page — extrapolated from the empty-array rationale + TAKE; L3A-DROPALL).
fn drop_axis(n: i64, dim: usize) -> Result<(usize, usize), Value> {
    if n == 0 {
        return Err(Value::Error(ErrorKind::Unsupported));
    }
    let k = n.unsigned_abs().min(dim as u64) as usize;
    let (start, end) = if n > 0 { (k, dim) } else { (0, dim - k) };
    if start >= end {
        return Err(Value::Error(ErrorKind::Calc));
    }
    Ok((start, end))
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

    // DROP(rect, 1) → drops the first row.
    #[test]
    fn drop_first_row() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(1.0))]),
            arr(
                2,
                3,
                vec![num(4.0), num(5.0), num(6.0), num(7.0), num(8.0), num(9.0)]
            )
        );
    }

    // DROP(rect, -2) → drops the last two rows, leaving the first.
    #[test]
    fn drop_last_two_rows() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(-2.0))]),
            arr(1, 3, vec![num(1.0), num(2.0), num(3.0)])
        );
    }

    // DROP(rect, 1, 1) → drop first row and first column.
    #[test]
    fn drop_row_and_col() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(1.0)), Scalar(num(1.0))]),
            arr(2, 2, vec![num(5.0), num(6.0), num(8.0), num(9.0)])
        );
    }

    // Dropping every row empties the result → #CALC! (L3A-DROPALL, assumed:
    // extrapolated from the empty-array rationale + TAKE, not on the DROP page).
    #[test]
    fn drop_all_rows_is_calc() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(3.0))]),
            Value::Error(ErrorKind::Calc)
        );
        // |count| exceeding the axis also empties → #CALC!.
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(-9.0))]),
            Value::Error(ErrorKind::Calc)
        );
    }

    // L3A-DROP0: a zero count refuses (doc-vs-logic contradiction).
    #[test]
    fn zero_count_refused() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(0.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
        // Explicit columns == 0 refuses too.
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(1.0)), Scalar(num(0.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // Omitted columns drops no columns (keeps all).
    #[test]
    fn omitted_columns_keeps_all() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(2.0))]),
            arr(1, 3, vec![num(7.0), num(8.0), num(9.0)])
        );
    }

    // L3A-DROPROWS: an elided rows argument refuses.
    #[test]
    fn elided_rows_refused() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Omitted, Scalar(num(1.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

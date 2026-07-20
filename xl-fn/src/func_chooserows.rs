//! `CHOOSEROWS` — return the specified rows from an array, in the given order.
//!
//! # Provenance
//! Behavior contract: Microsoft support "CHOOSEROWS function"
//! (<https://support.microsoft.com/en-us/office/chooserows-function-51ace882-9bab-4a44-9625-7274ef7507a3>,
//! verified by WebFetch 2026-07-15). No `docs/specs/CHOOSEROWS.md` exists; this
//! is a clean-room implementation of the documented behavior only. The `_xlfn.`
//! authoring prefix is stripped by the parser
//! (`xl-ast::parser::parse_func_name`), so the registry name is bare
//! `CHOOSEROWS`.
//!
//! # Behavior contract (one line)
//! `CHOOSEROWS(array, row_num1, [row_num2], …)` returns the listed rows of
//! `array`, in argument order.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - Row numbers are 1-based (Example 1). A negative row number counts from the
//!   end ("−1 refers to the last row", Example 3).
//! - "Excel returns a #VALUE error if the absolute value of any of the row_num
//!   arguments is zero or exceeds the number of rows in the array."
//! - The chosen rows' cell values are relocated verbatim (structural select).
//!
//! # Refused / assumed edges — see `docs/plans/2026-07-15-lane3a-probe-needed.md`
//! - **Unbounded whole-column/row / over-cap** — `arrayshape` (L3A-CAP).
//! - **Array-valued / fractional / elided `row_num`** (L3A-ARRIDX / L3A-FRAC):
//!   refused (`#UNSUPPORTED!`).

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::arrayshape::{Materialized, collect_indices, materialize, over_cap, spill};
use crate::context::EvalContext;

/// Evaluate a `CHOOSEROWS(array, row_num1, …)` call. See the module docs for the
/// semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let grid = match materialize(args, 0) {
        Materialized::Grid(g) => g,
        Materialized::Omitted => return Value::Error(ErrorKind::Unsupported),
        Materialized::Refused(k) => return Value::Error(k),
    };
    let rows = match collect_indices(args, grid.rows) {
        Ok(r) => r,
        Err(v) => return v,
    };
    let num_rows = rows.len();
    if over_cap(num_rows, grid.cols) {
        return Value::Error(ErrorKind::Unsupported);
    }
    let mut data: Vec<Value> = Vec::with_capacity(num_rows * grid.cols);
    for &r in &rows {
        for c in 0..grid.cols {
            data.push(grid.at(r, c).clone());
        }
    }
    spill(num_rows, grid.cols, data)
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
            cols: 2,
            data: vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0), num(6.0)],
        }
    }

    fn arr(rows: usize, cols: usize, data: Vec<Value>) -> Value {
        Value::Array(Array::new(rows, cols, data).unwrap())
    }

    // CHOOSEROWS(rect, 1, 3) → rows 1 and 3.
    #[test]
    fn choose_two_rows() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(1.0)), Scalar(num(3.0))]),
            arr(2, 2, vec![num(1.0), num(2.0), num(5.0), num(6.0)])
        );
    }

    // Rows are returned in the order requested (3 then 1).
    #[test]
    fn preserves_order() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(3.0)), Scalar(num(1.0))]),
            arr(2, 2, vec![num(5.0), num(6.0), num(1.0), num(2.0)])
        );
    }

    // Negative index counts from the end (-1 → last row).
    #[test]
    fn negative_index_from_end() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(-1.0))]),
            arr(1, 2, vec![num(5.0), num(6.0)])
        );
    }

    // A repeated row is allowed.
    #[test]
    fn repeat_row() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(2.0)), Scalar(num(2.0))]),
            arr(2, 2, vec![num(3.0), num(4.0), num(3.0), num(4.0)])
        );
    }

    // |index| == 0 → #VALUE!.
    #[test]
    fn zero_index_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(0.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // |index| exceeding the row count → #VALUE!.
    #[test]
    fn out_of_range_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(4.0))]),
            Value::Error(ErrorKind::Value)
        );
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(-4.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // L3A-ARRIDX: an array-valued index refuses.
    #[test]
    fn array_index_refused() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Range(vec![num(1.0), num(2.0)])]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // L3A-FRAC: a fractional index refuses.
    #[test]
    fn fractional_index_refused() {
        assert_eq!(
            eval_direct(eval, vec![rect(), Scalar(num(1.5))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

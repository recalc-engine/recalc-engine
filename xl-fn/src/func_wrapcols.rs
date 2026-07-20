//! `WRAPCOLS` — wrap a one-dimensional vector into columns of a fixed height.
//!
//! # Provenance
//! Behavior contract: Microsoft support "WRAPCOLS function"
//! (<https://support.microsoft.com/en-us/office/wrapcols-function-d038b05a-57b7-4ee0-be94-ded0792511e2>,
//! verified by WebFetch 2026-07-15). No `docs/specs/WRAPCOLS.md` exists; this is
//! a clean-room implementation of the documented behavior only. The `_xlfn.`
//! authoring prefix is stripped by the parser
//! (`xl-ast::parser::parse_func_name`), so the registry name is bare `WRAPCOLS`.
//! The column/row transpose of [`crate::func_wraprows`]; shared validation lives
//! in [`crate::arrayshape::wrap_inputs`].
//!
//! # Behavior contract (one line)
//! `WRAPCOLS(vector, wrap_count, [pad_with])` lays `vector` out into columns of
//! `wrap_count` values each, padding the final column.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - "The elements of the vector are placed into a 2-dimensional array by column.
//!   Each column has wrap_count elements. The column is padded with pad_with if
//!   there are insufficient elements to fill it." `pad_with` default is `#N/A`.
//! - Non-vector input (both dims > 1) → `#VALUE!`; `wrap_count < 1` → `#NUM!`.
//!
//! # Refused / assumed edges — see `docs/plans/2026-07-15-lane3a-probe-needed.md`
//! Identical to [`crate::func_wraprows`]: `wrap_count > len` (L3A-WRAP-SINGLE,
//! refused), non-integer / array-valued `wrap_count`, array-valued `pad_with`,
//! and unbounded / over-cap input.

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::arrayshape::{WrapInputs, over_cap, spill, wrap_inputs};
use crate::context::EvalContext;

/// Evaluate a `WRAPCOLS(vector, wrap_count, [pad_with])` call. See the module
/// docs for the semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let (elems, wrap, pad) = match wrap_inputs(args) {
        WrapInputs::Ready { elems, wrap, pad } => (elems, wrap, pad),
        WrapInputs::Return(v) => return v,
    };
    let n = elems.len();
    // Columns needed, each `wrap` tall (n >= wrap >= 1 here). Result is
    // `wrap` rows × `num_cols` columns.
    let num_cols = n.div_ceil(wrap);
    if over_cap(wrap, num_cols) {
        return Value::Error(ErrorKind::Unsupported);
    }
    let mut data: Vec<Value> = Vec::with_capacity(wrap * num_cols);
    // Row-major output: column `c` holds elems[c*wrap .. c*wrap+wrap].
    for r in 0..wrap {
        for c in 0..num_cols {
            let src = c * wrap + r;
            if src < n {
                data.push(elems[src].clone());
            } else {
                data.push(pad.clone());
            }
        }
    }
    spill(wrap, num_cols, data)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::{Array, ErrorKind, Value};

    fn arr(rows: usize, cols: usize, data: Vec<Value>) -> Value {
        Value::Array(Array::new(rows, cols, data).unwrap())
    }

    // WRAPCOLS({1..5}, 2) → 2 rows × 3 cols; column-major fill, last col padded.
    // Columns: [1;2], [3;4], [5;#N/A] → row-major {1,3,5; 2,4,#N/A}.
    #[test]
    fn wrap_with_padding() {
        let v = Array(vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0)]);
        assert_eq!(
            eval_direct(eval, vec![v, Scalar(num(2.0))]),
            arr(
                2,
                3,
                vec![
                    num(1.0),
                    num(3.0),
                    num(5.0),
                    num(2.0),
                    num(4.0),
                    Value::Error(ErrorKind::Na)
                ]
            )
        );
    }

    // Exact fill: {1..4} wrap 2 → columns [1;2],[3;4] → {1,3; 2,4}.
    #[test]
    fn exact_fill() {
        let v = Array(vec![num(1.0), num(2.0), num(3.0), num(4.0)]);
        assert_eq!(
            eval_direct(eval, vec![v, Scalar(num(2.0))]),
            arr(2, 2, vec![num(1.0), num(3.0), num(2.0), num(4.0)])
        );
    }

    // wrap_count == len → a single exactly-filled column.
    #[test]
    fn wrap_equals_len_single_column() {
        let v = Array(vec![num(1.0), num(2.0), num(3.0)]);
        assert_eq!(
            eval_direct(eval, vec![v, Scalar(num(3.0))]),
            arr(3, 1, vec![num(1.0), num(2.0), num(3.0)])
        );
    }

    // Non-vector input (2-D) → #VALUE!.
    #[test]
    fn non_vector_is_value_error() {
        let g = Rect {
            rows: 2,
            cols: 2,
            data: vec![num(1.0), num(2.0), num(3.0), num(4.0)],
        };
        assert_eq!(
            eval_direct(eval, vec![g, Scalar(num(2.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // wrap_count < 1 → #NUM!.
    #[test]
    fn wrap_count_below_one_is_num() {
        let v = Array(vec![num(1.0), num(2.0)]);
        assert_eq!(
            eval_direct(eval, vec![v, Scalar(num(-1.0))]),
            Value::Error(ErrorKind::Num)
        );
    }

    // L3A-WRAP-SINGLE: wrap_count > len refuses.
    #[test]
    fn wrap_count_over_len_refused() {
        let v = Array(vec![num(1.0), num(2.0)]);
        assert_eq!(
            eval_direct(eval, vec![v, Scalar(num(9.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

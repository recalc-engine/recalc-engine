//! `WRAPROWS` — wrap a one-dimensional vector into rows of a fixed width.
//!
//! # Provenance
//! Behavior contract: Microsoft support "WRAPROWS function"
//! (<https://support.microsoft.com/en-us/office/wraprows-function-796825f3-975a-4cee-9c84-1bbddf60ade0>,
//! verified by WebFetch 2026-07-15). No `docs/specs/WRAPROWS.md` exists; this is
//! a clean-room implementation of the documented behavior only. The `_xlfn.`
//! authoring prefix is stripped by the parser
//! (`xl-ast::parser::parse_func_name`), so the registry name is bare `WRAPROWS`.
//!
//! # Behavior contract (one line)
//! `WRAPROWS(vector, wrap_count, [pad_with])` lays `vector` out into rows of
//! `wrap_count` values each, padding the final row.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - "The elements of the vector are placed into a 2-dimensional array by row.
//!   Each row has wrap_count elements. The row is padded with pad_with if there
//!   are insufficient elements to fill it." `pad_with` default is `#N/A`.
//! - Non-vector input (both dims > 1) → `#VALUE!`; `wrap_count < 1` → `#NUM!`
//!   (both documented, enforced in [`crate::arrayshape::wrap_inputs`]).
//!
//! # Refused / assumed edges — see `docs/plans/2026-07-15-lane3a-probe-needed.md`
//! - **`wrap_count > len(vector)`** (L3A-WRAP-SINGLE): the page's "simply
//!   returned in a single row" contradicts its general padding rule → refused.
//!   `wrap_count == len` (one exactly-filled row) is supported.
//! - **Non-integer / array-valued `wrap_count`, array-valued `pad_with`,
//!   unbounded / over-cap input** — refused (`arrayshape`).

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::arrayshape::{WrapInputs, over_cap, spill, wrap_inputs};
use crate::context::EvalContext;

/// Evaluate a `WRAPROWS(vector, wrap_count, [pad_with])` call. See the module
/// docs for the semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let (elems, wrap, pad) = match wrap_inputs(args) {
        WrapInputs::Ready { elems, wrap, pad } => (elems, wrap, pad),
        WrapInputs::Return(v) => return v,
    };
    let n = elems.len();
    // Rows needed to hold `n` elements `wrap` per row (n >= wrap >= 1 here).
    let num_rows = n.div_ceil(wrap);
    if over_cap(num_rows, wrap) {
        return Value::Error(ErrorKind::Unsupported);
    }
    // Row-major output is simply the vector followed by padding: `elems` already
    // holds the first `n` cells in order, so grow to `num_rows * wrap` with `pad`.
    let mut data = elems;
    data.resize(num_rows * wrap, pad);
    spill(num_rows, wrap, data)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::{Array, ErrorKind, Value};

    fn arr(rows: usize, cols: usize, data: Vec<Value>) -> Value {
        Value::Array(Array::new(rows, cols, data).unwrap())
    }

    // WRAPROWS({1..5}, 2) → 3 rows of 2, last row padded with #N/A.
    #[test]
    fn wrap_with_padding() {
        let v = Array(vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0)]);
        assert_eq!(
            eval_direct(eval, vec![v, Scalar(num(2.0))]),
            arr(
                3,
                2,
                vec![
                    num(1.0),
                    num(2.0),
                    num(3.0),
                    num(4.0),
                    num(5.0),
                    Value::Error(ErrorKind::Na)
                ]
            )
        );
    }

    // Exact fill (n divisible by wrap_count) → no padding.
    #[test]
    fn exact_fill() {
        let v = Array(vec![num(1.0), num(2.0), num(3.0), num(4.0)]);
        assert_eq!(
            eval_direct(eval, vec![v, Scalar(num(2.0))]),
            arr(2, 2, vec![num(1.0), num(2.0), num(3.0), num(4.0)])
        );
    }

    // wrap_count == len → a single exactly-filled row.
    #[test]
    fn wrap_equals_len_single_row() {
        let v = Array(vec![num(1.0), num(2.0), num(3.0)]);
        assert_eq!(
            eval_direct(eval, vec![v, Scalar(num(3.0))]),
            arr(1, 3, vec![num(1.0), num(2.0), num(3.0)])
        );
    }

    // A column vector wraps the same way (reading order top-to-bottom).
    #[test]
    fn column_vector_input() {
        let v = Range(vec![num(1.0), num(2.0), num(3.0)]);
        assert_eq!(
            eval_direct(eval, vec![v, Scalar(num(2.0)), Scalar(num(0.0))]),
            arr(2, 2, vec![num(1.0), num(2.0), num(3.0), num(0.0)])
        );
    }

    // Custom pad value.
    #[test]
    fn custom_pad() {
        let v = Array(vec![num(1.0), num(2.0), num(3.0)]);
        assert_eq!(
            eval_direct(eval, vec![v, Scalar(num(2.0)), Scalar(num(-1.0))]),
            arr(2, 2, vec![num(1.0), num(2.0), num(3.0), num(-1.0)])
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
            eval_direct(eval, vec![v, Scalar(num(0.0))]),
            Value::Error(ErrorKind::Num)
        );
    }

    // L3A-WRAP-SINGLE: wrap_count > len refuses.
    #[test]
    fn wrap_count_over_len_refused() {
        let v = Array(vec![num(1.0), num(2.0), num(3.0)]);
        assert_eq!(
            eval_direct(eval, vec![v, Scalar(num(5.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

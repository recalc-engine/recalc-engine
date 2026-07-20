//! `EXPAND` — pad an array out to larger row/column dimensions.
//!
//! # Provenance
//! Behavior contract: Microsoft support "EXPAND function"
//! (<https://support.microsoft.com/en-us/office/expand-function-7433fba5-4ad1-41da-a904-d5d95808bc38>,
//! verified by WebFetch 2026-07-15). No `docs/specs/EXPAND.md` exists; this is a
//! clean-room implementation of the documented behavior only. The `_xlfn.`
//! authoring prefix is stripped by the parser
//! (`xl-ast::parser::parse_func_name`), so the registry name is bare `EXPAND`.
//!
//! # Behavior contract (one line)
//! `EXPAND(array, rows, [columns], [pad_with])` returns `array` grown to
//! `rows × columns`, filling the new cells with `pad_with`.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - `rows` (optional): "If rows isn't provided … the default value is the
//!   number of rows in the array argument." Same for `columns`.
//! - `pad_with` (optional): "The default is #N/A." Placed verbatim in every new
//!   cell.
//! - Shrink → `#VALUE!`: "Excel returns a #VALUE error when the rows or columns
//!   argument is less than the rows or columns in the array argument." (This also
//!   covers a zero/negative target, which is `< array` for a non-empty array.)
//!
//! # Refused / assumed edges — see `docs/plans/2026-07-15-lane3a-probe-needed.md`
//! - **Unbounded whole-column/row / over-cap** — `arrayshape` (L3A-CAP).
//! - **Non-integer / array-valued `rows`/`columns`** (L3A-FRAC / L3A-ARRIDX).
//! - **Array-valued `pad_with`** (L3A-PADARR): undocumented → refused.
//! - **Arity/`EXPAND(array)` alone** (L3A-EXPARITY): the exact minimum arity and
//!   the `EXPAND(array)` (both dims defaulted) form are enforced at `min_args = 2`
//!   pending a farm pin; the elided-`rows`-with-later-args form (`EXPAND(a,,3)`)
//!   **is** supported (defaults to the current row count).

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::arrayshape::{
    IntArg, MAX_MATERIALIZED_ELEMS, Materialized, int_arg, materialize, over_cap, read_pad, spill,
};
use crate::context::EvalContext;

/// Evaluate an `EXPAND(array, rows, [columns], [pad_with])` call. See the module
/// docs for the semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let grid = match materialize(args, 0) {
        Materialized::Grid(g) => g,
        Materialized::Omitted => return Value::Error(ErrorKind::Unsupported),
        Materialized::Refused(k) => return Value::Error(k),
    };

    // rows (arg 1): default = current row count. Must be >= current.
    let target_rows = match target_dim(args, 1, grid.rows) {
        Ok(n) => n,
        Err(v) => return v,
    };
    // columns (arg 2): default = current column count. Must be >= current.
    let target_cols = match target_dim(args, 2, grid.cols) {
        Ok(n) => n,
        Err(v) => return v,
    };

    // pad_with (arg 3): default #N/A.
    let pad = match read_pad(args, 3) {
        Ok(v) => v,
        Err(v) => return v,
    };

    if over_cap(target_rows, target_cols) {
        return Value::Error(ErrorKind::Unsupported);
    }

    let mut data: Vec<Value> = Vec::with_capacity(target_rows * target_cols);
    for r in 0..target_rows {
        for c in 0..target_cols {
            if r < grid.rows && c < grid.cols {
                data.push(grid.at(r, c).clone());
            } else {
                data.push(pad.clone());
            }
        }
    }
    spill(target_rows, target_cols, data)
}

/// Resolve a target dimension (arg `index`) against the array's `current` size:
/// omitted → `current`; a value `< current` (including zero/negative) → `#VALUE!`.
fn target_dim(args: &mut dyn CallArgs, index: usize, current: usize) -> Result<usize, Value> {
    match int_arg(args, index) {
        IntArg::Omitted => Ok(current),
        IntArg::Value(n) => {
            if n < current as i64 {
                // Cannot shrink (documented #VALUE!); also catches zero/negative.
                Err(Value::Error(ErrorKind::Value))
            } else if n as u64 > MAX_MATERIALIZED_ELEMS {
                // Over-cap *before* the `i64 -> usize` narrowing (which truncates
                // on wasm32); any single dim over the cap over-caps the result.
                // `n >= current >= 1` here, so `as u64` is exact.
                Err(Value::Error(ErrorKind::Unsupported))
            } else {
                Ok(n as usize)
            }
        }
        IntArg::NonInteger | IntArg::NonScalar => Err(Value::Error(ErrorKind::Unsupported)),
        IntArg::Err(k) => Err(Value::Error(k)),
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg, eval_direct, num, txt};
    use xl_value::{Array, ErrorKind, Value};

    use TestArg::*;

    fn arr(rows: usize, cols: usize, data: Vec<Value>) -> Value {
        Value::Array(Array::new(rows, cols, data).unwrap())
    }

    // EXPAND(col, 3) pads a 2×1 column to 3×1 with #N/A.
    #[test]
    fn expand_rows_pad_na() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), num(2.0)]), Scalar(num(3.0))]
            ),
            arr(3, 1, vec![num(1.0), num(2.0), Value::Error(ErrorKind::Na)])
        );
    }

    // EXPAND(scalar, 2, 2, 0) pads a 1×1 to 2×2 with a custom pad value 0.
    #[test]
    fn expand_both_dims_custom_pad() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(7.0)),
                    Scalar(num(2.0)),
                    Scalar(num(2.0)),
                    Scalar(num(0.0))
                ]
            ),
            arr(2, 2, vec![num(7.0), num(0.0), num(0.0), num(0.0)])
        );
    }

    // Elided rows defaults to the current row count; only columns grow.
    #[test]
    fn elided_rows_defaults_current() {
        // A 1×2 row, expand rows omitted, columns to 3 → 1×3, pad last with #N/A.
        assert_eq!(
            eval_direct(
                eval,
                vec![Array(vec![txt("a"), txt("b")]), Omitted, Scalar(num(3.0))]
            ),
            arr(1, 3, vec![txt("a"), txt("b"), Value::Error(ErrorKind::Na)])
        );
    }

    // Target equal to current is a no-op (no padding).
    #[test]
    fn equal_target_is_noop() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), num(2.0)]), Scalar(num(2.0))]
            ),
            arr(2, 1, vec![num(1.0), num(2.0)])
        );
    }

    // Shrinking (target < current) → #VALUE!.
    #[test]
    fn shrink_is_value_error() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), num(2.0), num(3.0)]), Scalar(num(2.0))]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // Zero/negative target is < current → #VALUE!.
    #[test]
    fn nonpositive_target_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0)), Scalar(num(0.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // B2 / L3A-CAP (32-bit/wasm32 guard): a target dimension above 2^32 refuses,
    // caught *before* the `i64 -> usize` narrowing that would truncate on wasm32.
    #[test]
    fn over_u32_target_refused() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0)), Scalar(num(4_294_967_301.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // L3A-PADARR: an array-valued pad refuses.
    #[test]
    fn array_pad_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(1.0)),
                    Scalar(num(2.0)),
                    Scalar(num(1.0)),
                    Range(vec![num(0.0), num(0.0)]),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

//! `HSTACK` — append arrays horizontally into one array.
//!
//! # Provenance
//! Behavior contract: Microsoft support "HSTACK function"
//! (<https://support.microsoft.com/en-us/office/hstack-function-98c4ab76-10fe-4b4f-8d5f-af1c125fe8c2>,
//! verified by WebFetch 2026-07-15). No `docs/specs/HSTACK.md` exists; this is a
//! clean-room implementation of the documented behavior only. The `_xlfn.`
//! authoring prefix is stripped by the parser
//! (`xl-ast::parser::parse_func_name`), so the registry name is bare `HSTACK`.
//! The column/row transpose of [`crate::func_vstack`].
//!
//! # Behavior contract (one line)
//! `HSTACK(array1, [array2], …)` stacks its arrays side by side.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - Result dimensions: "Rows: The maximum of the row count from each of the
//!   array arguments. Columns: The combined count of all the columns from each
//!   of the array arguments."
//! - Ragged padding: "If an array has fewer rows than the maximum … Excel
//!   returns a #N/A error in the additional rows." The default pad is `#N/A`.
//! - Cell values (including errors and blanks) are relocated verbatim — `HSTACK`
//!   is a structural combine and performs no arithmetic.
//!
//! # Refused / assumed edges — see `docs/plans/2026-07-15-lane3a-probe-needed.md`
//! Identical to [`crate::func_vstack`]: unbounded/over-cap (L3A-CAP, refused),
//! elided argument slot (L3A-STACKGAP, refused), and a whole-argument scalar
//! error placed verbatim (L3A-STACKERR, assumed).

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::arrayshape::{PAD_NA, collect_grids, over_cap, spill};
use crate::context::EvalContext;

/// Evaluate an `HSTACK(array1, [array2], …)` call. See the module docs for the
/// semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let grids = match collect_grids(args) {
        Ok(g) => g,
        Err(v) => return v,
    };
    // Rows: the maximum height. Columns: sum of all widths.
    let max_rows: usize = grids.iter().map(|g| g.rows).max().unwrap_or(0);
    let total_cols: usize = grids.iter().map(|g| g.cols).sum();
    if max_rows == 0 || total_cols == 0 {
        return Value::Error(ErrorKind::Unsupported);
    }
    if over_cap(max_rows, total_cols) {
        return Value::Error(ErrorKind::Unsupported);
    }

    let mut data: Vec<Value> = Vec::with_capacity(max_rows * total_cols);
    for r in 0..max_rows {
        for g in &grids {
            for c in 0..g.cols {
                if r < g.rows {
                    data.push(g.at(r, c).clone());
                } else {
                    data.push(PAD_NA);
                }
            }
        }
    }
    spill(max_rows, total_cols, data)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::{Array, ErrorKind, Value};

    fn arr(rows: usize, cols: usize, data: Vec<Value>) -> Value {
        Value::Array(Array::new(rows, cols, data).unwrap())
    }

    // Two equal-height rows stack into a wider row.
    #[test]
    fn stack_equal_height() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Array(vec![num(1.0), num(2.0)]), Array(vec![num(3.0)])]
            ),
            arr(1, 3, vec![num(1.0), num(2.0), num(3.0)])
        );
    }

    // Differing heights pad the shorter array's extra rows with #N/A.
    #[test]
    fn ragged_heights_pad_na() {
        // A 2×1 column beside a 1×1 → 2×2 with #N/A in row 2, column 2.
        let left = Range(vec![num(1.0), num(2.0)]); // 2×1
        let right = Scalar(num(3.0)); // 1×1
        assert_eq!(
            eval_direct(eval, vec![left, right]),
            arr(
                2,
                2,
                vec![num(1.0), num(3.0), num(2.0), Value::Error(ErrorKind::Na)]
            )
        );
    }

    // A 2×2 rect beside a matching-height column.
    #[test]
    fn rect_and_column() {
        let rect = Rect {
            rows: 2,
            cols: 2,
            data: vec![num(1.0), num(2.0), num(3.0), num(4.0)],
        };
        let col = Range(vec![num(5.0), num(6.0)]);
        assert_eq!(
            eval_direct(eval, vec![rect, col]),
            arr(
                2,
                3,
                vec![num(1.0), num(2.0), num(5.0), num(3.0), num(4.0), num(6.0)]
            )
        );
    }

    // L3A-STACKGAP: an elided argument slot refuses.
    #[test]
    fn elided_slot_refused() {
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(1.0)]), Omitted]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

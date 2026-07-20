//! `VSTACK` — append arrays vertically into one array.
//!
//! # Provenance
//! Behavior contract: Microsoft support "VSTACK function"
//! (<https://support.microsoft.com/en-us/office/vstack-function-a4b86897-be0f-48fc-adca-fcc10d795a9c>,
//! verified by WebFetch 2026-07-15). No `docs/specs/VSTACK.md` exists; this is a
//! clean-room implementation of the documented behavior only. The `_xlfn.`
//! authoring prefix is stripped by the parser
//! (`xl-ast::parser::parse_func_name`), so the registry name is bare `VSTACK`.
//!
//! # Behavior contract (one line)
//! `VSTACK(array1, [array2], …)` stacks its arrays one below another.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - Result dimensions: "Rows: the combined count of all the rows from each of
//!   the array arguments. Columns: The maximum of the column count from each of
//!   the array arguments."
//! - Ragged padding: "If an array has fewer columns than the maximum width of
//!   the selected arrays, Excel returns a #N/A error in the additional columns."
//!   The default pad is `#N/A`.
//! - Cell values (numbers, text, booleans, blanks, **and errors**) are relocated
//!   verbatim into their new positions — `VSTACK` is a structural combine and
//!   performs no arithmetic.
//!
//! # Refused / assumed edges — see `docs/plans/2026-07-15-lane3a-probe-needed.md`
//! - **Unbounded whole-column/row argument** and **over-cap** result — `arrayshape`
//!   (L3A-CAP). Refused.
//! - **Elided argument slot (`VSTACK(a,,b)`)** (L3A-STACKGAP): undocumented →
//!   refused.
//! - **A whole-argument scalar error (`VSTACK(a, #REF!)`)** (L3A-STACKERR):
//!   whether Excel *propagates* the error or *places* it as a 1×1 cell is
//!   undocumented; this places it verbatim (assumed). A cell error *within* a
//!   multi-cell argument is unambiguously placed.

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::arrayshape::{PAD_NA, collect_grids, over_cap, spill};
use crate::context::EvalContext;

/// Evaluate a `VSTACK(array1, [array2], …)` call. See the module docs for the
/// semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let grids = match collect_grids(args) {
        Ok(g) => g,
        Err(v) => return v,
    };
    // Rows: sum of all heights. Columns: the maximum width.
    let total_rows: usize = grids.iter().map(|g| g.rows).sum();
    let max_cols: usize = grids.iter().map(|g| g.cols).max().unwrap_or(0);
    if total_rows == 0 || max_cols == 0 {
        return Value::Error(ErrorKind::Unsupported);
    }
    if over_cap(total_rows, max_cols) {
        return Value::Error(ErrorKind::Unsupported);
    }

    let mut data: Vec<Value> = Vec::with_capacity(total_rows * max_cols);
    for g in &grids {
        for r in 0..g.rows {
            for c in 0..max_cols {
                if c < g.cols {
                    data.push(g.at(r, c).clone());
                } else {
                    data.push(PAD_NA);
                }
            }
        }
    }
    spill(total_rows, max_cols, data)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{Array, ErrorKind, Value};

    fn arr(rows: usize, cols: usize, data: Vec<Value>) -> Value {
        Value::Array(Array::new(rows, cols, data).unwrap())
    }

    // Two equal-width columns stack into a taller column.
    #[test]
    fn stack_equal_width() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), num(2.0)]), Range(vec![num(3.0)])]
            ),
            arr(3, 1, vec![num(1.0), num(2.0), num(3.0)])
        );
    }

    // Differing widths pad the narrower array's extra columns with #N/A.
    #[test]
    fn ragged_widths_pad_na() {
        // A 1×2 row atop a 1×1 → 2×2 with #N/A in the second row's column 2.
        let top = Array(vec![num(1.0), num(2.0)]); // 1×2
        let bottom = Scalar(num(3.0)); // 1×1
        assert_eq!(
            eval_direct(eval, vec![top, bottom]),
            arr(
                2,
                2,
                vec![num(1.0), num(2.0), num(3.0), Value::Error(ErrorKind::Na)]
            )
        );
    }

    // A cell error inside an argument is placed verbatim (structural combine).
    #[test]
    fn cell_error_placed_verbatim() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), Value::Error(ErrorKind::Div0)])]
            ),
            arr(2, 1, vec![num(1.0), Value::Error(ErrorKind::Div0)])
        );
    }

    // A 2-D rect stacks under a matching-width row.
    #[test]
    fn rect_and_row() {
        let rect = Rect {
            rows: 2,
            cols: 2,
            data: vec![num(1.0), num(2.0), num(3.0), num(4.0)],
        };
        let row = Array(vec![txt("a"), txt("b")]);
        assert_eq!(
            eval_direct(eval, vec![rect, row]),
            arr(
                3,
                2,
                vec![num(1.0), num(2.0), num(3.0), num(4.0), txt("a"), txt("b")]
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

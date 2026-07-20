//! `TOCOL` — return an array as a single column.
//!
//! # Provenance
//! Behavior contract: Microsoft support "TOCOL function"
//! (<https://support.microsoft.com/en-us/office/tocol-function-22839d9b-0b55-4fc1-b4e6-2761f8f122ed>,
//! verified by WebFetch 2026-07-15). No `docs/specs/TOCOL.md` exists; this is a
//! clean-room implementation of the documented behavior only. The `_xlfn.`
//! authoring prefix is stripped by the parser
//! (`xl-ast::parser::parse_func_name`), so the registry name is bare `TOCOL`.
//!
//! # Behavior contract (one line)
//! `TOCOL(array, [ignore], [scan_by_column])` flattens `array` into a single
//! column, in scan order, optionally dropping blanks and/or errors.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - `array` (arg 0): materialised into a dense rectangle (see `arrayshape`).
//! - `ignore` (arg 1, default `0`): "0 Keep all values (default)", "1 Ignore
//!   blanks", "2 Ignore errors", "3 Ignore blanks and errors".
//! - `scan_by_column` (arg 2, default FALSE): "Scan the array by column. By
//!   default, the array is scanned by row." FALSE → row-major flatten; TRUE →
//!   column-major flatten.
//!
//! # Refused edges (loud), pending probes — see `docs/plans/2026-07-15-lane3a-probe-needed.md`
//! - **Unbounded whole-column/row `array`** and **over-cap** input — `arrayshape`
//!   (L3A-CAP).
//! - **`ignore` outside {0,1,2,3}, fractional, or array-valued** (L3A-IGN): the
//!   page enumerates only 0–3; other values are undocumented → refused.
//! - **Array-valued `scan_by_column`** (L3A-SCAN): undocumented → refused.
//! - **Empty result** (L3A-EMPTY): if every value is filtered out (e.g. an
//!   all-blank array with `ignore=1`), the result is empty; the page does not
//!   pin the error (`#CALC!`?), so it is refused rather than guessed.

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::arrayshape::{
    Materialized, flatten, materialize, over_cap, read_ignore, read_scan, spill,
};
use crate::context::EvalContext;

/// Evaluate a `TOCOL(array, [ignore], [scan_by_column])` call. See the module
/// docs for the semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let grid = match materialize(args, 0) {
        Materialized::Grid(g) => g,
        Materialized::Omitted => return Value::Error(ErrorKind::Unsupported),
        Materialized::Refused(k) => return Value::Error(k),
    };
    let ignore = match read_ignore(args) {
        Ok(n) => n,
        Err(v) => return v,
    };
    let by_column = match read_scan(args) {
        Ok(b) => b,
        Err(v) => return v,
    };

    let data = flatten(&grid, ignore, by_column);
    if data.is_empty() {
        // Every value filtered out — the empty-result error is unpinned.
        return Value::Error(ErrorKind::Unsupported);
    }
    let n = data.len();
    if over_cap(n, 1) {
        return Value::Error(ErrorKind::Unsupported);
    }
    spill(n, 1, data)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{Array, ErrorKind, Value};

    fn arr(rows: usize, cols: usize, data: Vec<Value>) -> Value {
        Value::Array(Array::new(rows, cols, data).unwrap())
    }

    // TOCOL of a 2×2 rect, default (row-major, keep all).
    #[test]
    fn row_major_keep_all() {
        let g = Rect {
            rows: 2,
            cols: 2,
            data: vec![num(1.0), num(2.0), num(3.0), num(4.0)],
        };
        assert_eq!(
            eval_direct(eval, vec![g]),
            arr(4, 1, vec![num(1.0), num(2.0), num(3.0), num(4.0)])
        );
    }

    // scan_by_column=TRUE flattens column-major.
    #[test]
    fn column_major() {
        let g = Rect {
            rows: 2,
            cols: 2,
            data: vec![num(1.0), num(2.0), num(3.0), num(4.0)],
        };
        assert_eq!(
            eval_direct(eval, vec![g, Scalar(num(0.0)), Scalar(Value::Bool(true))]),
            arr(4, 1, vec![num(1.0), num(3.0), num(2.0), num(4.0)])
        );
    }

    // ignore=1 drops blanks.
    #[test]
    fn ignore_blanks() {
        let g = Rect {
            rows: 2,
            cols: 2,
            data: vec![num(1.0), Value::Blank, num(3.0), Value::Blank],
        };
        assert_eq!(
            eval_direct(eval, vec![g, Scalar(num(1.0))]),
            arr(2, 1, vec![num(1.0), num(3.0)])
        );
    }

    // ignore=2 drops errors; keeps blanks and everything else.
    #[test]
    fn ignore_errors() {
        let g = Rect {
            rows: 1,
            cols: 3,
            data: vec![num(1.0), Value::Error(ErrorKind::Div0), txt("x")],
        };
        assert_eq!(
            eval_direct(eval, vec![g, Scalar(num(2.0))]),
            arr(2, 1, vec![num(1.0), txt("x")])
        );
    }

    // ignore=3 drops blanks and errors.
    #[test]
    fn ignore_blanks_and_errors() {
        let g = Rect {
            rows: 1,
            cols: 4,
            data: vec![
                num(1.0),
                Value::Blank,
                Value::Error(ErrorKind::Na),
                num(2.0),
            ],
        };
        assert_eq!(
            eval_direct(eval, vec![g, Scalar(num(3.0))]),
            arr(2, 1, vec![num(1.0), num(2.0)])
        );
    }

    // Default keeps errors and blanks (ignore=0): an error value is preserved.
    #[test]
    fn keep_all_preserves_error() {
        let g = Rect {
            rows: 1,
            cols: 2,
            data: vec![num(1.0), Value::Error(ErrorKind::Div0)],
        };
        assert_eq!(
            eval_direct(eval, vec![g]),
            arr(2, 1, vec![num(1.0), Value::Error(ErrorKind::Div0)])
        );
    }

    // L3A-IGN: an out-of-range ignore refuses.
    #[test]
    fn ignore_out_of_range_refused() {
        let g = Range(vec![num(1.0), num(2.0)]);
        assert_eq!(
            eval_direct(eval, vec![g, Scalar(num(5.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // L3A-EMPTY: everything filtered out refuses.
    #[test]
    fn empty_result_refused() {
        let g = Range(vec![Value::Blank, Value::Blank]);
        assert_eq!(
            eval_direct(eval, vec![g, Scalar(num(1.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // A scalar input is a 1×1 → single-cell column.
    #[test]
    fn scalar_input() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(7.0))]),
            arr(1, 1, vec![num(7.0)])
        );
    }
}

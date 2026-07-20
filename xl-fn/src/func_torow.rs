//! `TOROW` — return an array as a single row.
//!
//! # Provenance
//! Behavior contract: Microsoft support "TOROW function"
//! (<https://support.microsoft.com/en-us/office/torow-function-b90d0964-a7d9-44b7-816b-ffa5c2fe2289>,
//! verified by WebFetch 2026-07-15). No `docs/specs/TOROW.md` exists; this is a
//! clean-room implementation of the documented behavior only. The `_xlfn.`
//! authoring prefix is stripped by the parser
//! (`xl-ast::parser::parse_func_name`), so the registry name is bare `TOROW`.
//! The row/column transpose of [`crate::func_tocol`]; the `ignore`/scan
//! semantics are identical and share [`crate::arrayshape`]'s helpers.
//!
//! # Behavior contract (one line)
//! `TOROW(array, [ignore], [scan_by_column])` flattens `array` into a single
//! row, in scan order, optionally dropping blanks and/or errors.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - `array` (arg 0): materialised into a dense rectangle (see `arrayshape`).
//! - `ignore` (arg 1, default `0`): "0 Keep all values (default)", "1 Ignore
//!   blanks", "2 Ignore errors", "3 Ignore blanks and errors".
//! - `scan_by_column` (arg 2, default FALSE): "Scan the array by column. By
//!   default, the array is scanned by row."
//!
//! # Refused edges (loud), pending probes — see `docs/plans/2026-07-15-lane3a-probe-needed.md`
//! Identical to [`crate::func_tocol`]: unbounded/over-cap input (L3A-CAP),
//! `ignore` outside {0,1,2,3} / fractional / array-valued (L3A-IGN), array-valued
//! `scan_by_column` (L3A-SCAN), and an empty filtered result (L3A-EMPTY).

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::arrayshape::{
    Materialized, flatten, materialize, over_cap, read_ignore, read_scan, spill,
};
use crate::context::EvalContext;

/// Evaluate a `TOROW(array, [ignore], [scan_by_column])` call. See the module
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
        return Value::Error(ErrorKind::Unsupported);
    }
    let n = data.len();
    if over_cap(1, n) {
        return Value::Error(ErrorKind::Unsupported);
    }
    spill(1, n, data)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::{Array, ErrorKind, Value};

    fn arr(rows: usize, cols: usize, data: Vec<Value>) -> Value {
        Value::Array(Array::new(rows, cols, data).unwrap())
    }

    // TOROW of a 2×2 rect, default (row-major, keep all) → a 1×4 row.
    #[test]
    fn row_major_keep_all() {
        let g = Rect {
            rows: 2,
            cols: 2,
            data: vec![num(1.0), num(2.0), num(3.0), num(4.0)],
        };
        assert_eq!(
            eval_direct(eval, vec![g]),
            arr(1, 4, vec![num(1.0), num(2.0), num(3.0), num(4.0)])
        );
    }

    // scan_by_column=TRUE flattens column-major into the row.
    #[test]
    fn column_major() {
        let g = Rect {
            rows: 2,
            cols: 2,
            data: vec![num(1.0), num(2.0), num(3.0), num(4.0)],
        };
        assert_eq!(
            eval_direct(eval, vec![g, Scalar(num(0.0)), Scalar(Value::Bool(true))]),
            arr(1, 4, vec![num(1.0), num(3.0), num(2.0), num(4.0)])
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
            arr(1, 2, vec![num(1.0), num(2.0)])
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
}

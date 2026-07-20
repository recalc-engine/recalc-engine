//! `SORT` — return `array` with its rows (or columns) reordered by one key
//! line, ascending or descending.
//!
//! # Provenance
//! Microsoft support page "SORT function"
//! (<https://support.microsoft.com/en-us/office/sort-function-22f63bd0-ccc8-492f-953d-c20e8e44b86c>),
//! fetched 2026-07-15. Clean-room from the page's unambiguous prose; ordering is
//! `xl-value`'s frozen [`compare`](xl_value::compare) total order — this module
//! never re-implements comparison.
//!
//! # Signature (page verbatim)
//! `SORT(array, [sort_index], [sort_order], [by_col])` — 1..=4 args. "Where
//! sort_index is not provided, row1/col1 will be presumed. Where order is not
//! provided, ascending order will be presumed." `sort_order`: "1 for ascending
//! order (default), -1 for descending order." `by_col`: "FALSE to sort by row
//! (default), TRUE to sort by column."
//!
//! # Semantics implemented
//! - **`by_col = FALSE`** (default): reorder the **rows** using column
//!   `sort_index` (1-based) as the key. **`by_col = TRUE`**: reorder the
//!   **columns** using row `sort_index` as the key.
//! - **Ordering** is [`compare`](xl_value::compare)'s frozen total order:
//!   numbers numerically, ASCII text case-insensitively, and cross-type by
//!   Excel's `Number < Text < Bool` rank. `sort_order = -1` reverses each
//!   non-equal comparison only.
//! - **Stability**: equal keys keep their original relative order (a stable
//!   sort) — the only *deterministic* tie-break, and cross-platform bit-identity
//!   is a Recalc feature. The MS page does not pin stability → recorded as an
//!   assumption (L3B probe doc).
//! - A **data error / blank in a non-key cell** rides along in place
//!   (Excel-faithful); only the *key line* participates in comparison.
//!
//! # Refused / assumed (see the probe doc and OXP-040)
//! - **Mixed-type / `Blank` key ordering** follows `compare` (numbers < text <
//!   logicals, `Blank` morphs to `0`/`""`/`FALSE`). Excel's real placement of
//!   blanks (reported last) and error/mixed order is **unpinned** — the already
//!   queued **OXP-040** — so this is a confident-but-unpinned assumption, not a
//!   guess pulled from memory.
//! - **An error in the key line** propagates leftmost-first (Principle 2 — its
//!   sorted placement is unpinned, OXP-040), rather than being ordered by a
//!   fabricated rule. **Non-ASCII text** in the key propagates `#UNSUPPORTED!`
//!   (`compare` refuses it — OXP-031).
//! - **`sort_order` not `1`/`-1`** → `#VALUE!` (the SORT page enumerates only
//!   those two; extrapolated from SORTBY's *documented* `#VALUE!` for the
//!   identical argument — recorded as an assumption).
//! - **`sort_index` out of range** → `#UNSUPPORTED!`: the page documents no
//!   error type for it, so it is refused rather than guessing `#VALUE!`/`#REF!`.
//! - **Whole-column/row inputs** (`A:A`): the dense walk refuses → `#UNSUPPORTED!`.

use std::cmp::Ordering;

use xl_value::{ErrorKind, Value, compare, to_bool, to_number};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;
use crate::dynarray::{materialize, precheck_compare_line, sort_order_descending, spill};

/// Evaluate a `SORT(...)` call. See the module docs for semantics/provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let data = match materialize(args, 0) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    if data.height == 0 || data.width == 0 {
        return Value::Error(ErrorKind::Calc);
    }

    // by_col (arg 3): FALSE when Omitted (absent OR an elided `,` slot — never
    // conflated with a provided `Blank`). A provided value coerces via `to_bool`
    // (error propagates; non-convertible text → #VALUE!).
    let by_col = if args.shape(3) == ArgShape::Omitted {
        false
    } else {
        match to_bool(&args.eval_scalar(3)) {
            Ok(b) => b,
            Err(k) => return Value::Error(k),
        }
    };

    // sort_index (arg 1): default 1 ("row1/col1 is presumed") when Omitted —
    // including the elided `SORT(rng,,-1)` descending idiom. A provided value is
    // floored and bounded by the key axis (a provided `Blank` coerces to 0 →
    // out of range).
    let axis = if by_col { data.height } else { data.width };
    let sort_index = if args.shape(1) == ArgShape::Omitted {
        1.0
    } else {
        match to_number(&args.eval_scalar(1)) {
            Ok(n) => n.floor(),
            Err(k) => return Value::Error(k),
        }
    };
    if sort_index < 1.0 || sort_index > axis as f64 {
        // Undocumented error type for an out-of-range index → refuse loudly.
        return Value::Error(ErrorKind::Unsupported);
    }
    let key_idx = sort_index as usize - 1;

    // sort_order (arg 2): ascending (default) when Omitted — including the elided
    // `SORT(rng,2,,TRUE)` slot. A provided value is 1/-1 (else #VALUE!).
    let descending = if args.shape(2) == ArgShape::Omitted {
        false
    } else {
        let n = match to_number(&args.eval_scalar(2)) {
            Ok(n) => n,
            Err(k) => return Value::Error(k),
        };
        match sort_order_descending(n) {
            Some(d) => d,
            None => return Value::Error(ErrorKind::Value),
        }
    };

    // The key line: a column (by_col FALSE) or a row (by_col TRUE). Pre-validate
    // it so `compare` cannot error inside the sort (see `precheck_compare_line`).
    let key_line: Vec<Value> = if by_col {
        data.rows[key_idx].clone()
    } else {
        data.column(key_idx)
    };
    if let Err(k) = precheck_compare_line(&key_line) {
        return Value::Error(k);
    }

    // Stable permutation of the sorted axis.
    let n = key_line.len();
    let mut perm: Vec<usize> = (0..n).collect();
    perm.sort_by(|&a, &b| {
        let ord = compare(&key_line[a], &key_line[b]).unwrap_or(Ordering::Equal);
        if descending { ord.reverse() } else { ord }
    });

    // Reassemble the whole rectangle with the axis reordered; non-key cells ride
    // along unchanged (data errors preserved in place).
    let mut flat: Vec<Value> = Vec::with_capacity(data.height * data.width);
    if by_col {
        for r in 0..data.height {
            for &c in &perm {
                flat.push(data.rows[r][c].clone());
            }
        }
    } else {
        for &r in &perm {
            flat.extend(data.rows[r].iter().cloned());
        }
    }
    spill(data.height, data.width, flat)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::dynarray::spill;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn ascending_single_column() {
        let got = eval_direct(eval, vec![Range(vec![num(3.0), num(1.0), num(2.0)])]);
        assert_eq!(got, spill(3, 1, vec![num(1.0), num(2.0), num(3.0)]));
    }

    #[test]
    fn descending_single_column() {
        let got = eval_direct(
            eval,
            vec![
                Range(vec![num(3.0), num(1.0), num(2.0)]),
                Scalar(num(1.0)),
                Scalar(num(-1.0)),
            ],
        );
        assert_eq!(got, spill(3, 1, vec![num(3.0), num(2.0), num(1.0)]));
    }

    #[test]
    fn sort_rows_by_second_column() {
        // 3×2 rows sorted by column 2 ascending.
        // rows: (1,30),(2,10),(3,20) → by col2 → (2,10),(3,20),(1,30).
        let got = eval_direct(
            eval,
            vec![
                Rect {
                    rows: 3,
                    cols: 2,
                    data: vec![
                        num(1.0),
                        num(30.0),
                        num(2.0),
                        num(10.0),
                        num(3.0),
                        num(20.0),
                    ],
                },
                Scalar(num(2.0)),
            ],
        );
        assert_eq!(
            got,
            spill(
                3,
                2,
                vec![
                    num(2.0),
                    num(10.0),
                    num(3.0),
                    num(20.0),
                    num(1.0),
                    num(30.0)
                ]
            )
        );
    }

    #[test]
    fn by_col_sorts_columns() {
        // 1×3 row {3,1,2}, by_col TRUE, sort_index 1 → columns reordered → {1,2,3}.
        let got = eval_direct(
            eval,
            vec![
                Array(vec![num(3.0), num(1.0), num(2.0)]),
                Scalar(num(1.0)),
                Scalar(num(1.0)),
                Scalar(Value::Bool(true)),
            ],
        );
        assert_eq!(got, spill(1, 3, vec![num(1.0), num(2.0), num(3.0)]));
    }

    #[test]
    fn stability_preserves_input_order_on_ties() {
        // Two rows with equal key col2 (=5) keep input order; distinct 3rd col
        // proves which came first.
        let got = eval_direct(
            eval,
            vec![
                Rect {
                    rows: 3,
                    cols: 2,
                    data: vec![num(5.0), txt("a"), num(1.0), txt("z"), num(5.0), txt("b")],
                },
                Scalar(num(1.0)),
            ],
        );
        // key col1: rows (5,a),(1,z),(5,b). ascending by col1: (1,z) first, then
        // the two 5-rows in original order: (5,a) then (5,b).
        assert_eq!(
            got,
            spill(
                3,
                2,
                vec![num(1.0), txt("z"), num(5.0), txt("a"), num(5.0), txt("b")]
            )
        );
    }

    #[test]
    fn invalid_order_is_value() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0)]), Scalar(num(1.0)), Scalar(num(2.0))]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn index_out_of_range_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), num(2.0)]), Scalar(num(2.0))]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn error_in_key_column_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(1.0),
                    Value::Error(ErrorKind::Div0),
                    num(2.0)
                ])]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn non_ascii_key_refused() {
        assert_eq!(
            eval_direct(eval, vec![Range(vec![txt("ä"), txt("z")])]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn non_key_error_rides_along() {
        // Sort by column 1; the #DIV/0! in column 2 rides along, not propagated.
        let got = eval_direct(
            eval,
            vec![
                Rect {
                    rows: 2,
                    cols: 2,
                    data: vec![num(2.0), Value::Error(ErrorKind::Div0), num(1.0), num(9.0)],
                },
                Scalar(num(1.0)),
            ],
        );
        assert_eq!(
            got,
            spill(
                2,
                2,
                vec![num(1.0), num(9.0), num(2.0), Value::Error(ErrorKind::Div0)]
            )
        );
    }

    #[test]
    fn whole_column_refused() {
        assert_eq!(
            eval_direct(eval, vec![Unbounded(vec![num(1.0), num(2.0)])]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn elided_sort_index_descending_idiom() {
        // SORT(rng,,-1): the elided sort_index defaults to column 1 (NOT a false
        // #UNSUPPORTED!); sort_order -1 → descending. THE canonical desc idiom.
        let got = eval_direct(
            eval,
            vec![
                Range(vec![num(1.0), num(3.0), num(2.0)]),
                Omitted,
                Scalar(num(-1.0)),
            ],
        );
        assert_eq!(got, spill(3, 1, vec![num(3.0), num(2.0), num(1.0)]));
    }

    #[test]
    fn elided_sort_order_ascending_default_across_columns() {
        // SORT(rect,2,,TRUE): the elided sort_order defaults ascending (NOT a
        // false #VALUE!); by_col TRUE reorders columns using row 2 as the key.
        // rows: [1,2,3] / key [30,10,20] → columns ordered col2,col3,col1.
        let got = eval_direct(
            eval,
            vec![
                Rect {
                    rows: 2,
                    cols: 3,
                    data: vec![
                        num(1.0),
                        num(2.0),
                        num(3.0),
                        num(30.0),
                        num(10.0),
                        num(20.0),
                    ],
                },
                Scalar(num(2.0)),
                Omitted,
                Scalar(Value::Bool(true)),
            ],
        );
        assert_eq!(
            got,
            spill(
                2,
                3,
                vec![
                    num(2.0),
                    num(3.0),
                    num(1.0),
                    num(10.0),
                    num(20.0),
                    num(30.0)
                ]
            )
        );
    }
}

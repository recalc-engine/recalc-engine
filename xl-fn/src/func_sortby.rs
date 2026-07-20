//! `SORTBY` — return `array` with its rows (or columns) reordered by one or more
//! separate `by_array` key lines, each with its own ascending/descending order.
//!
//! # Provenance
//! Microsoft support page "SORTBY function"
//! (<https://support.microsoft.com/en-us/office/sortby-function-cd2d7a62-1b93-435c-b561-d6a35134f28f>),
//! fetched 2026-07-15. Clean-room from the page's unambiguous prose; ordering is
//! `xl-value`'s frozen [`compare`](xl_value::compare) — never re-implemented.
//!
//! # Signature (page verbatim)
//! `SORTBY(array, by_array1, [sort_order1], [by_array2, sort_order2], …)` — 2..N
//! args. "The by_array arguments must either be one row high, or one column
//! wide." "All of the arguments must be the same size." `sort_order`: "1 for
//! ascending, -1 for descending. Default is ascending"; an invalid value "will
//! result in a #VALUE! error".
//!
//! # Argument grouping (strict positional)
//! The MS signature is **positional**, not shape-sniffed: over the 0-based
//! argument list, index 0 is `array`, the **odd** indices `1, 3, 5, …` are
//! `by_array` slots, and the **even** indices `2, 4, 6, …` are their optional
//! `sort_order` slots. A by_array slot is required (an elided `,,` there →
//! `#VALUE!`); an order slot is `1`/`-1` (else `#VALUE!`), defaults to ascending
//! when omitted/elided, and a **range/array in an order slot** (e.g.
//! `SORTBY(arr, by1, by2, -1)`, where `by2` lands positionally in the
//! `sort_order1` slot) is **unpinned in Excel → `#UNSUPPORTED!`**, never silently
//! treated as an extra key (L3B probe doc).
//!
//! # Semantics implemented
//! - The **orientation** is taken from `by_array1`: a column (`H×1`) sorts the
//!   **rows** of `array` (which must have `H` rows); a row (`1×W`) sorts the
//!   **columns** (`array` must have `W` columns).
//! - **Multi-key**: keys are compared in order; earlier keys dominate, ties fall
//!   through to the next. Each key's `sort_order` reverses only its non-equal
//!   comparisons. **Stable** on a full tie (input order preserved).
//! - Ordering is [`compare`](xl_value::compare)'s frozen total order (see `SORT`).
//! - A **data error / blank in `array`** rides along in place — only the
//!   `by_array` key lines participate in comparison.
//!
//! # Refused / assumed (see the probe doc and OXP-040)
//! - **Mixed-type / `Blank` key ordering** follows `compare` (unpinned placement
//!   — OXP-040); an **error in a `by_array`** propagates leftmost-first;
//!   **non-ASCII** key text → `#UNSUPPORTED!` (OXP-031).
//! - **`sort_order` not `1`/`-1`** → `#VALUE!` (documented).
//! - **A `by_array` of the wrong shape/size** (not one row / one column matching
//!   the sort axis, or a different size than `array`) → `#VALUE!` (the documented
//!   lockstep requirement; the brief pins `#VALUE!` for the mismatch).
//! - **Whole-column/row inputs** (`A:A`): the dense walk refuses → `#UNSUPPORTED!`.

use std::cmp::Ordering;

use xl_value::{ErrorKind, Value, compare, to_number};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;
use crate::dynarray::{Grid, materialize, precheck_compare_line, sort_order_descending, spill};

/// Evaluate a `SORTBY(...)` call. See the module docs for semantics/provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let data = match materialize(args, 0) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    if data.height == 0 || data.width == 0 {
        return Value::Error(ErrorKind::Calc);
    }

    // Strict POSITIONAL parse of the MS signature `SORTBY(array, by_array1,
    // [sort_order1], [by_array2, sort_order2], …)`: arg 0 = array; odd indices
    // (1, 3, 5, …) are by_array slots, even indices (2, 4, 6, …) are their
    // sort_order slots. This is a fixed pairing by index parity — NOT shape
    // sniffing (a range in a sort_order slot is unpinned, not a silent extra key).
    let n = args.count();
    let mut pairs: Vec<(Grid, bool)> = Vec::new();
    let mut idx = 1;
    while idx < n {
        // by_array slot (odd index): required. An elided slot (`,,`) →
        // `#VALUE!` — a by_array cannot be absent.
        if args.shape(idx) == ArgShape::Omitted {
            return Value::Error(ErrorKind::Value);
        }
        let by = match materialize(args, idx) {
            Ok(g) => g,
            Err(k) => return Value::Error(k),
        };
        // sort_order slot (the following even index; out-of-range classifies as
        // Omitted): Omitted → ascending default; Scalar → 1/-1 (else the
        // documented `#VALUE!`); a Range/Array in an order slot is unpinned →
        // `#UNSUPPORTED!`.
        let descending = match args.shape(idx + 1) {
            ArgShape::Omitted => false,
            ArgShape::Scalar => {
                let o = match to_number(&args.eval_scalar(idx + 1)) {
                    Ok(o) => o,
                    Err(k) => return Value::Error(k),
                };
                match sort_order_descending(o) {
                    Some(d) => d,
                    None => return Value::Error(ErrorKind::Value),
                }
            }
            ArgShape::Range | ArgShape::Array => return Value::Error(ErrorKind::Unsupported),
        };
        pairs.push((by, descending));
        idx += 2;
    }
    if pairs.is_empty() {
        // `by_array1` is required (the registry's min arity guards this in
        // production; keep it loud for a direct malformed call).
        return Value::Error(ErrorKind::Value);
    }

    // Orientation from by_array1: a column sorts rows; a row sorts columns.
    let by1 = &pairs[0].0;
    let (sort_rows, axis_len) = if by1.width == 1 {
        (true, by1.height)
    } else if by1.height == 1 {
        (false, by1.width)
    } else {
        // A genuinely 2-D by_array violates "one row high, or one column wide".
        return Value::Error(ErrorKind::Value);
    };

    // "All of the arguments must be the same size": the sorted axis of `array`
    // and every `by_array` must match `axis_len`.
    let data_axis = if sort_rows { data.height } else { data.width };
    if data_axis != axis_len {
        return Value::Error(ErrorKind::Value);
    }
    for (by, _) in &pairs {
        let ok = if sort_rows {
            by.width == 1 && by.height == axis_len
        } else {
            by.height == 1 && by.width == axis_len
        };
        if !ok {
            return Value::Error(ErrorKind::Value);
        }
    }

    // Extract + pre-validate each key line (leftmost-first error propagation,
    // per by_array in argument order).
    let mut keys: Vec<(Vec<Value>, bool)> = Vec::with_capacity(pairs.len());
    for (by, descending) in &pairs {
        let line: Vec<Value> = if sort_rows {
            by.column(0)
        } else {
            by.rows[0].clone()
        };
        if let Err(k) = precheck_compare_line(&line) {
            return Value::Error(k);
        }
        keys.push((line, *descending));
    }

    // Stable multi-key permutation.
    let mut perm: Vec<usize> = (0..axis_len).collect();
    perm.sort_by(|&a, &b| {
        for (line, descending) in &keys {
            let ord = compare(&line[a], &line[b]).unwrap_or(Ordering::Equal);
            let ord = if *descending { ord.reverse() } else { ord };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });

    // Reassemble; non-key cells ride along unchanged.
    let mut flat: Vec<Value> = Vec::with_capacity(data.height * data.width);
    if sort_rows {
        for &r in &perm {
            flat.extend(data.rows[r].iter().cloned());
        }
    } else {
        for r in 0..data.height {
            for &c in &perm {
                flat.push(data.rows[r][c].clone());
            }
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
    fn single_key_ascending() {
        // SORTBY({"a";"b";"c"}, {3;1;2}) → {"b";"c";"a"}.
        let got = eval_direct(
            eval,
            vec![
                Range(vec![txt("a"), txt("b"), txt("c")]),
                Range(vec![num(3.0), num(1.0), num(2.0)]),
            ],
        );
        assert_eq!(got, spill(3, 1, vec![txt("b"), txt("c"), txt("a")]));
    }

    #[test]
    fn single_key_descending() {
        let got = eval_direct(
            eval,
            vec![
                Range(vec![txt("a"), txt("b"), txt("c")]),
                Range(vec![num(3.0), num(1.0), num(2.0)]),
                Scalar(num(-1.0)),
            ],
        );
        assert_eq!(got, spill(3, 1, vec![txt("a"), txt("c"), txt("b")]));
    }

    #[test]
    fn two_keys_second_breaks_ties() {
        // array {"a";"b";"c";"d"}; by1 {1;1;2;2} asc; by2 {2;1;2;1} asc.
        // group key1=1: rows 0(k2=2),1(k2=1) → order 1,0. key1=2: rows 2(k2=2),3(k2=1) → 3,2.
        // → {"b";"a";"d";"c"}.
        let got = eval_direct(
            eval,
            vec![
                Range(vec![txt("a"), txt("b"), txt("c"), txt("d")]),
                Range(vec![num(1.0), num(1.0), num(2.0), num(2.0)]),
                Scalar(num(1.0)),
                Range(vec![num(2.0), num(1.0), num(2.0), num(1.0)]),
            ],
        );
        assert_eq!(
            got,
            spill(4, 1, vec![txt("b"), txt("a"), txt("d"), txt("c")])
        );
    }

    #[test]
    fn multi_column_array_rows_sorted() {
        // 3×2 array sorted by a separate column key {3;1;2} → rows reordered.
        let got = eval_direct(
            eval,
            vec![
                Rect {
                    rows: 3,
                    cols: 2,
                    data: vec![
                        num(1.0),
                        num(10.0),
                        num(2.0),
                        num(20.0),
                        num(3.0),
                        num(30.0),
                    ],
                },
                Range(vec![num(3.0), num(1.0), num(2.0)]),
            ],
        );
        // key {3,1,2} asc → order rows 1,2,0 → (2,20),(3,30),(1,10).
        assert_eq!(
            got,
            spill(
                3,
                2,
                vec![
                    num(2.0),
                    num(20.0),
                    num(3.0),
                    num(30.0),
                    num(1.0),
                    num(10.0)
                ]
            )
        );
    }

    #[test]
    fn invalid_order_is_value() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(num(5.0))
                ],
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn mismatched_by_array_size_is_value() {
        // by_array has 2 elements, array has 3 rows → #VALUE!.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![num(1.0), num(2.0)])
                ],
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn second_by_array_wrong_size_is_value() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(num(1.0)),
                    Range(vec![num(1.0), num(2.0)]),
                ],
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn error_in_by_array_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![txt("a"), txt("b")]),
                    Range(vec![Value::Error(ErrorKind::Div0), num(1.0)]),
                ],
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn whole_column_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Unbounded(vec![num(1.0), num(2.0)]),
                    Unbounded(vec![num(2.0), num(1.0)])
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn row_by_array_sorts_columns() {
        // A ROW by_array1 (1×3) sorts the COLUMNS of a matching-width array.
        // array 1×3 {"a","b","c"}; by row {3,1,2} asc → columns order 1,2,0.
        let got = eval_direct(
            eval,
            vec![
                Array(vec![txt("a"), txt("b"), txt("c")]),
                Array(vec![num(3.0), num(1.0), num(2.0)]),
            ],
        );
        assert_eq!(got, spill(1, 3, vec![txt("b"), txt("c"), txt("a")]));
    }

    #[test]
    fn elided_first_sort_order_defaults_ascending() {
        // SORTBY(arr, by1, , by2, -1): positional — the elided sort_order1 slot
        // defaults ascending (NOT a false #UNSUPPORTED!); by2 descending.
        let got = eval_direct(
            eval,
            vec![
                Range(vec![txt("a"), txt("b"), txt("c"), txt("d")]),
                Range(vec![num(1.0), num(1.0), num(2.0), num(2.0)]),
                Omitted,
                Range(vec![num(2.0), num(1.0), num(2.0), num(1.0)]),
                Scalar(num(-1.0)),
            ],
        );
        // key1=1 group: rows 0(k2=2),1(k2=1) desc → 0,1. key1=2 group: 2,3.
        assert_eq!(
            got,
            spill(4, 1, vec![txt("a"), txt("b"), txt("c"), txt("d")])
        );
    }

    #[test]
    fn range_in_sort_order_slot_refused() {
        // SORTBY(arr, by1, by2): positionally by2 lands in the sort_order1 slot
        // (index 2). A range there is UNPINNED → #UNSUPPORTED! (never silently a
        // second key).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![txt("a"), txt("b")]),
                    Range(vec![num(1.0), num(2.0)]),
                    Range(vec![num(3.0), num(4.0)]),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn elided_by_array_slot_is_value() {
        // SORTBY(arr, by1, 1, , -1): the by_array2 slot (index 3) is elided — a
        // by_array is required → #VALUE! (not a silent skip).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![txt("a"), txt("b")]),
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(num(1.0)),
                    Omitted,
                    Scalar(num(-1.0)),
                ],
            ),
            Value::Error(ErrorKind::Value)
        );
    }
}

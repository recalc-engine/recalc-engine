//! `FILTER` — return the rows (or columns) of `array` for which the lockstep
//! boolean `include` array is TRUE.
//!
//! # Provenance
//! Microsoft support page "FILTER function"
//! (<https://support.microsoft.com/en-us/office/filter-function-f4f7cb66-82eb-4767-8f7c-4877ad80c759>),
//! fetched 2026-07-15. Clean-room from the page's unambiguous prose; boolean
//! coercion is `xl-value`'s frozen [`to_bool`](xl_value::to_bool).
//!
//! # Signature (page verbatim)
//! `FILTER(array, include, [if_empty])` — 2..=3 args. `include` is "A Boolean
//! array whose height **or** width is the same as the array". If the result is
//! empty and `if_empty` is omitted, "a #CALC! error will result". "If any value
//! of the include argument is an error … or cannot be converted to a Boolean,
//! the FILTER function will return an error."
//!
//! # Semantics implemented
//! - **`include` a column** (`H×1`, `H == array` height) → keep matching **rows**.
//! - **`include` a row** (`1×W`, `W == array` width) → keep matching **columns**.
//! - Each `include` cell is coerced with [`to_bool`](xl_value::to_bool): `TRUE`/
//!   nonzero-number/`"TRUE"` include; `FALSE`/`0`/`Blank`/`"FALSE"` exclude; an
//!   **error** propagates and non-convertible **text** is `#VALUE!` — both
//!   surfaced leftmost-first (the "return an error" clause).
//! - **Empty result** → a *provided* `if_empty` (even a `Blank`); **genuinely
//!   absent** (2-arg call) → `#CALC!` (documented "Otherwise … a #CALC! error");
//!   an **elided `,`** slot is unpinned (`,` = "missing" vs a `Blank` value) →
//!   `#UNSUPPORTED!` (L3B probe doc) rather than a silent `Blank`.
//! - A **data error inside `array`** rides along into the result in place
//!   (Excel-faithful): FILTER never compares the data, only the `include` mask,
//!   so a `#DIV/0!` in a kept cell is preserved, not dropped.
//!
//! # Refused (see the probe doc)
//! - **`include` shape mismatch** — neither a column matching the height nor a
//!   row matching the width (including a genuinely 2-D `include`): `#VALUE!`
//!   (the documented lockstep requirement; the brief pins `#VALUE!` for this).
//! - **Whole-column/row inputs** (`A:A`): the dense walk refuses the unbounded
//!   range → `#UNSUPPORTED!` (`crate::dynarray` module docs).

use xl_value::{ErrorKind, Value, to_bool};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;
use crate::dynarray::{materialize, spill};

/// Evaluate a `FILTER(...)` call. See the module docs for semantics/provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let data = match materialize(args, 0) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    let inc = match materialize(args, 1) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };

    // Orientation: a column include (width 1) filters rows; a row include
    // (height 1) filters columns. Prefer the row filter when both fit (a 1×1
    // include on 1×1 data — either reading is the same single cell).
    let row_filter = inc.width == 1 && inc.height == data.height;
    let col_filter = inc.height == 1 && inc.width == data.width;

    if row_filter {
        filter_rows(args, &data, &inc)
    } else if col_filter {
        filter_cols(args, &data, &inc)
    } else {
        // Lockstep shape violation → #VALUE! (documented as an error; brief pins
        // #VALUE! for the mismatch).
        Value::Error(ErrorKind::Value)
    }
}

/// Keep the rows of `data` whose `include` column cell is TRUE.
fn filter_rows(
    args: &mut dyn CallArgs,
    data: &crate::dynarray::Grid,
    inc: &crate::dynarray::Grid,
) -> Value {
    let mut kept: Vec<Vec<Value>> = Vec::new();
    for r in 0..data.height {
        match to_bool(&inc.rows[r][0]) {
            Ok(true) => kept.push(data.rows[r].clone()),
            Ok(false) => {}
            Err(k) => return Value::Error(k),
        }
    }
    if kept.is_empty() {
        return if_empty(args);
    }
    let height = kept.len();
    let width = data.width;
    let flat: Vec<Value> = kept.into_iter().flatten().collect();
    spill(height, width, flat)
}

/// Keep the columns of `data` whose `include` row cell is TRUE.
fn filter_cols(
    args: &mut dyn CallArgs,
    data: &crate::dynarray::Grid,
    inc: &crate::dynarray::Grid,
) -> Value {
    let mut kept_cols: Vec<usize> = Vec::new();
    for c in 0..data.width {
        match to_bool(&inc.rows[0][c]) {
            Ok(true) => kept_cols.push(c),
            Ok(false) => {}
            Err(k) => return Value::Error(k),
        }
    }
    if kept_cols.is_empty() {
        return if_empty(args);
    }
    let height = data.height;
    let width = kept_cols.len();
    let mut flat: Vec<Value> = Vec::with_capacity(height * width);
    for r in 0..height {
        for &c in &kept_cols {
            flat.push(data.rows[r][c].clone());
        }
    }
    spill(height, width, flat)
}

/// The empty-result behavior, distinguishing three `if_empty` cases (Omitted
/// covers both an absent trailing arg and an elided `,` slot — told apart by
/// `count`):
/// - **genuinely absent** (arg count ≤ 2) → `#CALC!`: the page documents
///   "Otherwise, a #CALC! error will result" for a call with no 3rd argument.
/// - **elided `,`** (count > 2 but arg 2 `Omitted`, e.g. `FILTER(rng,inc,)`) →
///   `#UNSUPPORTED!`: whether Excel reads `,` as "missing" (`#CALC!`) or as a
///   `Blank` value is unpinned (OXP-080 precedent; L3B probe doc). Never silently
///   returns the `Blank` that `eval_scalar` would give.
/// - **provided** (even a `Blank`) → used as-is.
fn if_empty(args: &mut dyn CallArgs) -> Value {
    if args.count() <= 2 {
        Value::Error(ErrorKind::Calc)
    } else if args.shape(2) == ArgShape::Omitted {
        Value::Error(ErrorKind::Unsupported)
    } else {
        args.eval_scalar(2)
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::dynarray::spill;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn filter_rows_keeps_true() {
        // FILTER({10;20;30}, {TRUE;FALSE;TRUE}) → {10;30}.
        let got = eval_direct(
            eval,
            vec![
                Range(vec![num(10.0), num(20.0), num(30.0)]),
                Range(vec![
                    Value::Bool(true),
                    Value::Bool(false),
                    Value::Bool(true),
                ]),
            ],
        );
        assert_eq!(got, spill(2, 1, vec![num(10.0), num(30.0)]));
    }

    #[test]
    fn filter_rows_multicol_data() {
        // 3×2 data, keep rows 0 and 2 → 2×2.
        let got = eval_direct(
            eval,
            vec![
                Rect {
                    rows: 3,
                    cols: 2,
                    data: vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0), num(6.0)],
                },
                Range(vec![num(1.0), num(0.0), num(1.0)]),
            ],
        );
        assert_eq!(
            got,
            spill(2, 2, vec![num(1.0), num(2.0), num(5.0), num(6.0)])
        );
    }

    #[test]
    fn filter_columns() {
        // 2×3 data, include is a 1×3 row {TRUE,FALSE,TRUE} → keep cols 0,2 (2×2).
        let got = eval_direct(
            eval,
            vec![
                Rect {
                    rows: 2,
                    cols: 3,
                    data: vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0), num(6.0)],
                },
                Array(vec![
                    Value::Bool(true),
                    Value::Bool(false),
                    Value::Bool(true),
                ]),
            ],
        );
        assert_eq!(
            got,
            spill(2, 2, vec![num(1.0), num(3.0), num(4.0), num(6.0)])
        );
    }

    #[test]
    fn empty_result_no_if_empty_is_calc() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Range(vec![num(0.0), num(0.0)])
                ],
            ),
            Value::Error(ErrorKind::Calc)
        );
    }

    #[test]
    fn empty_result_uses_if_empty() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Range(vec![num(0.0), num(0.0)]),
                    Scalar(txt("none")),
                ],
            ),
            txt("none")
        );
    }

    #[test]
    fn shape_mismatch_is_value() {
        // include height 2 ≠ data height 3, and not a matching row → #VALUE!.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![Value::Bool(true), Value::Bool(false)]),
                ],
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn include_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Range(vec![Value::Error(ErrorKind::Na), Value::Bool(true)]),
                ],
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn include_nonconvertible_text_is_value() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Range(vec![txt("yes"), Value::Bool(true)]),
                ],
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn data_error_rides_along() {
        // A #DIV/0! in a kept row is preserved in the output, not dropped.
        let got = eval_direct(
            eval,
            vec![
                Range(vec![Value::Error(ErrorKind::Div0), num(2.0)]),
                Range(vec![Value::Bool(true), Value::Bool(false)]),
            ],
        );
        assert_eq!(got, spill(1, 1, vec![Value::Error(ErrorKind::Div0)]));
    }

    #[test]
    fn whole_column_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Unbounded(vec![num(1.0)]),
                    Unbounded(vec![Value::Bool(true)])
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn elided_if_empty_on_empty_is_refused() {
        // FILTER(rng,inc,) with an empty result and an ELIDED if_empty slot must
        // refuse (unpinned `,`) — NOT silently return the Blank that eval_scalar
        // would give. (Genuinely-absent 2-arg → documented #CALC!, tested above.)
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Range(vec![num(0.0), num(0.0)]),
                    Omitted,
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

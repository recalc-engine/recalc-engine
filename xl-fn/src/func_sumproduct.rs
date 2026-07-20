//! `SUMPRODUCT` — sum the products of corresponding array elements.
//!
//! # Provenance
//! Behavior contract: `docs/specs/SUMPRODUCT.md` (which cites the Microsoft Learn
//! `SUMPRODUCT` page, verified 2026-07-08). Two rules are quoted verbatim from
//! that page and drive this evaluator:
//! - *"The array arguments must have the same dimensions. If they do not,
//!   SUMPRODUCT returns the #VALUE! error value."*
//! - *"SUMPRODUCT treats non-numeric array entries as if they were zeros."*
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Multiplies the **corresponding** elements of each array argument and returns
//!   the sum of those products (SUMPRODUCT.md §1). A single array argument is the
//!   degenerate one-factor case → the sum of its (numeric) elements.
//! - **Arrays that share identical `(rows, cols)`** are multiplied
//!   position-by-position (SUMPRODUCT.md §Dimensions). On a mismatch the public
//!   page pins `#VALUE!` for "different dimensions". **OXP-115 RESOLVED by
//!   RUN-2026-07-11-oracle01**: the row-vs-column transpose
//!   `=SUMPRODUCT({1,2,3},{4;5;6})` (1×3 vs 3×1) returns `#VALUE!` — Excel does
//!   *not* form an outer product — so it joins the pinned `#VALUE!` path with a
//!   genuine same-orientation length mismatch (`9×1` vs `4×1`). The sole remaining
//!   deferral is a **1×1 scalar mixed with a larger array**
//!   (`SUMPRODUCT(A1:A10, 2)` = `2·ΣA`), the common scalar-broadcast idiom: it was
//!   not probed in that run, so it stays `#UNSUPPORTED!` (**OXP-115**, scalar
//!   branch open) rather than guess `#VALUE!` on a case Excel likely computes.
//!   Dimensions are read with [`CallArgs::dims`].
//! - **Non-numeric elements contribute 0.** Unlike `SUM`/`PRODUCT`'s range
//!   aggregate (which *skips* text/blank/logical), SUMPRODUCT folds every
//!   non-numeric element in as a literal `0` factor — doc-verified
//!   (SUMPRODUCT.md §Coercion). This is realised by reusing
//!   [`coerce_number_arg`] under [`CoercionMode::RangeAggregate`] and mapping its
//!   [`NumericArg::Skip`] (the "ignored in a range" verdict) to `0.0` rather than
//!   omitting the element. Logical `TRUE`/`FALSE` inside an array are non-numeric
//!   here, so both fold in as `0` (not 1/0).
//! - A **scalar** (`ArgShape::Scalar`) argument acts as a 1×1 array; a 1×1 range
//!   or array constant likewise reports `(1, 1)` from [`CallArgs::dims`].
//! - An **error** element propagates as the result (SUMPRODUCT.md §Error /
//!   **OXP-112 RESOLVED by RUN-2026-07-11-oracle01**). Error wins over a zero
//!   factor at the same position: even where another factor is 0, an error
//!   co-factor still propagates (Excel's array multiplication propagates errors;
//!   `0 * #DIV/0! = #DIV/0!`). `=SUMPRODUCT({1,2},{3,#N/A})` → `#N/A`. The
//!   short-circuit order is confirmed by `=SUMPRODUCT({#DIV/0!,1},{#N/A,1})` →
//!   `#DIV/0!`: the co-located `#DIV/0!` (first array) beats the `#N/A` (second
//!   array), i.e. the first factor scanned at the first offending position wins —
//!   exactly the array-then-array, position-order traversal below.
//! - Overflow (a non-finite running sum/product) becomes `#NUM!` via
//!   [`Value::number`]'s value invariant.
//!
//! # Whole-column / unbounded ranges → `#UNSUPPORTED!`
//! A whole-column/row range surfaces as [`CallArgs::dims`] `== None` **with a
//! non-scalar [`ArgShape`]** (and its dense [`CallArgs::for_each_row`] refuses).
//! SUMPRODUCT here needs the *dense* rectangle (blanks fold in as 0 factors and
//! participate positionally), so the used-extent walk is not a sound substitute:
//! the absent rows of one unbounded column would still multiply against populated
//! rows of another. Rather than guess an extent this returns `#UNSUPPORTED!`
//! (`OXP-113`); the used-extent path for SUMPRODUCT is explicitly out of scope
//! for this task. The public page itself advises against `SUMPRODUCT(A:A,B:B)`.
//!
//! **OXP-113 stays `#UNSUPPORTED!` (engineering guard, confirmed non-semantic).**
//! RUN-2026-07-11-oracle01 observed `=SUMPRODUCT(A:A,B:B)` = `130.0`, i.e. Excel
//! returns a *number*, not an error — so the deferral is purely our engineering
//! limitation (no cheap dense used-extent for an unbounded range at this layer),
//! never an Excel semantic. The observed `130.0` is not reproducible here anyway:
//! it depends on the probe sheet's A/B column data, which the sidecar does not
//! carry. A future `xl-io` used-range extent could turn the guard into a clamp.

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// One materialized array argument: a `rows × cols` rectangle flattened
/// row-major (`cells.len() == rows * cols`), blanks surfaced positionally.
struct Grid {
    rows: u32,
    cols: u32,
    cells: Vec<Value>,
}

/// Evaluate a `SUMPRODUCT(array1, [array2], ...)` call. See the module docs for
/// the semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Materialize every supplied (non-omitted) array argument as a dense
    // rectangle. A whole-column/unbounded range refuses → #UNSUPPORTED!.
    let mut grids: Vec<Grid> = Vec::new();
    for i in 0..args.count() {
        if args.shape(i) == ArgShape::Omitted {
            continue;
        }
        match materialize(args, i) {
            Ok(grid) => grids.push(grid),
            Err(k) => return Value::Error(k),
        }
    }

    // All-omitted degenerate call (arity guarantees ≥1 supplied position, so this
    // is only reached if every one was an elided slot): nothing to sum → 0.
    let Some(first) = grids.first() else {
        return Value::number(0.0);
    };
    let (rows, cols) = (first.rows, first.cols);

    // Dimension check. Every array sharing the reference rectangle exactly is the
    // computable case. On a mismatch, the public page pins `#VALUE!` for arrays of
    // "different dimensions". OXP-115 RESOLVED by RUN-2026-07-11-oracle01: the
    // row-vs-column transpose `=SUMPRODUCT({1,2,3},{4;5;6})` (1×3 vs 3×1) returns
    // `#VALUE!` in Excel — it is NOT broadcast into an outer product. So a
    // row-vs-column mismatch now takes the pinned `#VALUE!` path alongside a
    // genuine same-orientation length mismatch (`9×1` vs `4×1`). The one case that
    // remains deferred is a **1×1 scalar mixed with a larger array**
    // (`SUMPRODUCT(A1:A10, 2)` = `2·ΣA`): that scalar-broadcast idiom was NOT
    // probed in RUN-2026-07-11-oracle01, so we neither guess `#VALUE!` on a case
    // Excel likely computes nor invent the broadcast — it stays `#UNSUPPORTED!`
    // until an oracle observation pins it (OXP-115, scalar branch open).
    if grids.iter().any(|g| g.rows != rows || g.cols != cols) {
        let has_scalar = grids.iter().any(|g| g.rows == 1 && g.cols == 1);
        let has_nonscalar = grids.iter().any(|g| !(g.rows == 1 && g.cols == 1));
        let scalar_broadcast = has_scalar && has_nonscalar;
        return Value::Error(if scalar_broadcast {
            ErrorKind::Unsupported
        } else {
            ErrorKind::Value
        });
    }

    let n = (rows as usize) * (cols as usize);
    let mut acc = 0.0_f64;
    for k in 0..n {
        // Product of the corresponding element across every array. A non-numeric
        // element is a `0` factor; an error element propagates (error wins even
        // when another factor is 0 — we scan all factors before summing).
        let mut product = 1.0_f64;
        for g in &grids {
            match coerce_number_arg(&g.cells[k], CoercionMode::RangeAggregate) {
                NumericArg::Number(x) => product *= x,
                // Non-numeric entry → treated as zero (SUMPRODUCT.md §Coercion),
                // *not* skipped as SUM/PRODUCT would.
                NumericArg::Skip => product *= 0.0,
                // An error element propagates as the whole result.
                NumericArg::Error(e) => return Value::Error(e),
            }
        }
        acc += product;
    }

    // Non-finite running total (overflow) → #NUM! per the value invariant.
    Value::number(acc)
}

/// Materialize argument `index` into a dense [`Grid`].
///
/// - A **bounded** rectangle (`dims == Some`, including a 1×1 range/array
///   literal) is walked with [`CallArgs::for_each_row`] and padded to the
///   reported `cols` width with blanks so ragged rows still line up positionally.
/// - A **scalar expression** (`dims == None` **and** `ArgShape::Scalar`) is a 1×1
///   array holding its evaluated value.
/// - Anything else with `dims == None` is an unbounded whole-column/row range
///   (or an unresolvable range) → `Err(ErrorKind::Unsupported)` (OXP-113).
fn materialize(args: &mut dyn CallArgs, index: usize) -> Result<Grid, ErrorKind> {
    match args.dims(index) {
        Some((rows, cols)) => {
            let mut rows_buf: Vec<Vec<Value>> = Vec::new();
            args.for_each_row(index, &mut |row| {
                rows_buf.push(row.to_vec());
                ControlFlow::Continue(())
            })?;
            let mut cells = Vec::with_capacity((rows as usize).saturating_mul(cols as usize));
            for r in 0..rows as usize {
                let row = rows_buf.get(r);
                for c in 0..cols as usize {
                    let v = row
                        .and_then(|rw| rw.get(c))
                        .cloned()
                        .unwrap_or(Value::Blank);
                    cells.push(v);
                }
            }
            Ok(Grid { rows, cols, cells })
        }
        None => match args.shape(index) {
            // A true scalar expression acts as a 1×1 array.
            ArgShape::Scalar => Ok(Grid {
                rows: 1,
                cols: 1,
                cells: vec![args.eval_scalar(index)],
            }),
            // Unbounded whole-column/row range (or unresolvable) → guard (OXP-113).
            _ => Err(ErrorKind::Unsupported),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn two_rows_sum_of_products() {
        // SUMPRODUCT({1,2,3},{4,5,6}) = 4 + 10 + 18 = 32.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Array(vec![num(1.0), num(2.0), num(3.0)]),
                    Array(vec![num(4.0), num(5.0), num(6.0)]),
                ]
            ),
            num(32.0)
        );
    }

    #[test]
    fn two_columns_sum_of_products() {
        // Same, as 3×1 columns: 1*4 + 2*5 + 3*6 = 32.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![num(4.0), num(5.0), num(6.0)]),
                ]
            ),
            num(32.0)
        );
    }

    #[test]
    fn single_array_is_sum_of_elements() {
        // One factor per position → plain sum: 1 + 2 + 3 = 6.
        assert_eq!(
            eval_direct(eval, vec![Array(vec![num(1.0), num(2.0), num(3.0)])]),
            num(6.0)
        );
    }

    #[test]
    fn rectangles_elementwise() {
        // 2×2 · 2×2: 1*1 + 2*0 + 3*0 + 4*1 = 5 (identity picks the diagonal).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Rect {
                        rows: 2,
                        cols: 2,
                        data: vec![num(1.0), num(2.0), num(3.0), num(4.0)],
                    },
                    Rect {
                        rows: 2,
                        cols: 2,
                        data: vec![num(1.0), num(0.0), num(0.0), num(1.0)],
                    },
                ]
            ),
            num(5.0)
        );
    }

    #[test]
    fn scalar_acts_as_one_by_one() {
        // A lone scalar is a 1×1 array → its own value.
        assert_eq!(eval_direct(eval, vec![Scalar(num(7.0))]), num(7.0));
    }

    #[test]
    fn mismatched_dims_is_value_error() {
        // 1×3 vs 1×2 → #VALUE! (doc-verified same-dimensions rule).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Array(vec![num(1.0), num(2.0), num(3.0)]),
                    Array(vec![num(4.0), num(5.0)]),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn row_vs_column_transpose_is_value_error() {
        // OXP-115 RESOLVED by RUN-2026-07-11-oracle01:
        // `=SUMPRODUCT({1,2,3},{4;5;6})` (1×3 row vs 3×1 column) → #VALUE!.
        // Excel does NOT form a 3×3 outer product here — a transpose is a plain
        // dimension mismatch.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Array(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![num(4.0), num(5.0), num(6.0)]),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn scalar_times_array_is_deferred() {
        // SUMPRODUCT(range, scalar): real Excel broadcasts the 1×1 scalar across
        // the array (a very common idiom). This scalar-broadcast case was NOT
        // probed in RUN-2026-07-11-oracle01 (OXP-115 scalar branch still open), so
        // it stays #UNSUPPORTED! rather than guess #VALUE! on a case Excel likely
        // computes.
        assert_eq!(
            eval_direct(
                eval,
                vec![Array(vec![num(1.0), num(2.0), num(3.0)]), Scalar(num(2.0))]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn non_numeric_entries_are_zero() {
        // Text, blank, and logical entries fold in as 0 factors (NOT skipped):
        // position 0: 1*10 = 10
        // position 1: "x"->0 * 10 = 0
        // position 2: blank->0 * 10 = 0
        // position 3: TRUE->0 * 10 = 0
        // sum = 10.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Array(vec![num(1.0), txt("x"), Value::Blank, Value::Bool(true)]),
                    Array(vec![num(10.0), num(10.0), num(10.0), num(10.0)]),
                ]
            ),
            num(10.0)
        );
    }

    #[test]
    fn non_numeric_in_single_array_folds_to_zero() {
        // SUMPRODUCT({2,"a",FALSE,3}) — non-numerics are 0 → 2 + 0 + 0 + 3 = 5.
        assert_eq!(
            eval_direct(
                eval,
                vec![Array(vec![
                    num(2.0),
                    txt("a"),
                    Value::Bool(false),
                    num(3.0),
                ])]
            ),
            num(5.0)
        );
    }

    #[test]
    fn error_element_propagates() {
        // An error anywhere propagates as the result.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Array(vec![num(1.0), Value::Error(ErrorKind::Div0), num(3.0)]),
                    Array(vec![num(4.0), num(5.0), num(6.0)]),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn error_wins_over_zero_cofactor() {
        // Position 0 has a 0 factor and an error co-factor: error still wins
        // (0 * #VALUE! = #VALUE!), not a silent 0 contribution.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Array(vec![num(0.0)]),
                    Array(vec![Value::Error(ErrorKind::Value)]),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn oxp112_error_element_propagates() {
        // OXP-112 RESOLVED by RUN-2026-07-11-oracle01:
        // `=SUMPRODUCT({1,2},{3,#N/A})` → #N/A (the #N/A at position 1 propagates).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Array(vec![num(1.0), num(2.0)]),
                    Array(vec![num(3.0), Value::Error(ErrorKind::Na)]),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn oxp112_first_error_in_traversal_order_wins() {
        // OXP-112 RESOLVED by RUN-2026-07-11-oracle01:
        // `=SUMPRODUCT({#DIV/0!,1},{#N/A,1})` → #DIV/0!. At position 0 both arrays
        // hold an error; the first array's #DIV/0! is scanned first and wins over
        // the second array's #N/A. Confirms the array-then-array, position-order
        // short-circuit.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Array(vec![Value::Error(ErrorKind::Div0), num(1.0)]),
                    Array(vec![Value::Error(ErrorKind::Na), num(1.0)]),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn whole_column_is_unsupported() {
        // An unbounded whole-column range refuses the dense rectangle → guard
        // (OXP-113), never a silently wrong sparse fold.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Unbounded(vec![num(1.0), num(2.0), num(3.0)]),
                    Array(vec![num(4.0), num(5.0), num(6.0)]),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

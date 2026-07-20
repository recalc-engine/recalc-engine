//! `MINIFS` — the minimum of the cells matching **all** of N criteria
//! (logical AND).
//!
//! # Provenance
//! Behavior contract: Microsoft support "MINIFS function"
//! (<https://support.microsoft.com/en-us/office/minifs-function-6ca1ddaa-079b-4e74-80cc-72eef32e6599>,
//! verified by WebFetch 2026-07-15). No `docs/specs/MINIFS.md` exists in this
//! pass. This module mirrors [`crate::func_maxifs`] exactly — itself a mirror
//! of `SUMIFS` ([`crate::func_sumifs`]) — swapping the running **maximum** for
//! a running **minimum** (the `MIN`-vs-`MAX` fold). The criteria mini-language
//! is owned by [`crate::criteria`] and reused **unchanged**.
//!
//! # Behavior contract (one line)
//! `MINIFS(min_range, crit_range1, crit1, …)` = the smallest numeric
//! `min_range` cell whose aligned position satisfies every criteria pair; `0`
//! if none match.
//!
//! # Signature and argument order
//! `MINIFS(min_range, criteria_range1, criteria1, [criteria_range2,
//! criteria2], …)` — up to 126 criteria pairs; `min_range` comes first. The
//! registry enforces `min_args = 3`; an **even** argument count is a dangling
//! `criteria_range` with no `criteria` → `#VALUE!`.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - Takes the minimum over each `min_range` cell whose aligned position
//!   satisfies **every** `criteria_rangeN`/`criteriaN` pair (logical AND).
//! - "The size and shape of the min_range and all criteria_range arguments must
//!   be the same, otherwise these functions return the #VALUE! error." A
//!   mismatch → `#VALUE!`.
//! - Each `criteriaN` uses the identical mini-language as `SUMIFS`/`MAXIFS`
//!   ([`crate::criteria`]); an error-valued criterion propagates and an
//!   oracle-deferred criterion returns `#UNSUPPORTED!` (via
//!   [`Matcher::short_circuit`], in argument order).
//! - **Only numeric** `min_range` cells at a matched position participate;
//!   text/blank/logical cells there are [`NumericArg::Skip`] — ignored (the
//!   `MIN` `RangeAggregate` rule). An error in a candidate cell at a matched
//!   position propagates.
//! - **No cells meet the criteria** (or every matched cell is non-numeric) →
//!   `0` (documented: "Example 6" → `0`; mirrors `MIN`'s no-numeric-values-→`0`
//!   rule).
//!
//! # Whole-column ranges + Recalc sentinels — mirrors `SUMIFS`/`MAXIFS`
//! A whole-**column** range refuses the dense walk and defers loudly (the same
//! unobserved multi-range used-extent alignment `SUMIFS` flags). Recalc
//! sentinels in a criteria-tested cell propagate (kind preserved), checked
//! before [`criteria::matches`] in per-criterion scan order — identical to
//! [`crate::func_maxifs`]; see that module and
//! `docs/plans/2026-07-15-lane5-probe-needed.md`.

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::criteria::{self, Matcher};

/// Evaluate a `MINIFS(min_range, criteria_range1, criteria1, …)` call. See the
/// module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let count = args.count();
    // Arity: 1 `min_range` + N complete criteria pairs → odd and >= 3.
    if count < 3 || count.is_multiple_of(2) {
        return Value::Error(ErrorKind::Value);
    }
    let num_pairs = (count - 1) / 2;

    // Compile every criterion once, short-circuiting (in argument order) on an
    // oracle-deferred or error-valued criterion.
    let mut matchers: Vec<Matcher> = Vec::with_capacity(num_pairs);
    for k in 0..num_pairs {
        let crit_index = 2 + 2 * k;
        let matcher = criteria::parse(&args.eval_scalar(crit_index));
        if let Some(short) = matcher.short_circuit() {
            return short;
        }
        matchers.push(matcher);
    }

    // Buffer `min_range` (arg 0); a whole-column/unresolvable range refuses the
    // dense walk → defer loudly.
    let min_grid = match buffer_rows(args, 0) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    let min_dims = dims_of(&min_grid);

    // Buffer every `criteria_rangeN` and require it share `min_range`'s shape.
    let mut crit_grids: Vec<Vec<Vec<Value>>> = Vec::with_capacity(num_pairs);
    for k in 0..num_pairs {
        let cr_index = 1 + 2 * k;
        let grid = match buffer_rows(args, cr_index) {
            Ok(g) => g,
            Err(k) => return Value::Error(k),
        };
        if dims_of(&grid) != min_dims {
            return Value::Error(ErrorKind::Value);
        }
        crit_grids.push(grid);
    }

    // Lockstep AND walk: fold `min_range`'s cell into the running minimum iff
    // every criterion matches its aligned criteria-range cell.
    let (rows, cols) = min_dims;
    let mut best: Option<f64> = None;
    for r in 0..rows {
        for c in 0..cols {
            let mut sentinel: Option<ErrorKind> = None;
            let all_match = crit_grids.iter().zip(&matchers).all(|(grid, m)| {
                let cell = cell_at(grid, r, c);
                if let Some(k) = criteria::refuse_cell(m, cell) {
                    sentinel = Some(k);
                    return false;
                }
                criteria::matches(m, cell)
            });
            if let Some(k) = sentinel {
                return Value::Error(k);
            }
            if !all_match {
                continue;
            }
            match coerce_number_arg(cell_at(&min_grid, r, c), CoercionMode::RangeAggregate) {
                NumericArg::Number(n) => {
                    // First candidate wins ties; a later value replaces the
                    // running minimum only when strictly smaller (MIN's policy).
                    if best.is_none_or(|m| n < m) {
                        best = Some(n);
                    }
                }
                // Non-numeric matched cells are ignored (MIN RangeAggregate).
                NumericArg::Skip => {}
                // An error in a matched candidate cell propagates.
                NumericArg::Error(k) => return Value::Error(k),
            }
        }
    }
    // No matching numeric cell → 0 (documented).
    Value::number(best.unwrap_or(0.0))
}

/// Buffer an argument's rectangle via the **dense** [`CallArgs::for_each_row`]
/// walk. Identical to `func_sumifs::buffer_rows`.
fn buffer_rows(args: &mut dyn CallArgs, index: usize) -> Result<Vec<Vec<Value>>, ErrorKind> {
    let mut rows: Vec<Vec<Value>> = Vec::new();
    args.for_each_row(index, &mut |row| {
        rows.push(row.to_vec());
        ControlFlow::Continue(())
    })?;
    Ok(rows)
}

/// The dimensions `(rows, cols)` of a buffered grid. Identical to
/// `func_sumifs::dims_of`.
fn dims_of(grid: &[Vec<Value>]) -> (usize, usize) {
    let rows = grid.len();
    let cols = grid.iter().map(Vec::len).max().unwrap_or(0);
    (rows, cols)
}

/// The cell at `(r, c)`, or [`Value::Blank`] when absent. Identical to
/// `func_sumifs::cell_at`.
fn cell_at(grid: &[Vec<Value>], r: usize, c: usize) -> &Value {
    grid.get(r)
        .and_then(|row| row.get(c))
        .unwrap_or(&Value::Blank)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // Single criterion: min of matched cells. min=[10,20,30,40], cr=[1,2,3,4]
    // ">2" → min(30,40) = 30.
    #[test]
    fn single_criterion_min() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0), num(20.0), num(30.0), num(40.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0), num(4.0)]),
                    Scalar(txt(">2")),
                ]
            ),
            num(30.0)
        );
    }

    // Two criteria ANDed. min=[10,20,30,40]; cr1 ">1" (idx 1,2,3); cr2 "a"
    // (idx 0,2). AND → idx 2 → min(30) = 30.
    #[test]
    fn two_criteria_logical_and() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0), num(20.0), num(30.0), num(40.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0), num(4.0)]),
                    Scalar(txt(">1")),
                    Range(vec![txt("a"), txt("b"), txt("a"), txt("b")]),
                    Scalar(txt("a")),
                ]
            ),
            num(30.0)
        );
    }

    // No cells meet the criteria → 0 (documented).
    #[test]
    fn no_matches_is_zero() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(txt(">100")),
                ]
            ),
            num(0.0)
        );
    }

    // Non-numeric matched cells are ignored. min=[10,"x",30] all matched ">0" →
    // min(10,30) = 10 (the text does not become 0, which would wrongly be the
    // min).
    #[test]
    fn non_numeric_matched_cell_ignored() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0), txt("x"), num(30.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(txt(">0")),
                ]
            ),
            num(10.0)
        );
    }

    // Negatives: MINIFS returns the most negative. min=[-10,-20,-5] → -20.
    #[test]
    fn negatives_min_is_most_negative() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(-10.0), num(-20.0), num(-5.0)]),
                    Range(vec![num(1.0), num(1.0), num(1.0)]),
                    Scalar(num(1.0)),
                ]
            ),
            num(-20.0)
        );
    }

    // Shape mismatch → #VALUE!.
    #[test]
    fn dimension_mismatch_is_value_error() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(txt(">0")),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // Even arity → #VALUE!.
    #[test]
    fn even_arity_is_value_error() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(txt(">0")),
                    Range(vec![num(1.0), num(2.0)]),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // Error-valued criterion propagates.
    #[test]
    fn error_criteria_value_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(Value::Error(ErrorKind::Na)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // Deferred criterion → #UNSUPPORTED!.
    #[test]
    fn deferred_criteria_is_unsupported() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(txt(">ä")),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // Error in a matched candidate cell propagates.
    #[test]
    fn error_in_candidate_cell_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), Value::Error(ErrorKind::Div0), num(3.0)]),
                    Range(vec![num(1.0), num(1.0), num(1.0)]),
                    Scalar(num(1.0)),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    // 2-D rectangle alignment. min=[[10,20],[30,40]], cr=[[1,2],[3,4]] ">2" →
    // min(30,40) = 30.
    #[test]
    fn two_dimensional_rect_alignment() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Rect {
                        rows: 2,
                        cols: 2,
                        data: vec![num(10.0), num(20.0), num(30.0), num(40.0)]
                    },
                    Rect {
                        rows: 2,
                        cols: 2,
                        data: vec![num(1.0), num(2.0), num(3.0), num(4.0)]
                    },
                    Scalar(txt(">2")),
                ]
            ),
            num(30.0)
        );
    }

    // Whole-column range defers loudly.
    #[test]
    fn whole_column_range_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Unbounded(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(txt(">0")),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // A Recalc sentinel in a criteria cell propagates (kind preserved).
    #[test]
    fn sentinel_in_criteria_cell_propagates() {
        for k in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                eval_direct(
                    eval,
                    vec![
                        Range(vec![num(10.0), num(20.0), num(30.0)]),
                        Range(vec![num(1.0), Value::Error(k), num(3.0)]),
                        Scalar(txt(">0")),
                    ]
                ),
                Value::Error(k),
                "{k:?} should propagate"
            );
        }
    }

    // Control: a genuine error in a criteria cell keeps "excluded" behavior.
    // min=[10,20,30], cr=[1,#DIV/0!,3] ">0" → min(10,30) = 10.
    #[test]
    fn genuine_error_in_criteria_cell_excludes_position() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0), num(20.0), num(30.0)]),
                    Range(vec![num(1.0), Value::Error(ErrorKind::Div0), num(3.0)]),
                    Scalar(txt(">0")),
                ]
            ),
            num(10.0)
        );
    }
}

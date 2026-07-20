//! `MAXIFS` — the maximum of the cells matching **all** of N criteria
//! (logical AND).
//!
//! # Provenance
//! Behavior contract: Microsoft support "MAXIFS function"
//! (<https://support.microsoft.com/en-us/office/maxifs-function-dfd611e6-da2c-488a-919b-9b6376b28883>,
//! verified by WebFetch 2026-07-15). No `docs/specs/MAXIFS.md` exists in this
//! pass. This module is built by **mirroring `SUMIFS` exactly**
//! ([`crate::func_sumifs`]) for the criteria machinery, range-shape validation,
//! whole-column deferral, and Recalc-sentinel handling — swapping `SUMIFS`'s
//! running **sum** for a running **maximum** (the same `MAX`-vs-`SUM` fold
//! difference `func_max` makes over `func_sum`). The criteria mini-language is
//! owned by [`crate::criteria`] and reused **unchanged**.
//!
//! # Behavior contract (one line)
//! `MAXIFS(max_range, crit_range1, crit1, …)` = the largest numeric
//! `max_range` cell whose aligned position satisfies every criteria pair; `0`
//! if none match.
//!
//! # Signature and argument order
//! `MAXIFS(max_range, criteria_range1, criteria1, [criteria_range2,
//! criteria2], …)` — up to 126 criteria pairs. Like `SUMIFS`, `max_range`
//! comes first. The registry enforces `min_args = 3`; an **even** argument
//! count is a dangling `criteria_range` with no `criteria` (structurally
//! invalid) → `#VALUE!`.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - Takes the maximum over each `max_range` cell whose aligned position
//!   satisfies **every** `criteria_rangeN`/`criteriaN` pair (logical AND).
//! - "The size and shape of the max_range and criteria_rangeN arguments must be
//!   the same, otherwise these functions return the #VALUE! error." Every
//!   `criteria_rangeN` must share `max_range`'s `(rows, cols)`; a mismatch →
//!   `#VALUE!`.
//! - Each `criteriaN` uses the identical mini-language as `SUMIFS`
//!   ([`crate::criteria`]): numbers, `">100"` comparisons, wildcards, escaped
//!   literals, dynamic (pre-concatenated) criteria. Each criterion is evaluated
//!   once in scalar context and compiled once; an error-valued criterion
//!   propagates and an oracle-deferred criterion returns `#UNSUPPORTED!` (both
//!   via [`Matcher::short_circuit`], in argument order).
//! - **Only numeric** `max_range` cells at a matched position participate; a
//!   text/blank/logical cell there is [`NumericArg::Skip`] — ignored (the
//!   `MAX` `RangeAggregate` rule, **not** `SUMIFS`'s "contributes 0"). An error
//!   in a candidate cell at a matched position propagates.
//! - **No cells meet the criteria** (or every matched cell is non-numeric) →
//!   `0` (documented: "Example 6 … No cells match the criteria" → `0`; mirrors
//!   `MAX`'s no-numeric-values-→`0` rule).
//!
//! # Whole-column ranges — deferred (loud), mirrors `SUMIFS`
//! A whole-**column** range (`A:A`) refuses the dense row walk
//! ([`CallArgs::for_each_row`]); the multi-range used-extent alignment needed
//! to serve it is the same unobserved open question `SUMIFS`/`AVERAGEIFS`
//! flag. Rather than guess, any argument that refuses the dense walk returns
//! `#UNSUPPORTED!`. See those modules' docs and
//! `docs/plans/2026-07-15-lane5-probe-needed.md`.
//!
//! # Recalc sentinels in a criteria-tested cell propagate — mirrors `SUMIFS`
//! Identical fix and rationale to [`crate::func_sumifs`]'s "Recalc sentinels"
//! section: at each position the walk calls [`criteria::sentinel_of`] on every
//! criteria-tested cell *before* [`criteria::matches`], in per-criterion scan
//! order, and propagates the first sentinel found (kind preserved) — a
//! criterion that already fails a genuine mismatch before reaching a sentinel
//! still short-circuits the AND as before. A sentinel in an *unmatched*
//! `max_range` cell stays ignored; one in a **matched** numeric `max_range`
//! cell already propagates via `coerce_number_arg`.

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::criteria::{self, Matcher};

/// Evaluate a `MAXIFS(max_range, criteria_range1, criteria1, …)` call. See the
/// module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let count = args.count();
    // Arity: 1 `max_range` + N complete criteria pairs → odd and >= 3. The
    // registry already rejects < 3; an even count is a dangling `criteria_range`
    // with no `criteria` (structurally invalid) → #VALUE!.
    if count < 3 || count.is_multiple_of(2) {
        return Value::Error(ErrorKind::Value);
    }
    let num_pairs = (count - 1) / 2;

    // Compile every criterion once, in scalar context, short-circuiting (in
    // argument order) on an oracle-deferred or error-valued criterion.
    let mut matchers: Vec<Matcher> = Vec::with_capacity(num_pairs);
    for k in 0..num_pairs {
        let crit_index = 2 + 2 * k;
        let matcher = criteria::parse(&args.eval_scalar(crit_index));
        if let Some(short) = matcher.short_circuit() {
            return short;
        }
        matchers.push(matcher);
    }

    // Buffer `max_range` (arg 0) as a dense rectangle. A whole-column range or
    // an unresolvable range refuses the dense walk → defer loudly.
    let max_grid = match buffer_rows(args, 0) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    let max_dims = dims_of(&max_grid);

    // Buffer every `criteria_rangeN` and require it share `max_range`'s shape.
    let mut crit_grids: Vec<Vec<Vec<Value>>> = Vec::with_capacity(num_pairs);
    for k in 0..num_pairs {
        let cr_index = 1 + 2 * k;
        let grid = match buffer_rows(args, cr_index) {
            Ok(g) => g,
            Err(k) => return Value::Error(k),
        };
        // Mismatched shape across ranges → #VALUE! (documented).
        if dims_of(&grid) != max_dims {
            return Value::Error(ErrorKind::Value);
        }
        crit_grids.push(grid);
    }

    // Lockstep AND walk: fold `max_range`'s cell into the running maximum iff
    // every criterion matches its aligned criteria-range cell.
    let (rows, cols) = max_dims;
    let mut best: Option<f64> = None;
    for r in 0..rows {
        for c in 0..cols {
            // A Recalc sentinel in any criteria-tested cell at this position
            // propagates (kind preserved), checked in per-criterion scan order.
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
            match coerce_number_arg(cell_at(&max_grid, r, c), CoercionMode::RangeAggregate) {
                NumericArg::Number(n) => {
                    // First candidate wins ties; a later value replaces the
                    // running maximum only when strictly greater (MAX's policy).
                    if best.is_none_or(|m| n > m) {
                        best = Some(n);
                    }
                }
                // Non-numeric matched cells are ignored (MAX RangeAggregate).
                NumericArg::Skip => {}
                // An error in a matched candidate cell propagates.
                NumericArg::Error(k) => return Value::Error(k),
            }
        }
    }
    // No matching numeric cell → 0 (documented).
    Value::number(best.unwrap_or(0.0))
}

/// Buffer an argument's rectangle row-by-row via the **dense**
/// [`CallArgs::for_each_row`] walk. Identical to `func_sumifs::buffer_rows`; an
/// unbounded whole-column/row range (or unresolvable range) surfaces as
/// `Err(ErrorKind::Unsupported)` for a loud deferral.
fn buffer_rows(args: &mut dyn CallArgs, index: usize) -> Result<Vec<Vec<Value>>, ErrorKind> {
    let mut rows: Vec<Vec<Value>> = Vec::new();
    args.for_each_row(index, &mut |row| {
        rows.push(row.to_vec());
        ControlFlow::Continue(())
    })?;
    Ok(rows)
}

/// The rectangular dimensions `(rows, cols)` of a buffered dense grid.
/// Identical to `func_sumifs::dims_of`.
fn dims_of(grid: &[Vec<Value>]) -> (usize, usize) {
    let rows = grid.len();
    let cols = grid.iter().map(Vec::len).max().unwrap_or(0);
    (rows, cols)
}

/// The cell at `(r, c)` of a buffered grid, or [`Value::Blank`] when the
/// position is absent (a short row). Identical to `func_sumifs::cell_at`.
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

    // Single criterion: max of the max_range cells whose aligned criteria cell
    // passes. max=[10,20,30,40], cr=[1,2,3,4] ">2" → max(30,40) = 40.
    #[test]
    fn single_criterion_max() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0), num(20.0), num(30.0), num(40.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0), num(4.0)]),
                    Scalar(txt(">2")),
                ]
            ),
            num(40.0)
        );
    }

    // Two criteria ANDed. max=[10,20,30,40]; cr1=[1,2,3,4] ">1" (idx 1,2,3);
    // cr2=["a","b","a","b"] "a" (idx 0,2). AND → idx 2 → max(30) = 30.
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

    // Non-numeric matched cells are ignored (not treated as 0). max=[10,"x",30]
    // all matched ">0" → max(10,30) = 30, NOT max(10,0,30).
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
            num(30.0)
        );
    }

    // Negative numbers: MAXIFS returns the largest (least negative). max=
    // [-10,-20,-5] all matched → -5.
    #[test]
    fn negatives_max_is_least_negative() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(-10.0), num(-20.0), num(-5.0)]),
                    Range(vec![num(1.0), num(1.0), num(1.0)]),
                    Scalar(num(1.0)),
                ]
            ),
            num(-5.0)
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
                    Range(vec![num(1.0), num(2.0)]), // 2 rows vs 3
                    Scalar(txt(">0")),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // Even arity (dangling criteria_range) → #VALUE!.
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

    // Error-valued criterion propagates (via Matcher::short_circuit).
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

    // Deferred criterion (non-ASCII ordering, OXP-031 held) → #UNSUPPORTED!.
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

    // 2-D rectangle alignment. max=[[10,20],[30,40]], cr=[[1,2],[3,4]] ">2" →
    // max(30,40) = 40.
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
            num(40.0)
        );
    }

    // Whole-column (unbounded) range defers loudly (module docs).
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
    // max=[10,20,30], cr=[1,#DIV/0!,3] ">0" → max(10,30) = 30.
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
            num(30.0)
        );
    }
}

//! `SUMIFS` — sum the cells matching **all** of N criteria (logical AND).
//!
//! # Provenance
//! Behavior contract: `docs/specs/SUMIFS.md` (which cites the Microsoft support
//! `SUMIFS` page, verified 2026-07-05). The criteria mini-language is owned by
//! [`crate::criteria`] (shared with `SUMIF`/`COUNTIF`) and reused here
//! **unchanged**; the per-cell summation mirrors `SUMIF`'s range aggregation
//! ([`coerce_number_arg`] under [`CoercionMode::RangeAggregate`]).
//!
//! # Signature and argument order
//! `SUMIFS(sum_range, criteria_range1, criteria1, [criteria_range2, criteria2],
//! …)` — up to 127 criteria pairs. **Unlike `SUMIF`, `sum_range` comes first**
//! (SUMIFS.md §Signature). The registry enforces `min_args = 3`; oddness (one
//! `sum_range` plus N complete pairs) is enforced here — an **even** argument
//! count is a dangling `criteria_range` with no `criteria`, a structurally
//! invalid call, which returns `#VALUE!`.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Sums each `sum_range` cell whose aligned position satisfies **every**
//!   `criteria_rangeN`/`criteriaN` pair simultaneously — logical AND across
//!   pairs, no built-in OR (SUMIFS.md §1).
//! - `sum_range` and every `criteria_rangeN` must be the **same shape**
//!   (identical `(rows, cols)`); any mismatch returns `#VALUE!` (SUMIFS.md §2,
//!   §Error behavior — this multi-range shape check is what distinguishes
//!   SUMIFS from SUMIF, which has no such requirement).
//! - Each `criteriaN` uses the identical mini-language as `SUMIF`
//!   ([`crate::criteria`]): numbers, `">5"`/`"<>0"` comparisons, wildcards,
//!   escaped literals, dynamic (pre-concatenated) criteria (SUMIFS.md §3). Each
//!   criterion is evaluated once in scalar context and compiled once; an
//!   error-valued criterion propagates and an oracle-deferred criterion returns
//!   `#UNSUPPORTED!` (both via [`Matcher::short_circuit`], checked in argument
//!   order).
//! - **Only numeric** `sum_range` cells contribute; a text/blank/logical cell at
//!   a fully-matched position contributes `0`, exactly `SUM`'s `RangeAggregate`
//!   rule (SUMIFS.md §Coercion). An error in a summed cell at a matched position
//!   propagates as the result (SUMIFS.md §Error behavior).
//!
//! # Whole-column ranges — deferred (loud, never guessed)
//! A whole-**column** range (`A:A`) refuses the dense row walk
//! ([`CallArgs::for_each_row`]). `SUMIF` serves the single-range whole-column
//! case with a used-extent walk, but SUMIFS must align **N sparse columns**
//! whose populated rows differ, where a blank cell in one criteria range at a
//! row populated in another is semantically load-bearing for the AND. That
//! multi-range used-extent alignment — and the exact `#VALUE!`-vs-support rule
//! for unbounded shapes — is **unobserved** (SUMIFS.md §Oracle experiments: the
//! precise mismatch trigger for shapes is itself an open experiment). Rather
//! than guess, any argument that refuses the dense walk returns `#UNSUPPORTED!`.
//!
//! ```text
//! // OXP (unassigned): multi-range whole-column alignment for SUMIFS —
//! // =SUMIFS(A:A, B:B, ">5", C:C, "x") with sparse/gappy A/B/C columns:
//! // which populated rows align, and how do absent (blank) cells in one
//! // criteria column at rows populated in another affect the AND? Also the
//! // exact #VALUE! trigger for unbounded/mismatched whole-column shapes.
//! ```
//!
//! # Recalc sentinels in a criteria-tested cell propagate
//! A Recalc sentinel ([`xl_value::ErrorKind::is_recalc_sentinel`]) in any
//! `criteria_rangeN` cell at a position is different from a genuine Excel
//! error there: [`criteria::matches`]'s "excluded" default is correct only
//! when the cell genuinely holds that error in Excel too. Recalc never
//! evaluated a sentinel cell, so whether it would satisfy its criterion in
//! real Excel is unknowable, and reporting "excluded" would launder that gap
//! into a possibly-wrong sum. Per Recalc Principle 2, at each position the
//! walk calls [`criteria::sentinel_of`] on every criteria-tested cell
//! *before* [`criteria::matches`], in the existing per-criterion scan order,
//! and propagates the first sentinel found (kind preserved) out of the whole
//! call — so a criterion that already fails on a genuine mismatch **before**
//! reaching a sentinel still short-circuits the AND exactly as before
//! (no over-propagation). A sentinel in an *unmatched* `sum_range` cell
//! stays ignored (never criteria-tested); a sentinel in a **matched**
//! `sum_range` cell already propagates via `coerce_number_arg` (pre-existing,
//! unaffected by this fix).

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{CallArgs, scalar_literal_error};
use crate::context::EvalContext;
use crate::criteria::{self, Matcher};

/// Evaluate a `SUMIFS(sum_range, criteria_range1, criteria1, …)` call. See the
/// module docs and `docs/specs/SUMIFS.md`.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let count = args.count();
    // Arity: 1 `sum_range` + N complete criteria pairs → odd and >= 3. The
    // registry already rejects < 3; an even count is a dangling `criteria_range`
    // with no `criteria` (structurally invalid) → #VALUE!.
    if count < 3 || count.is_multiple_of(2) {
        return Value::Error(ErrorKind::Value);
    }
    let num_pairs = (count - 1) / 2;

    // A directly-written error as `sum_range` (`SUMIFS(#REF!, …)`, from a deleted
    // source reference) propagates rather than being walked as a lone
    // non-matching cell → silent `0` (Never-silently-wrong; the general
    // error-propagation contract, SUM.md / OXP-082). Only a scalar-LITERAL error
    // is caught here — an error *cell* inside a reference/range keeps its
    // separately-pinned handling (see `args::scalar_literal_error`).
    if let Some(k) = scalar_literal_error(args, 0) {
        return Value::Error(k);
    }

    // Compile every criterion once, in scalar context, short-circuiting (in
    // argument order) on an oracle-deferred or error-valued criterion. Each
    // `criteria_range` (arg 1 + 2k) gets the same scalar-literal-error
    // propagation as `sum_range`, in argument order.
    let mut matchers: Vec<Matcher> = Vec::with_capacity(num_pairs);
    for k in 0..num_pairs {
        let cr_index = 1 + 2 * k;
        if let Some(err) = scalar_literal_error(args, cr_index) {
            return Value::Error(err);
        }
        let crit_index = 2 + 2 * k;
        let matcher = criteria::parse(&args.eval_scalar(crit_index));
        if let Some(short) = matcher.short_circuit() {
            return short;
        }
        matchers.push(matcher);
    }

    // Buffer `sum_range` (arg 0) as a dense rectangle. A whole-column range or an
    // unresolvable range refuses the dense walk → defer loudly (see module OXP).
    let sum_grid = match buffer_rows(args, 0) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    let sum_dims = dims_of(&sum_grid);

    // Buffer every `criteria_rangeN` and require it share `sum_range`'s shape.
    let mut crit_grids: Vec<Vec<Vec<Value>>> = Vec::with_capacity(num_pairs);
    for k in 0..num_pairs {
        let cr_index = 1 + 2 * k;
        let grid = match buffer_rows(args, cr_index) {
            Ok(g) => g,
            Err(k) => return Value::Error(k),
        };
        // SUMIFS.md §2: mismatched shape across ranges → #VALUE!.
        if dims_of(&grid) != sum_dims {
            return Value::Error(ErrorKind::Value);
        }
        crit_grids.push(grid);
    }

    // Lockstep AND walk: include `sum_range`'s cell iff every criterion matches
    // its aligned criteria-range cell.
    let (rows, cols) = sum_dims;
    let mut acc = 0.0_f64;
    for r in 0..rows {
        for c in 0..cols {
            // A Recalc sentinel in any criteria-tested cell at this position
            // propagates (kind preserved) instead of being silently treated
            // as "no match" — checked in the existing per-criterion scan
            // order, so a criterion that already fails normally *before*
            // reaching a sentinel still short-circuits the AND as before
            // (see `criteria::sentinel_of`'s docs).
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
            match coerce_number_arg(cell_at(&sum_grid, r, c), CoercionMode::RangeAggregate) {
                NumericArg::Number(n) => acc += n,
                // Non-numeric summed cells contribute 0 (SUMIFS.md §Coercion).
                NumericArg::Skip => {}
                // An error in a summed cell at a matched position propagates.
                NumericArg::Error(k) => return Value::Error(k),
            }
        }
    }
    Value::number(acc)
}

/// Buffer an argument's rectangle row-by-row into an owned grid via the **dense**
/// [`CallArgs::for_each_row`] walk. An unbounded whole-column/row range (or an
/// unresolvable range) surfaces as `Err(ErrorKind::Unsupported)`, which the
/// caller turns into a loud `#UNSUPPORTED!` deferral (module OXP).
fn buffer_rows(args: &mut dyn CallArgs, index: usize) -> Result<Vec<Vec<Value>>, ErrorKind> {
    let mut rows: Vec<Vec<Value>> = Vec::new();
    args.for_each_row(index, &mut |row| {
        rows.push(row.to_vec());
        ControlFlow::Continue(())
    })?;
    Ok(rows)
}

/// The rectangular dimensions `(rows, cols)` of a buffered dense grid. Well-formed
/// rectangles have uniform-width rows (the engine surfaces blanks positionally);
/// `cols` takes the widest row defensively.
fn dims_of(grid: &[Vec<Value>]) -> (usize, usize) {
    let rows = grid.len();
    let cols = grid.iter().map(Vec::len).max().unwrap_or(0);
    (rows, cols)
}

/// The cell at position `(r, c)` of a buffered grid, or [`Value::Blank`] when the
/// position is absent (a short row) — matching the engine's positional blanks.
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

    // A single criteria pair must behave exactly like `SUMIF(range, crit,
    // sum_range)`: sum the `sum_range` cells whose aligned `criteria_range` cell
    // passes the criterion. sum=[10,20,30,40], cr=[1,2,3,4], ">2" → 30+40 = 70.
    #[test]
    fn single_criterion_matches_sumif() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0), num(20.0), num(30.0), num(40.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0), num(4.0)]),
                    Scalar(txt(">2")),
                ]
            ),
            num(70.0)
        );
    }

    // Two criteria are ANDed at each aligned position. sum=[10,20,30,40];
    // cr1=[1,2,3,4] ">1" (idx 1,2,3); cr2=["a","b","a","b"] "a" (idx 0,2).
    // AND → only idx 2 → 30.
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

    // A bare numeric criteria value is numeric equality. cr==2 at idx 0,2 →
    // 10+30 = 40.
    #[test]
    fn numeric_equality_criteria_value() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0), num(20.0), num(30.0)]),
                    Range(vec![num(2.0), num(5.0), num(2.0)]),
                    Scalar(num(2.0)),
                ]
            ),
            num(40.0)
        );
    }

    // Text criteria is case-insensitive equality. "apple" matches idx 0,2 →
    // 1+3 = 4.
    #[test]
    fn text_criteria_case_insensitive() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![txt("apple"), txt("pear"), txt("APPLE")]),
                    Scalar(txt("apple")),
                ]
            ),
            num(4.0)
        );
    }

    // A comparison operator criterion (">5") over a criteria range distinct from
    // the sum range. cr>5 at idx 1,2 → 200+300 = 500.
    #[test]
    fn operator_criteria() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(100.0), num(200.0), num(300.0)]),
                    Range(vec![num(5.0), num(6.0), num(7.0)]),
                    Scalar(txt(">5")),
                ]
            ),
            num(500.0)
        );
    }

    // A criteria range whose shape differs from `sum_range` → #VALUE!.
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

    // A second criteria range mismatching also triggers #VALUE!.
    #[test]
    fn second_criteria_range_mismatch_is_value_error() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(txt(">0")),
                    Range(vec![txt("a"), txt("b")]), // 2 rows vs 3
                    Scalar(txt("a")),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // No position satisfies the criteria → 0.
    #[test]
    fn no_matches_sums_to_zero() {
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

    // Even argument count (dangling criteria_range, no criteria) → #VALUE!.
    #[test]
    fn even_arity_is_value_error() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(txt(">0")),
                    Range(vec![num(1.0), num(2.0)]), // criteria_range2 with no criteria2
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // An error-valued criterion propagates (via Matcher::short_circuit).
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

    // A criteria-engine short-circuit propagates → #UNSUPPORTED!. Date/currency
    // operands now PARSE (OXP-101/162), so this uses a still-deferred operand: a
    // non-ASCII text ordering criterion (`">ä"`, OXP-031 HELD).
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

    // An error in a summed cell at a matched position propagates.
    #[test]
    fn error_in_summed_cell_propagates() {
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

    // A non-numeric summed cell at a matched position contributes 0 (not an
    // error): sum=[1,"x",3] all matched → 1 + 0 + 3 = 4.
    #[test]
    fn non_numeric_summed_cell_contributes_zero() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), txt("x"), num(3.0)]),
                    Range(vec![num(1.0), num(1.0), num(1.0)]),
                    Scalar(num(1.0)),
                ]
            ),
            num(4.0)
        );
    }

    // Two-dimensional (rect) ranges align positionally across the whole
    // rectangle. sum=[[10,20],[30,40]], cr=[[1,2],[3,4]] ">2" → 30+40 = 70.
    #[test]
    fn two_dimensional_rect_alignment() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Rect {
                        rows: 2,
                        cols: 2,
                        data: vec![num(10.0), num(20.0), num(30.0), num(40.0)],
                    },
                    Rect {
                        rows: 2,
                        cols: 2,
                        data: vec![num(1.0), num(2.0), num(3.0), num(4.0)],
                    },
                    Scalar(txt(">2")),
                ]
            ),
            num(70.0)
        );
    }

    // A whole-column (unbounded) range refuses the dense walk; multi-range
    // alignment is unobserved, so SUMIFS defers loudly (module OXP).
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

    // ---- Recalc sentinels propagate (Principle 2 fix) -----------------

    #[test]
    fn sentinel_in_criteria_cell_propagates_kind_preserved() {
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

    #[test]
    fn genuine_error_in_criteria_cell_still_excludes_that_position_unchanged() {
        // Control: a genuine error in a criteria-tested cell keeps the exact
        // "excluded" behavior — position skipped, not propagated.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0), num(20.0), num(30.0)]),
                    Range(vec![num(1.0), Value::Error(ErrorKind::Div0), num(3.0)]),
                    Scalar(txt(">0")),
                ]
            ),
            num(40.0) // 10 + 30; the #DIV/0! position excluded
        );
    }

    #[test]
    fn sentinel_after_a_definite_mismatch_does_not_over_propagate() {
        // The first criteria_range already fails a genuine (non-error)
        // mismatch BEFORE the second range's sentinel would be scanned — the
        // AND short-circuits exactly as before, so the position is simply
        // excluded, not propagated.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0)]),
                    Range(vec![num(1.0)]), // ">5" fails first
                    Scalar(txt(">5")),
                    Range(vec![Value::Error(ErrorKind::Unsupported)]), // never reached
                    Scalar(txt(">0")),
                ]
            ),
            num(0.0)
        );
    }

    #[test]
    fn sentinel_in_unmatched_sum_range_cell_stays_ignored() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![Value::Error(ErrorKind::Unsupported), num(20.0)]),
                    Range(vec![num(1.0), num(2.0)]), // neither matches ">5"
                    Scalar(txt(">5")),
                ]
            ),
            num(0.0)
        );
    }

    // ---- scalar-literal error ARGUMENT propagates (Principle 2 fix) ----
    // A directly-written error as `sum_range` or a `criteria_range`
    // (`SUMIFS(#REF!, #REF!, "x", …)`, from deleted source references) must
    // propagate, not silently return 0 — the general error-propagation contract
    // (SUM.md / OXP-082). Mismatch-mine (docs/mismatch-decomposition.md) found
    // corpus cells of `SUMIFS(#REF!, #REF!, "ION", …)` returning `0`.

    #[test]
    fn scalar_literal_error_sum_range_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Ref)),
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(txt(">0")),
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    #[test]
    fn scalar_literal_error_criteria_range_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(Value::Error(ErrorKind::Ref)),
                    Scalar(txt(">0")),
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }
}

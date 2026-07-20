//! `AVERAGEIFS` — average the cells matching **all** of N criteria (logical
//! AND).
//!
//! # Provenance
//! No `docs/specs/AVERAGEIFS.md` exists in this pass. Per the task
//! specification this module is built by **mirroring `SUMIFS`/`COUNTIFS`
//! exactly** ([`crate::func_sumifs`], [`crate::func_countifs`]) for the
//! matching and range-shape-validation logic, swapping `SUMIFS`'s running sum
//! for a running `(sum, count)` pair (the same swap `AVERAGEIF` makes over
//! `SUMIF`; see [`crate::func_averageif`]). The criteria mini-language is
//! unchanged, owned by [`crate::criteria`] and reused **unchanged**. The
//! empty-match-set result (`#DIV/0!`) is **inferred** from
//! [`crate::func_average`]'s own documented empty-set rule, exactly as
//! `AVERAGEIF` infers it — not independently farm-pinned for `AVERAGEIFS`.
//!
//! # Signature and argument order — the AVERAGEIF/AVERAGEIFS asymmetry
//! `AVERAGEIFS(average_range, criteria_range1, criteria1, [criteria_range2,
//! criteria2], …)` — up to 127 criteria pairs. **`average_range` is the
//! FIRST argument**, exactly like `SUMIFS`'s `sum_range` — this is the
//! well-known Excel asymmetry with `AVERAGEIF`, whose `[average_range]` is
//! instead an **optional LAST** argument (mirroring `SUMIF`'s `[sum_range]`).
//! The registry enforces `min_args = 3`; an **even** argument count is a
//! dangling `criteria_range` with no `criteria`, a structurally invalid call
//! → `#VALUE!` (identical to `SUMIFS`'s arity check).
//!
//! # Semantics implemented (mirrors `SUMIFS`, spec bullets renamed)
//! - Averages each `average_range` cell whose aligned position satisfies
//!   **every** `criteria_rangeN`/`criteriaN` pair simultaneously — logical AND
//!   across pairs, no built-in OR (SUMIFS.md §1, read for `AVERAGEIFS`).
//! - `average_range` and every `criteria_rangeN` must be the **same shape**
//!   (identical `(rows, cols)`); any mismatch returns `#VALUE!` (SUMIFS.md
//!   §2, §Error behavior).
//! - Each `criteriaN` uses the identical mini-language as `SUMIF`/`SUMIFS`
//!   ([`crate::criteria`]). Each criterion is evaluated once in scalar
//!   context and compiled once; an error-valued criterion propagates and an
//!   oracle-deferred criterion returns `#UNSUPPORTED!` (both via
//!   [`Matcher::short_circuit`], checked in argument order — criteria-error
//!   precedence over the dimension check, mirroring `SUMIFS`/`COUNTIFS`).
//! - **Only numeric** `average_range` cells at a fully-matched position
//!   contribute to *both* the sum and the denominator; a text/blank/logical
//!   cell there is [`NumericArg::Skip`] — excluded from both (the `AVERAGE`/
//!   `AVERAGEIF` `RangeAggregate` rule, **not** `SUMIFS`'s "contributes 0").
//!   An error in an averaged cell at a matched position propagates.
//! - **Empty match set** (no position satisfies every criterion, or every
//!   matched position's `average_range` cell is non-numeric) → `#DIV/0!`,
//!   inferred from `AVERAGE`'s own empty-set rule (see the module docs above)
//!   — not independently farm-pinned for `AVERAGEIFS`.
//!
//! # Whole-column ranges — deferred (loud, never guessed), mirrors `SUMIFS`
//! Identical to `SUMIFS`: a whole-**column** range (`A:A`) refuses the dense
//! row walk ([`CallArgs::for_each_row`]), and the multi-range used-extent
//! alignment needed to serve it (which populated rows across N sparse ranges
//! correspond, and the exact `#VALUE!`-vs-support boundary for unbounded
//! shapes) is **unobserved** — the same open question `SUMIFS`/`COUNTIFS`
//! flag. Rather than guess, any argument that refuses the dense walk returns
//! `#UNSUPPORTED!`.
//!
//! ```text
//! // OXP (unassigned): multi-range whole-column alignment for AVERAGEIFS —
//! // identical open question to SUMIFS'/COUNTIFS' own unassigned OXP; see
//! // those modules' docs. Not re-litigated here, just mirrored.
//! ```
//!
//! # Recalc sentinels in a criteria-tested cell propagate — mirrors `SUMIFS`
//! Identical fix and rationale to [`crate::func_sumifs`]'s own "Recalc
//! sentinels" section: at each position the walk calls
//! [`criteria::sentinel_of`] on every criteria-tested cell *before*
//! [`criteria::matches`], in the existing per-criterion scan order, and
//! propagates the first sentinel found (kind preserved) out of the whole
//! call — so a criterion that already fails on a genuine mismatch **before**
//! reaching a sentinel still short-circuits the AND exactly as before (no
//! over-propagation). A sentinel in an *unmatched* `average_range` cell
//! stays ignored; a sentinel in a **matched** `average_range` cell already
//! propagates via `coerce_number_arg` (pre-existing, unaffected).

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::criteria::{self, Matcher};

/// Evaluate an `AVERAGEIFS(average_range, criteria_range1, criteria1, …)`
/// call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let count = args.count();
    // Arity: 1 `average_range` + N complete criteria pairs → odd and >= 3.
    // The registry already rejects < 3; an even count is a dangling
    // `criteria_range` with no `criteria` (structurally invalid) → #VALUE!.
    // Identical to SUMIFS's arity check.
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

    // Buffer `average_range` (arg 0) as a dense rectangle. A whole-column
    // range or an unresolvable range refuses the dense walk → defer loudly
    // (see module OXP).
    let avg_grid = match buffer_rows(args, 0) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    let avg_dims = dims_of(&avg_grid);

    // Buffer every `criteria_rangeN` and require it share `average_range`'s
    // shape.
    let mut crit_grids: Vec<Vec<Vec<Value>>> = Vec::with_capacity(num_pairs);
    for k in 0..num_pairs {
        let cr_index = 1 + 2 * k;
        let grid = match buffer_rows(args, cr_index) {
            Ok(g) => g,
            Err(k) => return Value::Error(k),
        };
        // Mismatched shape across ranges → #VALUE! (SUMIFS.md §2).
        if dims_of(&grid) != avg_dims {
            return Value::Error(ErrorKind::Value);
        }
        crit_grids.push(grid);
    }

    // Lockstep AND walk: include `average_range`'s cell iff every criterion
    // matches its aligned criteria-range cell.
    let (rows, cols) = avg_dims;
    let mut sum = 0.0_f64;
    let mut count_n = 0u64;
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
            match coerce_number_arg(cell_at(&avg_grid, r, c), CoercionMode::RangeAggregate) {
                NumericArg::Number(n) => {
                    sum += n;
                    count_n += 1;
                }
                // Non-numeric averaged cells are excluded from both sum and
                // denominator (the AVERAGE/AVERAGEIF RangeAggregate rule).
                // OXP-190 (RUN-2026-07-13) CONFIRMS this against Excel and
                // against AVERAGEIF: with average_range {10,"x",20} all matched,
                // AVERAGEIFS = AVERAGEIF = 15 (the text cell skipped, NOT the
                // literal-doc-reading #DIV/0!); an empty match set → #DIV/0!.
                NumericArg::Skip => {}
                // An error in an averaged cell at a matched position
                // propagates.
                NumericArg::Error(k) => return Value::Error(k),
            }
        }
    }
    if count_n == 0 {
        // Inferred from func_average's empty-set rule (AVERAGE.md §4), not
        // independently farm-pinned for AVERAGEIFS — see module docs.
        return Value::Error(ErrorKind::Div0);
    }
    Value::number(sum / count_n as f64)
}

/// Buffer an argument's rectangle row-by-row into an owned grid via the
/// **dense** [`CallArgs::for_each_row`] walk. An unbounded whole-column/row
/// range (or an unresolvable range) surfaces as `Err(ErrorKind::Unsupported)`,
/// which the caller turns into a loud `#UNSUPPORTED!` deferral (module OXP).
/// Identical to `func_sumifs::buffer_rows`.
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

/// The cell at position `(r, c)` of a buffered grid, or [`Value::Blank`] when
/// the position is absent (a short row). Identical to `func_sumifs::cell_at`.
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

    // A single criteria pair must behave exactly like `AVERAGEIF(range, crit,
    // average_range)`, but with `average_range` FIRST (the arg-order quirk):
    // avg=[10,20,30,40], cr=[1,2,3,4] ">2" → mean(30,40) = 35.
    #[test]
    fn single_criterion_matches_averageif_with_average_range_first() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0), num(20.0), num(30.0), num(40.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0), num(4.0)]),
                    Scalar(txt(">2")),
                ]
            ),
            num(35.0)
        );
    }

    // Two criteria are ANDed at each aligned position. avg=[10,20,30,40];
    // cr1=[1,2,3,4] ">1" (idx 1,2,3); cr2=["a","b","a","b"] "a" (idx 0,2).
    // AND → only idx 2 → mean(30) = 30.
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
    // avg=[10,20,30] → mean(10,30) = 20.
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
            num(20.0)
        );
    }

    // Non-numeric averaged cells at a matched position are excluded from
    // both sum and denominator (Skip, not zero) — the AVERAGE-vs-SUM
    // contrast: avg=[10,"x",30], all criteria_range match ">0" → mean(10,30)
    // = 20, not mean(10,0,30) = 13.33.
    #[test]
    fn non_numeric_averaged_cell_excluded_from_denominator() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0), txt("x"), num(30.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(txt(">0")),
                ]
            ),
            num(20.0)
        );
    }

    // A criteria range whose shape differs from `average_range` → #VALUE!.
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

    // No position satisfies the criteria → empty match set → #DIV/0! (not 0,
    // the SUMIFS/COUNTIFS-family contrast — mirrors AVERAGE's own rule).
    #[test]
    fn no_matches_is_div0() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(txt(">100")),
                ]
            ),
            Value::Error(ErrorKind::Div0)
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

    // A criteria-engine short-circuit propagates → #UNSUPPORTED! (mirrors
    // SUMIFS/COUNTIFS: a still-deferred non-ASCII text ordering operand,
    // OXP-031).
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

    // An error in an averaged cell at a matched position propagates.
    #[test]
    fn error_in_averaged_cell_propagates() {
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

    // Criteria error takes precedence over a dimension mismatch (criteria
    // compiled and short-circuited before any range is buffered — mirrors
    // SUMIFS/COUNTIFS argument-order precedence).
    #[test]
    fn criteria_error_takes_precedence_over_dimension_mismatch() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(txt(">0")),
                    Range(vec![num(1.0), num(2.0), num(3.0)]), // 3 rows vs 2
                    Scalar(Value::Error(ErrorKind::Div0)),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    // Two-dimensional (rect) ranges align positionally across the whole
    // rectangle. avg=[[10,20],[30,40]], cr=[[1,2],[3,4]] ">2" → mean(30,40) =
    // 35.
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
            num(35.0)
        );
    }

    // A whole-column (unbounded) range refuses the dense walk; multi-range
    // alignment is unobserved, so AVERAGEIFS defers loudly (module OXP,
    // mirrors SUMIFS).
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

    // Too few args (< 3) → #VALUE! (registry min_args also enforces this, but
    // the internal guard is exercised directly here).
    #[test]
    fn too_few_args_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(1.0)]), Range(vec![num(1.0)])]),
            Value::Error(ErrorKind::Value)
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
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0), num(20.0), num(30.0)]),
                    Range(vec![num(1.0), Value::Error(ErrorKind::Div0), num(3.0)]),
                    Scalar(txt(">0")),
                ]
            ),
            num(20.0) // mean(10, 30); the #DIV/0! position excluded
        );
    }

    #[test]
    fn sentinel_after_a_definite_mismatch_does_not_over_propagate() {
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
            Value::Error(ErrorKind::Div0) // empty match set
        );
    }
}

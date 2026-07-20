//! `AVERAGEIF` — average the cells matching a single criterion.
//!
//! # Provenance
//! No `docs/specs/AVERAGEIF.md` exists in this pass. Per the task
//! specification this module is built by **mirroring `SUMIF` exactly**
//! ([`crate::func_sumif`]) — same range/criteria walk, same top-left-anchor
//! `[average_range]` geometry, same deferrals — swapping the accumulator from
//! a running sum to a running `(sum, count)` pair. The criteria mini-language
//! is unchanged, owned by [`crate::criteria`] and pinned by OXP-101/102/160/
//! 162/031 (see `docs/specs/SUMIF.md`/`COUNTIF.md`). The empty-match-set
//! result (`#DIV/0!`) is **inferred** from [`crate::func_average`]'s own
//! documented empty-set rule, not independently farm-pinned for `AVERAGEIF` —
//! flagged at the point it is returned, below.
//!
//! # Semantics implemented (mirrors `SUMIF`, spec bullets renamed)
//! - `AVERAGEIF(range, criteria, [average_range])`. For each `range` cell
//!   matching `criteria`, the **corresponding** `average_range` cell joins the
//!   running sum/count; when `average_range` is omitted, `range` is both the
//!   tested and averaged range (SUMIF.md §Signature, §1, read for `AVERAGEIF`).
//! - `criteria` is evaluated once in scalar context and compiled once
//!   (SUMIF.md §Coercion); an error criteria propagates and an
//!   oracle-deferred criterion returns `#UNSUPPORTED!` (via
//!   [`Matcher::short_circuit`]).
//! - **Only numeric** `average_range` cells contribute to *both* the sum and
//!   the denominator; text/blank/logical cells that pass the test are
//!   [`NumericArg::Skip`] — excluded from both, exactly `AVERAGE`'s
//!   `RangeAggregate` rule (not "contributes 0" — that is `SUM`'s rule, which
//!   `SUMIF` reuses for its own sum but would be wrong for an average
//!   denominator). An error in an averaged cell propagates as the result.
//! - `range` is walked positionally with [`CallArgs::for_each_row`] so blanks
//!   are surfaced (the `criteria=""` rule; SUMIF.md §Criteria). Error cells in
//!   `range` used only for matching are excluded, never propagated.
//! - **Empty match set** (no cell matches, or every matched cell is
//!   non-numeric) → `#DIV/0!`. This mirrors [`crate::func_average`]'s own
//!   "nothing numeric found anywhere → `#DIV/0!`" rule (AVERAGE.md §4); it is
//!   the natural reading (a matched-and-averaged set is just an `AVERAGE` over
//!   a filtered range) but has **not** been independently run on the Excel
//!   farm for `AVERAGEIF` specifically — see the `eval` functions' comments.
//!
//! # `average_range` resizing (top-left anchor) — mirrors `SUMIF` exactly
//! Reuses `SUMIF`'s documented anchor rule and its **exact same deferral**
//! (SUMIF.md §2, **OXP-103**): `average_range` **the same shape as, or larger
//! than, `range`** uses the top-left `range`-shaped sub-rectangle (fully
//! supported); `average_range` **smaller than `range`** in either dimension
//! would read sheet cells *outside* the argument, which `CallArgs` cannot
//! provide, so this returns `#UNSUPPORTED!` — never a guess. `SUMIF` currently
//! defers this same edge pending a cross-crate `CallArgs` RFC that is not yet
//! ratified; `AVERAGEIF` mirrors that unresolved state identically rather than
//! attempt an independent (and un-pinned) resolution.
//!
//! # Whole-column ranges (`A:A`) — used-extent iteration (RFC 0001)
//! Identical to `SUMIF`: a whole-**column** `range` (and `average_range`) is
//! walked over its **populated** rows via [`for_each_row_or_used`]. Two
//! corners defer loudly, exactly mirroring `SUMIF`'s OXP-103/OXP-104:
//! - **`average_range` shorter/narrower than a whole-column `range`**
//!   (OXP-103): the resize would read cells outside the `average_range`
//!   argument.
//! - **A blank-matching criterion over a whole-column `range` with an
//!   `average_range`** (OXP-104): the absent (blank) rows match but their
//!   `average_range` cells cannot be enumerated over an unbounded column.
//!
//! Unlike `SUMIF`, whose `sum_range`-omitted self-sum needs **no**
//! blank-matching guard (a matched blank cell contributes `0` to a sum either
//! way), `AVERAGEIF`'s self-range form *also* needs none — for the opposite
//! reason: a matched blank cell is [`NumericArg::Skip`] (excluded from **both**
//! sum and denominator) whether or not the used-extent walk actually visits
//! it, so an unvisited absent row and a visited-but-skipped blank row have the
//! *same* zero effect on the running `(sum, count)`. This is a real semantic
//! difference from `SUMIF`'s self-sum reasoning (SUMIF's blank contributes a
//! summed `0`; here it contributes nothing to the denominator either), spelled
//! out because it is easy to assume the guard should also transfer.
//!
//! # Whole-row ranges (`1:1`) — used-extent COLUMN iteration (RFC 0008)
//! Identical transpose to `SUMIF`: a whole-**row** `range`/`average_range` is
//! walked over its **populated columns**. The self-range form is
//! order-independent ([`for_each_row_or_used_any_axis`]); the `average_range`
//! lockstep ([`average_lockstep_used_cols`]) transposes the OXP-103/104 quadrants
//! keyed by relative column — bounded `range` × whole-row `average_range` and
//! whole-row × whole-row are supported, whole-row `range` × bounded
//! `average_range` (and shorter/blank corners, and mixed-axis pairings) defer.
//! Bounded ranges and array constants keep `SUMIF`'s exact prior behavior.
//!
//! # Recalc sentinels in the criteria-tested `range` cell propagate — mirrors `SUMIF`
//! Identical fix and rationale to [`crate::func_sumif`]'s own "Recalc
//! sentinels" section: every walk here calls [`criteria::sentinel_of`] on the
//! criteria-tested `range` cell *before* [`criteria::matches`] and propagates
//! the first sentinel found (kind preserved), in the walk's existing scan
//! order. Applies only to the criteria-tested cell; a sentinel in an
//! *unmatched* `average_range` cell stays ignored, and a sentinel in a
//! **matched** `average_range` cell already propagates via
//! `coerce_number_arg` (pre-existing, unaffected by this fix).

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{ArgShape, CallArgs, for_each_row_or_used_any_axis};
use crate::context::EvalContext;
use crate::criteria::{self, Matcher};

/// Evaluate an `AVERAGEIF(range, criteria, [average_range])` call. See the
/// module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // criteria (arg 1): evaluated once in scalar context, then compiled.
    let matcher = criteria::parse(&args.eval_scalar(1));
    if let Some(short) = matcher.short_circuit() {
        return short;
    }

    let has_average_range = args.count() >= 3 && args.shape(2) != ArgShape::Omitted;
    if has_average_range {
        eval_with_average_range(args, &matcher)
    } else {
        eval_self_range(args, &matcher)
    }
}

/// `average_range` omitted: average the matching `range` cells themselves.
/// Mirrors [`crate::func_sumif::eval_self_range`], tracking `(sum, count)`
/// instead of a bare sum.
fn eval_self_range(args: &mut dyn CallArgs, matcher: &Matcher) -> Value {
    let mut sum = 0.0_f64;
    let mut count = 0u64;
    let mut err: Option<ErrorKind> = None;
    let walk = for_each_row_or_used_any_axis(args, 0, &mut |_rel, row| {
        for cell in row {
            // A Recalc sentinel in the criteria-tested cell propagates
            // (kind preserved) instead of being silently excluded as "no
            // match" — see `criteria::sentinel_of`'s docs.
            if let Some(k) = criteria::refuse_cell(matcher, cell) {
                err = Some(k);
                return ControlFlow::Break(());
            }
            if criteria::matches(matcher, cell) {
                match coerce_number_arg(cell, CoercionMode::RangeAggregate) {
                    NumericArg::Number(n) => {
                        sum += n;
                        count += 1;
                    }
                    // Non-numeric matches are excluded from both sum and
                    // denominator (AVERAGE.md §2/§3's RangeAggregate rule).
                    NumericArg::Skip => {}
                    NumericArg::Error(k) => {
                        err = Some(k);
                        return ControlFlow::Break(());
                    }
                }
            }
        }
        ControlFlow::Continue(())
    });
    if let Err(k) = walk {
        return Value::Error(k);
    }
    if let Some(k) = err {
        return Value::Error(k);
    }
    if count == 0 {
        // Inferred from func_average's empty-set rule (AVERAGE.md §4), not
        // independently farm-pinned for AVERAGEIF — see module docs.
        return Value::Error(ErrorKind::Div0);
    }
    Value::number(sum / count as f64)
}

/// `average_range` present: lockstep positional walk of `range` and
/// `average_range` with the top-left-anchor resize. Mirrors
/// [`crate::func_sumif::eval_with_sum_range`] exactly, tracking `(sum, count)`.
fn eval_with_average_range(args: &mut dyn CallArgs, matcher: &Matcher) -> Value {
    let range_dense = buffer_rows(args, 0);
    let avg_dense = buffer_rows(args, 2);
    if let (Ok(range_rows), Ok(avg_rows)) = (&range_dense, &avg_dense) {
        // Both bounded → the dense lockstep, mirroring SUMIF byte-for-byte
        // (modulo the sum/count accumulator).
        return average_lockstep_dense(range_rows, avg_rows, matcher);
    }
    // At least one argument is unbounded. Pick the used-extent orientation, exactly
    // as SUMIF does: a whole-**row** argument routes to the relative-COLUMN
    // lockstep (RFC 0008); otherwise (whole-column) the relative-ROW one (RFC
    // 0001, unchanged). See RFC 0008 §2 orientation contract.
    if arg_is_whole_row(args, 0, &range_dense) || arg_is_whole_row(args, 2, &avg_dense) {
        average_lockstep_used_cols(args, range_dense, avg_dense, matcher)
    } else {
        average_lockstep_used_extent(args, range_dense, avg_dense, matcher)
    }
}

/// Whether argument `index` is a whole-**row** range — mirror of
/// [`crate::func_sumif`]'s `arg_is_whole_row`. Only meaningful once the dense
/// walk has refused; probes the used-**row** walk, which refuses a whole-row
/// range up front (visiting nothing) while serving a whole-column one.
fn arg_is_whole_row(
    args: &mut dyn CallArgs,
    index: usize,
    dense: &Result<Vec<Vec<Value>>, ErrorKind>,
) -> bool {
    dense.is_err()
        && args
            .for_each_used_row(index, &mut |_, _| ControlFlow::Break(()))
            .is_err()
}

/// The bounded lockstep: `range`'s dimensions govern the walk, the top-left
/// `r_rows × r_cols` sub-rectangle of `average_range` supplies the averaged
/// cells, and an `average_range` smaller in either dimension defers
/// (OXP-103, mirroring `SUMIF`).
fn average_lockstep_dense(
    range_rows: &[Vec<Value>],
    avg_rows: &[Vec<Value>],
    matcher: &Matcher,
) -> Value {
    let r_rows = range_rows.len();
    let r_cols = range_rows.iter().map(Vec::len).max().unwrap_or(0);
    let a_rows = avg_rows.len();
    let a_cols = avg_rows.iter().map(Vec::len).max().unwrap_or(0);
    if a_rows < r_rows || a_cols < r_cols {
        return Value::Error(ErrorKind::Unsupported);
    }

    let mut sum = 0.0_f64;
    let mut count = 0u64;
    for (r, range_row) in range_rows.iter().enumerate() {
        for c in 0..r_cols {
            let range_cell = range_row.get(c).unwrap_or(&Value::Blank);
            // A Recalc sentinel in the criteria-tested cell propagates
            // (kind preserved) instead of being silently excluded.
            if let Some(k) = criteria::refuse_cell(matcher, range_cell) {
                return Value::Error(k);
            }
            if !criteria::matches(matcher, range_cell) {
                continue;
            }
            // The positionally-corresponding averaged cell (guaranteed
            // in-bounds by the size check above).
            let avg_cell = avg_rows
                .get(r)
                .and_then(|row| row.get(c))
                .unwrap_or(&Value::Blank);
            match coerce_number_arg(avg_cell, CoercionMode::RangeAggregate) {
                NumericArg::Number(n) => {
                    sum += n;
                    count += 1;
                }
                // Non-numeric averaged cells are excluded from both sum and
                // denominator.
                NumericArg::Skip => {}
                // An error in an averaged cell propagates as the result.
                NumericArg::Error(k) => return Value::Error(k),
            }
        }
    }
    if count == 0 {
        return Value::Error(ErrorKind::Div0);
    }
    Value::number(sum / count as f64)
}

/// The used-extent lockstep (at least one of `range`/`average_range` is a
/// whole-column range). Mirrors
/// [`crate::func_sumif::sum_lockstep_used_extent`] exactly — same OXP-103/
/// OXP-104 deferrals, applied identically here (see module docs for why the
/// blank-matching guard is *also* required for `AVERAGEIF`, unlike `SUMIF`'s
/// self-sum exemption).
fn average_lockstep_used_extent(
    args: &mut dyn CallArgs,
    range_dense: Result<Vec<Vec<Value>>, ErrorKind>,
    avg_dense: Result<Vec<Vec<Value>>, ErrorKind>,
    matcher: &Matcher,
) -> Value {
    let range_unbounded = range_dense.is_err();
    let avg_unbounded = avg_dense.is_err();

    let range_map = match indexed_rows(args, 0, range_dense) {
        Ok(m) => m,
        Err(k) => return Value::Error(k),
    };
    // OXP-103: a full-height `range` against a finite `average_range`
    // (bounded, so shorter) reads averaged cells outside the argument →
    // defer.
    if range_unbounded && !avg_unbounded {
        return Value::Error(ErrorKind::Unsupported);
    }
    let avg_map = match indexed_rows(args, 2, avg_dense) {
        Ok(m) => m,
        Err(k) => return Value::Error(k),
    };

    let r_cols = range_map.values().map(Vec::len).max().unwrap_or(0);
    let a_cols = avg_map.values().map(Vec::len).max().unwrap_or(0);
    // OXP-103: `average_range` narrower than `range` → outside-argument read.
    if a_cols < r_cols {
        return Value::Error(ErrorKind::Unsupported);
    }
    // OXP-104: a blank-matching criterion over a whole-column range would
    // match unbounded absent rows whose averaged cells we cannot enumerate.
    if range_unbounded && criteria::matches(matcher, &Value::Blank) {
        return Value::Error(ErrorKind::Unsupported);
    }

    let mut sum = 0.0_f64;
    let mut count = 0u64;
    for (rel, range_row) in &range_map {
        for c in 0..r_cols {
            let range_cell = range_row.get(c).unwrap_or(&Value::Blank);
            // A Recalc sentinel in the criteria-tested cell propagates
            // (kind preserved) instead of being silently excluded.
            if let Some(k) = criteria::refuse_cell(matcher, range_cell) {
                return Value::Error(k);
            }
            if !criteria::matches(matcher, range_cell) {
                continue;
            }
            let avg_cell = avg_map
                .get(rel)
                .and_then(|row| row.get(c))
                .unwrap_or(&Value::Blank);
            match coerce_number_arg(avg_cell, CoercionMode::RangeAggregate) {
                NumericArg::Number(n) => {
                    sum += n;
                    count += 1;
                }
                NumericArg::Skip => {}
                NumericArg::Error(k) => return Value::Error(k),
            }
        }
    }
    if count == 0 {
        return Value::Error(ErrorKind::Div0);
    }
    Value::number(sum / count as f64)
}

/// The COLUMN-oriented used-extent lockstep (at least one of `range`/
/// `average_range` is a whole-**row** range) — the transpose of
/// [`average_lockstep_used_extent`], mirroring
/// [`crate::func_sumif::sum_lockstep_used_cols`] exactly (modulo the sum/count
/// accumulator). Correspondence is by **relative column**; the transposed
/// OXP-103/104 deferrals apply identically (whole-row `range` × bounded
/// `average_range` → defer; `average_range` shorter than `range` → defer;
/// blank-matching criterion over a whole-row `range` → defer; a mixed-axis
/// pairing surfaces `Err` from [`indexed_cols`] and defers).
fn average_lockstep_used_cols(
    args: &mut dyn CallArgs,
    range_dense: Result<Vec<Vec<Value>>, ErrorKind>,
    avg_dense: Result<Vec<Vec<Value>>, ErrorKind>,
    matcher: &Matcher,
) -> Value {
    let range_unbounded = range_dense.is_err();
    let avg_unbounded = avg_dense.is_err();

    let range_map = match indexed_cols(args, 0, range_dense) {
        Ok(m) => m,
        Err(k) => return Value::Error(k),
    };
    // OXP-103 (transposed): full-width `range` against a finite `average_range`.
    if range_unbounded && !avg_unbounded {
        return Value::Error(ErrorKind::Unsupported);
    }
    let avg_map = match indexed_cols(args, 2, avg_dense) {
        Ok(m) => m,
        Err(k) => return Value::Error(k),
    };

    let r_rows = range_map.values().map(Vec::len).max().unwrap_or(0);
    let a_rows = avg_map.values().map(Vec::len).max().unwrap_or(0);
    // OXP-103 (transposed): `average_range` shorter (fewer rows) than `range`.
    if a_rows < r_rows {
        return Value::Error(ErrorKind::Unsupported);
    }
    // OXP-104 (transposed): a blank-matching criterion over a whole-row range.
    if range_unbounded && criteria::matches(matcher, &Value::Blank) {
        return Value::Error(ErrorKind::Unsupported);
    }

    let mut sum = 0.0_f64;
    let mut count = 0u64;
    for (rel, range_col) in &range_map {
        for r in 0..r_rows {
            let range_cell = range_col.get(r).unwrap_or(&Value::Blank);
            if let Some(k) = criteria::refuse_cell(matcher, range_cell) {
                return Value::Error(k);
            }
            if !criteria::matches(matcher, range_cell) {
                continue;
            }
            let avg_cell = avg_map
                .get(rel)
                .and_then(|col| col.get(r))
                .unwrap_or(&Value::Blank);
            match coerce_number_arg(avg_cell, CoercionMode::RangeAggregate) {
                NumericArg::Number(n) => {
                    sum += n;
                    count += 1;
                }
                NumericArg::Skip => {}
                NumericArg::Error(k) => return Value::Error(k),
            }
        }
    }
    if count == 0 {
        return Value::Error(ErrorKind::Div0);
    }
    Value::number(sum / count as f64)
}

/// Buffer an argument's rectangle row-by-row into an owned grid using the
/// **dense** [`CallArgs::for_each_row`] walk only, so an unbounded
/// whole-column range surfaces as `Err(Unsupported)` (the caller then takes
/// the used-extent path). Identical to `func_sumif::buffer_rows`.
fn buffer_rows(args: &mut dyn CallArgs, index: usize) -> Result<Vec<Vec<Value>>, ErrorKind> {
    let mut rows: Vec<Vec<Value>> = Vec::new();
    args.for_each_row(index, &mut |row| {
        rows.push(row.to_vec());
        ControlFlow::Continue(())
    })?;
    Ok(rows)
}

/// Key an argument into a `rel → cells` map. A pre-buffered dense grid keys by
/// its 0-based dense row index; an argument whose dense walk refused (a
/// whole-column range) is (re)walked via the used-extent iterator, which
/// yields the relative row index directly. Identical to
/// `func_sumif::indexed_rows`.
fn indexed_rows(
    args: &mut dyn CallArgs,
    index: usize,
    dense: Result<Vec<Vec<Value>>, ErrorKind>,
) -> Result<BTreeMap<u32, Vec<Value>>, ErrorKind> {
    match dense {
        Ok(rows) => Ok(rows
            .into_iter()
            .enumerate()
            .map(|(i, r)| (i as u32, r))
            .collect()),
        Err(_) => {
            let mut map: BTreeMap<u32, Vec<Value>> = BTreeMap::new();
            args.for_each_used_row(index, &mut |rel, row| {
                map.insert(rel, row.to_vec());
                ControlFlow::Continue(())
            })?;
            Ok(map)
        }
    }
}

/// Key an argument into a `rel_col → column` map (column-oriented) — the
/// transpose of [`indexed_rows`], identical to `func_sumif::indexed_cols`. A
/// dense (bounded) grid is transposed; a whole-**row** argument is walked via
/// [`CallArgs::for_each_used_col`]; a whole-**column** argument refuses that walk
/// and surfaces `Err` so the caller defers.
fn indexed_cols(
    args: &mut dyn CallArgs,
    index: usize,
    dense: Result<Vec<Vec<Value>>, ErrorKind>,
) -> Result<BTreeMap<u32, Vec<Value>>, ErrorKind> {
    match dense {
        Ok(rows) => {
            let width = rows.iter().map(Vec::len).max().unwrap_or(0);
            let mut map: BTreeMap<u32, Vec<Value>> = BTreeMap::new();
            for c in 0..width {
                let col: Vec<Value> = rows
                    .iter()
                    .map(|r| r.get(c).cloned().unwrap_or(Value::Blank))
                    .collect();
                map.insert(c as u32, col);
            }
            Ok(map)
        }
        Err(_) => {
            let mut map: BTreeMap<u32, Vec<Value>> = BTreeMap::new();
            args.for_each_used_col(index, &mut |rel, col| {
                map.insert(rel, col.to_vec());
                ControlFlow::Continue(())
            })?;
            Ok(map)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // Self-range form: average=[10,20,30,40], criteria ">2" over the same
    // range → matches 3,4 → mean(30,40) = 35.
    #[test]
    fn self_range_averages_matching_cells() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0), num(4.0)]),
                    Scalar(txt(">2")),
                ]
            ),
            num(3.5)
        );
    }

    // 3-arg form mirrors SUMIF's positional lockstep: range=[1,2,3,4] ">2"
    // (idx 2,3), average_range=[10,20,30,40] → mean(30,40) = 35.
    #[test]
    fn average_range_lockstep_matches_sumif_shape() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0), num(4.0)]),
                    Scalar(txt(">2")),
                    Range(vec![num(10.0), num(20.0), num(30.0), num(40.0)]),
                ]
            ),
            num(35.0)
        );
    }

    // Numeric equality criteria value. range=[2,5,2], average=[10,20,30] →
    // idx 0,2 match → mean(10,30) = 20.
    #[test]
    fn numeric_equality_criteria_value() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(2.0), num(5.0), num(2.0)]),
                    Scalar(num(2.0)),
                    Range(vec![num(10.0), num(20.0), num(30.0)]),
                ]
            ),
            num(20.0)
        );
    }

    // Text criteria is case-insensitive equality, wildcard supported.
    #[test]
    fn wildcard_text_criteria() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![txt("apple"), txt("apricot"), txt("banana")]),
                    Scalar(txt("a*")),
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                ]
            ),
            num(1.5) // mean(1,2)
        );
    }

    // Dynamic criteria (`">"&A1`) arrives pre-concatenated as a plain text
    // value — the module never sees the formula, only the final ">2".
    #[test]
    fn dynamic_criteria_pre_concatenated() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(txt(">2")),
                    Range(vec![num(10.0), num(20.0), num(30.0)]),
                ]
            ),
            num(30.0)
        );
    }

    // Blank-matching criteria (`""`): a blank `range` cell counts, and a
    // non-numeric matched `average_range` cell is excluded from the
    // denominator (Skip, not zero) — the AVERAGE-vs-SUM contrast.
    #[test]
    fn blank_criteria_matches_blank_range_cell() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![Value::Blank, num(1.0)]),
                    Scalar(txt("")),
                    Range(vec![num(5.0), num(100.0)]),
                ]
            ),
            num(5.0)
        );
    }

    // Non-numeric matched average_range cells are excluded from *both* sum
    // and denominator (Skip) — not summed as 0 the way SUMIF would. range
    // all match ">0"; average=[10, "x", 20] → mean(10,20) = 15, not
    // mean(10,0,20)=10.
    #[test]
    fn non_numeric_averaged_cell_excluded_from_denominator() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(txt(">0")),
                    Range(vec![num(10.0), txt("x"), num(20.0)]),
                ]
            ),
            num(15.0)
        );
    }

    // Empty match set → #DIV/0! (inferred from AVERAGE's own empty-set rule;
    // see module docs — not independently farm-pinned for AVERAGEIF).
    #[test]
    fn empty_match_set_is_div0() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(txt(">100")),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(txt(">0")),
                    Range(vec![txt("a"), txt("b")]), // matched but non-numeric
                ]
            ),
            Value::Error(ErrorKind::Div0)
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
                    Scalar(Value::Error(ErrorKind::Na)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // A criteria-engine short-circuit propagates → #UNSUPPORTED! (mirrors
    // SUMIF/SUMIFS: a still-deferred non-ASCII text ordering operand, OXP-031).
    #[test]
    fn deferred_criteria_is_unsupported() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), num(2.0)]), Scalar(txt(">ä"))]
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
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(txt(">0")),
                    Range(vec![num(1.0), Value::Error(ErrorKind::Div0), num(3.0)]),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    // `average_range` smaller than `range` → #UNSUPPORTED! (OXP-103, mirrors
    // SUMIF's deferred sum_range-resize edge exactly).
    #[test]
    fn average_range_smaller_than_range_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(txt(">0")),
                    Range(vec![num(10.0), num(20.0)]), // 2 rows vs 3
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // A whole-column `range` against a bounded `average_range` defers
    // (OXP-103, used-extent path).
    #[test]
    fn whole_column_range_against_bounded_average_range_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Unbounded(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(txt(">0")),
                    Range(vec![num(10.0), num(20.0), num(30.0)]),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // A blank-matching criterion over a whole-column `range` with an
    // `average_range` defers (OXP-104, mirrors SUMIF).
    #[test]
    fn blank_matching_criterion_over_whole_column_with_average_range_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    UsedRows(vec![(0, vec![num(1.0)]), (2, vec![num(3.0)])]),
                    Scalar(txt("")),
                    UsedRows(vec![(0, vec![num(10.0)]), (2, vec![num(30.0)])]),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // Whole-column `range` against an equally-whole-column `average_range`
    // is supported (top-left-anchor correspondence by relative row) — mirrors
    // SUMIF's supported whole-column/whole-column pairing.
    #[test]
    fn whole_column_range_and_average_range_supported() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    UsedRows(vec![(0, vec![num(1.0)]), (1, vec![num(5.0)])]),
                    Scalar(txt(">2")),
                    UsedRows(vec![(0, vec![num(100.0)]), (1, vec![num(200.0)])]),
                ]
            ),
            num(200.0) // only relative row 1 (value 5) matches ">2"
        );
    }

    // ---- Whole-row ranges (RFC 0008) ----------------------------------

    /// AVERAGEIF self-range over a whole-ROW range averages matching columns.
    /// `AVERAGEIF(1:1, ">2")` over [1,6,3] → mean(6,3) = 4.5.
    #[test]
    fn self_range_whole_row() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    UsedCols(vec![
                        (0, vec![num(1.0)]),
                        (1, vec![num(6.0)]),
                        (2, vec![num(3.0)]),
                    ]),
                    Scalar(txt(">2")),
                ]
            ),
            num(4.5)
        );
    }

    /// Mixed-orientation quadrant (RFC 0008 §5(c)): a bounded 1×3 `range`
    /// lock-stepped against a whole-ROW `average_range`, keyed by relative
    /// column. ">2" matches cols 1,2; average_range [10,20,30] → mean(20,30) = 25.
    #[test]
    fn bounded_range_whole_row_average_range_supported() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Array(vec![num(1.0), num(6.0), num(3.0)]),
                    Scalar(txt(">2")),
                    UsedCols(vec![
                        (0, vec![num(10.0)]),
                        (1, vec![num(20.0)]),
                        (2, vec![num(30.0)]),
                    ]),
                ]
            ),
            num(25.0)
        );
    }

    /// whole-ROW `range` × whole-ROW `average_range` → supported. Only rel col 1
    /// (value 6) matches ">2" → average of {200} = 200.
    #[test]
    fn whole_row_range_and_average_range_supported() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    UsedCols(vec![(0, vec![num(1.0)]), (1, vec![num(6.0)])]),
                    Scalar(txt(">2")),
                    UsedCols(vec![(0, vec![num(100.0)]), (1, vec![num(200.0)])]),
                ]
            ),
            num(200.0)
        );
    }

    /// whole-ROW `range` against a bounded `average_range` → defer (transposed
    /// OXP-103).
    #[test]
    fn whole_row_range_bounded_average_range_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    UsedCols(vec![(0, vec![num(1.0)]), (1, vec![num(6.0)])]),
                    Scalar(txt(">0")),
                    Array(vec![num(10.0), num(20.0)]),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// A blank-matching criterion over a whole-ROW `range` with an
    /// `average_range` → defer (transposed OXP-104).
    #[test]
    fn blank_matching_whole_row_with_average_range_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    UsedCols(vec![(0, vec![num(1.0)]), (2, vec![num(3.0)])]),
                    Scalar(txt("")),
                    UsedCols(vec![(0, vec![num(10.0)]), (2, vec![num(30.0)])]),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // ---- Recalc sentinels propagate (Principle 2 fix) -----------------

    #[test]
    fn self_range_sentinel_in_tested_cell_propagates_kind_preserved() {
        for k in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                eval_direct(
                    eval,
                    vec![
                        Range(vec![num(10.0), Value::Error(k), num(30.0)]),
                        Scalar(txt(">0")),
                    ]
                ),
                Value::Error(k),
                "{k:?} should propagate"
            );
        }
    }

    #[test]
    fn with_average_range_sentinel_in_tested_cell_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![
                        num(1.0),
                        Value::Error(ErrorKind::Unsupported),
                        num(3.0)
                    ]),
                    Scalar(txt(">0")),
                    Range(vec![num(10.0), num(20.0), num(30.0)]),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn genuine_error_in_tested_cell_still_excluded_unchanged() {
        // Control: a genuine error in the criteria-tested cell keeps the
        // exact prior "excluded, not propagated" behavior.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0), Value::Error(ErrorKind::Div0), num(30.0)]),
                    Scalar(txt(">0")),
                ]
            ),
            num(20.0) // mean(10, 30)
        );
    }

    #[test]
    fn sentinel_in_unmatched_average_range_cell_stays_ignored() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]), // neither matches ">5"
                    Scalar(txt(">5")),
                    Range(vec![Value::Error(ErrorKind::Unsupported), num(20.0)]),
                ]
            ),
            Value::Error(ErrorKind::Div0) // empty match set
        );
    }

    #[test]
    fn sentinel_in_matched_average_range_cell_still_propagates_preexisting() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0)]),
                    Scalar(txt(">0")),
                    Range(vec![Value::Error(ErrorKind::Blocked)]),
                ]
            ),
            Value::Error(ErrorKind::Blocked)
        );
    }
}

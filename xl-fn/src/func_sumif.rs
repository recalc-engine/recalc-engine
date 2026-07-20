//! `SUMIF` — sum the cells matching a single criterion.
//!
//! # Provenance
//! Behavior contract: `docs/specs/SUMIF.md` (which cites the Microsoft Learn
//! `SUMIF` page, verified 2026-07-05). The criteria mini-language is owned by
//! [`crate::criteria`] (shared with `COUNTIF`); the summation mirrors `SUM`'s
//! range aggregation ([`coerce_number_arg`] under [`CoercionMode::RangeAggregate`]).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `SUMIF(range, criteria, [sum_range])`. For each `range` cell matching
//!   `criteria`, the **corresponding** `sum_range` cell is added; when
//!   `sum_range` is omitted, `range` is both the tested and summed range
//!   (SUMIF.md §Signature, §1).
//! - `criteria` is evaluated once in scalar context and compiled once
//!   (SUMIF.md §Coercion); an error criteria propagates and an oracle-deferred
//!   criterion returns `#UNSUPPORTED!` (via [`Matcher::short_circuit`]).
//! - **Only numeric** `sum_range` cells contribute; text/blank/logical cells
//!   that pass the test contribute 0 (SUMIF.md §Coercion) — exactly `SUM`'s
//!   `RangeAggregate` rule. An error in a summed cell propagates as the result
//!   (SUMIF.md §Error behavior).
//! - `range` is walked positionally with [`CallArgs::for_each_row`] so blanks
//!   are surfaced (the `criteria=""` rule; SUMIF.md §Criteria). Error cells in
//!   `range` used only for matching are excluded, never propagated.
//!
//! # `sum_range` resizing (top-left anchor)
//! Excel anchors `sum_range` at its top-left cell and gives it `range`'s
//! dimensions (SUMIF.md §2). This module reproduces that using the two buffered
//! rectangles:
//! - `sum_range` **the same shape as, or larger than, `range`** → the top-left
//!   `range`-shaped sub-rectangle of `sum_range` is used (fully supported).
//! - `sum_range` **smaller than `range`** in either dimension → the resize
//!   would read sheet cells *outside* the `sum_range` argument, which the
//!   `CallArgs` surface cannot provide, and SUMIF.md flags the exact irregular-
//!   shape rule as an oracle experiment. Rather than guess (e.g. treat the
//!   missing cells as blank), this returns `#UNSUPPORTED!` (**OXP-103**).
//!
//! # Whole-column ranges (`A:A`) — used-extent ROW iteration (RFC 0001)
//! A whole-**column** `range` (and `sum_range`) is summed over its **populated**
//! rows via the used-extent ROW walk: the sparse cell store enumerates them in
//! `O(populated)` instead of scanning 1,048,576 rows. Summation is
//! order-independent, so the compaction is exact for the common criteria. Two
//! corners defer loudly rather than risk a wrong answer:
//! - **`sum_range` shorter/narrower than a whole-column `range`** (OXP-103,
//!   as before): the resize would read cells outside the `sum_range` argument.
//!   Concretely, a whole-column `range` against a **bounded** `sum_range`
//!   defers; a bounded `range` against a taller whole-column `sum_range`, and a
//!   whole-column `range` against an equally-tall whole-column `sum_range`
//!   (`SUMIF(A:A,">5",B:B)`), are supported (top-left-anchor correspondence by
//!   relative row).
//! - **A blank-matching criterion over a whole-column `range` with a
//!   `sum_range`** (OXP-104): the absent (blank) rows match but their `sum_range`
//!   cells cannot be enumerated over an unbounded column → defer. (The
//!   `sum_range`-omitted self-sum has no such issue — matched blank cells
//!   contribute 0 — so it is always supported.)
//!
//! # Whole-row ranges (`1:1`) — used-extent COLUMN iteration (RFC 0008)
//! A whole-**row** `range`/`sum_range` is the horizontal transpose, summed over
//! its **populated columns** via the used-extent COLUMN walk. The self-sum form
//! is order-independent (orientation-blind [`for_each_row_or_used_any_axis`]).
//! The `sum_range` lockstep transposes the OXP-103/104 quadrants exactly, keyed
//! by relative **column** instead of relative row ([`sum_lockstep_used_cols`]):
//! - whole-row `range` × **bounded** `sum_range` → **defer** (mirror of
//!   whole-column × bounded);
//! - **bounded** `range` × whole-row `sum_range` → **supported** (the
//!   mixed-orientation transposed quadrant, RFC 0008 §2 — the dense `range`
//!   buffer is transposed to columns so it lock-steps by relative column);
//! - whole-row `range` × whole-row `sum_range` → **supported**;
//! - `sum_range` shorter (fewer rows) than `range`, or a blank-matching
//!   criterion over a whole-row `range` → **defer** (transposed OXP-103/104).
//!
//! A mixed-**axis** pairing (a whole-column argument alongside a whole-row one)
//! is not in any pinned quadrant → `#UNSUPPORTED!`, never guessed. Bounded ranges
//! and array constants keep their exact prior behavior.
//!
//! # Recalc sentinels in the criteria-tested `range` cell propagate
//! "Error cells in `range` used only for matching are excluded, never
//! propagated" (above) is about **genuine** Excel errors — [`criteria::matches`]'s
//! documented conservative default. A Recalc sentinel
//! ([`xl_value::ErrorKind::is_recalc_sentinel`]) is different: Recalc never
//! actually evaluated that cell, so whether it would have matched `criteria`
//! in real Excel is unknowable, and reporting "excluded" would launder that
//! gap into a possibly-wrong sum. Per Recalc Principle 2, every walk here
//! calls [`criteria::sentinel_of`] on the criteria-tested `range` cell
//! *before* [`criteria::matches`] and propagates the first sentinel found
//! (kind preserved), in the walk's existing scan order. This applies **only**
//! to the criteria-tested cell — a sentinel in an *unmatched* `sum_range`
//! cell (never tested by `criteria`) is genuinely irrelevant to the sum and
//! stays ignored, unchanged. A sentinel in a **matched** `sum_range` cell
//! already propagates via `coerce_number_arg`'s `Error` arm (kind-preserving
//! for any `ErrorKind`, sentinel or genuine) — pre-existing behavior, not
//! part of this fix.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{ArgShape, CallArgs, for_each_row_or_used_any_axis, scalar_literal_error};
use crate::context::EvalContext;
use crate::criteria::{self, Matcher};

/// Evaluate a `SUMIF(range, criteria, [sum_range])` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // A directly-written error as the `range` argument (`SUMIF(#REF!, …)`, from a
    // deleted source reference) propagates rather than being walked as a lone
    // non-matching cell → silent `0` (Never-silently-wrong; the general
    // error-propagation contract, SUM.md / OXP-082). Only a scalar-LITERAL error
    // is caught — an error *cell* inside a reference/range keeps its separately-
    // pinned handling (see `scalar_literal_error`'s docs and the tests below).
    if let Some(k) = scalar_literal_error(args, 0) {
        return Value::Error(k);
    }

    // criteria (arg 1): evaluated once in scalar context, then compiled.
    let matcher = criteria::parse(&args.eval_scalar(1));
    if let Some(short) = matcher.short_circuit() {
        return short;
    }

    let has_sum_range = args.count() >= 3 && args.shape(2) != ArgShape::Omitted;
    if has_sum_range {
        // Same rule for a scalar-literal error `sum_range` (`SUMIF(r, c, #REF!)`).
        if let Some(k) = scalar_literal_error(args, 2) {
            return Value::Error(k);
        }
        eval_with_sum_range(args, &matcher)
    } else {
        eval_self_range(args, &matcher)
    }
}

/// `sum_range` omitted: sum the matching `range` cells themselves.
///
/// A whole-column `range` uses the used-extent ROW walk, and a whole-**row**
/// `range` the used-extent COLUMN walk (RFC 0008), both over populated cells
/// only. Summation is order-independent, so the orientation-blind
/// [`for_each_row_or_used_any_axis`] is safe here. This needs **no**
/// blank-matching guard: a criterion that matches blank cells (the `""` rule)
/// sums those cells, and a blank cell coerces to 0 under `RangeAggregate`, so the
/// absent cells the walk cannot see would each have contributed 0 — the sum is
/// unaffected.
fn eval_self_range(args: &mut dyn CallArgs, matcher: &Matcher) -> Value {
    let mut acc = 0.0_f64;
    let mut err: Option<ErrorKind> = None;
    let walk = for_each_row_or_used_any_axis(args, 0, &mut |_rel, row| {
        for cell in row {
            // A Recalc sentinel in the criteria-tested cell propagates
            // (kind preserved) instead of being silently excluded as
            // "no match" — see `criteria::sentinel_of`'s docs.
            if let Some(k) = criteria::refuse_cell(matcher, cell) {
                err = Some(k);
                return ControlFlow::Break(());
            }
            if criteria::matches(matcher, cell) {
                match coerce_number_arg(cell, CoercionMode::RangeAggregate) {
                    NumericArg::Number(n) => acc += n,
                    // Non-numeric matches contribute 0 (SUMIF.md §Coercion).
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
    Value::number(acc)
}

/// `sum_range` present: lockstep positional walk of `range` and `sum_range`
/// with the top-left-anchor resize. Uses the dense buffer when both arguments
/// are bounded (exact prior behavior), and the used-extent path when either is
/// a whole-column range.
fn eval_with_sum_range(args: &mut dyn CallArgs, matcher: &Matcher) -> Value {
    let range_dense = buffer_rows(args, 0);
    let sum_dense = buffer_rows(args, 2);
    if let (Ok(range_rows), Ok(sum_rows)) = (&range_dense, &sum_dense) {
        // Both bounded → the original dense lockstep, byte-for-byte unchanged.
        return sum_lockstep_dense(range_rows, sum_rows, matcher);
    }
    // At least one argument is unbounded. Pick the used-extent orientation: a
    // whole-**row** argument is not row-servable, so if either unbounded argument
    // is whole-row, correspondence is by relative COLUMN (RFC 0008); otherwise
    // (whole-**column**) it is by relative ROW (RFC 0001, unchanged). The
    // orientation contract (RFC 0008 §2) makes this branch load-bearing.
    if arg_is_whole_row(args, 0, &range_dense) || arg_is_whole_row(args, 2, &sum_dense) {
        sum_lockstep_used_cols(args, range_dense, sum_dense, matcher)
    } else {
        sum_lockstep_used_extent(args, range_dense, sum_dense, matcher)
    }
}

/// Whether argument `index` is a whole-**row** range (unbounded columns). Only
/// meaningful once its dense walk has already refused (`dense.is_err()`): a
/// bounded argument short-circuits to `false` without probing. Detected
/// geometrically — the used-**row** walk refuses a whole-row range *up front*
/// (visiting nothing) while it serves a whole-column one — so a probe whose
/// visitor breaks immediately classifies the axis at O(1) without materialising
/// the argument. (An unresolvable range also refuses the row walk; it is treated
/// as whole-row here and defers in the column path just the same.)
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

/// The original bounded lockstep: `range`'s dimensions govern the walk, the
/// top-left `r_rows × r_cols` sub-rectangle of `sum_range` supplies the summed
/// cells, and a `sum_range` smaller in either dimension defers (OXP-103).
fn sum_lockstep_dense(
    range_rows: &[Vec<Value>],
    sum_rows: &[Vec<Value>],
    matcher: &Matcher,
) -> Value {
    let r_rows = range_rows.len();
    let r_cols = range_rows.iter().map(Vec::len).max().unwrap_or(0);
    let s_rows = sum_rows.len();
    let s_cols = sum_rows.iter().map(Vec::len).max().unwrap_or(0);
    if s_rows < r_rows || s_cols < r_cols {
        return Value::Error(ErrorKind::Unsupported);
    }

    let mut acc = 0.0_f64;
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
            // The positionally-corresponding summed cell (guaranteed in-bounds
            // by the size check above).
            let sum_cell = sum_rows
                .get(r)
                .and_then(|row| row.get(c))
                .unwrap_or(&Value::Blank);
            match coerce_number_arg(sum_cell, CoercionMode::RangeAggregate) {
                NumericArg::Number(n) => acc += n,
                // Non-numeric summed cells contribute 0 (SUMIF.md §Coercion).
                NumericArg::Skip => {}
                // An error in a summed cell propagates as the result.
                NumericArg::Error(k) => return Value::Error(k),
            }
        }
    }
    Value::number(acc)
}

/// The used-extent lockstep (at least one of `range`/`sum_range` is a
/// whole-column range). Correspondence is by **relative row** (the top-left
/// anchor), so both arguments are keyed into `rel → cells` maps: a bounded
/// argument by its dense row index, a whole-column one by the populated relative
/// row from the used-extent walk. Missing sum cells within the supported extent
/// fill as blank.
///
/// Deferrals (OXP-103 / OXP-104), all `#UNSUPPORTED!`:
/// - a whole-column (full-height) `range` against a **bounded** (finite-height)
///   `sum_range` — the resize reads sum cells outside the argument;
/// - `sum_range` **narrower** than `range` — likewise outside the argument;
/// - a **blank-matching** criterion over a whole-column `range` — the unbounded
///   absent rows match but their summed cells cannot be enumerated.
fn sum_lockstep_used_extent(
    args: &mut dyn CallArgs,
    range_dense: Result<Vec<Vec<Value>>, ErrorKind>,
    sum_dense: Result<Vec<Vec<Value>>, ErrorKind>,
    matcher: &Matcher,
) -> Value {
    let range_unbounded = range_dense.is_err();
    let sum_unbounded = sum_dense.is_err();

    let range_map = match indexed_rows(args, 0, range_dense) {
        Ok(m) => m,
        Err(k) => return Value::Error(k),
    };
    // OXP-103: a full-height `range` against a finite `sum_range` (bounded, so
    // shorter) reads sum cells outside the argument → defer.
    if range_unbounded && !sum_unbounded {
        return Value::Error(ErrorKind::Unsupported);
    }
    let sum_map = match indexed_rows(args, 2, sum_dense) {
        Ok(m) => m,
        Err(k) => return Value::Error(k),
    };

    let r_cols = range_map.values().map(Vec::len).max().unwrap_or(0);
    let s_cols = sum_map.values().map(Vec::len).max().unwrap_or(0);
    // OXP-103: `sum_range` narrower than `range` → outside-argument read.
    if s_cols < r_cols {
        return Value::Error(ErrorKind::Unsupported);
    }
    // OXP-104: a blank-matching criterion over a whole-column range would match
    // unbounded absent rows whose summed cells we cannot enumerate.
    if range_unbounded && criteria::matches(matcher, &Value::Blank) {
        return Value::Error(ErrorKind::Unsupported);
    }

    let mut acc = 0.0_f64;
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
            let sum_cell = sum_map
                .get(rel)
                .and_then(|row| row.get(c))
                .unwrap_or(&Value::Blank);
            match coerce_number_arg(sum_cell, CoercionMode::RangeAggregate) {
                NumericArg::Number(n) => acc += n,
                NumericArg::Skip => {}
                NumericArg::Error(k) => return Value::Error(k),
            }
        }
    }
    Value::number(acc)
}

/// The COLUMN-oriented used-extent lockstep (at least one of `range`/`sum_range`
/// is a whole-**row** range) — the transpose of [`sum_lockstep_used_extent`].
/// Correspondence is by **relative column** (the top-left anchor transposed), so
/// both arguments are keyed into `rel_col → column` maps: a bounded argument by
/// transposing its dense buffer, a whole-row one by the used-extent COLUMN walk.
/// Missing sum cells within the supported extent fill as blank.
///
/// Deferrals (transposed OXP-103 / OXP-104), all `#UNSUPPORTED!`:
/// - a whole-row (full-**width**) `range` against a **bounded** (finite-width)
///   `sum_range` — the resize reads sum cells outside the argument;
/// - `sum_range` **shorter** (fewer rows) than `range` — likewise outside it;
/// - a **blank-matching** criterion over a whole-row `range` — the unbounded
///   absent columns match but their summed cells cannot be enumerated.
///
/// A doubly-unbounded mixed-axis pairing (a whole-**column** argument alongside a
/// whole-**row** one) cannot be served either way: the whole-column argument
/// refuses the COLUMN walk here, so [`indexed_cols`] returns `Err` and the call
/// defers — never a guess.
fn sum_lockstep_used_cols(
    args: &mut dyn CallArgs,
    range_dense: Result<Vec<Vec<Value>>, ErrorKind>,
    sum_dense: Result<Vec<Vec<Value>>, ErrorKind>,
    matcher: &Matcher,
) -> Value {
    let range_unbounded = range_dense.is_err();
    let sum_unbounded = sum_dense.is_err();

    let range_map = match indexed_cols(args, 0, range_dense) {
        Ok(m) => m,
        Err(k) => return Value::Error(k),
    };
    // OXP-103 (transposed): a full-width `range` against a finite `sum_range`
    // (bounded, so narrower) reads sum cells outside the argument → defer.
    if range_unbounded && !sum_unbounded {
        return Value::Error(ErrorKind::Unsupported);
    }
    let sum_map = match indexed_cols(args, 2, sum_dense) {
        Ok(m) => m,
        Err(k) => return Value::Error(k),
    };

    let r_rows = range_map.values().map(Vec::len).max().unwrap_or(0);
    let s_rows = sum_map.values().map(Vec::len).max().unwrap_or(0);
    // OXP-103 (transposed): `sum_range` shorter (fewer rows) than `range` →
    // outside-argument read.
    if s_rows < r_rows {
        return Value::Error(ErrorKind::Unsupported);
    }
    // OXP-104 (transposed): a blank-matching criterion over a whole-row range
    // would match unbounded absent columns whose summed cells we cannot enumerate.
    if range_unbounded && criteria::matches(matcher, &Value::Blank) {
        return Value::Error(ErrorKind::Unsupported);
    }

    let mut acc = 0.0_f64;
    for (rel, range_col) in &range_map {
        for r in 0..r_rows {
            let range_cell = range_col.get(r).unwrap_or(&Value::Blank);
            // A Recalc sentinel in the criteria-tested cell propagates
            // (kind preserved) instead of being silently excluded.
            if let Some(k) = criteria::refuse_cell(matcher, range_cell) {
                return Value::Error(k);
            }
            if !criteria::matches(matcher, range_cell) {
                continue;
            }
            let sum_cell = sum_map
                .get(rel)
                .and_then(|col| col.get(r))
                .unwrap_or(&Value::Blank);
            match coerce_number_arg(sum_cell, CoercionMode::RangeAggregate) {
                NumericArg::Number(n) => acc += n,
                NumericArg::Skip => {}
                NumericArg::Error(k) => return Value::Error(k),
            }
        }
    }
    Value::number(acc)
}

/// Buffer an argument's rectangle row-by-row into an owned grid using the
/// **dense** [`CallArgs::for_each_row`] walk only, so an unbounded whole-column
/// range surfaces as `Err(Unsupported)` (the caller then takes the used-extent
/// path).
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
/// whole-column range) is (re)walked via the used-extent iterator, which yields
/// the relative row index directly.
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
/// transpose of [`indexed_rows`]. A pre-buffered dense grid (bounded) is
/// **transposed** so relative column `c` maps to the column vector
/// `[row0[c], row1[c], …]` (absent cells filled with `Value::Blank`); an argument
/// whose dense walk refused (a whole-**row** range) is walked via the used-extent
/// COLUMN iterator [`CallArgs::for_each_used_col`], which yields each populated
/// column keyed by its relative column directly. A whole-**column** argument
/// refuses that walk (it is not column-servable), surfacing `Err` so the caller
/// defers.
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

    // ---- Whole-row ranges (RFC 0008) ----------------------------------

    /// SUMIF self-range over a whole-ROW range sums matching columns.
    /// `SUMIF(1:1, ">2")` over [1,6,3] → 6+3 = 9.
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
            num(9.0)
        );
    }

    /// RFC 0008 §5(c) mixed-orientation quadrant: a **bounded** single-row
    /// `range` lock-stepped against a **wider whole-ROW** `sum_range`, keyed by
    /// relative COLUMN (the dense `range` buffer is transposed to columns).
    /// range = [1,6,3] (1×3 row), ">2" matches cols 1,2; sum_range whole-row
    /// [10,20,30] → 20+30 = 50. This is where a transpose bug would hide.
    #[test]
    fn bounded_range_wider_whole_row_sum_range_supported() {
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
            num(50.0)
        );
    }

    /// whole-ROW `range` × whole-ROW `sum_range` → supported (relative-column
    /// lockstep). Only rel col 1 (value 6) matches ">2" → sum cell 200.
    #[test]
    fn whole_row_range_and_sum_range_supported() {
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

    /// whole-ROW `range` against a **bounded** `sum_range` → defer (transposed
    /// OXP-103): the resize would read sum cells outside the argument.
    #[test]
    fn whole_row_range_bounded_sum_range_defers() {
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

    /// A blank-matching criterion over a whole-ROW `range` with a `sum_range`
    /// → defer (transposed OXP-104).
    #[test]
    fn blank_matching_whole_row_with_sum_range_defers() {
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

    /// A mixed-**axis** pairing (whole-COLUMN `range` × whole-ROW `sum_range`)
    /// is in no pinned quadrant → defer, never guessed.
    #[test]
    fn mixed_axis_whole_column_range_whole_row_sum_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Unbounded(vec![num(1.0), num(6.0)]),
                    Scalar(txt(">0")),
                    UsedCols(vec![(0, vec![num(10.0)]), (1, vec![num(20.0)])]),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // ---- Recalc sentinels propagate (Principle 2 fix) -----------------

    #[test]
    fn self_range_sentinel_in_tested_cell_propagates_kind_preserved() {
        // sum_range omitted: range serves as both tested and summed range. A
        // sentinel range cell propagates instead of being silently excluded.
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
    fn with_sum_range_sentinel_in_tested_cell_propagates() {
        // sum_range present (dense lockstep): the sentinel sits in `range`
        // (the criteria-tested cell), not `sum_range`.
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
            num(40.0)
        );
    }

    #[test]
    fn sentinel_in_unmatched_sum_range_cell_stays_ignored() {
        // A sentinel in a `sum_range` cell whose aligned `range` cell does
        // NOT match the criterion is genuinely irrelevant — must stay
        // ignored (only the criteria-tested and matched-aggregated cells
        // carry the sentinel contract).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]), // neither matches ">5"
                    Scalar(txt(">5")),
                    Range(vec![Value::Error(ErrorKind::Unsupported), num(20.0)]),
                ]
            ),
            num(0.0)
        );
    }

    #[test]
    fn sentinel_in_matched_sum_range_cell_still_propagates_preexisting() {
        // A sentinel in a MATCHED sum_range cell already propagates via
        // coerce_number_arg (pre-existing behavior, verified unbroken here).
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

    // ---- scalar-literal error ARGUMENT propagates (Principle 2 fix) ----
    // A directly-written error as the `range`/`sum_range` argument (from a
    // deleted source reference, `SUMIF(#REF!, …)`) must propagate, not silently
    // return 0 — the general error-propagation contract (SUM.md / OXP-082).
    // Mismatch-mine (docs/mismatch-decomposition.md) found ~185 corpus cells of
    // `SUMIF(#REF!, crit, #REF!)` returning `0` where Excel returns `#REF!`.

    #[test]
    fn scalar_literal_error_range_propagates() {
        // range is a literal #REF! (no sum_range, and with sum_range).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Ref)), Scalar(txt(">0"))]
            ),
            Value::Error(ErrorKind::Ref)
        );
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Ref)),
                    Scalar(txt(">0")),
                    Scalar(Value::Error(ErrorKind::Ref)),
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    #[test]
    fn scalar_literal_error_sum_range_propagates() {
        // A valid range/criteria, but sum_range is a literal #REF!.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(txt(">0")),
                    Scalar(Value::Error(ErrorKind::Ref)),
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    #[test]
    fn error_cell_in_range_still_excluded_not_propagated() {
        // GUARD: the fix must NOT change the pinned behavior for an error *cell*
        // inside a multi-cell range — still excluded (see
        // `genuine_error_in_tested_cell_still_excluded_unchanged`).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(10.0), Value::Error(ErrorKind::Ref), num(30.0)]),
                    Scalar(txt(">0")),
                ]
            ),
            num(40.0)
        );
    }
}

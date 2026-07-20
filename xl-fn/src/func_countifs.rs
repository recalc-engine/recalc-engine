//! `COUNTIFS` — count positions where **every** `(criteria_range, criteria)`
//! pair matches simultaneously (logical AND across pairs).
//!
//! # Provenance
//! Behavior contract: `docs/specs/COUNTIFS.md` (which cites the Microsoft
//! `COUNTIFS` support page, verified 2026-07-11). COUNTIFS is the multi-pair
//! generalization of [`crate::func_countif`]: the criteria mini-language is
//! owned by [`crate::criteria`] (shared with `SUMIF`/`COUNTIF`), and this
//! module is the thin count-and-lockstep-walk shell over it. This module does
//! **not** modify [`crate::criteria`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `COUNTIFS(range1, crit1, [range2, crit2], ...)`: counts the positions at
//!   which **all** `critN` match their `rangeN` cell (COUNTIFS.md §1 — AND, no
//!   built-in OR). A single pair (`count == 2`) is exactly `COUNTIF`.
//! - Each `critN` is evaluated **once** in scalar context and compiled to a
//!   [`crate::criteria::Matcher`] (COUNTIFS.md §Coercion, identical to
//!   `COUNTIF`). A criteria error propagates and an oracle-deferred criterion
//!   returns `#UNSUPPORTED!`, both via [`Matcher::short_circuit`] — checked for
//!   every pair before any range is walked (criteria-error precedence).
//! - Every `criteria_rangeN` is walked positionally with [`CallArgs::for_each_row`]
//!   so blank cells are surfaced at their position (a `""`/blank-matching
//!   criterion sees them; COUNTIFS.md §Semantics). Error cells in a range are
//!   simply not matched (excluded), never propagated (COUNTIF.md §Error
//!   behavior, inherited through [`crate::criteria::matches`]).
//! - **All `criteria_rangeN` must share dimensions**; a shape mismatch (row or
//!   column count differs from the first range) returns `#VALUE!`
//!   (COUNTIFS.md §3). A scalar argument is treated as a 1×1 range, uniformly.
//! - **Arity**: the call is a flat list of `(range, criteria)` pairs, so the
//!   argument count must be **even and ≥ 2**. A malformed odd/too-few arity is
//!   a structural error (not an Excel semantic corner) → `#VALUE!`.
//!
//! # Whole-column `criteria_range` (`A:A`) — oracle-deferred
//! Unlike single-range `COUNTIF` (which serves a whole-column range via the
//! sparse used-extent walk, RFC 0001), a whole-column `criteria_range` here
//! defers to `#UNSUPPORTED!`. Counting across N ranges in lockstep requires the
//! populated rows of every range to be **aligned** into shared positions; how
//! Excel aligns sparse whole-column ranges of differing populated extents in a
//! multi-criteria AND is unobserved, and guessing an alignment would risk a
//! silently-wrong count (Recalc Principle 2). Bounded ranges (`A1:A100`),
//! array constants, and scalars keep full support (blanks included). The dense
//! [`CallArgs::for_each_row`] walk refuses an unbounded range, and that refusal
//! is surfaced as the deferral.
//!
//! # Recalc sentinels in a criteria-tested cell propagate
//! A Recalc sentinel ([`xl_value::ErrorKind::is_recalc_sentinel`]) in any
//! `rangeN` cell at a position is different from a genuine Excel error there
//! ("simply not matched (excluded)", above): Recalc never evaluated a
//! sentinel cell, so whether it would satisfy its criterion in real Excel is
//! unknowable, and reporting "excluded" would launder that gap into a
//! possibly-wrong count. Per Recalc Principle 2, at each position the
//! walk calls [`criteria::sentinel_of`] on every criteria-tested cell
//! *before* [`criteria::matches`], in the existing per-criterion scan order,
//! and propagates the first sentinel found (kind preserved) out of the whole
//! call — so a criterion that already fails on a genuine mismatch **before**
//! reaching a sentinel still short-circuits the AND exactly as before (no
//! over-propagation).
//
// OXP (unassigned): COUNTIFS over one or more unbounded whole-column
// criteria_ranges — the multi-range used-extent lockstep alignment (how Excel
// pairs populated rows across ranges of differing extents under AND) is
// unobserved. Probe: =COUNTIFS(A:A,">1",B:B,"<9") with staggered gaps in A/B,
// vs the same over bounded A1:An/B1:Bn. Deferred to #UNSUPPORTED! until run.

use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::criteria::{self, Matcher};

/// Evaluate a `COUNTIFS(...)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let count = args.count();
    // The call is a flat list of (criteria_range, criteria) pairs: the count
    // must be even and ≥ 2. Anything else is a malformed call → #VALUE!.
    if count < 2 || !count.is_multiple_of(2) {
        return Value::Error(ErrorKind::Value);
    }
    let npairs = count / 2;

    // 1. Compile every criterion first; a criteria error / oracle-deferral
    //    short-circuits the whole call (criteria-error precedence over the
    //    dimension check), exactly as COUNTIF short-circuits its one matcher.
    let mut matchers: Vec<Matcher> = Vec::with_capacity(npairs);
    for k in 0..npairs {
        let m = criteria::parse(&args.eval_scalar(2 * k + 1));
        if let Some(short) = m.short_circuit() {
            return short;
        }
        matchers.push(m);
    }

    // 2. Materialize every criteria_range densely (blanks surfaced positionally).
    //    An unbounded whole-column range (or an unresolvable range) surfaces its
    //    error — whole-column lockstep is oracle-deferred (see module docs).
    let mut ranges: Vec<(u32, u32, Vec<Value>)> = Vec::with_capacity(npairs);
    for k in 0..npairs {
        match materialize(args, 2 * k) {
            Ok(r) => ranges.push(r),
            Err(kind) => return Value::Error(kind),
        }
    }

    // 3. All criteria_ranges must share dimensions; a mismatch → #VALUE!.
    let (rows0, cols0) = (ranges[0].0, ranges[0].1);
    if ranges.iter().any(|(r, c, _)| *r != rows0 || *c != cols0) {
        return Value::Error(ErrorKind::Value);
    }

    // 4. Count positions where every (range, criterion) pair matches (AND).
    //    All ranges share dimensions, so every `data` has the same length.
    let positions = ranges[0].2.len();
    let mut hits = 0u64;
    for p in 0..positions {
        // A Recalc sentinel in any criteria-tested cell at this position
        // propagates (kind preserved) instead of being silently treated as
        // "no match" — checked in the existing per-criterion scan order, so
        // a criterion that already fails normally *before* reaching a
        // sentinel still short-circuits the AND as before (see
        // `criteria::sentinel_of`'s docs).
        let mut sentinel: Option<ErrorKind> = None;
        let all_match = matchers.iter().zip(&ranges).all(|(m, (_, _, data))| {
            let cell = &data[p];
            if let Some(k) = criteria::refuse_cell(m, cell) {
                sentinel = Some(k);
                return false;
            }
            criteria::matches(m, cell)
        });
        if let Some(k) = sentinel {
            return Value::Error(k);
        }
        if all_match {
            hits += 1;
        }
    }
    Value::number(hits as f64)
}

/// Materialize one `criteria_range` argument into `(rows, cols, row-major
/// cells)` via the dense [`CallArgs::for_each_row`] walk (blanks surfaced at
/// their column position). A scalar is a 1×1 rectangle; an omitted/empty
/// argument is `(0, 0, [])`.
///
/// # Errors
/// Propagates the dense walk's `Err(ErrorKind::Unsupported)` for an unbounded
/// whole-column/row range or an unresolvable range — the whole-column deferral
/// documented at the module level.
fn materialize(args: &mut dyn CallArgs, index: usize) -> Result<(u32, u32, Vec<Value>), ErrorKind> {
    let mut data: Vec<Value> = Vec::new();
    let mut rows = 0u32;
    let mut cols = 0u32;
    args.for_each_row(index, &mut |row| {
        // Rectangles are uniform-width; the first row fixes the column count.
        if rows == 0 {
            cols = row.len() as u32;
        }
        data.extend(row.iter().cloned());
        rows += 1;
        ControlFlow::Continue(())
    })?;
    Ok((rows, cols, data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg, eval_direct, num, txt};

    fn call(args: Vec<TestArg>) -> Value {
        eval_direct(eval, args)
    }

    fn col(vs: &[Value]) -> TestArg {
        TestArg::Range(vs.to_vec())
    }

    // ---- single criterion (== COUNTIF) ----------------------------------

    #[test]
    fn single_pair_behaves_like_countif() {
        // COUNTIFS(range, ">1") over [1,2,3] counts 2 and 3 → 2.
        let v = call(vec![
            col(&[num(1.0), num(2.0), num(3.0)]),
            TestArg::Scalar(txt(">1")),
        ]);
        assert_eq!(v, num(2.0));
    }

    #[test]
    fn single_pair_numeric_equality_value() {
        // A bare numeric criteria value is numeric equality (COUNTIF parity).
        let v = call(vec![
            col(&[num(5.0), num(5.0), txt("5"), num(6.0)]),
            TestArg::Scalar(num(5.0)),
        ]);
        // OXP-177: matches by numeric value — the two numeric-5 cells AND the
        // numeric-text "5" cell.
        assert_eq!(v, num(3.0));
    }

    // ---- two-criteria AND -----------------------------------------------

    #[test]
    fn two_criteria_and_intersects() {
        // range1=[1,2,3,4] ">1" → positions 1,2,3; range2=[a,b,a,b] "a" →
        // positions 0,2. AND → position 2 only → 1.
        let v = call(vec![
            col(&[num(1.0), num(2.0), num(3.0), num(4.0)]),
            TestArg::Scalar(txt(">1")),
            col(&[txt("a"), txt("b"), txt("a"), txt("b")]),
            TestArg::Scalar(txt("a")),
        ]);
        assert_eq!(v, num(1.0));
    }

    #[test]
    fn three_criteria_and() {
        // Add a third pair narrowing further; here nothing survives → 0.
        let v = call(vec![
            col(&[num(1.0), num(2.0), num(3.0), num(4.0)]),
            TestArg::Scalar(txt(">1")),
            col(&[txt("a"), txt("b"), txt("a"), txt("b")]),
            TestArg::Scalar(txt("a")),
            col(&[num(10.0), num(20.0), num(30.0), num(40.0)]),
            TestArg::Scalar(txt(">99")),
        ]);
        assert_eq!(v, num(0.0));
    }

    // ---- operator / text / mixed criteria -------------------------------

    #[test]
    fn operator_and_wildcard_criteria() {
        // ">=2" AND text wildcard "a*" on the second range.
        let v = call(vec![
            col(&[num(1.0), num(2.0), num(3.0)]),
            TestArg::Scalar(txt(">=2")),
            col(&[txt("apple"), txt("apricot"), txt("banana")]),
            TestArg::Scalar(txt("a*")),
        ]);
        // pos1 (2>=2, "apricot" matches "a*"); pos2 (3>=2, "banana" no) → 1.
        assert_eq!(v, num(1.0));
    }

    // ---- 2-D rectangles walked in lockstep ------------------------------

    #[test]
    fn rectangular_ranges_lockstep() {
        let r1 = TestArg::Rect {
            rows: 2,
            cols: 2,
            data: vec![num(1.0), num(2.0), num(3.0), num(4.0)],
        };
        let r2 = TestArg::Rect {
            rows: 2,
            cols: 2,
            data: vec![txt("a"), txt("b"), txt("c"), txt("d")],
        };
        // ">1" → positions 1,2,3; "b" → position 1. AND → position 1 → 1.
        let v = call(vec![
            r1,
            TestArg::Scalar(txt(">1")),
            r2,
            TestArg::Scalar(txt("b")),
        ]);
        assert_eq!(v, num(1.0));
    }

    // ---- zero matches ---------------------------------------------------

    #[test]
    fn zero_matches_returns_zero() {
        let v = call(vec![
            col(&[num(1.0), num(2.0), num(3.0)]),
            TestArg::Scalar(txt(">5")),
        ]);
        assert_eq!(v, num(0.0));
    }

    // ---- blank cells surfaced (bounded range) ---------------------------

    #[test]
    fn empty_criteria_counts_blank_cells() {
        // A bounded range's blank cells are surfaced positionally, so `""`
        // (blank-matching) is fully supported here (no whole-column deferral).
        let v = call(vec![
            col(&[num(1.0), Value::Blank, txt(""), num(2.0)]),
            TestArg::Scalar(txt("")),
        ]);
        // Blank cell + empty-string cell → 2.
        assert_eq!(v, num(2.0));
    }

    // ---- dimension mismatch → #VALUE! -----------------------------------

    #[test]
    fn row_count_mismatch_is_value_error() {
        let v = call(vec![
            col(&[num(1.0), num(2.0), num(3.0)]),
            TestArg::Scalar(txt(">0")),
            col(&[num(1.0), num(2.0)]),
            TestArg::Scalar(txt(">0")),
        ]);
        assert_eq!(v, Value::Error(ErrorKind::Value));
    }

    #[test]
    fn orientation_mismatch_is_value_error() {
        // Same cell count but different shape: a column (3×1) vs a row (1×3).
        let v = call(vec![
            col(&[num(1.0), num(2.0), num(3.0)]),
            TestArg::Scalar(txt(">0")),
            TestArg::Array(vec![num(1.0), num(2.0), num(3.0)]),
            TestArg::Scalar(txt(">0")),
        ]);
        assert_eq!(v, Value::Error(ErrorKind::Value));
    }

    // ---- arity ----------------------------------------------------------

    #[test]
    fn odd_arity_is_value_error() {
        let v = call(vec![
            col(&[num(1.0), num(2.0)]),
            TestArg::Scalar(txt(">0")),
            col(&[num(3.0), num(4.0)]),
        ]);
        assert_eq!(v, Value::Error(ErrorKind::Value));
    }

    #[test]
    fn too_few_args_is_value_error() {
        assert_eq!(call(vec![]), Value::Error(ErrorKind::Value));
        assert_eq!(call(vec![col(&[num(1.0)])]), Value::Error(ErrorKind::Value));
    }

    // ---- criteria error / deferral precedence ---------------------------

    #[test]
    fn criteria_error_propagates() {
        // An error criteria *value* propagates (before the dimension check).
        let v = call(vec![
            col(&[num(1.0), num(2.0)]),
            TestArg::Scalar(Value::Error(ErrorKind::Na)),
        ]);
        assert_eq!(v, Value::Error(ErrorKind::Na));
    }

    #[test]
    fn deferred_criterion_is_unsupported() {
        // A criteria-engine short-circuit propagates. Date/currency operands now
        // PARSE (OXP-101/162), so this uses a still-deferred operand: a non-ASCII
        // text ordering criterion (`">ä"`, OXP-031 HELD locale-collation defer).
        let v = call(vec![col(&[num(1.0), num(2.0)]), TestArg::Scalar(txt(">ä"))]);
        assert_eq!(v, Value::Error(ErrorKind::Unsupported));
    }

    #[test]
    fn date_ordering_criterion_now_counts() {
        // OXP-101 (RUN-2026-07-11-oracle01): a full-date ordering operand parses
        // to its serial (43831) and counts numerically — no longer deferred.
        let v = call(vec![
            col(&[num(43830.0), num(43832.0), num(43900.0)]),
            TestArg::Scalar(txt(">1/1/2020")),
        ]);
        assert_eq!(v, num(2.0)); // 43832, 43900 are > 43831
    }

    #[test]
    fn criteria_error_takes_precedence_over_dimension_mismatch() {
        // range1 (2 rows) vs range2 (3 rows) would be #VALUE!, but the errored
        // criteria value short-circuits first.
        let v = call(vec![
            col(&[num(1.0), num(2.0)]),
            TestArg::Scalar(txt(">0")),
            col(&[num(1.0), num(2.0), num(3.0)]),
            TestArg::Scalar(Value::Error(ErrorKind::Div0)),
        ]);
        assert_eq!(v, Value::Error(ErrorKind::Div0));
    }

    // ---- whole-column deferral ------------------------------------------

    #[test]
    fn whole_column_range_is_deferred() {
        // A whole-column criteria_range defers to #UNSUPPORTED! (module docs).
        let v = call(vec![
            TestArg::Unbounded(vec![num(1.0), num(2.0), num(3.0)]),
            TestArg::Scalar(txt(">1")),
        ]);
        assert_eq!(v, Value::Error(ErrorKind::Unsupported));
    }

    // ---- Recalc sentinels propagate (Principle 2 fix) --------------------

    #[test]
    fn sentinel_in_criteria_cell_propagates_kind_preserved() {
        for k in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            let v = call(vec![
                col(&[num(1.0), Value::Error(k), num(3.0)]),
                TestArg::Scalar(txt(">0")),
            ]);
            assert_eq!(v, Value::Error(k), "{k:?} should propagate");
        }
    }

    #[test]
    fn genuine_error_in_criteria_cell_still_excludes_that_position_unchanged() {
        // Control: a genuine error keeps the exact "excluded" behavior.
        let v = call(vec![
            col(&[num(1.0), Value::Error(ErrorKind::Div0), num(3.0)]),
            TestArg::Scalar(txt(">0")),
        ]);
        assert_eq!(v, num(2.0)); // idx 0, 2 match; the error position excluded
    }

    #[test]
    fn sentinel_after_a_definite_mismatch_does_not_over_propagate() {
        // range1's criterion fails a genuine mismatch BEFORE range2's
        // sentinel would be scanned — the AND short-circuits as before.
        let v = call(vec![
            col(&[num(1.0)]),
            TestArg::Scalar(txt(">5")),                   // fails first
            col(&[Value::Error(ErrorKind::Unsupported)]), // never reached
            TestArg::Scalar(txt(">0")),
        ]);
        assert_eq!(v, num(0.0));
    }
}

//! `COUNTIF` — count the cells in a range that satisfy a single criterion.
//!
//! # Provenance
//! Behavior contract: `docs/specs/COUNTIF.md` (which cites the Microsoft Learn
//! `COUNTIF` page, verified 2026-07-05). The criteria mini-language is owned by
//! [`crate::criteria`] (shared with `SUMIF`); this module is the thin
//! count-and-walk shell over it.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `COUNTIF(range, criteria)`: every `range` cell matching `criteria` adds 1
//!   to the count regardless of its type (COUNTIF.md §1, §2 — no `sum_range`,
//!   so no non-numeric-contributes-0 concern).
//! - `criteria` is evaluated **once** in scalar context and compiled to a
//!   [`crate::criteria::Matcher`] (COUNTIF.md §Coercion). An error criteria
//!   propagates; an oracle-deferred criterion (see the criteria module) returns
//!   `#UNSUPPORTED!` — both via [`Matcher::short_circuit`].
//! - `range` is walked positionally with [`CallArgs::for_each_row`] so blank
//!   cells are surfaced (the `criteria=""` rule counts blank cells; COUNTIF.md
//!   §3). Error cells in `range` are simply not matched (excluded), never
//!   propagated (COUNTIF.md §Error behavior; the error-tolerant counting form).
//!
//! # Whole-column `range` (`A:A`) — used-extent iteration (RFC 0001)
//! A whole-**column** `range` is counted over its **populated** cells via the
//! used-extent walk ([`for_each_row_or_used`]): the sparse cell store enumerates
//! them in `O(populated)` instead of scanning 1,048,576 rows. Counting is
//! order-independent, so compacting away absent rows is exact for any criterion
//! that does **not** match a blank cell (`">5"`, `"apple"`, `5`).
//!
//! The exception is a criterion that **matches blank cells** (the `""` rule, or
//! `parse` producing a blank-matching matcher): over a whole column the absent
//! (blank) cells are unbounded and unobservable from the populated set, so
//! COUNTIF would under-count. That case defers to `#UNSUPPORTED!` (**OXP-104**)
//! on the used-extent path rather than return a wrong count.
//!
//! # Whole-**row** `range` (`1:1`) — used-extent COLUMN iteration (RFC 0008)
//! A whole-**row** `range` (unbounded columns) is the horizontal transpose,
//! counted over its **populated** columns via the used-extent COLUMN walk (the
//! [`for_each_row_or_used_any_axis`] fallback). Counting is order-independent, so
//! the same reasoning holds: exact for any non-blank-matching criterion, and the
//! same blank-matching deferral (OXP-104, transposed) applies. Bounded ranges
//! (`A1:A100`) and array constants keep their exact prior behavior.
//!
//! # OXP-165 (RUN-2026-07-11-oracle01) — blank *value* criterion vs. `""`
//! Over `A = [0, "", <blank>, 5]` the run pins `COUNTIF(A:A, 0) = 1` and
//! `COUNTIF(A:A, <blank-ref>) = 1` — both already hold here: a **blank criteria
//! value** compiles (OXP-102) to numeric equality-to-`0`, which does **not**
//! match blank cells, so the used-extent count of the single `0`-cell is exact.
//! The distinct `COUNTIF(A:A, "")` = 1_048_574 (a **literal empty-string**
//! criterion counting every empty cell in the whole column) is a
//! whole-column-blank *total* that needs sheet-geometry (total rows minus
//! non-empty) beyond the populated-cell walk — it stays `#UNSUPPORTED!` under the
//! blank-matching defer above.
//! `// OXP (unassigned)`: the whole-column empty-cell count is a separate,
//! not-yet-assigned geometry task; it is deferred here, never guessed.
//!
//! # Recalc sentinels in the criteria-tested `range` cell propagate
//! "Error cells in `range` are simply not matched (excluded), never
//! propagated" (above) is about **genuine** Excel errors —
//! [`criteria::matches`]'s documented conservative default. A Recalc
//! sentinel ([`xl_value::ErrorKind::is_recalc_sentinel`]) is different:
//! Recalc never actually evaluated that cell, so whether it would have
//! matched `criteria` in real Excel is unknowable, and reporting "excluded"
//! would launder that gap into a possibly-wrong count. Per the Recalc design rules
//! Principle 2, the walk calls [`criteria::sentinel_of`] on each `range`
//! cell *before* [`criteria::matches`] and propagates the first sentinel
//! found (kind preserved), in the walk's existing scan order. Genuine
//! (non-sentinel) errors are unaffected — they keep the exact "excluded,
//! never propagated" behavior.

use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value};

use crate::args::{CallArgs, for_each_row_or_used_any_axis};
use crate::context::EvalContext;
use crate::criteria;

/// Evaluate a `COUNTIF(range, criteria)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // criteria (arg 1): evaluated once in scalar context, then compiled.
    let matcher = criteria::parse(&args.eval_scalar(1));
    if let Some(short) = matcher.short_circuit() {
        return short;
    }

    // range (arg 0): dense positional walk (blanks surfaced for the `""` rule),
    // falling back to the used-extent ROW walk for a whole-column range and the
    // used-extent COLUMN walk for a whole-row range (RFC 0008). Counting is
    // order-independent, so this orientation-blind helper is safe here.
    let matches_blank = criteria::matches(&matcher, &Value::Blank);
    let mut count = 0u64;
    let mut sentinel: Option<ErrorKind> = None;
    let used_extent = match for_each_row_or_used_any_axis(args, 0, &mut |_rel, row| {
        for cell in row {
            // A Recalc sentinel in the criteria-tested cell propagates
            // (kind preserved) instead of being silently excluded as "no
            // match" — see `criteria::sentinel_of`'s docs.
            if let Some(k) = criteria::refuse_cell(&matcher, cell) {
                sentinel = Some(k);
                return ControlFlow::Break(());
            }
            if criteria::matches(&matcher, cell) {
                count += 1;
            }
        }
        ControlFlow::Continue(())
    }) {
        Ok(used) => used,
        // Unresolvable range → #UNSUPPORTED! (documented policy).
        Err(k) => return Value::Error(k),
    };
    if let Some(k) = sentinel {
        return Value::Error(k);
    }
    // On the used-extent path (whole-column via the row walk, or whole-row via the
    // column walk) only populated cells are visited; a blank-matching criterion
    // would under-count the unbounded absent cells, so defer loudly rather than
    // return a wrong count (OXP-104). This is the `COUNTIF(A:A, "")` = 1_048_574
    // case (and its `COUNTIF(1:1, "")` whole-row transpose) — an empty-cell total
    // that needs sheet geometry. // OXP (unassigned): deferred, never guessed.
    if used_extent && matches_blank {
        return Value::Error(ErrorKind::Unsupported);
    }

    Value::number(count as f64)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    /// The OXP-165 whole column `A = [0, "", <truly blank>, 5]` as the
    /// used-extent walk sees it (the truly-blank A3 is absent).
    fn oxp165_column() -> Vec<(u32, Vec<Value>)> {
        vec![(0, vec![num(0.0)]), (1, vec![txt("")]), (3, vec![num(5.0)])]
    }

    #[test]
    fn oxp165_countif_zero_over_whole_column() {
        // RUN-2026-07-11-oracle01 / OXP-165: COUNTIF(A:A, 0) = 1 (only A1=0).
        assert_eq!(
            eval_direct(eval, vec![UsedRows(oxp165_column()), Scalar(num(0.0))]),
            Value::number(1.0)
        );
    }

    #[test]
    fn oxp165_countif_blank_ref_criterion_matches_zero_cell() {
        // OXP-165 / OXP-102: a blank criteria *value* (a bare reference to an
        // empty cell) is numeric equality to 0, so COUNTIF(A:A, <blank-ref>) = 1
        // — it counts the single 0-cell, NOT the empty cells.
        assert_eq!(
            eval_direct(eval, vec![UsedRows(oxp165_column()), Scalar(Value::Blank)]),
            Value::number(1.0)
        );
    }

    #[test]
    fn oxp165_countif_empty_string_over_whole_column_defers() {
        // DEFER: COUNTIF(A:A, "") = 1_048_574 counts every empty cell in the whole
        // column — a sheet-geometry total beyond the populated-cell walk. It stays
        // #UNSUPPORTED! (// OXP unassigned), never guessed.
        assert_eq!(
            eval_direct(eval, vec![UsedRows(oxp165_column()), Scalar(txt(""))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // ---- Whole-row range (RFC 0008) ----------------------------------

    /// COUNTIF over a whole-ROW range counts populated columns
    /// (order-independent). `COUNTIF(1:1, ">2")` over row cells [1,6,3] → 2.
    #[test]
    fn countif_whole_row_counts_matching_columns() {
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
            num(2.0)
        );
    }

    /// A blank-matching criterion over a whole-ROW range defers (transposed
    /// OXP-104) — the unbounded absent columns cannot be counted.
    #[test]
    fn countif_blank_matching_over_whole_row_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    UsedCols(vec![(0, vec![num(1.0)]), (2, vec![num(3.0)])]),
                    Scalar(txt("")),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // ---- Recalc sentinels propagate (Principle 2 fix) -----------------

    #[test]
    fn sentinel_in_tested_cell_propagates_kind_preserved() {
        for k in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                eval_direct(
                    eval,
                    vec![
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
    fn genuine_error_in_tested_cell_still_excluded_unchanged() {
        // Control: a genuine error cell keeps the exact "excluded, never
        // propagated" behavior.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), Value::Error(ErrorKind::Div0), num(3.0)]),
                    Scalar(txt(">0")),
                ]
            ),
            num(2.0)
        );
    }

    // ---- OXP-189: non-ASCII cell-side collation (end-to-end) -------------

    /// The OXP-189 scaffold A1:A3 = {ä, zz, ß} (RUN 2026-07-13, Excel 16.0).
    fn oxp189_range() -> Vec<Value> {
        vec![txt("ä"), txt("zz"), txt("ß")]
    }

    #[test]
    fn oxp189_ordering_criterion_over_non_ascii_cells_defers() {
        // Excel: COUNTIF({ä,zz,ß},">z") = 1 (only `zz`; `ä`<`z`, `ß`<`z`) and
        // `"<z"` = 2 — locale collation Recalc cannot reproduce (its code-point
        // rank would count 3 / 0). Defer loudly rather than return a wrong count.
        for crit in [">z", "<z"] {
            assert_eq!(
                eval_direct(eval, vec![Range(oxp189_range()), Scalar(txt(crit))]),
                Value::Error(ErrorKind::Unsupported),
                "{crit}"
            );
        }
    }

    #[test]
    fn oxp189_literal_equality_over_non_ascii_cells_defers() {
        // Excel: COUNTIF({ä,zz,ß},"ss") = 1 (`ß`=`ss`) and `"<>ss"` = 2 — a
        // case-folded literal comparison Recalc's `ci_eq` gets wrong. Defer.
        for crit in ["ss", "<>ss"] {
            assert_eq!(
                eval_direct(eval, vec![Range(oxp189_range()), Scalar(txt(crit))]),
                Value::Error(ErrorKind::Unsupported),
                "{crit}"
            );
        }
    }

    #[test]
    fn oxp189_structural_wildcard_over_non_ascii_cells_computes() {
        // Excel: COUNTIF({ä,zz,ß},"?") = 2 — `?` is length-based and
        // collation-INDEPENDENT (matches the single-char `ä`/`ß`, not `zz`), so
        // Recalc reproduces it exactly and must NOT defer.
        assert_eq!(
            eval_direct(eval, vec![Range(oxp189_range()), Scalar(txt("?"))]),
            num(2.0)
        );
    }
}

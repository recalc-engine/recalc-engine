//! `ROW` — the 1-based row number of a reference (or of the calling cell).
//!
//! # Provenance
//! Behavior contract: `docs/specs/ROW.md` (which cites the Microsoft Learn `ROW`
//! function page). Enabled by the RFC 0005 reference-position channel on
//! [`CallArgs`] (`rfcs/0005-callargs-reference-position.md`, ratified the contract review
//! tech-lead decision 2026-07-11) — before it, `ROW` was `#UNSUPPORTED!` because
//! `xl-fn` could reach an argument's *value* and *size* but never a reference's
//! **position**.
//!
//! # Semantics implemented (Microsoft Learn `ROW`)
//! - **`ROW()`** (no argument) → the 1-based row of **the cell that contains the
//!   formula**, read from [`CallArgs::anchor`] (`row + 1`). If no anchor is
//!   available (`anchor() == None`) → `#UNSUPPORTED!` rather than a guessed
//!   origin.
//! - **`ROW(reference)`** where `reference` resolves to a single rectangular
//!   area of **any height** → the reference's **top** row, 1-based, from
//!   [`CallArgs::arg_ref_extent`] (`rect.row + 1`). A single cell and a
//!   single-row multi-column range (`A1:C1`) give that one row; a multi-row
//!   range (`A1:A5`) gives its top row. A `reference` that is not a resolvable
//!   single-area reference (a literal, an array constant, a reference union, an
//!   errored `OFFSET`) has no address → `#UNSUPPORTED!`.
//!
//! # Multi-row reference → the top row (FARM-PINNED, OXP-167)
//! `ROW(A1:A5)` is Excel's dynamic-array form: entered as a spilling formula it
//! yields a *column* of row numbers `{1;2;3;4;5}`. v1 does not spill; the
//! scalar this engine returns is the value Excel produces for the same reference
//! in a **scalar / implicit-intersection** context. `RUN-2026-07-11-oracle01`
//! experiment **OXP-167** pins that scalar to the range's **top** row — `1` at
//! *every* formula position (not the intersecting row, not `#VALUE!`, not a
//! spill). `arg_ref_extent().row + 1` is exactly that top row, so the multi-row
//! form is now resolved (it was deferred to `#UNSUPPORTED!` under the RFC-0005
//! stopgap). Only `ROW` was farm-probed; see [`crate::func_column`] for the
//! `COLUMN` mirror (resolved by symmetry, flagged there).
use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `ROW([reference])` call. See the module docs for semantics and
/// provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // ROW() — no argument: the 1-based row of the calling (formula-owning) cell.
    if args.count() == 0 {
        return match args.anchor() {
            Some((_sheet, row, _col)) => Value::number(f64::from(row) + 1.0),
            // No per-cell anchor available (no infra) — do not guess an origin.
            None => Value::Error(ErrorKind::Unsupported),
        };
    }
    // ROW(reference) — the reference's top row, 1-based (OXP-167: a multi-row
    // reference resolves to its top row, `1` for `A1:A5`, at every position).
    match args.arg_ref_extent(0) {
        Some(rect) => Value::number(f64::from(rect.row) + 1.0),
        // A non-reference / unresolvable argument (literal, array constant,
        // reference union, errored OFFSET) has no address → #UNSUPPORTED!.
        None => Value::Error(ErrorKind::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::args::RefRect;
    use crate::test_support::{TestArg::*, eval_anchored, eval_direct};
    use xl_value::{ErrorKind, SheetId, Value};

    fn rect(row: u32, col: u32, height: u32, width: u32) -> RefRect {
        RefRect {
            row,
            col,
            height,
            width,
        }
    }

    #[test]
    fn no_arg_returns_calling_cell_row() {
        // ROW() in the cell at 0-based (row 6, col 2) => 1-based row 7.
        assert_eq!(
            eval_anchored(eval, vec![], Some((SheetId(0), 6, 2))),
            Value::number(7.0)
        );
    }

    #[test]
    fn no_arg_without_anchor_is_unsupported() {
        // ROW() with no anchor infra (a bare mock) defers rather than guessing.
        assert_eq!(
            eval_anchored(eval, vec![], None),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn single_cell_ref_returns_top_row() {
        // ROW(B3): B3 is 0-based (row 2, col 1) => 1-based row 3.
        assert_eq!(
            eval_direct(eval, vec![Reference(rect(2, 1, 1, 1))]),
            Value::number(3.0)
        );
    }

    #[test]
    fn single_row_multi_col_ref_returns_that_row() {
        // ROW(A5:C5): a one-row-tall area at 0-based row 4 => 1-based row 5.
        assert_eq!(
            eval_direct(eval, vec![Reference(rect(4, 0, 1, 3))]),
            Value::number(5.0)
        );
    }

    #[test]
    fn multi_row_ref_returns_top_row() {
        // OXP-167 (RUN-2026-07-11-oracle01): ROW(A1:A5) over a multi-row
        // reference returns the range's TOP row — 1-based row 1 — at every
        // formula position (not the intersecting row, not #VALUE!, not a spill).
        assert_eq!(
            eval_direct(eval, vec![Reference(rect(0, 0, 5, 1))]),
            Value::number(1.0)
        );
        // A multi-row range not anchored at row 0: ROW(B3:B9) => top row 3.
        assert_eq!(
            eval_direct(eval, vec![Reference(rect(2, 1, 7, 1))]),
            Value::number(3.0)
        );
    }

    #[test]
    fn non_reference_arg_is_unsupported() {
        // ROW(5): a literal has no reference address => #UNSUPPORTED!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::number(5.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

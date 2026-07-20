//! `COLUMN` — the 1-based column number of a reference (or of the calling cell).
//!
//! The column-axis mirror of [`crate::func_row`]; every decision below is that
//! module's, transposed onto the column dimension. See it for the full rationale
//! and the RFC 0005 provenance.
//!
//! # Provenance
//! Behavior contract: `docs/specs/COLUMN.md` (which cites the Microsoft Learn
//! `COLUMN` function page). Enabled by the RFC 0005 reference-position channel on
//! [`CallArgs`] (`rfcs/0005-callargs-reference-position.md`, ratified the contract review
//! tech-lead decision 2026-07-11).
//!
//! # Semantics implemented (Microsoft Learn `COLUMN`)
//! - **`COLUMN()`** (no argument) → the 1-based column of **the cell that
//!   contains the formula**, read from [`CallArgs::anchor`] (`col + 1`). No
//!   anchor available → `#UNSUPPORTED!`.
//! - **`COLUMN(reference)`** where `reference` resolves to a single rectangular
//!   area of **any width** → the reference's **left** column, 1-based, from
//!   [`CallArgs::arg_ref_extent`] (`rect.col + 1`). A single cell and a
//!   single-column multi-row range (`A1:A4`) give that one column; a
//!   multi-column range (`A1:C1`) gives its left column. A non-reference /
//!   unresolvable argument → `#UNSUPPORTED!`.
//!
//! # Multi-column reference → the left column (OXP-187, pinned)
//! `COLUMN(A1:C1)` is Excel's dynamic-array form: entered as a spill it yields a
//! *row* of column numbers `{1,2,3}`; in scalar / implicit-intersection context
//! Excel returns the range's **left** column. This is the column-axis mirror of
//! [`crate::func_row`]'s multi-row resolution (`ROW`'s multi-row → top row was
//! pinned by **OXP-167**). The `COLUMN` transpose is now **directly farm-pinned
//! by OXP-187 (RUN-2026-07-13)**: `=COLUMN(A1:C1)` returned `1` (the left column)
//! at every probed formula position — on-column (B10, C10), at the boundary
//! (D10), off-column (F10), and for the 2-D form `=COLUMN(A1:C3)` (H1) — so the
//! result is column-position-independent, exactly matching `arg_ref_extent().col
//! + 1`. This confirms the earlier by-symmetry reading was correct.
use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `COLUMN([reference])` call. See the module docs for semantics and
/// provenance; column-axis transpose of [`crate::func_row::eval`].
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // COLUMN() — no argument: the 1-based column of the calling cell.
    if args.count() == 0 {
        return match args.anchor() {
            Some((_sheet, _row, col)) => Value::number(f64::from(col) + 1.0),
            None => Value::Error(ErrorKind::Unsupported),
        };
    }
    // COLUMN(reference) — the reference's left column, 1-based (OXP-187: a
    // multi-column reference resolves to its left column, `1` for `A1:C1`, at
    // every formula position).
    match args.arg_ref_extent(0) {
        Some(rect) => Value::number(f64::from(rect.col) + 1.0),
        // A non-reference / unresolvable argument has no address → #UNSUPPORTED!.
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
    fn no_arg_returns_calling_cell_column() {
        // COLUMN() in the cell at 0-based (row 6, col 2) => 1-based column 3.
        assert_eq!(
            eval_anchored(eval, vec![], Some((SheetId(0), 6, 2))),
            Value::number(3.0)
        );
    }

    #[test]
    fn no_arg_without_anchor_is_unsupported() {
        assert_eq!(
            eval_anchored(eval, vec![], None),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn single_cell_ref_returns_left_column() {
        // COLUMN(B3): B3 is 0-based (row 2, col 1) => 1-based column 2.
        assert_eq!(
            eval_direct(eval, vec![Reference(rect(2, 1, 1, 1))]),
            Value::number(2.0)
        );
    }

    #[test]
    fn single_col_multi_row_ref_returns_that_column() {
        // COLUMN(C1:C9): a one-column-wide area at 0-based col 2 => 1-based col 3.
        assert_eq!(
            eval_direct(eval, vec![Reference(rect(0, 2, 9, 1))]),
            Value::number(3.0)
        );
    }

    #[test]
    fn multi_col_ref_returns_left_column() {
        // OXP-187 (RUN-2026-07-13, pinned directly — was OXP-167-by-symmetry):
        // COLUMN(A1:C1) over a multi-column reference returns the range's LEFT
        // column — 1-based column 1 — at every formula position (Excel returned
        // 1 on-column, at the boundary, off-column, and for the 2-D A1:C3).
        assert_eq!(
            eval_direct(eval, vec![Reference(rect(0, 0, 1, 3))]),
            Value::number(1.0)
        );
        // A multi-column range not anchored at col 0: COLUMN(C3:E3) => left col 3.
        assert_eq!(
            eval_direct(eval, vec![Reference(rect(2, 2, 1, 3))]),
            Value::number(3.0)
        );
    }

    #[test]
    fn non_reference_arg_is_unsupported() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::number(5.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

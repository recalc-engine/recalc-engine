//! `INDEX` — return the value at `(row_num, col_num)` of an array/range.
//!
//! # Provenance
//! Behavior contract: `docs/specs/INDEX.md` (which cites the Microsoft
//! `support.microsoft.com` INDEX page, verified 2026-07-05). This module
//! implements the **array (value-returning) form** only:
//! `INDEX(array, row_num, [col_num])`. The **reference form**
//! (`INDEX(reference, row_num, [col_num], [area_num])`, used e.g. inside
//! `INDEX(...):INDEX(...)` to build a *lazy* range for another function) is
//! out of scope for v0 — it needs an `xl-ast`/`xl-graph`-level reference
//! object this crate does not have, not merely a materialized rectangle.
//! [`CallArgs::for_each_row`] always hands this module an already-materialized
//! rectangle, so it can never actually be asked to build that kind of lazy
//! reference.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! 1. `INDEX(array, row_num, col_num)` with both indices nonzero and in
//!    bounds returns the single value at that 1-based position (INDEX.md
//!    §1). An error sitting in the indexed cell is returned as-is
//!    (§Error behavior) — never "caught".
//! 2. `row_num = 0` (with a resolved, nonzero, in-bounds `col_num`) returns
//!    the entire column `col_num` as a [`Value::Array`] (INDEX.md §2).
//! 3. `col_num = 0` (with a resolved, nonzero, in-bounds `row_num`) returns
//!    the entire row `row_num` as a [`Value::Array`] (INDEX.md §3).
//! 4. A single-row or single-column `array` makes the *other* index
//!    optional: an omitted `col_num` against a single-column array defaults
//!    to `1` (ordinary indexing); an omitted `col_num` against a
//!    single-*row* array reinterprets the supplied `row_num` argument as the
//!    column index into that row, with the row forced to `1` — the
//!    documented "single dimension" convenience (INDEX.md §4).
//! 5. Out-of-bounds `row_num`/`col_num` → `#REF!` (INDEX.md §6).
//!
//! # Whole-column/row `array` — in-rectangle absolute read (RFC 0006, OXP-106)
//! A whole-column/row `array` (`A:A`, `1:1`) is served by reading at **absolute
//! positions inside the AST-declared rectangle**, *not* by the RFC-0001
//! used-extent compaction (which, being positional, would mis-index). When the
//! dense [`CallArgs::for_each_row`] walk refuses the unbounded range,
//! [`eval_whole_axis`] reads the range's geometry from
//! [`CallArgs::arg_ref_extent`] (top-left + full sheet-axis extent) and the one
//! indexed cell from [`CallArgs::cell_at`] at its absolute coordinate:
//! `INDEX(A:A, 3)` is exactly `A3` by absolute position (RUN-2026-07-11-oracle01:
//! `= 30`), and the `#REF!` bound is the reference's full 1,048,576-row height —
//! never a 1M materialization or a dependency-graph hazard, since every read
//! stays inside a static precedent of the formula. See
//! `rfcs/0006-in-rectangle-absolute-read.md`.
//!
//! The `row_num = 0` / `col_num = 0` forms over a whole column/row remain
//! **deferred** (`#UNSUPPORTED!`): they return an entire column/row as a spilled
//! array of up to 1,048,576 / 16,384 elements — a spill scenario out of v0 scope
//! (RFC 0001 §3).
//!
//! # Oracle-deferred
//! - **`col_num` omitted against a genuinely 2-D array** (more than one row
//!   *and* more than one column): INDEX.md's own "Oracle experiments
//!   needed" flags the exact interaction (implicit intersection vs. a
//!   spilled array vs. `#VALUE!`) as version/mode-dependent, so rather than
//!   guess this returns `#UNSUPPORTED!`.
//! - **`row_num = 0` and `col_num = 0` together**: not documented by either
//!   spec bullet (each is only specified with the *other* index resolved
//!   and nonzero); defers to `#UNSUPPORTED!` rather than guess whether
//!   Excel returns the whole array, `#REF!`, or something else.
//! - **A negative `row_num`/`col_num`**: INDEX.md's own "Oracle experiments
//!   needed" flags the exact error type (`#VALUE!` vs `#REF!`) as
//!   unconfirmed; defers to `#UNSUPPORTED!`.
//! - **`area_num`** / multi-area reference unions: not applicable — the
//!   reference form is out of scope (see above).
//!
//! # Coercion
//! `row_num`/`col_num` are numeric-coerced via [`to_number`] and **floored**
//! — per this task's explicit direction, since INDEX.md documents
//! truncation as the general index-argument pattern across Excel without
//! pinning the exact rounding direction; the floor choice is *not* claimed
//! as oracle-verified (see INDEX.md "Oracle experiments needed"). The
//! `array` argument itself is never coerced — INDEX returns the underlying
//! cell's native value/type unchanged (INDEX.md §Coercion).

use std::ops::ControlFlow;

use xl_value::{Array, ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `INDEX(...)` call (array/value-returning form only). See the
/// module docs for the semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // --- array (arg 0): buffer the rectangle positionally, exactly like
    // VLOOKUP/HLOOKUP's table_array. `for_each_row` refuses an unbounded
    // whole-column/row range.
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let walk = args.for_each_row(0, &mut |row| {
        rows.push(row.to_vec());
        ControlFlow::Continue(())
    });
    if let Err(k) = walk {
        // The dense materialization refused: an unbounded whole-column/row
        // `array` (the RFC-0001 guardrail). RFC 0006 serves it as O(1) reads at
        // absolute positions inside the AST-declared rectangle — no 1M
        // materialization, no dependency-graph hazard.
        return eval_whole_axis(args, k);
    }
    if rows.is_empty() {
        // Nothing to index; any row/col request is out of bounds.
        return Value::Error(ErrorKind::Ref);
    }
    let height = rows.len();
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return Value::Error(ErrorKind::Ref);
    }

    // --- row_num (arg 1): numeric-coerced, floored; error propagates.
    let row_raw = match to_number(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let mut row_num = row_raw.floor();

    // --- col_num (arg 2, optional): numeric-coerced, floored when present.
    // When absent, resolved per the single-row/single-column convenience
    // (INDEX.md §4); a genuinely 2-D array with col_num omitted is
    // oracle-deferred.
    let col_num = if args.count() >= 3 {
        match to_number(&args.eval_scalar(2)) {
            Ok(n) => n.floor(),
            Err(k) => return Value::Error(k),
        }
    } else if width == 1 {
        1.0
    } else if height == 1 {
        // Single-row array: the supplied row_num arg is actually indexing
        // the column; the row is forced to 1.
        let c = row_num;
        row_num = 1.0;
        c
    } else {
        return Value::Error(ErrorKind::Unsupported);
    };

    // row_num = 0 and col_num = 0 together is undocumented.
    if row_num == 0.0 && col_num == 0.0 {
        return Value::Error(ErrorKind::Unsupported);
    }

    // row_num = 0: return the entire resolved column as an array.
    if row_num == 0.0 {
        if col_num < 0.0 {
            return Value::Error(ErrorKind::Unsupported);
        }
        let col = col_num as usize;
        if !(1..=width).contains(&col) {
            return Value::Error(ErrorKind::Ref);
        }
        let data: Vec<Value> = rows
            .iter()
            .map(|r| r.get(col - 1).cloned().unwrap_or(Value::Blank))
            .collect();
        return match Array::new(height, 1, data) {
            Ok(a) => Value::Array(a),
            Err(_) => Value::Error(ErrorKind::Ref),
        };
    }

    // col_num = 0: return the entire resolved row as an array.
    if col_num == 0.0 {
        if row_num < 0.0 {
            return Value::Error(ErrorKind::Unsupported);
        }
        let row = row_num as usize;
        if !(1..=height).contains(&row) {
            return Value::Error(ErrorKind::Ref);
        }
        let mut data: Vec<Value> = rows[row - 1].clone();
        data.resize(width, Value::Blank);
        return match Array::new(1, width, data) {
            Ok(a) => Value::Array(a),
            Err(_) => Value::Error(ErrorKind::Ref),
        };
    }

    // Both indices nonzero: single-cell lookup.
    if row_num < 0.0 || col_num < 0.0 {
        return Value::Error(ErrorKind::Unsupported);
    }
    let row = row_num as usize;
    let col = col_num as usize;
    if !(1..=height).contains(&row) || !(1..=width).contains(&col) {
        return Value::Error(ErrorKind::Ref);
    }
    rows[row - 1].get(col - 1).cloned().unwrap_or(Value::Blank)
}

/// `INDEX(array, row_num, [col_num])` over an unbounded **whole-column/row**
/// `array` (OXP-106, RFC 0006): read the geometry from
/// [`CallArgs::arg_ref_extent`] and the single indexed cell from
/// [`CallArgs::cell_at`] at its absolute position — never materialising the
/// 1,048,576-row rectangle. `INDEX(A:A, 3)` is exactly the cell `A3` by
/// **absolute** position (RUN-2026-07-11-oracle01: `= 30`); out-of-extent
/// indices are `#REF!` (INDEX.md §6), bounded against the reference's full
/// sheet-axis size.
///
/// `refused` is the error the dense walk returned; it is propagated unchanged
/// when the argument is not a resolvable single-area reference (an unresolved
/// name, a computed reference that errored), so a genuine `#UNSUPPORTED!` stays
/// distinguishable.
///
/// The `row_num = 0` / `col_num = 0` forms would return an entire whole
/// column/row as a spilled array (up to 1,048,576 / 16,384 elements) — a spill
/// scenario out of v0 scope — so they **defer** (`#UNSUPPORTED!`) here rather
/// than materialise a giant array (RFC 0001 §3, RFC 0006).
fn eval_whole_axis(args: &mut dyn CallArgs, refused: ErrorKind) -> Value {
    // Only a resolvable single-area reference has an extent; anything else keeps
    // the original refusal so a real refusal stays loud and distinguishable.
    let Some(rect) = args.arg_ref_extent(0) else {
        return Value::Error(refused);
    };
    let height = rect.height; // full sheet-axis extent, >= 1
    let width = rect.width; // >= 1

    // row_num (arg 1): numeric-coerced, floored; error propagates.
    let row_raw = match to_number(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let mut row_num = row_raw.floor();

    // col_num (arg 2, optional): floored when present; else the single-row/
    // single-column convenience (INDEX.md §4). A genuinely 2-D whole-column band
    // (`A:B`) with col_num omitted is oracle-deferred, mirroring the bounded arm.
    let col_num = if args.count() >= 3 {
        match to_number(&args.eval_scalar(2)) {
            Ok(n) => n.floor(),
            Err(k) => return Value::Error(k),
        }
    } else if width == 1 {
        1.0
    } else if height == 1 {
        // Single-row range (`1:1`): the supplied row_num arg indexes the column;
        // the row is forced to 1.
        let c = row_num;
        row_num = 1.0;
        c
    } else {
        return Value::Error(ErrorKind::Unsupported);
    };

    // row_num = 0 / col_num = 0 whole-column/row spill forms: deferred (v0 spill
    // scope) rather than materialise up to 1,048,576 elements.
    if row_num == 0.0 || col_num == 0.0 {
        return Value::Error(ErrorKind::Unsupported);
    }
    // A negative index's exact error type is unconfirmed (INDEX.md) → defer.
    if row_num < 0.0 || col_num < 0.0 {
        return Value::Error(ErrorKind::Unsupported);
    }

    // Bounds against the full extent → #REF! (INDEX.md §6). `as u64` saturates
    // any absurd magnitude; both indices are already >= 1 here.
    let row = row_num as u64;
    let col = col_num as u64;
    if row > u64::from(height) || col > u64::from(width) {
        return Value::Error(ErrorKind::Ref);
    }

    // Absolute position inside the declared rectangle, then one O(1) read.
    let abs_row = rect.row + (row as u32 - 1);
    let abs_col = rect.col + (col as u32 - 1);
    match args.cell_at(0, abs_row, abs_col) {
        Some(v) => v,
        // Bounds-checked above, so `None` only if the infra cannot serve the
        // position; treat it as out-of-range rather than fabricate a value.
        None => Value::Error(ErrorKind::Ref),
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::args::RefRect;
    use crate::test_support::{TestArg, eval_direct, num};
    use xl_value::{ErrorKind, Value};

    /// A whole-column `A:A` reference (RFC 0006): full 1,048,576-row extent, one
    /// column, with A1=10, A2=20, A3=30 at absolute rows 0/1/2.
    fn whole_column_a() -> TestArg {
        TestArg::RefCells {
            rect: RefRect {
                row: 0,
                col: 0,
                height: 1_048_576,
                width: 1,
            },
            cells: vec![(0, 0, num(10.0)), (1, 0, num(20.0)), (2, 0, num(30.0))],
        }
    }

    /// OXP-106 (RUN-2026-07-11-oracle01), RFC 0006: `INDEX(A:A, 3)` reads `A3` by
    /// absolute position over the whole-column range → `30`, no `#UNSUPPORTED!`.
    #[test]
    fn index_whole_column_absolute_read() {
        assert_eq!(
            eval_direct(eval, vec![whole_column_a(), TestArg::Scalar(num(3.0))]),
            num(30.0)
        );
        // A blank (absent) cell inside the extent reads as a blank, not an error.
        assert_eq!(
            eval_direct(eval, vec![whole_column_a(), TestArg::Scalar(num(5.0))]),
            Value::Blank
        );
    }

    /// INDEX.md §6: an index beyond the reference's full sheet-axis extent →
    /// `#REF!` (bounded against 1,048,576 rows, not the populated count).
    #[test]
    fn index_whole_column_out_of_bounds_is_ref() {
        assert_eq!(
            eval_direct(
                eval,
                vec![whole_column_a(), TestArg::Scalar(num(1_048_577.0))]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    /// The `row_num = 0` whole-column **spill** form stays deferred
    /// (`#UNSUPPORTED!`, v0 spill scope) — RFC 0001 §3 / RFC 0006.
    #[test]
    fn index_whole_column_row_zero_spill_deferred() {
        assert_eq!(
            eval_direct(eval, vec![whole_column_a(), TestArg::Scalar(num(0.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// A whole-**row** `1:1` reference: the single-row convenience form
    /// reinterprets the lone index as the column. `INDEX(1:1, 3)` = `C1`.
    #[test]
    fn index_whole_row_single_row_convenience() {
        let whole_row = TestArg::RefCells {
            rect: RefRect {
                row: 0,
                col: 0,
                height: 1,
                width: 16_384,
            },
            cells: vec![(0, 0, num(1.0)), (0, 1, num(2.0)), (0, 2, num(3.0))],
        };
        assert_eq!(
            eval_direct(eval, vec![whole_row, TestArg::Scalar(num(3.0))]),
            num(3.0)
        );
    }
}

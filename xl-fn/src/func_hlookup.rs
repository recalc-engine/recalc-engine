//! `HLOOKUP` — search the first row of a table for a key and return a value
//! from the same column in a chosen row.
//!
//! # Provenance
//! Behavior contract: `docs/specs/HLOOKUP.md`, which is documented as the
//! row/column transpose of `docs/specs/VLOOKUP.md` in every respect (verified
//! 2026-07-05). This module is therefore [`func_vlookup`](crate::func_vlookup)
//! with rows and columns swapped: the search walks the table's first **row**
//! instead of its first column, and `row_index_num` selects the result
//! **row** instead of a column. Value ordering (approximate mode) and
//! non-`Blank`-involved equality (exact mode) are deferred to `xl-value`
//! ([`compare`] / `values_equal`). **Exact-mode** equality where a `Blank` is
//! involved instead goes through the shared, oracle-scoped
//! [`crate::lookup::exact_eq`] (also used by VLOOKUP and MATCH) — see that
//! module's docs for the pinned/deferred pair table.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `HLOOKUP(lookup_value, table_array, row_index_num, [range_lookup])`.
//!   `range_lookup` defaults to `TRUE` (approximate match) when omitted
//!   (HLOOKUP.md §Signature, mirroring VLOOKUP.md §Signature).
//! - **Exact match** (`range_lookup = FALSE`): a linear left-to-right scan of
//!   `table_array`'s first row; the **first** cell equal to `lookup_value`
//!   (equality via [`crate::lookup::exact_eq`]) wins; no match → `#N/A`.
//! - **Approximate match** (`range_lookup = TRUE`/omitted): Excel assumes the
//!   first row is sorted ascending and runs a **binary search** for the
//!   largest value `<= lookup_value` — identical algorithm to
//!   [`func_vlookup::approx_search`](crate::func_vlookup::approx_search), just
//!   walking the first row instead of the first column, so it reproduces the
//!   same documented-largest-`<=`-on-sorted-data / algorithm-order-dependent-
//!   on-unsorted-data behavior.
//! - Returns row `row_index_num` (1-based) of the matched column.
//!   `row_index_num < 1` → `#VALUE!`; `row_index_num >` table height →
//!   `#REF!`. An error value sitting in the returned cell of the matched
//!   column propagates as HLOOKUP's result (it is simply returned).
//! - Error in `lookup_value`, `row_index_num`, or `range_lookup` propagates
//!   immediately.
//!
//! # Whole-column/row `table_array` — in-rectangle absolute read (RFC 0006, OXP-105)
//! An unbounded whole-column/row `table_array` is served by reading at **absolute
//! positions inside the AST-declared rectangle**, not by the RFC-0001
//! (row-compacting) used-extent walk — which was the wrong tool because HLOOKUP
//! searches the table's **first row** and bounds `row_index_num` against the
//! table **height**, both of which need the reference's absolute top and its full
//! 1,048,576-row extent, precisely what a row-compacting iterator drops.
//!
//! Instead, when the dense [`CallArgs::for_each_row`] walk refuses the unbounded
//! range, [`eval_whole_axis`] reads the reference's geometry from
//! [`CallArgs::arg_ref_extent`] and each needed cell from [`CallArgs::cell_at`]:
//! it scans only the rectangle's **top row** (the bounded search axis) and then
//! reads the single result cell at `(row_index, matched col)` — never the
//! 1,048,576-row rectangle, and never a dependency-graph hazard (every read is a
//! static precedent). `row_index_num` is bounded against the reference's full
//! height (`> height` → `#REF!`). Because the search reads positionally (blanks
//! surfaced at their true columns), a whole-column HLOOKUP matches the equivalent
//! bounded range exactly — no used-extent compaction, hence no OXP-104-style
//! *compaction* hazard. This does **not** exempt the search from a genuinely
//! `Blank` cell that *is* present in the declared rectangle, though: a
//! `table_array` wider than the sheet's populated data puts real `Blank`
//! cells in the search row (the worst case here — every column past the
//! populated data is `Blank`), and the binary search can probe one directly;
//! [`approx_search`]'s touch-a-`Blank` defer applies uniformly to this path,
//! same as the bounded one. See `rfcs/0006-in-rectangle-absolute-read.md`.
//!
//! Bounded ranges (`A1:D100`), array constants, and a single-cell `table_array`
//! keep their existing behavior (the dense walk still serves them).
//!
//! # Oracle-deferred
//! - **`OXP-088`** — shared with VLOOKUP: the *exact* answers approximate
//!   mode returns on **unsorted** data, duplicate-key tie-breaking, and
//!   whether a comparison error encountered *during* the search aborts or is
//!   skipped. HLOOKUP.md explicitly calls for its own row-oriented oracle
//!   experiments (not assumed identical to VLOOKUP's column-oriented ones,
//!   even though the algorithm is shared code here).
//! - **`OXP-089`** — shared with VLOOKUP: exact-mode wildcard matching (`*`,
//!   `?`, `~` escaping) and the truncation direction for a non-integer
//!   `row_index_num` both defer to `#UNSUPPORTED!` rather than guess.
//! - **Blank-touch defer** — shared with VLOOKUP/MATCH/LOOKUP: approximate
//!   mode's binary search now refuses (`#UNSUPPORTED!`) whenever it would
//!   compare a `Blank` (the key, or a probed search-row cell) rather than let
//!   `compare`'s operator-only-pinned `Blank` morphs decide this unpinned
//!   lookup ordering. See [`approx_search`].

use std::cmp::Ordering;
use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value, compare, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::lookup::{LookupEq, approx_touches_blank, exact_eq};

/// Evaluate an `HLOOKUP(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // --- lookup_value (arg 0): evaluated in scalar context; error propagates.
    let lookup = args.eval_scalar(0);
    if let Value::Error(k) = lookup {
        return Value::Error(k);
    }

    // --- row_index_num (arg 2): numeric-coerced; error propagates.
    let row_index = match to_number(&args.eval_scalar(2)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    // OXP-089: a non-integer row_index_num's truncation direction is
    // unconfirmed (round vs truncate), so defer rather than guess.
    if row_index.fract() != 0.0 {
        return Value::Error(ErrorKind::Unsupported);
    }
    // row_index_num < 1 → #VALUE!. Checked independently of the table
    // height, which governs the #REF! case below.
    if row_index < 1.0 {
        return Value::Error(ErrorKind::Value);
    }

    // --- range_lookup (arg 3, optional): defaults to TRUE (approximate) when
    // the argument is absent. When present it is boolean-coerced via
    // xl-value; an error propagates.
    let approximate = if args.count() >= 4 {
        match xl_value::to_bool(&args.eval_scalar(3)) {
            Ok(b) => b,
            Err(k) => return Value::Error(k),
        }
    } else {
        true
    };

    // --- table_array (arg 1): buffer the rectangle positionally. `for_each_row`
    // surfaces blanks at their column positions and refuses an unbounded
    // whole-column/row range (the RFC-0001 dense-walk guardrail).
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let walk = args.for_each_row(1, &mut |row| {
        rows.push(row.to_vec());
        ControlFlow::Continue(())
    });
    if let Err(k) = walk {
        // Unbounded whole-column/row table_array: serve it by absolute
        // in-rectangle reads (search the first row, return from `row_index`)
        // rather than refuse — RFC 0006 / OXP-105. A non-reference argument
        // keeps the original refusal.
        return eval_whole_axis(args, &lookup, row_index, approximate, k);
    }

    // Table height governs the #REF! bound.
    let height = rows.len();
    // row_index_num > number of rows → #REF!.
    if (row_index as usize) > height {
        return Value::Error(ErrorKind::Ref);
    }
    let row = row_index as usize; // 1-based, validated 1..=height.

    // Width of the first row (the search row). A rectangle has a uniform
    // width; an empty table (no columns in the first row) has nothing to
    // match against.
    let width = rows.first().map_or(0, Vec::len);
    if width == 0 {
        return Value::Error(ErrorKind::Na);
    }

    // The search reads the first row positionally; `get(i)` is that row's
    // column `i` (or `Blank` for a short/missing row).
    let matched_col = {
        let mut get = |i: usize| {
            rows.first()
                .and_then(|r| r.get(i))
                .cloned()
                .unwrap_or(Value::Blank)
        };
        let res = if approximate {
            approx_search(width, &mut get, &lookup)
        } else {
            exact_search(width, &mut get, &lookup)
        };
        match res {
            Ok(Some(i)) => i,
            Ok(None) => return Value::Error(ErrorKind::Na),
            Err(k) => return Value::Error(k),
        }
    };

    // Return the value in the chosen row of the matched column. An error
    // sitting there is returned as-is, i.e. propagates.
    rows.get(row - 1)
        .and_then(|r| r.get(matched_col))
        .cloned()
        .unwrap_or(Value::Blank)
}

/// Whole-column/row `table_array` (OXP-105, RFC 0006): search the table's first
/// row and return from `row_index` by reading at **absolute positions inside the
/// declared rectangle**, never materialising the 1,048,576-row range.
///
/// The search row is the rectangle's **top** row (`rect.row`); each search-row
/// cell is read at its absolute column via [`CallArgs::cell_at`], so the
/// binary/linear scan touches only the (bounded) first row plus one result read,
/// not the unbounded row axis. `row_index_num` is bounded against the reference's
/// **full** sheet-axis height (`> height` → `#REF!`). Reading positionally
/// (blanks surfaced at their true columns) means the whole-column search matches
/// the equivalent bounded range exactly — there is no used-extent *compaction*
/// hazard. A genuinely `Blank` cell can still be probed here (e.g. a declared
/// width past the populated data), which [`approx_search`]'s touch-a-`Blank`
/// defer still catches — see the module docs.
///
/// `refused` is propagated unchanged when the argument is not a resolvable
/// single-area reference, keeping a genuine `#UNSUPPORTED!` distinguishable.
fn eval_whole_axis(
    args: &mut dyn CallArgs,
    lookup: &Value,
    row_index: f64, // validated: integer, >= 1
    approximate: bool,
    refused: ErrorKind,
) -> Value {
    let Some(rect) = args.arg_ref_extent(1) else {
        return Value::Error(refused);
    };
    // row_index_num > the reference's full row extent → #REF!.
    let row_index_u = row_index as u64;
    if row_index_u > u64::from(rect.height) {
        return Value::Error(ErrorKind::Ref);
    }
    let width = rect.width as usize; // columns of the table (search axis span)
    if width == 0 {
        return Value::Error(ErrorKind::Na);
    }

    let search_row = rect.row;
    let col_base = rect.col;
    // Search the first row at absolute positions; `get(i)` reads the search-row
    // cell at absolute column `col_base + i`.
    let matched_col = {
        let mut get = |i: usize| {
            args.cell_at(1, search_row, col_base + i as u32)
                .unwrap_or(Value::Blank)
        };
        let res = if approximate {
            approx_search(width, &mut get, lookup)
        } else {
            exact_search(width, &mut get, lookup)
        };
        match res {
            Ok(Some(i)) => i,
            Ok(None) => return Value::Error(ErrorKind::Na),
            Err(k) => return Value::Error(k),
        }
    };

    // Result cell at (row_index, matched col), absolute. An absent cell inside
    // the rectangle reads as blank (as it would for a bounded table).
    let abs_row = rect.row + (row_index_u as u32 - 1);
    let abs_col = col_base + matched_col as u32;
    args.cell_at(1, abs_row, abs_col).unwrap_or(Value::Blank)
}

/// Exact-match scan (`range_lookup = FALSE`): the first column (left to right) of
/// the first row whose cell equals `lookup` (case-insensitive text / cross-type
/// per [`crate::lookup::exact_eq`]). `get(i)` supplies the search-row cell at
/// column `i` (the same accessor for the buffered-bounded and absolute
/// whole-column paths). `Ok(None)` if no column matches.
///
/// Mirrors [`func_vlookup::exact_search`](crate::func_vlookup::exact_search):
/// wildcard patterns are **not** interpreted (OXP-089), and `Blank`-involved
/// pairs go through the shared, oracle-scoped [`crate::lookup::exact_eq`]
/// rather than a raw `values_equal` call (see that module's docs) — a `Blank`
/// lookup key deferring (`#UNSUPPORTED!`) on an unpinned `""`/`FALSE`/`Blank`
/// candidate rather than silently matching or silently skipping it.
fn exact_search(
    width: usize,
    get: &mut dyn FnMut(usize) -> Value,
    lookup: &Value,
) -> Result<Option<usize>, ErrorKind> {
    if let Value::Text(t) = lookup
        && t.as_str().contains(['*', '?'])
    {
        return Err(ErrorKind::Unsupported);
    }
    for i in 0..width {
        match exact_eq(&get(i), lookup) {
            Ok(LookupEq::Match) => return Ok(Some(i)),
            Ok(LookupEq::NoMatch) => {}
            Ok(LookupEq::Defer) => return Err(ErrorKind::Unsupported),
            Err(k) => return Err(k),
        }
    }
    Ok(None)
}

/// Approximate-match binary search (`range_lookup = TRUE`/omitted), mirroring
/// [`func_vlookup::approx_search`](crate::func_vlookup::approx_search) with the
/// first **row** walked instead of the first column. `get(i)` supplies the
/// search-row cell at column `i`. See that function's docs for the exact
/// probe-sequence rationale (shared algorithm; the row-oriented OXP-088
/// unsorted/duplicate/error behavior is not claimed as verified here).
///
/// A **`Blank` touching a comparison — the key, or a probed cell — defers**
/// (`Err(ErrorKind::Unsupported)`) instead of calling `compare`, for the same
/// reason as `VLOOKUP`'s `approx_search`: `compare`'s `Blank` morphs are
/// pinned only for operators, not this unpinned approximate ordering (see
/// [`crate::lookup::approx_touches_blank`]). This matters in particular for
/// the whole-column/row path ([`eval_whole_axis`]): a declared `table_array`
/// width wider than the sheet's populated data puts genuinely `Blank` cells
/// in the search row, which the binary search can probe directly. A search
/// that never touches a `Blank` is unaffected byte-for-byte.
fn approx_search(
    width: usize,
    get: &mut dyn FnMut(usize) -> Value,
    lookup: &Value,
) -> Result<Option<usize>, ErrorKind> {
    let mut lo: isize = 0;
    let mut hi: isize = width as isize - 1;
    let mut best: Option<usize> = None;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let cell = get(mid as usize);
        if approx_touches_blank(&cell, lookup) {
            return Err(ErrorKind::Unsupported);
        }
        match compare(&cell, lookup)? {
            Ordering::Greater => hi = mid - 1,
            Ordering::Less | Ordering::Equal => {
                best = Some(mid as usize);
                lo = mid + 1;
            }
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::args::RefRect;
    use crate::test_support::{TestArg, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    /// A whole-column `A:E` `table_array` (RFC 0006): 5 columns, full row extent.
    /// Search row (row 0): `[1, 3, 5]` in A/B/C (D/E blank, sorted ascending).
    /// Result row (row 1): `[10, 30, 50]` in A/B/C.
    fn whole_column_table() -> TestArg {
        TestArg::RefCells {
            rect: RefRect {
                row: 0,
                col: 0,
                height: 1_048_576,
                width: 5,
            },
            cells: vec![
                (0, 0, num(1.0)),
                (0, 1, num(3.0)),
                (0, 2, num(5.0)),
                (1, 0, num(10.0)),
                (1, 1, num(30.0)),
                (1, 2, num(50.0)),
            ],
        }
    }

    /// OXP-105, RFC 0006: whole-column HLOOKUP now searches the first row and
    /// returns from `row_index` via absolute in-rectangle reads.
    /// `HLOOKUP(3, A:E, 2)` (approximate) matches column B and returns `B2 = 30`.
    #[test]
    fn hlookup_whole_column_approximate() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(num(3.0)),
                    whole_column_table(),
                    TestArg::Scalar(num(2.0)),
                ]
            ),
            num(30.0)
        );
    }

    /// Exact mode (`range_lookup = FALSE`) over the same whole-column table:
    /// the first column whose search-row cell equals `3` is B → `B2 = 30`.
    #[test]
    fn hlookup_whole_column_exact() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(num(3.0)),
                    whole_column_table(),
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            num(30.0)
        );
    }

    /// `row_index_num` beyond the reference's full sheet-axis height → `#REF!`.
    #[test]
    fn hlookup_whole_column_row_index_out_of_bounds_is_ref() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(num(3.0)),
                    whole_column_table(),
                    TestArg::Scalar(num(1_048_577.0)),
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    /// A key smaller than every search-row cell → `#N/A` (approximate mode).
    #[test]
    fn hlookup_whole_column_no_match_is_na() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(num(0.0)),
                    whole_column_table(),
                    TestArg::Scalar(num(2.0)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // --- Blank-scoped exact equality (`crate::lookup::exact_eq`) -----------

    /// Pinned NO-MATCH (OXP-104): a Blank key over `{1, 2, <blank>, 4}` (row
    /// form) never matches — the non-zero cells are ordinary skips, the
    /// truly-blank cell is a confirmed no-match — so the scan reaches the end
    /// → `#N/A`.
    #[test]
    fn hlookup_blank_key_no_zero_cell_is_na() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(Value::Blank),
                    TestArg::Rect {
                        rows: 2,
                        cols: 4,
                        data: vec![
                            num(1.0),
                            num(2.0),
                            Value::Blank,
                            num(4.0),
                            txt("r1"),
                            txt("r2"),
                            txt("r3"),
                            txt("r4"),
                        ],
                    },
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    /// DEFER: a Blank key's first candidate is a populated `""` cell — unpinned.
    #[test]
    fn hlookup_blank_key_vs_empty_text_first_candidate_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(Value::Blank),
                    TestArg::Rect {
                        rows: 2,
                        cols: 2,
                        data: vec![txt(""), num(5.0), num(100.0), num(500.0)],
                    },
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// DEFER: a real (non-Blank) `0` key against a truly-`Blank` first
    /// search-row cell — the unpinned reverse direction of OXP-165.
    #[test]
    fn hlookup_zero_key_vs_blank_cell_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(num(0.0)),
                    TestArg::Rect {
                        rows: 2,
                        cols: 1,
                        data: vec![Value::Blank, num(100.0)],
                    },
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// UNCHANGED: cross-type strictness (`5 <> "5"`) is untouched.
    #[test]
    fn hlookup_number_key_does_not_match_text_digit_cell() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(num(5.0)),
                    TestArg::Rect {
                        rows: 2,
                        cols: 1,
                        data: vec![txt("5"), num(999.0)],
                    },
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // --- Approximate-mode touch-a-Blank defer (this fix) --------------------

    /// THE BUG THIS FIX CLOSES: a Blank key in APPROXIMATE mode over a
    /// bounded `table_array` used to have no guard at all (HLOOKUP never had
    /// even VLOOKUP's whole-column-only ad-hoc pre-check). It must now defer.
    #[test]
    fn hlookup_blank_key_approx_mode_over_bounded_range_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(Value::Blank),
                    TestArg::Rect {
                        rows: 1,
                        cols: 3,
                        data: vec![num(1.0), num(3.0), num(5.0)],
                    },
                    TestArg::Scalar(num(1.0)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// THE BUG THIS FIX CLOSES: `HLOOKUP(999, <20-col row>, 2, TRUE)` over a
    /// search row populated only in columns 1-10 (ascending 1..10) with a
    /// Blank tail in columns 11-20. The search must defer the moment it
    /// probes a Blank column, instead of silently walking through it.
    #[test]
    fn hlookup_blank_cell_touched_by_approx_search_defers() {
        let mut data: Vec<Value> = Vec::new();
        for i in 1..=10 {
            data.push(num(f64::from(i)));
        }
        for _ in 0..10 {
            data.push(Value::Blank);
        }
        for i in 0..20 {
            data.push(num(f64::from(i))); // result row, unreached
        }
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(num(999.0)),
                    TestArg::Rect {
                        rows: 2,
                        cols: 20,
                        data,
                    },
                    TestArg::Scalar(num(2.0)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// THE BUG THIS FIX CLOSES, whole-column variant (RFC 0006 / OXP-105):
    /// `whole_column_table()` declares 5 columns but only A/B/C (indices
    /// 0-2) are populated in the search row; D/E (indices 3-4) are genuinely
    /// Blank. `HLOOKUP(10, A:E, 2)` (approximate) probes into D/E — before
    /// this fix `compare` would morph those Blank cells to `0`, walk the
    /// search to the last (Blank) column, and silently return a Blank
    /// result cell. It must now defer instead.
    #[test]
    fn hlookup_whole_column_blank_trailing_columns_defer() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(num(10.0)),
                    whole_column_table(),
                    TestArg::Scalar(num(2.0)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// Blank-free approximate search, plain bounded range: byte-identical
    /// best-tracking answer, unaffected by the touch-a-Blank defer.
    #[test]
    fn hlookup_blank_free_approx_search_still_works() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(num(4.0)),
                    TestArg::Rect {
                        rows: 2,
                        cols: 3,
                        data: vec![
                            num(1.0),
                            num(3.0),
                            num(5.0),
                            num(10.0),
                            num(30.0),
                            num(50.0),
                        ],
                    },
                    TestArg::Scalar(num(2.0)),
                ]
            ),
            num(30.0)
        );
    }
}

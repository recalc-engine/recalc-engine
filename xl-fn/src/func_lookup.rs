//! `LOOKUP` — approximate-match lookup over a vector or a 2-D array, in either
//! of the function's two documented forms.
//!
//! # Provenance
//! Behavior contract: `docs/specs/LOOKUP.md` (which cites the Microsoft
//! `support.microsoft.com` LOOKUP page, verified 2026-07-08). **All** value
//! comparison is deferred to `xl-value` ([`compare`]) — this module never
//! re-implements ordering, so LOOKUP inherits Excel's case-insensitive text rule
//! and cross-type ordering (`Number < Text < Bool`) automatically, exactly as
//! `VLOOKUP`'s approximate mode does (the search here is the *same* algorithm;
//! see [`approx_search`] and `func_vlookup`).
//!
//! # The two forms (LOOKUP.md §Signature)
//! LOOKUP has no exact-match mode: it *always* assumes its search data is sorted
//! ascending and returns the largest value `<= lookup_value` (an `#N/A` if
//! `lookup_value` is smaller than every entry). The two forms differ only in
//! *where* the search sequence and the aligned result sequence come from:
//!
//! - **Vector form** — `LOOKUP(lookup_value, lookup_vector, [result_vector])`.
//!   `lookup_vector` is a single row or single column, assumed sorted ascending.
//!   The matched position is looked up in `result_vector` (same size as
//!   `lookup_vector`), or in `lookup_vector` itself when `result_vector` is
//!   omitted. Selected by a 3-argument call, or a 2-argument call whose second
//!   argument is a vector (one row or one column).
//! - **Array form** — `LOOKUP(lookup_value, array)`. If `array` is **wider than
//!   tall** (more columns than rows) LOOKUP searches the **first row** and
//!   returns the correspondingly-positioned cell of the **last row**; otherwise
//!   (square, or taller than wide) it searches the **first column** and returns
//!   from the **last column** (LOOKUP.md §Array form — the square case is
//!   documented as "searches in the first column"). Selected by a 2-argument
//!   call whose second argument is a genuine 2-D rectangle. The MS page notes
//!   `VLOOKUP`/`HLOOKUP` are usually preferred to the array form.
//!
//! Both forms reduce to the same core: build a `search` vector and an aligned
//! `result` vector of the same length, binary-search `search` for the largest
//! `<= lookup_value`, and return the same-positioned element of `result`.
//!
//! # Whole-column / unbounded ranges — deferred (not the used-extent path)
//! Unlike `VLOOKUP` (which opts into the RFC-0001 used-extent walk), LOOKUP
//! buffers its operand rectangle via the **dense** [`CallArgs::for_each_row`]
//! walk, which *refuses* an unbounded whole-column/row range with
//! `Err(ErrorKind::Unsupported)`. That refusal is surfaced as LOOKUP's result:
//! a whole-column `lookup_vector`/`array`/`result_vector` yields `#UNSUPPORTED!`
//! rather than a silently-wrong answer. The used-extent path (populated rows
//! only) is deliberately **out of scope** here — LOOKUP's array-form shape rule
//! (wider-than-tall vs taller-than-wide) is defined over the *materialized*
//! rectangle's dimensions, which a compacted populated-rows view would distort.
//! Bounded ranges, array constants, and scalars are fully supported.
//!
//! # `Blank`-involved equality
//! Unlike `VLOOKUP`/`HLOOKUP`/`MATCH`, LOOKUP has **no exact-match linear
//! scan** at all (see "The two forms" above — it is always the binary
//! [`approx_search`]), so the scoped, oracle-pinned
//! [`crate::lookup::exact_eq`] shared by those three functions' exact mode
//! does not apply here: there is no `exact_search`-style call site to route
//! through it. What *does* apply — shared with the other three functions'
//! approximate-mode search — is the touch-a-`Blank` defer: [`approx_search`]
//! now refuses (`Err(ErrorKind::Unsupported)`) whenever it would compare a
//! `Blank` (the lookup key, or a probed `search` element) instead of calling
//! `compare` directly. `compare`'s `Blank` morphs are pinned only for the
//! `=`/`<>`/ordering **operators**, not for this unpinned approximate lookup
//! ordering (OXP-088's probe data had no blanks; OXP-104 marks
//! interspersed-blank ordering unverified) — see
//! [`crate::lookup::approx_touches_blank`] and that function's own docs. A
//! `Blank`-free search is unaffected byte-for-byte.
//!
//! # Oracle-resolved (RUN-2026-07-11-oracle01)
//! - **`OXP-088`** (shared with `VLOOKUP`) — the *exact* answers approximate
//!   search returns on **unsorted**/duplicate data are pinned to Excel: a
//!   **floor**-midpoint binary search that returns on an exact hit immediately
//!   (so equal keys resolve to whichever the probe lands on — not "bottom-most")
//!   and otherwise settles on index `hi`. An error entry at a probe midpoint
//!   does not abort the search (Excel moves right past it). See [`approx_search`]
//!   (the identical algorithm to `VLOOKUP`'s).
//! - **`OXP-114`** — LOOKUP-specific corners now pinned by the oracle: (a) a
//!   `result_vector` shorter than `lookup_vector` returns the aligned cell when
//!   the matched position is within it (H1: `LOOKUP(3, A1:A5, C1:C3)` → `r3`),
//!   and `#N/A` when the position falls outside it; (b) a genuinely 2-D
//!   lookup_vector in the 3-argument vector form is resolved by the array-form
//!   orientation rule (H2: `LOOKUP(3, A1:B5, C1:C5)` searches column A → `r3`).
//!   A 2-D *result_vector* remains `#UNSUPPORTED!` (unprobed — not guessed).

use std::cmp::Ordering;
use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value, compare};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::lookup::approx_touches_blank;

/// Evaluate a `LOOKUP(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // --- lookup_value (arg 0): scalar context; error propagates immediately.
    let lookup = args.eval_scalar(0);
    if let Value::Error(k) = lookup {
        return Value::Error(k);
    }

    // --- arg 1 (lookup_vector / array): buffer the dense rectangle. The dense
    // walk refuses an unbounded whole-column/row range (→ #UNSUPPORTED!), which
    // is exactly the guardrail we want (the used-extent path is out of scope).
    let arr = match buffer_rect(args, 1) {
        Ok(rows) => rows,
        Err(k) => return Value::Error(k),
    };
    let nrows = arr.len();
    let ncols = arr.iter().map(Vec::len).max().unwrap_or(0);

    // Decide the form and build the aligned (search, result) vectors.
    let (search, result): (Vec<Value>, Vec<Value>) = if args.count() >= 3 {
        // --- Vector form with an explicit result_vector.
        let search = match linearize(&arr) {
            Some(v) => v,
            // OXP-114 (RUN-2026-07-11-oracle01): a genuine 2-D lookup_vector in
            // the 3-argument vector form is resolved by LOOKUP's array-form
            // orientation rule — first column when the block is taller-than-wide
            // or square, else first row. Observed: `=LOOKUP(3, A1:B5, C1:C5)`
            // over a 5×2 block searches column A and returns `r3`.
            None => oriented_search_vector(&arr, nrows, ncols),
        };
        let res_rect = match buffer_rect(args, 2) {
            Ok(rows) => rows,
            Err(k) => return Value::Error(k),
        };
        let result = match linearize(&res_rect) {
            Some(v) => v,
            None => return Value::Error(ErrorKind::Unsupported),
        };
        (search, result)
    } else if nrows <= 1 || ncols <= 1 {
        // --- Vector form, result omitted: search and return the same vector.
        // A single row or single column (including 1×1) linearizes cleanly.
        let search = match linearize(&arr) {
            Some(v) => v,
            None => return Value::Error(ErrorKind::Unsupported),
        };
        (search.clone(), search)
    } else if ncols > nrows {
        // --- Array form, wider than tall: search first row, return last row.
        (arr[0].clone(), arr[nrows - 1].clone())
    } else {
        // --- Array form, square or taller: search first column, return last.
        // (The square case is documented as "searches in the first column".)
        let search = arr
            .iter()
            .map(|r| r.first().cloned().unwrap_or(Value::Blank))
            .collect();
        let result = arr
            .iter()
            .map(|r| r.get(ncols - 1).cloned().unwrap_or(Value::Blank))
            .collect();
        (search, result)
    };

    // Binary-search the (assumed-sorted) search vector for the largest <= lookup.
    match approx_search(&search, &lookup) {
        // Return the aligned result cell. An error sitting there is returned
        // as-is, i.e. propagates. A position outside a mismatched-length
        // result vector → #N/A (OXP-114).
        Ok(Some(i)) => result
            .get(i)
            .cloned()
            .unwrap_or(Value::Error(ErrorKind::Na)),
        // lookup_value smaller than every entry (or empty) → #N/A.
        Ok(None) => Value::Error(ErrorKind::Na),
        // A comparison error encountered mid-search propagates (OXP-088).
        Err(k) => Value::Error(k),
    }
}

/// Buffer a range/array argument's dense rectangle, row by row.
///
/// Uses the dense [`CallArgs::for_each_row`] walk (blanks surfaced positionally,
/// a scalar treated as a 1×1 rectangle). An unbounded whole-column/row range —
/// or an argument that resolves to no rectangle — returns `Err(Unsupported)`,
/// which the caller surfaces as LOOKUP's `#UNSUPPORTED!`.
fn buffer_rect(args: &mut dyn CallArgs, index: usize) -> Result<Vec<Vec<Value>>, ErrorKind> {
    let mut rows: Vec<Vec<Value>> = Vec::new();
    args.for_each_row(index, &mut |row| {
        rows.push(row.to_vec());
        ControlFlow::Continue(())
    })?;
    Ok(rows)
}

/// Flatten a buffered rectangle into a single vector *iff* it is a vector — a
/// single row, a single column, or 1×1. Returns `None` for a genuine 2-D
/// rectangle (both dimensions > 1), which the vector form does not define
/// (OXP-114).
fn linearize(rows: &[Vec<Value>]) -> Option<Vec<Value>> {
    let ncols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if rows.len() <= 1 {
        // Empty, or a single row: take it as-is.
        Some(rows.first().cloned().unwrap_or_default())
    } else if ncols <= 1 {
        // A single column: take each row's first cell.
        Some(
            rows.iter()
                .map(|r| r.first().cloned().unwrap_or(Value::Blank))
                .collect(),
        )
    } else {
        None
    }
}

/// Extract a search vector from a genuine 2-D rectangle by LOOKUP's array-form
/// orientation rule: the **first row** when the block is wider-than-tall, else
/// (taller-than-wide or square) the **first column** (OXP-114).
fn oriented_search_vector(rows: &[Vec<Value>], nrows: usize, ncols: usize) -> Vec<Value> {
    if ncols > nrows {
        rows.first().cloned().unwrap_or_default()
    } else {
        rows.iter()
            .map(|r| r.first().cloned().unwrap_or(Value::Blank))
            .collect()
    }
}

/// Approximate-match binary search — the **same** algorithm as `VLOOKUP`'s
/// approximate mode, operating on a linear sequence (pinned by **OXP-088**,
/// RUN-2026-07-11-oracle01).
///
/// Excel assumes `seq` is sorted ascending and binary-searches with a **floor**
/// midpoint `mid = lo + (hi - lo) / 2`: an **exact** hit returns that probe's
/// index immediately (so duplicate/unsorted equal keys resolve to whichever the
/// probe lands on), otherwise `< lookup` ⇒ `lo = mid + 1` and `> lookup` ⇒
/// `hi = mid - 1`. After the loop Excel returns index `hi` (the last `<= lookup`
/// probe on sorted data; its algorithm-order-dependent answer on unsorted data),
/// or `Ok(None)` (→ `#N/A`) when `hi < 0`. An error entry at a probe midpoint
/// does not abort the search (Excel moves right past it, `lo = mid + 1`); a
/// non-error comparison that itself errors still propagates as `Err`.
///
/// A **`Blank` touching a comparison — the key, or a probed entry — defers**
/// (`Err(ErrorKind::Unsupported)`) instead of calling `compare`: `compare`'s
/// `Blank` morphs are pinned only for operators, not this unpinned
/// approximate ordering (see [`crate::lookup::approx_touches_blank`]). The
/// check sits after the error-entry skip above, so an error entry is still
/// skipped without ever "touching" the key, and a `Blank`-free search is
/// unaffected byte-for-byte.
fn approx_search(seq: &[Value], lookup: &Value) -> Result<Option<usize>, ErrorKind> {
    let mut lo: isize = 0;
    let mut hi: isize = seq.len() as isize - 1;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let cell = &seq[mid as usize];
        if matches!(cell, Value::Error(_)) {
            lo = mid + 1;
            continue;
        }
        if approx_touches_blank(cell, lookup) {
            return Err(ErrorKind::Unsupported);
        }
        match compare(cell, lookup)? {
            Ordering::Equal => return Ok(Some(mid as usize)),
            Ordering::Less => lo = mid + 1,
            Ordering::Greater => hi = mid - 1,
        }
    }
    if hi < 0 {
        Ok(None)
    } else {
        Ok(Some(hi as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{
        TestArg::{self, *},
        eval_direct, num, txt,
    };
    use xl_value::{ErrorKind, Value};

    fn call(args: Vec<TestArg>) -> Value {
        eval_direct(eval, args)
    }

    // --- Vector form, result_vector omitted -------------------------------

    #[test]
    fn vector_form_column_no_result_largest_le() {
        // lookup_vector = column [1,3,5,7]; lookup 4 → largest <= 4 is 3.
        let v = call(vec![
            Scalar(num(4.0)),
            Range(vec![num(1.0), num(3.0), num(5.0), num(7.0)]),
        ]);
        assert_eq!(v, num(3.0));
    }

    #[test]
    fn vector_form_row_no_result_exact_hit() {
        // lookup_vector = row {1,3,5}; lookup 5 → 5 (exact, top boundary).
        let v = call(vec![
            Scalar(num(5.0)),
            Array(vec![num(1.0), num(3.0), num(5.0)]),
        ]);
        assert_eq!(v, num(5.0));
    }

    // --- Vector form, explicit result_vector ------------------------------

    #[test]
    fn vector_form_with_result_vector() {
        // lookup [1,3,5], result [10,30,50]; lookup 4 → position of 3 → 30.
        let v = call(vec![
            Scalar(num(4.0)),
            Range(vec![num(1.0), num(3.0), num(5.0)]),
            Range(vec![num(10.0), num(30.0), num(50.0)]),
        ]);
        assert_eq!(v, num(30.0));
    }

    #[test]
    fn vector_form_result_orientation_may_differ() {
        // lookup_vector a column, result_vector a row of the same length; the
        // match maps positionally regardless of orientation.
        let v = call(vec![
            Scalar(num(2.0)),
            Range(vec![num(1.0), num(2.0), num(3.0)]),
            Array(vec![txt("a"), txt("b"), txt("c")]),
        ]);
        assert_eq!(v, txt("b"));
    }

    // --- #N/A below all ----------------------------------------------------

    #[test]
    fn below_all_is_na() {
        // lookup 1 is smaller than every entry of [2,4,6] → #N/A.
        let v = call(vec![
            Scalar(num(1.0)),
            Range(vec![num(2.0), num(4.0), num(6.0)]),
        ]);
        assert_eq!(v, Value::Error(ErrorKind::Na));
    }

    // --- Array form, wider than tall (search row 0, return last row) -------

    #[test]
    fn array_form_wide_searches_first_row_returns_last_row() {
        // 2×3, wider than tall: search row0 [1,3,5], return row1 [10,30,50].
        // lookup 4 → position of 3 (index 1) → 30.
        let v = call(vec![
            Scalar(num(4.0)),
            Rect {
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
        ]);
        assert_eq!(v, num(30.0));
    }

    // --- Array form, taller than wide (search col 0, return last col) ------

    #[test]
    fn array_form_tall_searches_first_col_returns_last_col() {
        // 3×2, taller than wide: search col0 [1,3,5], return col1 [a,b,c].
        // lookup 5 → index 2 → "c".
        let v = call(vec![
            Scalar(num(5.0)),
            Rect {
                rows: 3,
                cols: 2,
                data: vec![num(1.0), txt("a"), num(3.0), txt("b"), num(5.0), txt("c")],
            },
        ]);
        assert_eq!(v, txt("c"));
    }

    // --- Array form, square (documented: searches the FIRST column) --------

    #[test]
    fn array_form_square_searches_first_col() {
        // 2×2 square → first column [1,3], return last column [x,y].
        // lookup 3 → index 1 → "y".
        let v = call(vec![
            Scalar(num(3.0)),
            Rect {
                rows: 2,
                cols: 2,
                data: vec![num(1.0), txt("x"), num(3.0), txt("y")],
            },
        ]);
        assert_eq!(v, txt("y"));
    }

    // --- Duplicate keys: the probe's exact hit wins (OXP-088) --------------

    #[test]
    fn duplicate_keys_return_probe_hit() {
        // RUN-2026-07-11-oracle01 / OXP-088: lookup 2 over [1,2,2,3]; Excel's
        // binary search returns on the exact hit at the floor midpoint (index 1),
        // not a "bottom-most" rule → result 20.
        let v = call(vec![
            Scalar(num(2.0)),
            Range(vec![num(1.0), num(2.0), num(2.0), num(3.0)]),
            Range(vec![num(10.0), num(20.0), num(21.0), num(30.0)]),
        ]);
        assert_eq!(v, num(20.0));
    }

    // --- OXP-114: short result vector, and 2-D lookup vector (vector form) --

    #[test]
    fn oxp114_short_result_vector_position_in_range() {
        // RUN-2026-07-11-oracle01 / OXP-114 H1: LOOKUP(3, A1:A5, C1:C3). The
        // matched position (3) is within the shorter result vector → r3.
        let v = call(vec![
            Scalar(num(3.0)),
            Range(vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0)]),
            Range(vec![txt("r1"), txt("r2"), txt("r3")]),
        ]);
        assert_eq!(v, txt("r3"));
    }

    #[test]
    fn oxp114_two_d_lookup_vector_uses_orientation() {
        // RUN-2026-07-11-oracle01 / OXP-114 H2: LOOKUP(3, A1:B5, C1:C5). The 5×2
        // lookup block is taller-than-wide → search first column [1..5]; the
        // separate result vector C1:C5 gives r3.
        let v = call(vec![
            Scalar(num(3.0)),
            Rect {
                rows: 5,
                cols: 2,
                data: vec![
                    num(1.0),
                    num(101.0),
                    num(2.0),
                    num(102.0),
                    num(3.0),
                    num(103.0),
                    num(4.0),
                    num(104.0),
                    num(5.0),
                    num(105.0),
                ],
            },
            Range(vec![txt("r1"), txt("r2"), txt("r3"), txt("r4"), txt("r5")]),
        ]);
        assert_eq!(v, txt("r3"));
    }

    // --- Error propagation -------------------------------------------------

    #[test]
    fn lookup_value_error_propagates() {
        let v = call(vec![
            Scalar(Value::Error(ErrorKind::Div0)),
            Range(vec![num(1.0), num(2.0), num(3.0)]),
        ]);
        assert_eq!(v, Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn error_in_returned_cell_propagates() {
        // The matched result cell holds an error → returned as-is.
        let v = call(vec![
            Scalar(num(2.0)),
            Range(vec![num(1.0), num(2.0), num(3.0)]),
            Range(vec![num(10.0), Value::Error(ErrorKind::Ref), num(30.0)]),
        ]);
        assert_eq!(v, Value::Error(ErrorKind::Ref));
    }

    // --- Whole-column / unbounded → #UNSUPPORTED! (dense-walk refusal) -----

    #[test]
    fn unbounded_lookup_vector_is_unsupported() {
        let v = call(vec![
            Scalar(num(2.0)),
            Unbounded(vec![num(1.0), num(2.0), num(3.0)]),
        ]);
        assert_eq!(v, Value::Error(ErrorKind::Unsupported));
    }

    // --- Genuine 2-D vector-form argument → #UNSUPPORTED! (OXP-114) --------

    #[test]
    fn two_d_result_vector_is_unsupported() {
        // A 2×2 result argument in the (3-arg) vector form is undocumented.
        let v = call(vec![
            Scalar(num(2.0)),
            Range(vec![num(1.0), num(2.0)]),
            Rect {
                rows: 2,
                cols: 2,
                data: vec![num(10.0), num(11.0), num(20.0), num(21.0)],
            },
        ]);
        assert_eq!(v, Value::Error(ErrorKind::Unsupported));
    }

    // --- Approximate-search touch-a-Blank defer (this fix) -----------------
    // LOOKUP is *always* approximate, and (unlike VLOOKUP/MATCH) never had
    // any ad-hoc Blank-key guard at all before this fix.

    /// THE BUG THIS FIX CLOSES: a Blank `lookup_value` used to fall straight
    /// through to the raw `compare` call with no guard whatsoever. It must
    /// now defer.
    #[test]
    fn blank_key_defers() {
        let v = call(vec![
            Scalar(Value::Blank),
            Range(vec![num(1.0), num(3.0), num(5.0)]),
        ]);
        assert_eq!(v, Value::Error(ErrorKind::Unsupported));
    }

    /// THE BUG THIS FIX CLOSES: `LOOKUP(999, <20-element vector>)` over a
    /// vector populated only in positions 1-10 (ascending 1..10) with a
    /// Blank tail in positions 11-20. Before this fix the search would probe
    /// into the Blank tail, have `compare` morph a Blank entry to `0`, and
    /// silently settle on a Blank result instead of recognizing the ordering
    /// as unpinned. It must now defer the moment it touches a Blank entry.
    #[test]
    fn blank_entry_touched_by_approx_search_defers() {
        let mut data: Vec<Value> = Vec::new();
        for i in 1..=10 {
            data.push(num(f64::from(i)));
        }
        for _ in 0..10 {
            data.push(Value::Blank);
        }
        let v = call(vec![Scalar(num(999.0)), Range(data)]);
        assert_eq!(v, Value::Error(ErrorKind::Unsupported));
    }

    /// Blank-free approximate search is byte-identical to before this fix —
    /// re-affirms the OXP-088 duplicate-key pinned case explicitly.
    #[test]
    fn blank_free_approx_search_still_byte_identical() {
        let v = call(vec![
            Scalar(num(2.0)),
            Range(vec![num(1.0), num(2.0), num(2.0), num(3.0)]),
            Range(vec![num(10.0), num(20.0), num(21.0), num(30.0)]),
        ]);
        assert_eq!(v, num(20.0)); // OXP-088, unchanged (see duplicate_keys_return_probe_hit).
    }
}

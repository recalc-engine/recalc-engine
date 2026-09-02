//! `MATCH` — return the 1-based **position** of `lookup_value` within a 1-D
//! `lookup_array`, not the value itself.
//!
//! # Provenance
//! Behavior contract: `docs/specs/MATCH.md` (verified 2026-07-05). Value
//! ordering (`match_type = 1`/`-1`) is deferred to `xl-value` ([`compare`]),
//! and non-`Blank`-involved equality (`match_type = 0`) to `values_equal` —
//! this module never re-implements those. `match_type = 0`'s `Blank`-involved
//! equality instead goes through the shared, oracle-scoped
//! [`crate::lookup::exact_eq`] (also used by VLOOKUP/HLOOKUP) — see that
//! module's docs. The `match_type = 1`/`-1` binary-search modes reuse
//! [`func_vlookup`](crate::func_vlookup)'s exact algorithm shape (`1` is
//! textually the same probe sequence as [`func_vlookup::approx_search`]);
//! `-1` is that same probe sequence with the comparison mirrored for a
//! descending array. `match_type = 0` reuses
//! [`func_vlookup::exact_search`]'s linear-scan-plus-wildcard-deferral
//! shape.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `MATCH(lookup_value, lookup_array, [match_type])`, `match_type`
//!   defaults to `1` (MATCH.md §Signature).
//! - `match_type = 1` (default): `lookup_array` assumed sorted ascending;
//!   returns the position of the largest value `<= lookup_value` via
//!   binary search (MATCH.md §2) — the same probe sequence as VLOOKUP's
//!   approximate mode, so it inherits VLOOKUP's documented "wrong answer on
//!   unsorted data" behavior rather than a "corrected" linear scan.
//! - `match_type = 0`: exact match via a linear top-to-bottom scan
//!   (case-insensitive text / cross-type per [`crate::lookup::exact_eq`]);
//!   the first match wins (MATCH.md §3).
//! - `match_type = -1`: `lookup_array` assumed sorted descending; returns
//!   the position of the smallest value `>= lookup_value` via a mirrored
//!   binary search (MATCH.md §4).
//! - No match under the applicable mode → `#N/A` (MATCH.md §5).
//! - `lookup_array` must be a single row or column (MATCH.md §Signature); a
//!   genuinely 2-D rectangle is oracle-deferred (see below).
//!
//! # Oracle-deferred
//! - **`OXP-088`** (shared with VLOOKUP/HLOOKUP) — the exact answers
//!   `match_type = 1`/`-1` binary search returns on **unsorted** data,
//!   duplicate-key tie-breaking, and whether a comparison error hit
//!   *during* the search aborts or is skipped.
//! - **`OXP-089`** (shared with VLOOKUP/HLOOKUP) — `match_type = 0`
//!   wildcard matching (`*`, `?`, `~` escaping) for text `lookup_value`.
//! - **Invalid `match_type`** (any integer other than `-1`, `0`, `1`, or a
//!   non-integer value): MATCH.md documents this as unconfirmed ("behavior
//!   on other integers — error type — needs confirmation"), so it returns
//!   `#UNSUPPORTED!` rather than guessing `#N/A` vs `#VALUE!`.
//! - **A genuinely 2-D `lookup_array`** (more than one row *and* more than
//!   one column): MATCH is documented only for a single row or column;
//!   there is no prose-documented flattening order for a 2-D shape, so it
//!   returns `#UNSUPPORTED!` rather than guess row-major vs. column-major.
//!
//! # Whole-column `lookup_array` (`A:A`) — used-extent iteration (RFC 0001)
//! A whole-**column** `lookup_array` is searched over its **populated** cells
//! via the used-extent walk ([`for_each_row_or_used`]). MATCH returns a 1-based
//! **position**, so — unlike VLOOKUP, which returns a value — the compaction is
//! *not* invisible: the position returned is the matched cell's **relative row
//! within the range** (`relative row + 1`), computed from the row index the
//! used-extent walk yields, **not** its index among the populated cells. So
//! `MATCH(x, A:A, 0)` where `x` sits in `A7` returns `7` even if `A1:A6` are
//! blank/absent. Only a single-column whole range is supported: a multi-column
//! whole-column range (`A:D`) is genuinely 2-D → `#UNSUPPORTED!`.
//!
//! # Whole-**row** `lookup_array` (`1:1`) — used-extent COLUMN iteration (RFC 0008)
//! The horizontal transpose: a **single-row** whole-row `lookup_array` is
//! searched over its **populated columns** via the used-extent COLUMN walk
//! ([`match_used_extent_cols`]), returning `relative column + 1`. A **multi-row**
//! whole-row range (`1:5`) is genuinely 2-D → `#UNSUPPORTED!` (the transpose of
//! the `width > 1` whole-column rule). A `Blank` `lookup_value` over a whole-row
//! range **defers unconditionally** — OXP-165 pins blank-matches-`0` only on the
//! whole-column axis, so the whole-row axis is unpinned and never guessed.
//!
//! ## Blank `lookup_value` over a whole column — OXP-165 (exact) / OXP-104 (rest)
//! `RUN-2026-07-11-oracle01` experiment **OXP-165** pins that in **exact** mode a
//! `Blank` lookup key **matches a `0`-valued cell**: over `A = [0, "", <blank>,
//! 5]`, `MATCH(<blank>, A:A, 0) = 1` — the leading `0`-cell, at position 1. This
//! is exactly what [`crate::lookup::exact_eq`] returns for that pair (no
//! override needed); [`match_used_extent`] no longer blanket-defers a `Blank`
//! key in exact mode. **`Blank` ↔ `""`/`Blank` ↔ `Blank`, unlike the raw
//! operator contract, do *not* silently match** — `exact_eq` overrides
//! `Blank` vs a truly-`Blank` cell to a confirmed no-match (OXP-104) and
//! defers (`#UNSUPPORTED!`) on the unpinned `Blank` vs populated `""`/`FALSE`
//! candidates; see that module's docs for the full table. This resolves
//! **OXP-104's clean half**: a populated match is trusted only when no
//! *absent* row precedes it (the populated rows are contiguous from the top
//! through the match, `rels[i] == i`) — otherwise it still defers
//! (`#UNSUPPORTED!`); this pre-existing contiguity guard is conservative but
//! left as-is (unrelated to and unaffected by the `exact_eq` scoping above —
//! see that module's own docs for why an absent row cannot actually hide an
//! extra match). A `Blank`-key exact scan that **completes with no match**
//! returns `#N/A` (used-extent clamp): **OXP-104 H1**
//! pins `MATCH(<blank>, A:A, 0)` over the whole column `{1, 2, <truly
//! blank>, 4}` to `#N/A`, and the blank-vs-blank NoMatch pin means the
//! absent rows are confirmed no-matches — the used-extent answer equals the
//! bounded walk's. Unpinned `Blank` pairs met mid-scan still Defer first,
//! bit-for-bit unchanged; an entirely EMPTY whole column with a `Blank` key
//! still defers (never probed). In the sorted-array modes (`match_type = 1`/`-1`) the
//! treatment of blank cells interspersed in the column is unverified
//! (OXP-104), so a `Blank` key there still defers — and, as of this fix,
//! **uniformly** so: [`ascending_search`]/[`descending_search`] now refuse
//! (`#UNSUPPORTED!`) whenever a probe would compare a `Blank` (the key, *or*
//! a probed element), over a **bounded** `lookup_array` too, not only a
//! whole column — `compare`'s `Blank` morphs are pinned for operators, not
//! for this unpinned lookup ordering (see
//! [`crate::lookup::approx_touches_blank`]). A search that never touches a
//! `Blank` is unaffected byte-for-byte.

use std::cmp::Ordering;
use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value, compare, to_number};

use crate::args::{CallArgs, for_each_row_or_used};
use crate::context::EvalContext;
use crate::lookup::{LookupEq, approx_touches_blank, exact_eq};

/// Evaluate a `MATCH(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // --- lookup_value (arg 0): evaluated in scalar context; error propagates.
    let lookup = args.eval_scalar(0);
    if let Value::Error(k) = lookup {
        return Value::Error(k);
    }

    // --- match_type (arg 2, optional): defaults to 1; numeric-coerced;
    // error propagates. Only -1/0/1 are documented; anything else
    // (including a non-integer) is oracle-deferred rather than guessed.
    let match_type = if args.count() >= 3 {
        match to_number(&args.eval_scalar(2)) {
            Ok(n) => n,
            Err(k) => return Value::Error(k),
        }
    } else {
        1.0
    };
    if match_type != -1.0 && match_type != 0.0 && match_type != 1.0 {
        return Value::Error(ErrorKind::Unsupported);
    }

    // --- lookup_array (arg 1): buffer the rectangle positionally, tracking each
    // row's relative index. A bounded range/array/scalar uses the dense walk; a
    // whole-**column** range falls back to the used-extent ROW walk (populated
    // rows); a whole-**row** range falls back to the used-extent COLUMN walk
    // (populated columns — RFC 0008), which the row path refuses.
    let mut rows: Vec<(u32, Vec<Value>)> = Vec::new();
    match for_each_row_or_used(args, 1, &mut |rel, row| {
        rows.push((rel, row.to_vec()));
        ControlFlow::Continue(())
    }) {
        Ok(true) => return match_used_extent(rows, match_type, &lookup),
        Ok(false) => { /* bounded/array/scalar → the flatten path below */ }
        // The row path refused (dense + used-row both up front, so `rows` is
        // still empty): a whole-row `lookup_array` (RFC 0008) or an unresolvable
        // range. Serve a single-ROW whole-row vector via the COLUMN walk; a
        // multi-row whole-row range is genuinely 2-D → `#UNSUPPORTED!`.
        Err(_) => {
            let mut cols: Vec<(u32, Vec<Value>)> = Vec::new();
            return match args.for_each_used_col(1, &mut |rel, col| {
                cols.push((rel, col.to_vec()));
                ControlFlow::Continue(())
            }) {
                Ok(()) => match_used_extent_cols(cols, match_type, &lookup),
                Err(k) => Value::Error(k),
            };
        }
    }

    // Bounded/array/scalar path — flatten to a 1-D array per MATCH.md's "single
    // row or column" contract (the relative index is unused here): a single
    // materialized row is used as-is (row vector, or a 1×1 scalar); more than one
    // row is treated as a column vector (each row's first element). A genuinely
    // 2-D shape (multiple rows where any row is wider than one cell) has no
    // documented flattening order.
    let rows: Vec<Vec<Value>> = rows.into_iter().map(|(_, cells)| cells).collect();
    let flat: Vec<Value> = if rows.len() <= 1 {
        rows.into_iter().next().unwrap_or_default()
    } else if rows.iter().all(|r| r.len() <= 1) {
        rows.into_iter()
            .map(|r| r.into_iter().next().unwrap_or(Value::Blank))
            .collect()
    } else {
        return Value::Error(ErrorKind::Unsupported);
    };

    if flat.is_empty() {
        return Value::Error(ErrorKind::Na);
    }

    match search(&flat, match_type, &lookup) {
        Ok(Some(i)) => Value::number((i + 1) as f64),
        Ok(None) => Value::Error(ErrorKind::Na),
        Err(k) => Value::Error(k),
    }
}

/// Dispatch to the search mode for a validated `match_type` (`-1`/`0`/`1`).
fn search(flat: &[Value], match_type: f64, lookup: &Value) -> Result<Option<usize>, ErrorKind> {
    if match_type == 0.0 {
        exact_search(flat, lookup)
    } else if match_type == 1.0 {
        ascending_search(flat, lookup)
    } else {
        descending_search(flat, lookup)
    }
}

/// Whole-column (used-extent) `lookup_array`: a single-column search whose
/// returned position is the matched cell's **relative row + 1** (its absolute
/// position within the whole column), not its index among the populated cells.
///
/// Only a single-column whole range is meaningful for MATCH: a multi-column
/// whole-column range is genuinely 2-D → `#UNSUPPORTED!`.
///
/// A `Blank` `lookup_value` is answerable only in **exact** mode (OXP-165: a
/// blank key matches a `0`-valued cell, and — unlike the raw operator
/// contract — does *not* match a truly-`Blank` cell, OXP-104; see
/// [`crate::lookup::exact_eq`]). A `Blank`-key match is trusted only when no
/// *absent* row precedes it (`rels[i] == i`) — a pre-existing,
/// extra-conservative guard kept as-is (not required by OXP-104 itself,
/// which already rules out a Blank key matching *any* blank cell, absent or
/// populated). A `Blank`-key exact scan that completes with **no match**
/// returns `#N/A` — pinned directly by OXP-104 H1 (L2-A; see the arm's
/// comment). In the sorted-array modes (`match_type = 1`/`-1`) a `Blank`
/// key still defers (OXP-104: the treatment of blank cells interspersed in a
/// sorted column is unverified) — no local pre-check is needed for that here,
/// though, since [`ascending_search`]/[`descending_search`]'s general
/// touch-a-`Blank` defer (shared with the bounded path below and with
/// `VLOOKUP`/`HLOOKUP`/`LOOKUP`) already catches it on the first probe.
fn match_used_extent(rows: Vec<(u32, Vec<Value>)>, match_type: f64, lookup: &Value) -> Value {
    let is_blank_lookup = matches!(lookup, Value::Blank);
    let width = rows.iter().map(|(_, cells)| cells.len()).max().unwrap_or(0);
    if width == 0 {
        // No populated rows. A non-blank key has nothing to match (#N/A). A
        // Blank key still defers — kept bit-for-bit (L2-A condition 2): unlike
        // the populated no-match case (pinned by OXP-104 H1), an ENTIRELY
        // empty column was never probed, so #N/A would rest on composing the
        // blank-vs-blank NoMatch pair alone.
        return if is_blank_lookup {
            Value::Error(ErrorKind::Unsupported)
        } else {
            Value::Error(ErrorKind::Na)
        };
    }
    if width > 1 {
        // A whole-column multi-column range is 2-D for MATCH.
        return Value::Error(ErrorKind::Unsupported);
    }
    let rels: Vec<u32> = rows.iter().map(|(rel, _)| *rel).collect();
    let flat: Vec<Value> = rows
        .into_iter()
        .map(|(_, cells)| cells.into_iter().next().unwrap_or(Value::Blank))
        .collect();

    match search(&flat, match_type, lookup) {
        Ok(Some(i)) => {
            // Pre-existing, extra-conservative guard (unchanged, out of scope
            // for this fix): a populated Blank-key match is trusted only
            // when no absent row precedes it — the populated rows are
            // contiguous from the top through the match (`rels[i] == i`).
            // Not required by OXP-104 itself (`exact_eq` already proves a
            // Blank key never matches any blank cell, absent or populated),
            // but kept as a belt-and-braces margin.
            if is_blank_lookup && rels[i] as usize != i {
                return Value::Error(ErrorKind::Unsupported);
            }
            // Position = relative row + 1 (gaps between populated rows are real
            // positions), not the compacted index (OXP-104).
            Value::number((rels[i] + 1) as f64)
        }
        // No-match completion → #N/A, Blank key included (L2-A). Pinned, not
        // composed: OXP-104 H1 (RUN-2026-07-11-oracle01) observed
        // `MATCH(C1, A:A, 0)` with C1 blank over the whole column
        // {1, 2, <truly blank>, 4} → `#N/A`. Reaching this arm means every
        // populated cell was a confirmed NoMatch (any unpinned pair Defers →
        // `Err(k)` below, preserved bit-for-bit), and OXP-104 pins that a
        // Blank key matches no truly-blank (absent) cell — so the completed
        // scan's #N/A equals the already-pinned bounded walk's answer. (Until
        // this change the Blank-key case carried an extra-conservative defer —
        // the L2-A corpus refusal.) The sorted modes never reach here with a
        // Blank involved: the touch-a-Blank defer fires first.
        Ok(None) => Value::Error(ErrorKind::Na),
        Err(k) => Value::Error(k),
    }
}

/// Whole-**row** (used-extent COLUMN) `lookup_array` (RFC 0008): a single-ROW
/// horizontal search whose returned position is the matched cell's **relative
/// column + 1** (its absolute position within the whole row), computed from the
/// column index the used-extent COLUMN walk yields — the transpose of
/// [`match_used_extent`].
///
/// Each yielded column slice is a full row-span-tall vector, so its length is the
/// range's row span. A length `> 1` means a **multi-row** whole-row range
/// (`1:5`), which is genuinely 2-D for MATCH → `#UNSUPPORTED!` (the transpose of
/// [`match_used_extent`]'s `width > 1` rule). Only a single-row whole-row range
/// (`1:1`) is a 1-D lookup vector.
///
/// A `Blank` `lookup_value` **defers unconditionally** here: OXP-165 pins the
/// blank-matches-`0` behavior only on the whole-**column** axis, so reusing it on
/// the whole-**row** axis would guess axis symmetry (Recalc Principle 2). The
/// whole-column path's OXP-165 exact-mode match is therefore *not* mirrored.
fn match_used_extent_cols(cols: Vec<(u32, Vec<Value>)>, match_type: f64, lookup: &Value) -> Value {
    if matches!(lookup, Value::Blank) {
        // OXP-165 is a whole-column pin only; the whole-row axis is unpinned.
        return Value::Error(ErrorKind::Unsupported);
    }
    let height = cols.iter().map(|(_, cells)| cells.len()).max().unwrap_or(0);
    if height == 0 {
        // No populated columns → nothing to match.
        return Value::Error(ErrorKind::Na);
    }
    if height > 1 {
        // A multi-row whole-row range is 2-D for MATCH.
        return Value::Error(ErrorKind::Unsupported);
    }
    let rels: Vec<u32> = cols.iter().map(|(rel, _)| *rel).collect();
    let flat: Vec<Value> = cols
        .into_iter()
        .map(|(_, cells)| cells.into_iter().next().unwrap_or(Value::Blank))
        .collect();

    match search(&flat, match_type, lookup) {
        // Position = relative column + 1 (gaps between populated columns are real
        // positions), not the compacted index — the column transpose of the
        // whole-column relative-row rule.
        Ok(Some(i)) => Value::number((rels[i] + 1) as f64),
        Ok(None) => Value::Error(ErrorKind::Na),
        Err(k) => Value::Error(k),
    }
}

/// `match_type = 0`: first (top-to-bottom / left-to-right) element equal to
/// `lookup` (case-insensitive text / cross-type per
/// [`crate::lookup::exact_eq`]). Mirrors
/// [`func_vlookup::exact_search`](crate::func_vlookup::exact_search): a text
/// `lookup` carrying a wildcard is oracle-deferred (OXP-089) rather than
/// matched literally or guessed, and a `Blank`-involved pair goes through the
/// shared, oracle-scoped [`crate::lookup::exact_eq`] rather than a raw
/// `values_equal` call (see that module's docs) — deferring
/// (`#UNSUPPORTED!`) on an unpinned `""`/`FALSE`/`Blank` candidate rather
/// than silently matching or silently skipping it.
fn exact_search(arr: &[Value], lookup: &Value) -> Result<Option<usize>, ErrorKind> {
    if let Value::Text(t) = lookup
        && t.as_str().contains(['*', '?'])
    {
        return Err(ErrorKind::Unsupported);
    }
    for (i, v) in arr.iter().enumerate() {
        match exact_eq(v, lookup) {
            Ok(LookupEq::Match) => return Ok(Some(i)),
            Ok(LookupEq::NoMatch) => {}
            Ok(LookupEq::Defer) => return Err(ErrorKind::Unsupported),
            Err(k) => return Err(k),
        }
    }
    Ok(None)
}

/// `match_type = 1`: binary search for the position of the largest element
/// `<= lookup`, assuming `arr` is sorted ascending. Identical probe
/// sequence to [`func_vlookup::approx_search`](crate::func_vlookup::approx_search)
/// (see that function's docs for the exact variant and its OXP-088
/// unsorted-data caveat), specialized to a flat 1-D array.
///
/// A **`Blank` touching a comparison — the key, or a probed element —
/// defers** (`Err(ErrorKind::Unsupported)`) instead of calling `compare`,
/// same rationale as `VLOOKUP`'s `approx_search`: see
/// [`crate::lookup::approx_touches_blank`]. A `Blank`-free search is
/// unaffected byte-for-byte.
fn ascending_search(arr: &[Value], lookup: &Value) -> Result<Option<usize>, ErrorKind> {
    let mut lo: isize = 0;
    let mut hi: isize = arr.len() as isize - 1;
    let mut best: Option<usize> = None;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let cell = &arr[mid as usize];
        if approx_touches_blank(cell, lookup) {
            return Err(ErrorKind::Unsupported);
        }
        match compare(cell, lookup)? {
            Ordering::Greater => hi = mid - 1,
            Ordering::Less | Ordering::Equal => {
                best = Some(mid as usize);
                lo = mid + 1;
            }
        }
    }
    Ok(best)
}

/// `match_type = -1`: binary search for the position of the smallest
/// element `>= lookup`, assuming `arr` is sorted descending — the mirror
/// image of [`ascending_search`]. Same OXP-088 unsorted-data caveat applies,
/// confirmed independently for this mode per MATCH.md, and the same
/// touch-a-`Blank` defer applies too (see [`ascending_search`]'s docs).
fn descending_search(arr: &[Value], lookup: &Value) -> Result<Option<usize>, ErrorKind> {
    let mut lo: isize = 0;
    let mut hi: isize = arr.len() as isize - 1;
    let mut best: Option<usize> = None;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let cell = &arr[mid as usize];
        if approx_touches_blank(cell, lookup) {
            return Err(ErrorKind::Unsupported);
        }
        match compare(cell, lookup)? {
            Ordering::Less => hi = mid - 1,
            Ordering::Greater | Ordering::Equal => {
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
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    /// The OXP-165 whole column `A = [0, "", <truly blank>, 5]` as the
    /// used-extent (populated-rows) walk sees it: A1=0 (rel 0), A2="" (rel 1),
    /// A4=5 (rel 3). The truly-blank A3 is absent and never yielded.
    fn oxp165_column() -> Vec<(u32, Vec<Value>)> {
        vec![(0, vec![num(0.0)]), (1, vec![txt("")]), (3, vec![num(5.0)])]
    }

    #[test]
    fn oxp165_zero_key_matches_zero_cell_exact() {
        // RUN-2026-07-11-oracle01 / OXP-165: MATCH(0, A:A, 0) = 1 (the 0-cell A1).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.0)),
                    UsedRows(oxp165_column()),
                    Scalar(num(0.0))
                ],
            ),
            Value::number(1.0)
        );
    }

    #[test]
    fn oxp165_blank_key_matches_zero_cell_exact() {
        // RUN-2026-07-11-oracle01 / OXP-165: MATCH(<blank>, A:A, 0) = 1 — a Blank
        // lookup key coerces to 0 and matches the leading 0-cell at position 1
        // (resolves OXP-104's clean half; the frozen compare contract already
        // treats Blank ↔ 0 / Blank ↔ "").
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Blank),
                    UsedRows(oxp165_column()),
                    Scalar(num(0.0)),
                ],
            ),
            Value::number(1.0)
        );
    }

    #[test]
    fn oxp104_blank_key_with_absent_row_before_match_defers() {
        // Pre-existing, extra-conservative guard (unchanged, out of scope for
        // this fix): the first populated cell is at rel 2 (rows 0,1 absent),
        // so a Blank key's populated match at position 3 still defers rather
        // than claim position 3, even though OXP-104 itself already rules out
        // an absent row being a hidden match.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Blank),
                    UsedRows(vec![(2, vec![num(0.0)])]),
                    Scalar(num(0.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// OXP-104 (H1, RUN-2026-07-11-oracle01) — the used-extent view of OXP-104's
    /// own whole-column fixture `A = {1, 2, <truly blank>, 4}`:
    /// `MATCH(C1, A:A, 0)` with C1 blank is pinned to **`#N/A`**. Every
    /// populated cell is a confirmed NoMatch (non-zero numbers) and the absent
    /// row is a truly-blank cell, pinned NoMatch for a Blank key — so the
    /// completed exact scan's `#N/A` is fully determined by pinned facts, same
    /// as the bounded walk already answers
    /// (`oxp104_blank_key_over_bounded_array_no_zero_element_is_na`). L2-A.
    #[test]
    fn oxp104_blank_key_no_match_over_whole_column_is_na() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Blank),
                    UsedRows(vec![
                        (0, vec![num(1.0)]),
                        (1, vec![num(2.0)]),
                        (3, vec![num(4.0)])
                    ]),
                    Scalar(num(0.0)),
                ],
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    /// PRESERVED bit-for-bit (L2-A condition 2): an unpinned Defer *pair* met
    /// mid-scan — a populated `""` cell against a Blank key (OXP-171 queued) —
    /// still aborts the whole-column exact scan with `#UNSUPPORTED!`.
    #[test]
    fn blank_key_whole_column_with_empty_text_cell_still_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Blank),
                    UsedRows(vec![
                        (0, vec![num(1.0)]),
                        (1, vec![txt("")]),
                        (3, vec![num(4.0)])
                    ]),
                    Scalar(num(0.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn oxp104_blank_key_approx_mode_still_defers() {
        // Only exact mode (match_type 0) is pinned by OXP-165; the sorted-array
        // modes remain OXP-104-unverified for a Blank key → defer.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Blank),
                    UsedRows(oxp165_column()),
                    Scalar(num(1.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn used_extent_position_is_absolute_row() {
        // A non-blank key over a gapped whole column returns the matched cell's
        // absolute position (rel + 1), not its compacted index: 5 sits at rel 3.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(5.0)),
                    UsedRows(oxp165_column()),
                    Scalar(num(0.0))
                ],
            ),
            Value::number(4.0)
        );
    }

    // --- Bounded-range (dense-walk) blank-scoped equality --------------------
    // These cover the same `crate::lookup::exact_eq` decision table over a
    // plain bounded array (not a whole column), where a genuinely `Blank`
    // element is walked directly rather than compacted away as "absent".

    /// Pinned NO-MATCH (OXP-104): a Blank key over `{1, 2, <truly blank>, 4}`
    /// never matches — the non-zero elements are ordinary skips, the
    /// truly-blank element is a confirmed no-match — so the scan reaches the
    /// end → `#N/A`. Critical: the non-zero elements must not be deferred, or
    /// the scan would abort with `#UNSUPPORTED!` before ever reaching the
    /// blank element.
    #[test]
    fn oxp104_blank_key_over_bounded_array_no_zero_element_is_na() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Blank),
                    Range(vec![num(1.0), num(2.0), Value::Blank, num(4.0)]),
                    Scalar(num(0.0)),
                ],
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    /// DEFER: a Blank key's first candidate is a populated `""` element (no
    /// `0`-element precedes it) — unpinned.
    #[test]
    fn blank_key_vs_empty_text_first_candidate_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Blank),
                    Range(vec![txt(""), num(5.0)]),
                    Scalar(num(0.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// DEFER: a Blank key's first candidate is a populated `FALSE` element —
    /// unpinned, same as the `""` case above.
    #[test]
    fn blank_key_vs_false_first_candidate_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Blank),
                    Range(vec![Value::Bool(false), num(5.0)]),
                    Scalar(num(0.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// DEFER: a real (non-Blank) `0` key against a truly-`Blank` first
    /// element — the unpinned reverse direction of OXP-165.
    #[test]
    fn zero_key_vs_blank_element_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.0)),
                    Range(vec![Value::Blank, num(5.0)]),
                    Scalar(num(0.0))
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// UNCHANGED: cross-type strictness (`5 <> "5"`) is untouched.
    #[test]
    fn number_key_does_not_match_text_digit_element() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(5.0)),
                    Range(vec![txt("5"), num(999.0)]),
                    Scalar(num(0.0))
                ],
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    /// UNCHANGED: an ordinary non-blank exact hit still works.
    #[test]
    fn ordinary_non_blank_exact_hit_still_works() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(3.0)),
                    Range(vec![num(1.0), num(3.0)]),
                    Scalar(num(0.0))
                ],
            ),
            Value::number(2.0)
        );
    }

    // --- Whole-row lookup_array (RFC 0008) ----------------------------------

    /// A single-ROW whole-row `lookup_array` is a 1-D horizontal vector.
    /// `MATCH(30, 1:1, 0)` over row [10,30,50] (cols 0,1,2) → position 2.
    #[test]
    fn whole_row_single_row_exact_position() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(30.0)),
                    UsedCols(vec![
                        (0, vec![num(10.0)]),
                        (1, vec![num(30.0)]),
                        (2, vec![num(50.0)]),
                    ]),
                    Scalar(num(0.0)),
                ],
            ),
            Value::number(2.0)
        );
    }

    /// The returned position is the matched cell's **relative column + 1** (gaps
    /// are real positions), not its compacted index: 5 sits at rel col 3 → 4.
    #[test]
    fn whole_row_position_is_relative_col_plus_one() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(5.0)),
                    UsedCols(vec![
                        (0, vec![num(0.0)]),
                        (1, vec![txt("")]),
                        (3, vec![num(5.0)]),
                    ]),
                    Scalar(num(0.0)),
                ],
            ),
            Value::number(4.0)
        );
    }

    /// A **multi-row** whole-row range (`1:2`) is genuinely 2-D for MATCH →
    /// `#UNSUPPORTED!` (transpose of the `width > 1` whole-column rule). Each
    /// column slice is 2-tall.
    #[test]
    fn multi_row_whole_row_is_2d_unsupported() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(30.0)),
                    UsedCols(vec![
                        (0, vec![num(10.0), num(11.0)]),
                        (1, vec![num(30.0), num(31.0)]),
                    ]),
                    Scalar(num(0.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// A `Blank` `lookup_value` over a whole-ROW range defers unconditionally:
    /// OXP-165 pins blank-matches-`0` only on the whole-COLUMN axis, so the
    /// whole-row axis is unpinned and never guessed.
    #[test]
    fn blank_lookup_over_whole_row_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Blank),
                    UsedCols(vec![(0, vec![num(0.0)]), (1, vec![num(5.0)])]),
                    Scalar(num(0.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// No match over a whole-ROW range → `#N/A` (a non-blank key with nothing
    /// equal to it).
    #[test]
    fn whole_row_no_match_is_na() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(999.0)),
                    UsedCols(vec![(0, vec![num(10.0)]), (1, vec![num(30.0)])]),
                    Scalar(num(0.0)),
                ],
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // --- Approximate-mode (match_type 1/-1) touch-a-Blank defer (this fix) --

    /// THE BUG THIS FIX CLOSES: a Blank key in `match_type = 1` (ascending)
    /// over a **bounded** (non-whole-column) `lookup_array` used to have no
    /// guard at all — only the whole-column path deferred. It must now defer.
    #[test]
    fn blank_key_ascending_search_over_bounded_array_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Blank),
                    Range(vec![num(1.0), num(3.0), num(5.0)]),
                    Scalar(num(1.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// Same bug, `match_type = -1` (descending) path.
    #[test]
    fn blank_key_descending_search_over_bounded_array_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Blank),
                    Range(vec![num(5.0), num(3.0), num(1.0)]),
                    Scalar(num(-1.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// THE BUG THIS FIX CLOSES: `MATCH(999, <20-element array>, 1)` over an
    /// array populated only in positions 1-10 (ascending 1..10) with a Blank
    /// tail in positions 11-20. The search must defer the moment it probes a
    /// Blank element, instead of silently walking through it.
    #[test]
    fn blank_element_touched_by_ascending_search_defers() {
        let mut data: Vec<Value> = Vec::new();
        for i in 1..=10 {
            data.push(num(f64::from(i)));
        }
        for _ in 0..10 {
            data.push(Value::Blank);
        }
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(999.0)), Range(data), Scalar(num(1.0))]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    /// Blank-free ascending search, plain bounded array: byte-identical
    /// largest-`<=` answer, unaffected by the touch-a-Blank defer.
    #[test]
    fn ascending_search_blank_free_still_works() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(4.0)),
                    Range(vec![num(1.0), num(3.0), num(5.0)]),
                    Scalar(num(1.0)),
                ],
            ),
            Value::number(2.0)
        );
    }

    /// Blank-free descending search, plain bounded array: byte-identical
    /// smallest-`>=` answer, unaffected by the touch-a-Blank defer.
    #[test]
    fn descending_search_blank_free_still_works() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(4.0)),
                    Range(vec![num(5.0), num(3.0), num(1.0)]),
                    Scalar(num(-1.0)),
                ],
            ),
            Value::number(1.0)
        );
    }
}

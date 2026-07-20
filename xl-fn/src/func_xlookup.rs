//! `XLOOKUP` — search `lookup_array` for `lookup_value` and return the aligned
//! item(s) from `return_array` (which may be multi-cell and spill).
//!
//! # Provenance
//! Microsoft support page "XLOOKUP function"
//! (<https://support.microsoft.com/en-us/office/xlookup-function-b7fd680e-6d10-43e6-84f9-88eae8bf5929>),
//! fetched 2026-07-15. Clean-room from the page's unambiguous prose; equality is
//! the shared [`crate::lookup::exact_eq`] used by `VLOOKUP`/`MATCH`. Refused
//! edges are queued in `docs/plans/2026-07-15-lane3b-probe-needed.md`.
//!
//! # Signature (page verbatim)
//! `XLOOKUP(lookup_value, lookup_array, return_array, [if_not_found],
//! [match_mode], [search_mode])` — 3..=6 args. Defaults: `match_mode = 0`
//! (exact), `search_mode = 1` (first). "If a valid match is not found" and
//! `if_not_found` is omitted, XLOOKUP "returns #N/A".
//!
//! # Semantics implemented
//! - **Exact match** (`match_mode = 0`, default): the shared
//!   [`crate::lookup::exact_eq`] scan — case-insensitive text, cross-type strict,
//!   OXP-104/165 `Blank` scoping — honouring the search direction (`search_mode
//!   = 1` first, `-1` last).
//! - **Multi-cell return** (page Example 2 — "return an array with multiple
//!   items"): when the match sits at position *i* of a vertical `lookup_array`,
//!   the whole row *i* of `return_array` is returned (a `1×W`
//!   [`Value::Array`] that spills); for a horizontal `lookup_array`, the whole
//!   column *i* (an `H×1` array). A single-cell result is returned as a scalar.
//! - **`if_not_found`**: a *provided* value (even a `Blank`) is returned when an
//!   exact search finds no match; **genuinely absent** (3-arg call) → `#N/A`
//!   (documented). An **elided `,,`** slot (with later modes supplied) is
//!   unpinned (`,,` = "missing" vs a `Blank` value) → `#UNSUPPORTED!` (L3B probe
//!   doc) rather than a silent `Blank`.
//! - **`match_mode = -1` / `1`**: the documented **exact** phase is served
//!   (honouring direction); with no exact match the "next smaller/larger"
//!   fallback is **refused** (unpinned tie/unsorted behavior), so `if_not_found`
//!   is *not* substituted there (Excel would return the neighbour, not
//!   `if_not_found`).
//!
//! # Refused / assumed (see the probe doc)
//! - **`match_mode = 2`** (wildcard) and **`search_mode = 2` / `-2`** (binary):
//!   refused in [`crate::dynarray::resolve_modes`] — undocumented collation /
//!   unpinned unsorted-and-tie behavior (OXP-088/089-class). Any other mode
//!   value is undocumented → refused.
//! - **`return_array` not aligned** with the `lookup_array` search axis (its
//!   length along that axis differs): the exact error type is undocumented on
//!   the page, so it is refused `#UNSUPPORTED!` rather than guessing
//!   `#VALUE!`/`#REF!` (L3B probe doc).
//! - **Genuinely 2-D `lookup_array`**: no documented 1-D flattening → refused.
//! - **Whole-column/row inputs** (`A:A`): the dense walk refuses the unbounded
//!   range → `#UNSUPPORTED!` (a whole-axis spill is out of v0 scope).
//! - A **data error inside `return_array`** at the selected position is returned
//!   in place (Excel-faithful), never dropped; an error reached in the
//!   `lookup_array` scan *before* a match propagates.

use xl_value::{ErrorKind, Value};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;
use crate::dynarray::{
    Orient, XMatchMode, exact_scan, flatten_1d, materialize, resolve_modes, spill,
};

/// Evaluate an `XLOOKUP(...)` call. See the module docs for semantics/provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // lookup_value (arg 0): scalar context; a lookup error propagates.
    let lookup = args.eval_scalar(0);
    if let Value::Error(k) = lookup {
        return Value::Error(k);
    }

    // match_mode (arg 4) + search_mode (arg 5): validated / hazard-refused.
    let (mode, forward) = match resolve_modes(args, 4, 5) {
        Ok(m) => m,
        Err(k) => return Value::Error(k),
    };

    // lookup_array (arg 1) → 1-D vector with orientation. Materialize refuses the
    // unbounded whole-column/row range; flatten refuses a genuinely 2-D shape.
    let la = match materialize(args, 1) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    let (lookup_vec, orient) = match flatten_1d(&la) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };

    // return_array (arg 2): materialize; its search-axis length must equal the
    // lookup vector's length.
    let ra = match materialize(args, 2) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    let axis_len = match orient {
        Orient::Vertical => ra.height,
        Orient::Horizontal => ra.width,
    };
    if axis_len != lookup_vec.len() {
        // Undocumented error type for a misaligned return_array → refuse loudly.
        return Value::Error(ErrorKind::Unsupported);
    }

    match exact_scan(&lookup_vec, &lookup, forward) {
        Ok(Some(i)) => extract_result(&ra, orient, i),
        Ok(None) => match mode {
            XMatchMode::Exact => not_found(args),
            // Approximate modes with no exact hit: the neighbour-selection is
            // unpinned, so refuse (do NOT substitute if_not_found here).
            XMatchMode::ExactOrSmaller | XMatchMode::ExactOrLarger => {
                Value::Error(ErrorKind::Unsupported)
            }
        },
        Err(k) => Value::Error(k),
    }
}

/// The exact-mode not-found result, distinguishing three `if_not_found` cases
/// (Omitted covers both an absent trailing arg *and* an elided `,,` slot — they
/// are told apart by `count`):
/// - **genuinely absent** (arg count ≤ 3) → `#N/A` (the page documents this).
/// - **elided `,,`** (count > 3 but arg 3 `Omitted`, e.g. `XLOOKUP(x,r,ret,,0,1)`)
///   → `#UNSUPPORTED!`: whether Excel reads `,,` as "missing" (`#N/A`) or as a
///   `Blank` value is unpinned (OXP-080 precedent for elided value slots; L3B
///   probe doc). Never silently returns the `Blank` that `eval_scalar` would give.
/// - **provided** (even a `Blank`) → used as-is.
fn not_found(args: &mut dyn CallArgs) -> Value {
    if args.count() <= 3 {
        Value::Error(ErrorKind::Na)
    } else if args.shape(3) == ArgShape::Omitted {
        Value::Error(ErrorKind::Unsupported)
    } else {
        args.eval_scalar(3)
    }
}

/// Pull the matched result out of `return_array` at search position `i`.
///
/// - Vertical `lookup_array`: the whole **row** `i` of `ra` (`1×W`). A single
///   column (`W == 1`) returns the scalar cell.
/// - Horizontal `lookup_array`: the whole **column** `i` of `ra` (`H×1`). A
///   single row (`H == 1`) returns the scalar cell.
///
/// A multi-cell result becomes a spillable [`Value::Array`]; a data error in a
/// returned cell rides along in place.
fn extract_result(ra: &crate::dynarray::Grid, orient: Orient, i: usize) -> Value {
    match orient {
        Orient::Vertical => {
            let row = &ra.rows[i];
            if ra.width == 1 {
                row[0].clone()
            } else {
                spill(1, ra.width, row.clone())
            }
        }
        Orient::Horizontal => {
            let col = ra.column(i);
            if ra.height == 1 {
                col[0].clone()
            } else {
                spill(ra.height, 1, col)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::dynarray::spill;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn exact_single_column_return() {
        // XLOOKUP("b", {"a";"b";"c"}, {10;20;30}) → 20.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("b")),
                    Range(vec![txt("a"), txt("b"), txt("c")]),
                    Range(vec![num(10.0), num(20.0), num(30.0)]),
                ],
            ),
            num(20.0)
        );
    }

    #[test]
    fn not_found_returns_na() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("z")),
                    Range(vec![txt("a"), txt("b")]),
                    Range(vec![num(1.0), num(2.0)]),
                ],
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn not_found_uses_if_not_found() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("z")),
                    Range(vec![txt("a"), txt("b")]),
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(txt("missing")),
                ],
            ),
            txt("missing")
        );
    }

    #[test]
    fn multi_column_return_spills_row() {
        // Vertical lookup, 2-column return → the matched row as a 1×2 array.
        // lookup {"a";"b"} at "b" → row 1 of return {{1,2},{3,4}} = {3,4}.
        let got = eval_direct(
            eval,
            vec![
                Scalar(txt("b")),
                Range(vec![txt("a"), txt("b")]),
                Rect {
                    rows: 2,
                    cols: 2,
                    data: vec![num(1.0), num(2.0), num(3.0), num(4.0)],
                },
            ],
        );
        assert_eq!(got, spill(1, 2, vec![num(3.0), num(4.0)]));
    }

    #[test]
    fn reverse_search_last_match() {
        // search_mode = -1 → last "a" wins (row 2) → return 30.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("a")),
                    Range(vec![txt("a"), txt("b"), txt("a")]),
                    Range(vec![num(10.0), num(20.0), num(30.0)]),
                    Scalar(Value::Blank),
                    Scalar(num(0.0)),
                    Scalar(num(-1.0)),
                ],
            ),
            num(30.0)
        );
    }

    #[test]
    fn horizontal_lookup_returns_column() {
        // Horizontal lookup row {1,2,3}; return is a single row {"x","y","z"};
        // match 2 at col 1 → "y".
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(2.0)),
                    Array(vec![num(1.0), num(2.0), num(3.0)]),
                    Array(vec![txt("x"), txt("y"), txt("z")]),
                ],
            ),
            txt("y")
        );
    }

    #[test]
    fn misaligned_return_array_refused() {
        // lookup has 3 rows, return has 2 → undocumented error type → refuse.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("a")),
                    Range(vec![txt("a"), txt("b"), txt("c")]),
                    Range(vec![num(1.0), num(2.0)]),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn approx_mode_no_exact_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(15.0)),
                    Range(vec![num(10.0), num(20.0)]),
                    Range(vec![txt("a"), txt("b")]),
                    Scalar(txt("nf")),
                    Scalar(num(-1.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn wildcard_mode_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("a*")),
                    Range(vec![txt("abc")]),
                    Range(vec![num(1.0)]),
                    Scalar(Value::Blank),
                    Scalar(num(2.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn binary_search_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(2.0)),
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(Value::Blank),
                    Scalar(num(0.0)),
                    Scalar(num(2.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn whole_column_lookup_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(1.0)),
                    Unbounded(vec![num(1.0), num(2.0)]),
                    Unbounded(vec![num(9.0), num(8.0)]),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn selected_return_error_rides_along() {
        // The matched row's return cell is #DIV/0! → returned in place.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("b")),
                    Range(vec![txt("a"), txt("b")]),
                    Range(vec![num(1.0), Value::Error(ErrorKind::Div0)]),
                ],
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn elided_if_not_found_on_miss_is_refused() {
        // XLOOKUP(z,{a,b},{1,2},,0,1): if_not_found ELIDED but modes supplied; a
        // not-found must refuse (unpinned `,,`) — NOT silently return the Blank
        // that eval_scalar would give for the elided slot.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("z")),
                    Range(vec![txt("a"), txt("b")]),
                    Range(vec![num(1.0), num(2.0)]),
                    Omitted,
                    Scalar(num(0.0)),
                    Scalar(num(1.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn provided_blank_if_not_found_used_as_is() {
        // A *provided* Blank if_not_found is returned as-is on a miss (distinct
        // from the elided case above, and from the documented 3-arg #N/A).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("z")),
                    Range(vec![txt("a"), txt("b")]),
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(Value::Blank),
                ],
            ),
            Value::Blank
        );
    }
}

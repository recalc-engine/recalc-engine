//! `XMATCH` — return the 1-based **relative position** of `lookup_value` within
//! a single-row/column `lookup_array`.
//!
//! # Provenance
//! Microsoft support page "XMATCH function"
//! (<https://support.microsoft.com/en-us/office/xmatch-function-d966da31-7a6b-4a13-a1c6-5a33ed6a0312>),
//! fetched 2026-07-15. No `docs/specs/XMATCH.md` exists; this is a clean-room
//! implementation of only the page's unambiguous prose. Comparison/equality is
//! deferred to `xl-value` / the shared [`crate::lookup`] helpers — this module
//! never re-implements them. Refused edges are queued in
//! `docs/plans/2026-07-15-lane3b-probe-needed.md`.
//!
//! # Signature (page verbatim)
//! `XMATCH(lookup_value, lookup_array, [match_mode], [search_mode])` — 2..=4
//! args. Returns "the item's relative position", numbered from 1.
//!
//! # Semantics implemented
//! - **`match_mode = 0`** (default): exact match, via the shared
//!   [`crate::lookup::exact_eq`] scan (case-insensitive text, cross-type strict,
//!   OXP-104/165 `Blank` scoping) — the same equality `VLOOKUP`/`MATCH` use.
//! - **`search_mode = 1`** (default, first-to-last) and **`-1`** (last-to-first,
//!   reverse): documented directions; the reverse scan makes the **last** match
//!   win.
//! - **`match_mode = -1` / `1`** (exact-or-next-smaller / -larger): the **exact**
//!   phase is served (it is documented as identical to an exact match — "Exact
//!   match or next smallest/largest item"), honouring the search direction. If
//!   no exact match exists, the *approximate fallback* is **refused**
//!   (`#UNSUPPORTED!`) — see below.
//! - **No match** under a served mode → `#N/A` (see the assumption note below).
//!
//! # Refused / assumed (see the probe doc)
//! - **`match_mode = 2`** (wildcard) and **`search_mode = 2` / `-2`** (binary):
//!   refused in [`crate::dynarray::resolve_modes`] — wildcard collation and the
//!   binary-search unsorted/tie behavior are undocumented on the page (the
//!   OXP-088/089-class hazards). Any other mode value is undocumented → refused.
//! - **`match_mode = -1` / `1` with no exact match**: the "next smaller/larger"
//!   choice on ties / unsorted data is unpinned, so it is refused rather than
//!   guessed. The exact-match phase still resolves.
//! - **Not-found error type** is *assumed* `#N/A`: the XMATCH page does not name
//!   it, but its sibling `XLOOKUP` documents `#N/A`, and `MATCH` documents `#N/A`
//!   — a cross-documented value, not a memory guess. Queued for confirmation
//!   (L3B probe doc).
//! - **Genuinely 2-D `lookup_array`** (more than one row *and* column): no
//!   documented flattening order → `#UNSUPPORTED!` (mirrors `MATCH`).
//! - **Whole-column/row `lookup_array`** (`A:A`): the dense walk refuses the
//!   unbounded range → `#UNSUPPORTED!` (`crate::dynarray` module docs).

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::dynarray::{XMatchMode, exact_scan, flatten_1d, materialize, resolve_modes};

/// Evaluate an `XMATCH(...)` call. See the module docs for semantics/provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // lookup_value (arg 0): scalar context; a lookup error propagates.
    let lookup = args.eval_scalar(0);
    if let Value::Error(k) = lookup {
        return Value::Error(k);
    }

    // match_mode (arg 2) + search_mode (arg 3): validated / hazard-refused.
    let (mode, forward) = match resolve_modes(args, 2, 3) {
        Ok(m) => m,
        Err(k) => return Value::Error(k),
    };

    // lookup_array (arg 1): materialize the bounded rectangle (refuses the
    // unbounded whole-column/row range), then flatten to a 1-D vector; a
    // genuinely 2-D shape has no documented flattening order → #UNSUPPORTED!.
    let grid = match materialize(args, 1) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    let (vec, _orient) = match flatten_1d(&grid) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };

    match exact_scan(&vec, &lookup, forward) {
        Ok(Some(i)) => Value::number((i + 1) as f64),
        Ok(None) => match mode {
            // Exact mode: a genuine not-found is #N/A (assumed; see module docs).
            XMatchMode::Exact => Value::Error(ErrorKind::Na),
            // Approximate modes: the exact phase found nothing and the
            // next-smaller/larger fallback is unpinned → refuse loudly.
            XMatchMode::ExactOrSmaller | XMatchMode::ExactOrLarger => {
                Value::Error(ErrorKind::Unsupported)
            }
        },
        Err(k) => Value::Error(k),
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn exact_first_match_vertical() {
        // XMATCH(30, {10;30;50}) → position 2 (default exact, first).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(30.0)),
                    Range(vec![num(10.0), num(30.0), num(50.0)])
                ]
            ),
            Value::number(2.0)
        );
    }

    #[test]
    fn exact_match_horizontal() {
        // A single-row lookup_array works too (Array models a 1×N row).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("b")), Array(vec![txt("a"), txt("b"), txt("c")])]
            ),
            Value::number(2.0)
        );
    }

    #[test]
    fn reverse_search_returns_last_match() {
        // search_mode = -1: the LAST equal item wins → position 3 of {5;5;5}.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(5.0)),
                    Range(vec![num(5.0), num(5.0), num(5.0)]),
                    Scalar(num(0.0)),
                    Scalar(num(-1.0)),
                ],
            ),
            Value::number(3.0)
        );
    }

    #[test]
    fn not_found_is_na() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(99.0)), Range(vec![num(1.0), num(2.0)])]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn approx_mode_exact_hit_resolves() {
        // match_mode = -1 with an exact hit present resolves the exact phase.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(20.0)),
                    Range(vec![num(10.0), num(20.0), num(30.0)]),
                    Scalar(num(-1.0)),
                ],
            ),
            Value::number(2.0)
        );
    }

    #[test]
    fn approx_mode_no_exact_refuses() {
        // match_mode = 1, no exact match → the next-larger fallback is unpinned.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(15.0)),
                    Range(vec![num(10.0), num(20.0), num(30.0)]),
                    Scalar(num(1.0)),
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
                vec![Scalar(txt("a*")), Range(vec![txt("abc")]), Scalar(num(2.0))],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn binary_search_mode_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(2.0)),
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Scalar(num(0.0)),
                    Scalar(num(2.0)),
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn two_d_lookup_array_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(1.0)),
                    Rect {
                        rows: 2,
                        cols: 2,
                        data: vec![num(1.0), num(2.0), num(3.0), num(4.0)]
                    },
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn whole_column_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(1.0)), Unbounded(vec![num(1.0), num(2.0)])]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn lookup_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Div0)), Range(vec![num(1.0)])],
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn literal_star_matches_literally_in_exact_mode() {
        // In exact mode `*` is a literal char (wildcards only in match_mode 2).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("a*b")), Array(vec![txt("a*b"), txt("axb")])]
            ),
            Value::number(1.0)
        );
    }
}

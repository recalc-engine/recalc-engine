//! Shared lookup-family helpers scoped around `Blank`: exact-match equality
//! ([`exact_eq`], for `VLOOKUP`/`HLOOKUP`/`MATCH` with `match_type = 0`) and
//! the approximate-search touch-a-`Blank` defer ([`approx_touches_blank`],
//! for all four functions' binary/linear approximate searches — `VLOOKUP`,
//! `HLOOKUP`, `MATCH` with `match_type = 1`/`-1`, and `LOOKUP`, which is
//! always approximate).
//!
//! # Provenance — why `exact_eq` exists
//! `VLOOKUP`/`HLOOKUP`/`MATCH`'s exact-match linear scans used to delegate
//! equality straight to `xl_value::values_equal`/[`compare`](xl_value::compare).
//! That function is the frozen contract for the `=`/`<>` **operators**, where
//! `Blank` is documented to morph by the other operand — `Blank ↔ Number(0)`,
//! `Blank ↔ Text("")`, `Blank ↔ Bool(false)` (`xl-value/src/coerce.rs`
//! ~1200-1211, OXP-030) — and `Blank` compares equal to another `Blank`.
//! Those morphs were never pinned for LOOKUP-family exact equality, and one
//! is **oracle-contradicted**. `RUN-2026-07-11-oracle01`:
//! - **OXP-165** pins that a `Blank` lookup key **matches** a `Number(0)`
//!   cell (`VLOOKUP(<blank>, A:B, 2, FALSE)` over `A = [0, "", <blank>, 5]`
//!   returns the `0`-row). This is exactly what `values_equal` already
//!   returns, so it needs no override here.
//! - **OXP-104** pins that a `Blank` lookup key does **NOT** match a
//!   truly-`Blank` cell (over `{1, 2, <truly-blank>, 4}`, `MATCH(<blank>,
//!   A:A, 0)` / `VLOOKUP(<blank>, ..., FALSE)` → `#N/A`) — the opposite of
//!   `values_equal`'s trivial `Blank == Blank`. This is overridden below to
//!   `NoMatch`.
//!
//! Every other `Blank`-involved pair is unpinned. Two kinds show up:
//! - **Plausible-but-unconfirmed** extensions of the OXP-165 pattern: since a
//!   `Blank` key is *known* to match `Number(0)` (a type's "zero"/empty
//!   representation), it is plausible — but not confirmed — that it also
//!   matches `Text("")` or `Bool(false)` (the analogous representation for
//!   those types), and plausible that the *reverse* direction (a literal `0`/
//!   `""`/`FALSE` key against a `Blank` cell) also matches by the same
//!   symmetry. `values_equal` currently answers all of these with a silent
//!   `Match` (the live `Recalc-matches-where-Excel-#N/A` corpus divergence);
//!   here they are overridden to `Defer` (`#UNSUPPORTED!`) instead.
//! - **Not plausible under any interpretation consistent with the two pinned
//!   facts above**: `Blank` against a *non-zero* `Number`, a *non-empty*
//!   `Text`, or `Bool(true)` (in either direction). No reading of OXP-165/
//!   OXP-104 suggests these could match, so they are left exactly as
//!   `values_equal` already answers them (`NoMatch`) rather than deferred.
//!   This is not a shortcut: deferring on *every* `Blank`-involved pair
//!   would abort an ordinary numeric/text column scan the moment it passed a
//!   non-zero number next to a `Blank` key — which is precisely OXP-104's own
//!   `{1, 2, <blank>, 4}` table, pinned to resolve to `#N/A`, not
//!   `#UNSUPPORTED!`.
//!
//! A `Blank`-involved pair where the *other* side is an `Error` is not a
//! blank-morph question at all — `values_equal`'s error propagation is
//! untouched and unambiguous, so it is left alone.
//!
//! # Provenance — why `approx_touches_blank` exists
//! The four **approximate-match** searches (`VLOOKUP`/`HLOOKUP`/`MATCH`'s
//! `match_type = 1`/`-1`/`LOOKUP`'s always-approximate mode) used to call
//! `xl_value::compare` directly on every probe, with no `Blank` scoping at
//! all. `compare`'s `Blank` morphs (`Blank ↔ Number(0)`/`Text("")`/
//! `Bool(false)`, `Blank == Blank`) are the frozen, oracle-pinned contract
//! for the `=`/`<>`/ordering **operators** — but that pinning is specific to
//! operator semantics. Nothing pins the same morphs for approximate *lookup
//! ordering*: **OXP-088**'s probe workbooks (which pinned the floor-midpoint/
//! settle-on-`hi` binary-search algorithm) had no `Blank` cells in the search
//! data, and **OXP-104** explicitly marks the ordering of `Blank` cells
//! interspersed in a sorted range as unverified. So letting a probe silently
//! call `compare` when either side is `Blank` was a guess, not a citation —
//! e.g. `VLOOKUP(999, A1:B100, 2, TRUE)` with real data only in rows 1–10 and
//! a `Blank` tail could probe into that tail, have `compare` morph the
//! `Blank` cell to `0`, decide `0 < 999`, and walk further into blanks,
//! eventually settling on a `Blank` row instead of row 10's answer — silently
//! wrong, not `#UNSUPPORTED!`.
//!
//! [`approx_touches_blank`] is the uniform guard for this: every one of the
//! four searches calls it immediately before (or at) each `compare(cell,
//! key)` site and short-circuits to `Err(ErrorKind::Unsupported)` when it
//! returns `true`, rather than letting `compare` decide. It replaces the
//! partial, inconsistent ad-hoc guards that predated this fix (a
//! whole-column-only, approximate-mode-only `Blank`-*key* pre-check existed
//! for `VLOOKUP`/`MATCH`; `HLOOKUP`/`LOOKUP` had no guard at all, and none of
//! the four caught a `Blank` *search-vector cell*) with one rule applied at
//! every probe, in every function, over both bounded and whole-axis ranges.
//! A search that never touches a `Blank` — no `Blank` key, no `Blank` probed
//! cell — is completely unaffected: same probe order, same OXP-088-pinned
//! answers.

use xl_value::{ErrorKind, Value, values_equal};

/// Whether an approximate-match search's next comparison would involve a
/// `Blank` — the lookup `key`, or the probed `cell` — and must therefore
/// defer (`#UNSUPPORTED!`) rather than let [`xl_value::compare`]'s
/// operator-only-pinned `Blank` morphs decide an unpinned lookup ordering.
/// See the module docs for the full rationale. Call sites: `VLOOKUP`/
/// `HLOOKUP`/`LOOKUP`'s `approx_search`, and `MATCH`'s `ascending_search`/
/// `descending_search` — immediately before each `compare(cell, key)`
/// (after any pinned error-cell skip, so an error cell is still skipped
/// without ever "touching" the key).
pub(crate) fn approx_touches_blank(cell: &Value, key: &Value) -> bool {
    matches!(cell, Value::Blank) || matches!(key, Value::Blank)
}

/// Outcome of [`exact_eq`] for one lookup-array cell against the lookup key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LookupEq {
    /// `cell` is `key`'s exact-match answer — the search stops here.
    Match,
    /// `cell` is confirmed not equal to `key` — keep scanning.
    NoMatch,
    /// The `(cell, key)` pair is not pinned by any oracle experiment for
    /// LOOKUP-family exact equality. The caller must surface
    /// `#UNSUPPORTED!` rather than silently match *or* silently skip it.
    Defer,
}

/// Exact-match equality for the LOOKUP family, scoped for `Blank`-involved
/// pairs per the module docs above. `cell` is the lookup-array/table
/// candidate; `key` is the lookup value — the same argument order as the
/// `values_equal(cell, key)` call this replaces, so error propagation order
/// (errors in `cell` reported before errors in `key`, per
/// [`compare`](xl_value::compare)) is unchanged.
///
/// When **neither** `cell` nor `key` is `Blank`, this is a direct pass
/// through to [`values_equal`] — behavior (including the `Number ≠ Text`
/// cross-type strictness) is completely unchanged from before this helper
/// existed.
pub(crate) fn exact_eq(cell: &Value, key: &Value) -> Result<LookupEq, ErrorKind> {
    // BC-6 (RFC-0012, B2 ROUTE; Rider ii-adjacent): a lambda key/cell must never
    // fall through to `values_equal` → `compare` (lambda equality is unpinned).
    // Hoisted ABOVE the `Blank` scoping so it fires for EVERY lambda-involved
    // pair — including a lambda against a *non-Blank* value, which would
    // otherwise skip the guard entirely and hit `compare`. Defer explicitly:
    // lookup refuses lambdas itself, until an OXP pins the behavior.
    if matches!(cell, Value::Lambda(_)) || matches!(key, Value::Lambda(_)) {
        return Ok(LookupEq::Defer);
    }
    if matches!(cell, Value::Blank) || matches!(key, Value::Blank) {
        match (key, cell) {
            // OXP-104: a Blank key does not match a truly-Blank cell.
            (Value::Blank, Value::Blank) => return Ok(LookupEq::NoMatch),
            // Unpinned extension of OXP-165's pattern to the Text/Bool "zero"
            // representations, in either direction.
            (Value::Blank, Value::Text(t)) | (Value::Text(t), Value::Blank)
                if t.as_str().is_empty() =>
            {
                return Ok(LookupEq::Defer);
            }
            (Value::Blank, Value::Bool(false)) | (Value::Bool(false), Value::Blank) => {
                return Ok(LookupEq::Defer);
            }
            // Unpinned reverse direction of OXP-165 (a real, non-Blank 0-key
            // against a Blank cell); OXP-165 only probed Blank-key-vs-0-cell.
            (Value::Number(n), Value::Blank) if *n == 0.0 => return Ok(LookupEq::Defer),
            _ => {}
        }
    }
    match values_equal(cell, key) {
        Ok(true) => Ok(LookupEq::Match),
        Ok(false) => Ok(LookupEq::NoMatch),
        Err(k) => Err(k),
    }
}

#[cfg(test)]
mod tests {
    use super::{LookupEq, exact_eq};
    use crate::test_support::{num, txt};
    use xl_value::{ErrorKind, Value};

    // --- OXP-165: pinned MATCH -------------------------------------------

    #[test]
    fn blank_key_matches_zero_cell() {
        assert_eq!(exact_eq(&num(0.0), &Value::Blank), Ok(LookupEq::Match));
    }

    #[test]
    fn zero_key_matches_zero_cell_unchanged() {
        // Neither side Blank: ordinary Number == Number, unaffected.
        assert_eq!(exact_eq(&num(0.0), &num(0.0)), Ok(LookupEq::Match));
    }

    // --- OXP-104: pinned NO-MATCH ------------------------------------------

    #[test]
    fn blank_key_does_not_match_blank_cell() {
        assert_eq!(
            exact_eq(&Value::Blank, &Value::Blank),
            Ok(LookupEq::NoMatch)
        );
    }

    // --- Defer: plausible-but-unconfirmed candidates ------------------------

    #[test]
    fn blank_key_vs_empty_text_cell_defers() {
        assert_eq!(exact_eq(&txt(""), &Value::Blank), Ok(LookupEq::Defer));
    }

    #[test]
    fn blank_key_vs_false_cell_defers() {
        assert_eq!(
            exact_eq(&Value::Bool(false), &Value::Blank),
            Ok(LookupEq::Defer)
        );
    }

    #[test]
    fn zero_key_vs_blank_cell_defers() {
        assert_eq!(exact_eq(&Value::Blank, &num(0.0)), Ok(LookupEq::Defer));
    }

    #[test]
    fn empty_text_key_vs_blank_cell_defers() {
        assert_eq!(exact_eq(&Value::Blank, &txt("")), Ok(LookupEq::Defer));
    }

    #[test]
    fn false_key_vs_blank_cell_defers() {
        assert_eq!(
            exact_eq(&Value::Blank, &Value::Bool(false)),
            Ok(LookupEq::Defer)
        );
    }

    // --- Not plausible: no defer, matches values_equal's NoMatch ------------

    #[test]
    fn blank_key_vs_nonzero_number_cell_is_no_match_not_defer() {
        // Critical: deferring here would break OXP-104's own {1,2,<blank>,4}
        // table (see below) — a non-zero number next to a Blank key must be
        // an ordinary skip, not a search-aborting #UNSUPPORTED!.
        assert_eq!(exact_eq(&num(5.0), &Value::Blank), Ok(LookupEq::NoMatch));
    }

    #[test]
    fn blank_key_vs_nonempty_text_cell_is_no_match() {
        assert_eq!(
            exact_eq(&txt("hello"), &Value::Blank),
            Ok(LookupEq::NoMatch)
        );
    }

    #[test]
    fn blank_key_vs_true_cell_is_no_match() {
        assert_eq!(
            exact_eq(&Value::Bool(true), &Value::Blank),
            Ok(LookupEq::NoMatch)
        );
    }

    // --- Unchanged: neither side Blank --------------------------------------

    #[test]
    fn number_vs_text_stays_type_strict() {
        // `5 <> "5"`: cross-type strictness is untouched by this helper.
        assert_eq!(exact_eq(&txt("5"), &num(5.0)), Ok(LookupEq::NoMatch));
    }

    #[test]
    fn error_cell_propagates() {
        assert_eq!(
            exact_eq(&Value::Error(ErrorKind::Div0), &num(1.0)),
            Err(ErrorKind::Div0)
        );
    }

    #[test]
    fn error_cell_propagates_even_with_blank_key() {
        // A Blank-involved pair where the *other* side is an Error is not a
        // blank-morph question; the error still propagates.
        assert_eq!(
            exact_eq(&Value::Error(ErrorKind::Div0), &Value::Blank),
            Err(ErrorKind::Div0)
        );
    }

    // --- BC-6: lambda-involved pairs always Defer (hoisted above Blank) ------

    #[test]
    fn lambda_cell_vs_nonblank_key_defers() {
        // Regression for the hoist: a lambda paired with a *non-Blank* value
        // never enters the Blank guard, so before the hoist it fell through to
        // `values_equal` → `compare` (which errors on lambdas). It must Defer.
        assert_eq!(
            exact_eq(&Value::test_lambda(), &num(1.0)),
            Ok(LookupEq::Defer)
        );
    }

    #[test]
    fn lambda_key_vs_nonblank_cell_defers() {
        assert_eq!(
            exact_eq(&txt("x"), &Value::test_lambda()),
            Ok(LookupEq::Defer)
        );
    }

    #[test]
    fn lambda_vs_blank_still_defers() {
        // The Blank-involved direction is unchanged by the hoist.
        assert_eq!(
            exact_eq(&Value::test_lambda(), &Value::Blank),
            Ok(LookupEq::Defer)
        );
    }

    // --- approx_touches_blank ------------------------------------------------

    use super::approx_touches_blank;

    #[test]
    fn touches_blank_when_key_is_blank() {
        assert!(approx_touches_blank(&num(5.0), &Value::Blank));
    }

    #[test]
    fn touches_blank_when_cell_is_blank() {
        assert!(approx_touches_blank(&Value::Blank, &num(5.0)));
    }

    #[test]
    fn touches_blank_when_both_are_blank() {
        assert!(approx_touches_blank(&Value::Blank, &Value::Blank));
    }

    #[test]
    fn does_not_touch_blank_for_ordinary_values() {
        assert!(!approx_touches_blank(&num(5.0), &num(5.0)));
        assert!(!approx_touches_blank(&txt("x"), &num(5.0)));
        assert!(!approx_touches_blank(
            &Value::Error(ErrorKind::Div0),
            &num(5.0)
        ));
    }
}

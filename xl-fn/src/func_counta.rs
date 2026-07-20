//! `COUNTA` — counts the cells/values that are not empty.
//!
//! # Provenance
//! Behavior contract: `docs/specs/COUNTA.md` (which cites the Microsoft
//! Learn COUNTA function page).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Counts every value that is **not** [`Value::Blank`] — numbers, text
//!   (including `""`), logicals, and errors all count (COUNTA.md §1). This
//!   is the one function in this family where **scalar and range/array
//!   arguments behave identically**: no numeric coercion happens at all
//!   (COUNTA.md §Coercion).
//! - The classic `""` vs `Blank` distinction: a formula-computed empty
//!   string `""` counts (it is `Value::Text("")`, not `Value::Blank`), but
//!   a truly empty cell does not (COUNTA.md §2). `Value::is_blank` is
//!   exactly the frozen `xl-value` predicate for this split.
//! - **Errors count as present and never propagate.** COUNTA.md §Error
//!   behavior is explicit and unambiguous here (unlike `COUNT`'s
//!   oracle-deferred scalar-error case): "COUNTA is documented as
//!   error-tolerant by design — it counts error cells, it doesn't fail on
//!   them." This applies uniformly to a scalar error argument *and* an
//!   error value streamed from a range/array — for a **genuine** Excel
//!   error. See the Recalc-sentinel exception immediately below for the one
//!   case this does *not* cover.
//! - An omitted argument slot evaluates to `Blank` per
//!   [`CallArgs::eval_scalar`]'s contract, so it is not counted — the same
//!   treatment as a bare reference to a genuinely empty cell.
//! - No oracle-deferred cases beyond the sentinel exception below: COUNTA's
//!   rule ("not blank ⇒ counts", no coercion, no error propagation) fully
//!   covers every genuine `Value`, so nothing else here returns
//!   `#UNSUPPORTED!`.
//!
//! # Recalc sentinels (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`) propagate
//! The "errors count as present" rule above is about **genuine** Excel
//! errors — a real `#DIV/0!`/`#N/A`/… cell, which Excel itself would also
//! show, so counting it as one non-blank value is provably correct. A
//! Recalc sentinel ([`xl_value::ErrorKind::is_recalc_sentinel`]) is not that:
//! it is Recalc's own admission that a cell's value was never actually
//! computed, and the channel that carries it is ambiguous in a way a
//! genuine error is not. `for_each_cell` uses the *same*
//! `Value::Error(ErrorKind::Unsupported)` shape for two different things it
//! cannot tell apart here:
//! 1. a **single in-range cell** whose own formula legitimately produced
//!    `#UNSUPPORTED!` (a real, single non-blank value — COUNTA counting it
//!    once would be *provably correct*, exactly like a genuine error cell);
//! 2. a **synthetic visit standing in for an entire unresolvable range**
//!    (see [`CallArgs::for_each_cell`]'s doc contract) — counting that as
//!    `1` is *provably wrong* whenever the range holds anything other than
//!    exactly one non-blank cell, which is the overwhelmingly common case.
//!
//! Since the two cannot be distinguished from inside this function, the old
//! "count every error as 1" behavior silently returned a wrong count for
//! case 2 every time it occurred. Per Recalc Principle 2 ("never silently
//! wrong"), COUNTA now **propagates** any sentinel it encounters (kind
//! preserved, never collapsed to a different sentinel) — at the first
//! sentinel cell/argument in left-to-right scan order — covering both a
//! scalar argument and a range/array cell. This is a real trade: it gives up
//! the *sometimes*-correct case-1 count in order to stop guessing on the
//! *always*-wrong case-2 count. A future RFC could split the channel (e.g.
//! tag the synthetic whole-range visit distinctly from a real per-cell
//! sentinel) to reclaim case 1's correct count — **not implemented here**,
//! tracked as a follow-up. Genuine (non-sentinel) errors are completely
//! unaffected: they keep counting as 1, exactly as before.
//!
//! ```text
//! // OXP (unassigned): split the synthetic whole-range-unresolvable visit
//! // from a real single-cell sentinel so COUNTA could count the latter
//! // (case 1 above) instead of propagating it — needs a CallArgs channel
//! // change, out of scope for this fix.
//! ```

use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value};

use crate::args::{CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Evaluate a `COUNTA(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut count = 0u64;

    for i in 0..args.count() {
        // RFC 0010: a single-cell REFERENCE rides the range arm; COUNTA's rule
        // ("count every non-blank") is identical on both arms, so the reroute is
        // behavior-preserving here — a non-blank cell still counts, a blank cell
        // still does not.
        match eff_shape(args, i) {
            // Omitted evaluates to `Blank` (not counted), same as a
            // genuinely empty scalar cell reference — see the module docs.
            EffShape::Omitted | EffShape::ScalarLiteral => {
                let v = args.eval_scalar_array_arg(i);
                // Lane 6 (RFC-0011 array context): a genuine multi-cell
                // consumed array reaching COUNTA's scalar arm is UNPINNED — only
                // SUM's fold is pinned (consumed-array spec §4.5 born-refusing
                // boundary; OXP-201). COUNTA does not route through
                // `coerce_number_arg`, so without this guard a `Value::Array`
                // (not an error, not blank) would silently miscount as 1
                // (Principle 2 violation — `unsupported → mismatch`; Excel would
                // count its elements, which we have NOT pinned for COUNTA).
                // Refuse loudly; a 1×1 array collapses to its element (base
                // behavior preserved). Mirrors COUNT's `Array | Ref` refuse arm.
                if matches!(&v, Value::Array(a) if a.as_scalar().is_none()) {
                    return Value::Error(ErrorKind::Unsupported);
                }
                // BC-6 (RFC-0012): a lambda is not a COUNTA-countable value —
                // refuse loudly rather than count it as 1 via the `!is_blank()`
                // fallback below (COUNT has its own lambda arm; COUNTA needs the
                // same). Latent today (no in-engine lambda producer) but the
                // same one-line class of guard.
                if matches!(&v, Value::Lambda(_)) {
                    return Value::Error(ErrorKind::Unsupported);
                }
                // A Recalc sentinel propagates (kind preserved) instead of
                // counting as 1 — see the module docs' case-1-vs-case-2
                // trade. Genuine errors keep counting as 1 (COUNTA.md
                // §Error behavior), handled by the `!is_blank()` fallback.
                if let Value::Error(k) = v {
                    if k.is_recalc_sentinel() {
                        return Value::Error(k);
                    }
                    count += 1;
                    continue;
                }
                if !v.is_blank() {
                    count += 1;
                }
            }
            EffShape::Aggregate => {
                let mut sentinel: Option<ErrorKind> = None;
                args.for_each_cell(i, &mut |v| {
                    if let Value::Error(k) = v {
                        if k.is_recalc_sentinel() {
                            sentinel = Some(*k);
                            return ControlFlow::Break(());
                        }
                        count += 1;
                        return ControlFlow::Continue(());
                    }
                    // `for_each_cell` already elides genuinely-empty cells
                    // upstream, but a defensive check keeps this correct
                    // even against a visitor that still surfaces `Blank`
                    // explicitly (e.g. a materialized cell that itself
                    // computed to `Blank`).
                    if !v.is_blank() {
                        count += 1;
                    }
                    ControlFlow::Continue(())
                });
                if let Some(k) = sentinel {
                    return Value::Error(k);
                }
            }
        }
    }

    Value::number(count as f64)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn counts_numbers_text_bools_and_genuine_errors() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(1.0),
                    txt("x"),
                    Value::Bool(true),
                    Value::Error(ErrorKind::Div0),
                    Value::Blank,
                ])]
            ),
            num(4.0)
        );
    }

    #[test]
    fn empty_string_counts_but_blank_does_not() {
        assert_eq!(
            eval_direct(eval, vec![Range(vec![txt(""), Value::Blank])]),
            num(1.0)
        );
    }

    #[test]
    fn omitted_argument_not_counted() {
        assert_eq!(eval_direct(eval, vec![Omitted]), num(0.0));
    }

    // ---- Recalc sentinels propagate (Principle 2 fix) -----------------

    #[test]
    fn scalar_sentinel_propagates_kind_preserved() {
        for k in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                eval_direct(eval, vec![Scalar(num(1.0)), Scalar(Value::Error(k))]),
                Value::Error(k),
                "{k:?} should propagate, not count as 1"
            );
        }
    }

    #[test]
    fn range_sentinel_propagates_kind_preserved() {
        for k in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                eval_direct(eval, vec![Range(vec![num(1.0), Value::Error(k), num(2.0)])]),
                Value::Error(k),
                "{k:?} should propagate out of the range walk"
            );
        }
    }

    #[test]
    fn scalar_genuine_error_still_counts_as_one_unchanged() {
        // Control: a genuine error argument keeps counting as 1
        // (COUNTA.md §Error behavior), unaffected by this fix.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(1.0)), Scalar(Value::Error(ErrorKind::Na))]
            ),
            num(2.0)
        );
    }

    #[test]
    fn range_genuine_error_still_counts_as_one_unchanged() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), Value::Error(ErrorKind::Na), num(2.0)])]
            ),
            num(3.0)
        );
    }

    // RFC 0010: a single-cell *reference* rides the range arm. COUNTA counts
    // every non-blank value on either arm, so a text reference still counts (1)
    // and a blank reference still does not (0) — behavior-preserving, matching
    // the scalar-literal path.
    #[test]
    fn rfc0010_reference_counts_non_blank_only() {
        assert_eq!(eval_direct(eval, vec![CellRef(txt("x"))]), num(1.0));
        assert_eq!(eval_direct(eval, vec![CellRef(Value::Blank)]), num(0.0));
        // The scalar *literal* path agrees: non-blank text counts.
        assert_eq!(eval_direct(eval, vec![Scalar(txt("x"))]), num(1.0));
    }
}

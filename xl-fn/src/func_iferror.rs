//! `IFERROR` — the canonical single-value error suppressor: return `value`
//! unless it errors, in which case return `value_if_error` instead.
//!
//! # Provenance
//! Behavior contract: `docs/specs/IFERROR.md` (which cites the Microsoft
//! support IFERROR function page,
//! <https://support.microsoft.com/en-us/office/iferror-function-c526fd07-caeb-47b8-8bb6-63f3e417f611>,
//! verified 2026-07-05). IFERROR is named in `implementation-plan.md` §2's
//! error-eating hit-list alongside AGGREGATE/SUMPRODUCT.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Evaluate `value` (argument 0) first — it is *both* the error test and,
//!   on success, the returned value (IFERROR.md §1/§3). If it is **any**
//!   [`Value::Error`], return `value_if_error` (argument 1) instead
//!   (IFERROR.md §1); otherwise return `value` unchanged, passing its
//!   value/type through with no coercion (IFERROR.md §2, §Coercion).
//! - **Lazy w.r.t. `value_if_error`** (IFERROR.md §3): `value_if_error` is
//!   forced *only* when `value` errors. A non-erroring `value` never triggers
//!   evaluation of the fallback branch — so a division-by-zero, a
//!   `#UNSUPPORTED!` construct, or a volatile read sitting in `value_if_error`
//!   cannot affect (or even be observed by) the result.
//! - IFERROR **does not distinguish genuine Excel error subtypes**
//!   (IFERROR.md §Error behavior): unlike `IFNA` (out of Tier-0 scope), it
//!   catches every genuine Excel error kind — `#N/A`, `#VALUE!`, `#DIV/0!`,
//!   `#REF!`, `#NAME?`, `#NUM!`, `#NULL!`, `#GETTING_DATA`, `#SPILL!`,
//!   `#CALC!` — via a kind-agnostic test, exactly the predicate
//!   `func_iserror` uses (restricted to non-sentinel kinds, see below).
//! - If `value_if_error` *itself* evaluates to an error, that error
//!   propagates as the final result — IFERROR does not recursively suppress
//!   its own fallback branch (IFERROR.md §Error behavior).
//!
//! ## Recalc sentinels (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`) are propagated, not caught
//! An earlier revision of this module read IFERROR.md §1's example listing
//! `#UNSUPPORTED!` in the caught set as license to catch all three Recalc
//! sentinels ([`ErrorKind::Unsupported`], [`ErrorKind::Blocked`],
//! [`ErrorKind::Resource`] — [`xl_value::ErrorKind::is_recalc_sentinel`])
//! alongside genuine Excel errors, on the theory that "there is nothing an
//! Excel probe could confirm or refute about Recalc-internal kinds." That
//! reasoning is a non-sequitur: a Recalc sentinel is not a value Excel ever
//! sees — it is Recalc's own declared admission that it failed to compute
//! `value` at all. Catching it and substituting `value_if_error` answers a
//! question ("did `value` error?") about a computation that never actually
//! happened; real Excel might have produced a genuine error there (in which
//! case catching would happen to be right) or a perfectly fine value (in
//! which case catching is silently wrong) — either way it is a guess, which
//! the Recalc design rules's Principle 2 ("never silently wrong") forbids. Concretely, this
//! closes the `IFERROR(MISSINGFN(), 0)` hole: the old behavior returned `0`
//! (a confident, unearned answer) for a function Recalc doesn't implement;
//! the fixed behavior returns `#UNSUPPORTED!` unchanged, so the gap surfaces
//! as `xl-bench`'s explicit `EngineUnsupported` rather than a silent
//! `Mismatch`. Genuine Excel error kinds are unaffected and are still caught
//! and replaced by `value_if_error`, per IFERROR.md §1.
//!
//! ## Array/spill operands are oracle-deferred
//! IFERROR.md's §Semantics pins only the **scalar** case; the `Array behavior:
//! scalar-lift` header line and the spec body never document element-wise
//! catching over a genuine multi-cell `value` (Excel spills a per-element
//! IFERROR result there). A 1×1 range/array lifts to its element and is
//! handled as a scalar (the engine classifies it [`ArgShape::Scalar`]); a
//! wider [`ArgShape::Range`]/[`ArgShape::Array`] operand — in *either*
//! argument position — is refused with [`ErrorKind::Unsupported`] rather than
//! guessing the spilled shape (Recalc Principle 2). See the `OXP
//! (unassigned)` notes at each guard.

use xl_value::{ErrorKind, Value};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Is argument `index` a genuine multi-cell operand (a range or array literal
/// wider than 1×1)?
///
/// The engine classifies a 1×1 range/array as [`ArgShape::Scalar`] (it lifts to
/// its lone element), so only [`ArgShape::Range`]/[`ArgShape::Array`] shapes
/// reach here as *true* multi-cell operands — the array/spill case the spec
/// leaves undocumented. Purely a shape query: it never forces evaluation, so
/// consulting it for the fallback preserves IFERROR's laziness.
fn is_array_operand(args: &mut dyn CallArgs, index: usize) -> bool {
    matches!(args.shape(index), ArgShape::Range | ArgShape::Array)
}

/// Evaluate an `IFERROR(value, value_if_error)` call. See the module docs for
/// the semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Both arguments are guaranteed present by the registry arity check
    // (min_args = max_args = 2).

    // Element-wise IFERROR over a genuine multi-cell `value` (Excel spills a
    // per-element result) is undocumented in IFERROR.md's scalar-only
    // §Semantics — defer loudly rather than let scalar-context coercion turn
    // the array into a spurious `#UNSUPPORTED!` and then "catch" it.
    //
    // OXP (unassigned): grid `IFERROR(A1:A3/B1:B3, 0)` and `IFERROR({1,#N/A,3},
    // 0)` — confirm the spilled rectangle shape and that catching is per-element
    // (each errored cell replaced independently) before implementing.
    if is_array_operand(args, 0) {
        return Value::Error(ErrorKind::Unsupported);
    }

    let value = args.eval_scalar(0);

    // A Recalc sentinel is not a genuine error IFERROR can decide to catch —
    // it is Recalc's declared admission that `value` was never actually
    // computed, so whether it would have errored in real Excel is
    // unknowable. Propagate it unchanged rather than guess (the Recalc design rules
    // Principle 2); `value_if_error` is never forced, exactly like the
    // non-error passthrough below.
    if let Value::Error(kind) = &value
        && kind.is_recalc_sentinel()
    {
        return value;
    }

    if !matches!(value, Value::Error(_)) {
        // Non-error `value` passes through unchanged; `value_if_error` is never
        // forced (laziness, IFERROR.md §3).
        return value;
    }

    // `value` errored with a genuine Excel error kind → the result is
    // `value_if_error`. A multi-cell fallback is the same undocumented spill
    // shape as above.
    //
    // OXP (unassigned): grid `IFERROR(1/0, A1:A3)` — does a multi-cell
    // `value_if_error` spill, or lift/error? The documented case is a scalar
    // fallback.
    if is_array_operand(args, 1) {
        return Value::Error(ErrorKind::Unsupported);
    }
    args.eval_scalar(1)
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    fn err(kind: ErrorKind) -> Value {
        Value::Error(kind)
    }

    // IFERROR.md §Error behavior: the seven classic Excel error kinds are all
    // caught and replaced by `value_if_error`.
    #[test]
    fn iferror_catches_classic_excel_errors() {
        for kind in [
            ErrorKind::Null,
            ErrorKind::Div0,
            ErrorKind::Value,
            ErrorKind::Ref,
            ErrorKind::Name,
            ErrorKind::Num,
            ErrorKind::Na,
        ] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(err(kind)), Scalar(num(0.0))]),
                num(0.0),
                "{kind:?} should be caught by IFERROR"
            );
        }
    }

    // IFERROR.md §1 explicitly lists the dynamic-array-era errors in the
    // caught set (genuine Excel error kinds, not Recalc sentinels).
    #[test]
    fn iferror_catches_modern_excel_errors() {
        for kind in [ErrorKind::GettingData, ErrorKind::Spill, ErrorKind::Calc] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(err(kind)), Scalar(txt("x"))]),
                txt("x"),
                "{kind:?} should be caught by IFERROR"
            );
        }
    }

    // The fix: a Recalc sentinel `value` is not a genuine error IFERROR can
    // decide to catch — it is Recalc's admission that `value` was never
    // computed. IFERROR propagates it unchanged instead of substituting
    // `value_if_error`, closing the `IFERROR(MISSINGFN(), 0)` hole (a
    // `Poison` fallback proves it is never even forced).
    #[test]
    fn iferror_propagates_recalc_sentinels() {
        for kind in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(err(kind)), Poison]),
                err(kind),
                "{kind:?} should propagate unchanged, not be caught"
            );
        }
    }

    // IFERROR.md §2: a non-erroring `value` passes through unchanged, for every
    // value kind — number, text, boolean, blank.
    #[test]
    fn iferror_passes_non_error_value_through() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(num(42.0)), Scalar(num(0.0))]),
            num(42.0)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(txt("ok")), Scalar(num(0.0))]),
            txt("ok")
        );
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Scalar(Value::Bool(true)), Scalar(num(0.0))]
            ),
            Value::Bool(true)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(Value::Blank), Scalar(num(0.0))]),
            Value::Blank
        );
    }

    // IFERROR.md §3 (laziness): when `value` does not error, `value_if_error`
    // is never evaluated. A `Poison` fallback panics if forced, so the passing
    // assertion *is* the proof that the fallback stayed untouched.
    #[test]
    fn iferror_does_not_evaluate_fallback_when_value_ok() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(num(7.0)), Poison]),
            num(7.0)
        );
    }

    // IFERROR.md §1: the fallback is returned on error — and returned as-is,
    // whatever its (non-error) value/type.
    #[test]
    fn iferror_returns_fallback_on_error() {
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Scalar(err(ErrorKind::Div0)), Scalar(txt("n/a"))]
            ),
            txt("n/a")
        );
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Scalar(err(ErrorKind::Na)), Scalar(Value::Blank)]
            ),
            Value::Blank
        );
    }

    // IFERROR.md §Error behavior: IFERROR does not suppress *its own* fallback
    // branch — an errored `value_if_error` propagates as the final result.
    #[test]
    fn iferror_propagates_fallback_error() {
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Scalar(err(ErrorKind::Na)), Scalar(err(ErrorKind::Div0))]
            ),
            err(ErrorKind::Div0)
        );
    }

    // Array/spill deferral: a genuine multi-cell `value` is refused with
    // `#UNSUPPORTED!` (OXP), never silently coerced-then-"caught". The array
    // mixes a non-error and an error so no per-element story is being assumed.
    #[test]
    fn iferror_defers_multicell_value() {
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Range(vec![num(1.0), err(ErrorKind::Na)]), Scalar(num(0.0))]
            ),
            err(ErrorKind::Unsupported)
        );
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Array(vec![num(1.0), num(2.0)]), Scalar(num(0.0))]
            ),
            err(ErrorKind::Unsupported)
        );
    }

    // Array/spill deferral, fallback position: reached only because `value`
    // errored, so this also confirms the multi-cell fallback guard fires
    // *after* the error test (laziness intact) rather than spilling a guess.
    #[test]
    fn iferror_defers_multicell_fallback_on_error() {
        assert_eq!(
            eval_direct(
                super::eval,
                vec![
                    Scalar(err(ErrorKind::Value)),
                    Range(vec![num(1.0), num(2.0)])
                ]
            ),
            err(ErrorKind::Unsupported)
        );
    }

    // A multi-cell fallback is *not* consulted when `value` is fine — laziness
    // means the fallback guard never runs, so `value` still passes through.
    #[test]
    fn iferror_ignores_multicell_fallback_when_value_ok() {
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Scalar(num(5.0)), Range(vec![num(1.0), num(2.0)])]
            ),
            num(5.0)
        );
    }
}

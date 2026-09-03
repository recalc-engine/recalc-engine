//! `ISERROR` — TRUE iff `value` is any error value, containing the error
//! rather than letting it propagate.
//!
//! # Provenance
//! Behavior contract: `docs/specs/ISERROR.md` (Microsoft Learn combined
//! "IS functions" reference page).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `TRUE` iff `value` evaluates to [`Value::Error`], for **any**
//!   [`ErrorKind`] — the broadest predicate in the ISERROR/ISERR/ISNA
//!   family; unlike `ISERR` (out of Tier-0 scope), ISERROR also catches
//!   `#N/A` (ISERROR.md §1/§2).
//! - `FALSE` for every non-error value/kind: number, text, boolean, blank
//!   (ISERROR.md §3).
//!
//! ## Recalc sentinels (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`) are propagated, not answered
//! `ErrorKind` carries three variants with no Excel counterpart:
//! [`ErrorKind::Unsupported`], [`ErrorKind::Blocked`], and
//! [`ErrorKind::Resource`] ([`xl_value::ErrorKind::is_recalc_sentinel`]).
//! An earlier revision of this module argued that, because ISERROR.md's
//! spec-writer scope is genuine Excel error *values*, the kind-agnostic
//! `matches!(v, Value::Error(_))` reading "naturally" extended to these
//! three too, and that there was "nothing an Excel probe could confirm or
//! refute about Recalc-internal error kinds." That reasoning is a
//! non-sequitur: a Recalc sentinel is not a value Excel ever sees — it is
//! Recalc's own declared admission that it failed to compute `value` at
//! all. The question ISERROR is actually answering is not "what does Excel
//! do with its own error kinds" (settled, in scope) but "what would Excel
//! have returned for the value Recalc never actually computed" — and
//! *that* is exactly the kind of unknowable fact the Recalc design rules's Principle 2
//! ("never silently wrong") forbids guessing at. `TRUE` may happen to be
//! right (Excel would also have errored) or wrong (Excel would have
//! produced a perfectly fine value); either way it is a guess wearing a
//! boolean's clothes. So ISERROR instead **propagates the sentinel
//! unchanged**, surfacing the gap as `xl-bench`'s explicit
//! `EngineUnsupported` rather than a silent `Mismatch`. Genuine Excel error
//! kinds are unaffected and still answer `TRUE`, per ISERROR.md §1/§2.
//!
//! ## Error behavior — the containment guarantee
//! ISERROR is the one IS-function whose entire purpose is error
//! *containment*: `args.eval_scalar(0)` fully evaluates the argument
//! (however deep the error originates within it), yielding a
//! [`Value::Error`] rather than a Rust `Err` — evaluation never "throws" in
//! this codebase, it returns an error *value*. ISERROR simply inspects
//! that returned value's kind and converts it to a boolean, so the error
//! is contained here rather than bubbling further up the formula tree
//! (ISERROR.md §Error behavior).
//!
//! # Array-position arguments (M2 lane 6 follow-up, 2026-09-04)
//! An argument in a range/array position is evaluated under the consumed-array
//! gate (RFC-0011; `docs/plans/2026-07-14-consumed-array-eval-spec.md` §2).
//! A materialized multi-cell array reaching this function is **refused** with a
//! loud `#UNSUPPORTED!` plus an engine diagnostic (spec §4, born-refusing
//! boundary): only the SUM/SUMPRODUCT consumers are oracle-pinned (OXP-201), and
//! the legacy alternative — a silent, host-row-dependent implicit intersection —
//! is a "never silently wrong" violation. Plain ranges are unchanged.

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `ISERROR(...)` call. See the module docs for the semantics
/// and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let v = args.eval_scalar(0);
    // A `Value::Array` reaching a variant test — a consumed range materialized
    // under the RFC-0011 array-context gate (`SUM(ISNUMBER(range)*1)`) or a
    // function-produced array — has no oracle-pinned element-wise semantics
    // here. Refuse loudly rather than answer FALSE for the array as a whole,
    // which fed a silent 0 into the enclosing aggregator (Principle 2).
    if matches!(v, Value::Array(_)) {
        return Value::Error(ErrorKind::Unsupported);
    }
    if let Value::Error(kind) = &v
        && kind.is_recalc_sentinel()
    {
        return v;
    }
    Value::Bool(matches!(v, Value::Error(_)))
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{Array, ErrorKind, Value};

    // ISERROR.md §1/§2: TRUE for every genuine Excel error kind, including
    // #N/A (the broadest predicate in the family).
    #[test]
    fn iserror_true_for_genuine_error_kinds() {
        for kind in [
            ErrorKind::Null,
            ErrorKind::Div0,
            ErrorKind::Value,
            ErrorKind::Ref,
            ErrorKind::Name,
            ErrorKind::Num,
            ErrorKind::Na,
            ErrorKind::GettingData,
            ErrorKind::Spill,
            ErrorKind::Calc,
        ] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(Value::Error(kind))]),
                Value::Bool(true),
                "{kind:?} should be caught by ISERROR"
            );
        }
    }

    // ISERROR.md §3: FALSE for every non-error kind.
    #[test]
    fn iserror_false_for_non_error_values() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(num(42.0))]),
            Value::Bool(false)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(txt("x"))]),
            Value::Bool(false)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(Value::Bool(false))]),
            Value::Bool(false)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(Value::Blank)]),
            Value::Bool(false)
        );
    }

    // The fix: a Recalc sentinel argument is propagated unchanged, never
    // laundered into a guessed TRUE.
    #[test]
    fn iserror_propagates_recalc_sentinels() {
        for kind in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(Value::Error(kind))]),
                Value::Error(kind),
                "{kind:?} should propagate unchanged, not report TRUE"
            );
        }
    }
    // A materialized multi-cell array (array-context gate) or a 1×1 computed
    // array has no pinned element-wise IS* rule: loud `#UNSUPPORTED!`, never a
    // silent FALSE.
    #[test]
    fn array_operand_refuses_loudly() {
        let arr = Value::Array(Array::new(2, 1, vec![num(1.0), txt("a")]).unwrap());
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(arr)]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

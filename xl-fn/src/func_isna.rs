//! `ISNA` — TRUE iff `value` is specifically the `#N/A` error.
//!
//! # Provenance
//! No `docs/specs/ISNA.md` exists in this pass. Reconstructed clean-room
//! from the same combined Microsoft Learn "IS functions" reference page
//! already cited by the sibling specs
//! (`docs/specs/ISNUMBER.md`/`ISERROR.md`/`ISBLANK.md`/`ISTEXT.md`):
//! `https://support.microsoft.com/en-us/office/is-functions-0f2d7971-6019-40a0-a171-f2d869135665`,
//! whose text is explicit and unambiguous: `ISNA(value)` returns `TRUE` if
//! `value` refers to the `#N/A` error value, `FALSE` otherwise. This is the
//! narrowest predicate in the ISERROR/ISERR/ISNA family — contrast
//! `ISERROR`, which matches every error kind (`func_iserror.rs`); `ISNA`
//! matches only `#N/A`.
//!
//! # Semantics implemented
//! - `TRUE` iff `value` evaluates to `Value::Error(ErrorKind::Na)`.
//! - `FALSE` for every *other genuine Excel* error kind — none of which are
//!   `#N/A` — and for every non-error value (number, text, boolean, blank).
//! - Never propagates a *genuine* error: like every IS-function, `value` is
//!   fully evaluated first (an error is a returned [`Value::Error`], never a
//!   Rust `Err` in this codebase — see `func_iserror.rs`'s containment note)
//!   and then only its kind is inspected; a `#N/A` argument returns `TRUE`
//!   rather than bubbling the `#N/A` further up the formula tree, and any
//!   other genuine-Excel-error argument is contained the same way and
//!   reported as `FALSE`.
//!
//! ## Recalc sentinels (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`) are propagated
//! This crate's Recalc-specific kinds ([`ErrorKind::Unsupported`],
//! [`ErrorKind::Blocked`], [`ErrorKind::Resource`] —
//! [`ErrorKind::is_recalc_sentinel`]) are not genuine values ISNA can
//! explicitly answer `#N/A`-or-not about: each is Recalc's declared admission
//! that `value` was never actually computed, so whether real Excel would
//! have produced `#N/A` there is unknowable. Reporting `FALSE` (the old
//! behavior) would launder that gap into a confident, possibly-wrong
//! answer. Per Recalc Principle 2 ("never silently wrong"), ISNA instead
//! propagates the sentinel unchanged, surfacing it as `xl-bench`'s explicit
//! `EngineUnsupported` rather than a silent `Mismatch`.
//!
//! No behavior here is ambiguous or oracle-deferred: the source's own text
//! is a complete, exact predicate definition for genuine Excel error kinds,
//! and the sentinel-propagation contract is a Recalc-internal decision
//! governed directly by Principle 2, not something an Excel probe could
//! confirm or refute.
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

/// Evaluate an `ISNA(...)` call. See the module docs for the semantics and
/// their provenance.
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
    Value::Bool(matches!(v, Value::Error(ErrorKind::Na)))
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{Array, ErrorKind, Value};

    // TRUE only for #N/A.
    #[test]
    fn isna_true_for_na() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(Value::Error(ErrorKind::Na))]),
            Value::Bool(true)
        );
    }

    // FALSE for every other genuine Excel error kind, and for non-error
    // values.
    #[test]
    fn isna_false_for_other_genuine_errors_and_non_errors() {
        for kind in [ErrorKind::Div0, ErrorKind::Value, ErrorKind::Ref] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(Value::Error(kind))]),
                Value::Bool(false),
                "{kind:?} should report FALSE, not propagate"
            );
        }
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(num(5.0))]),
            Value::Bool(false)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(txt("x"))]),
            Value::Bool(false)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(Value::Blank)]),
            Value::Bool(false)
        );
    }

    // The fix: a Recalc sentinel argument is propagated unchanged, never
    // laundered into a guessed FALSE.
    #[test]
    fn isna_propagates_recalc_sentinels() {
        for kind in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(Value::Error(kind))]),
                Value::Error(kind),
                "{kind:?} should propagate unchanged, not report FALSE"
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

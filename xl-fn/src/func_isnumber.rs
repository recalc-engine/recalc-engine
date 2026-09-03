//! `ISNUMBER` — TRUE iff `value` is a `Number`.
//!
//! # Provenance
//! Behavior contract: `docs/specs/ISNUMBER.md` (Microsoft Learn combined
//! "IS functions" reference page).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `TRUE` iff `value` evaluates to [`Value::Number`] — including
//!   date/time serials, which are stored as numbers (ISNUMBER.md §1).
//! - `FALSE` for numeric-looking text (`"5"`); the kind is inspected
//!   directly, never coerced (ISNUMBER.md §2/§Coercion).
//! - `FALSE` for `Bool` — booleans are a distinct kind from numbers for
//!   IS-function purposes (ISNUMBER.md §3).
//! - `FALSE` for `Blank` and for any *genuine Excel* error, **without
//!   propagating** the error (ISNUMBER.md §4/§Error behavior; the
//!   "IS-functions report, do not propagate" hit-list rule shared by the
//!   whole family).
//!
//! ## Recalc sentinels (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`) are propagated
//! A Recalc sentinel ([`xl_value::ErrorKind::is_recalc_sentinel`]) is not a
//! value ISNUMBER can explicitly answer a predicate about — it is Recalc's own
//! declared admission that `value` was never actually computed, so whether
//! real Excel would have produced a number there is unknowable. Reporting
//! `FALSE` (the old behavior) would launder that gap into a confident,
//! possibly-wrong answer. Per Recalc Principle 2 ("never silently
//! wrong"), ISNUMBER instead propagates the sentinel unchanged, so a
//! Recalc-caused gap surfaces as `xl-bench`'s explicit `EngineUnsupported`
//! rather than a silent `Mismatch`.
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

/// Evaluate an `ISNUMBER(...)` call. See the module docs for the semantics
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
    Value::Bool(matches!(v, Value::Number(_)))
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{Array, ErrorKind, Value};

    // ISNUMBER.md §1: TRUE for numbers.
    #[test]
    fn isnumber_true_for_number() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(num(5.0))]),
            Value::Bool(true)
        );
    }

    // ISNUMBER.md §2/§3/§4: FALSE for text, bool, blank, and every genuine
    // Excel error kind — including `#N/A`.
    #[test]
    fn isnumber_false_for_non_number_and_genuine_errors() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(txt("5"))]),
            Value::Bool(false)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(Value::Bool(true))]),
            Value::Bool(false)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(Value::Blank)]),
            Value::Bool(false)
        );
        for kind in [ErrorKind::Na, ErrorKind::Div0, ErrorKind::Value] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(Value::Error(kind))]),
                Value::Bool(false),
                "{kind:?} should report FALSE, not propagate"
            );
        }
    }

    // The fix: a Recalc sentinel argument is propagated unchanged, never
    // laundered into a guessed FALSE.
    #[test]
    fn isnumber_propagates_recalc_sentinels() {
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

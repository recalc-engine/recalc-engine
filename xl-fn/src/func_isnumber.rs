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

use xl_value::Value;

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `ISNUMBER(...)` call. See the module docs for the semantics
/// and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let v = args.eval_scalar(0);
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
    use xl_value::{ErrorKind, Value};

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
}

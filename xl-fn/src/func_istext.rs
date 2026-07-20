//! `ISTEXT` — TRUE iff `value` is `Text`.
//!
//! # Provenance
//! Behavior contract: `docs/specs/ISTEXT.md` (Microsoft Learn combined "IS
//! functions" reference page).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `TRUE` iff `value` evaluates to [`Value::Text`], including
//!   numeric-looking text (`"5"` → `TRUE`) and the empty string (`""` →
//!   `TRUE`) — the kind is inspected directly, never coerced (ISTEXT.md
//!   §1/§3/§Coercion). This is the direct complement of `ISBLANK("")` =
//!   `FALSE` (ISBLANK.md's ""-vs-Blank hit-list item, viewed from the
//!   opposite direction).
//! - `FALSE` for numbers, booleans, `Blank`, and any *genuine Excel* error,
//!   **without propagating** the error (ISTEXT.md §2/§Error behavior; the
//!   "IS-functions report, do not propagate" hit-list rule shared by the
//!   whole family).
//!
//! ## Recalc sentinels (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`) are propagated
//! A Recalc sentinel ([`xl_value::ErrorKind::is_recalc_sentinel`]) is not a
//! value ISTEXT can explicitly answer a predicate about — it is Recalc's own
//! declared admission that `value` was never actually computed, so whether
//! real Excel would have produced text there is unknowable. Reporting
//! `FALSE` (the old behavior) would launder that gap into a confident,
//! possibly-wrong answer. Per Recalc Principle 2 ("never silently
//! wrong"), ISTEXT instead propagates the sentinel unchanged, so a
//! Recalc-caused gap surfaces as `xl-bench`'s explicit `EngineUnsupported`
//! rather than a silent `Mismatch`.

use xl_value::Value;

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `ISTEXT(...)` call. See the module docs for the semantics
/// and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let v = args.eval_scalar(0);
    if let Value::Error(kind) = &v
        && kind.is_recalc_sentinel()
    {
        return v;
    }
    Value::Bool(matches!(v, Value::Text(_)))
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // ISTEXT.md §1/§3: TRUE for text, including numeric-looking and empty.
    #[test]
    fn istext_true_for_text() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(txt("5"))]),
            Value::Bool(true)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(txt(""))]),
            Value::Bool(true)
        );
    }

    // ISTEXT.md §2: FALSE for number, bool, blank, and every genuine Excel
    // error kind — including `#N/A`.
    #[test]
    fn istext_false_for_non_text_and_genuine_errors() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(num(5.0))]),
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
    fn istext_propagates_recalc_sentinels() {
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

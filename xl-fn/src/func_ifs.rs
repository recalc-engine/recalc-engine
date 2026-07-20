//! `IFS` — return the value paired with the first `logical_test` that is TRUE.
//!
//! # Provenance
//! Behavior contract: Microsoft support "IFS function"
//! (<https://support.microsoft.com/en-us/office/ifs-function-36329a26-37b2-467c-972b-4a39bd951d45>,
//! verified by WebFetch 2026-07-15). No `docs/specs/IFS.md` exists in this
//! pass. Boolean coercion of each `logical_test` is `xl-value`'s frozen
//! [`to_bool`] contract — the same one `IF` uses for its test (`func_if`).
//!
//! # Behavior contract (one line)
//! `IFS(test1, val1, [test2, val2], …)` evaluates the tests left-to-right and
//! returns the `valN` of the **first** TRUE `testN`; no TRUE test → `#N/A`.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - Up to 127 `logical_test`/`value_if_true` pairs ("the IFS function allows
//!   you to test up to 127 different conditions"). Each `logical_test` is
//!   evaluated in scalar context, in argument order, and coerced with
//!   [`to_bool`].
//! - The **first** test that coerces to TRUE selects its paired value, which is
//!   the only value forced ([`eval_scalar`](CallArgs::eval_scalar)) — lazy, so
//!   an error / `#UNSUPPORTED!` / division-by-zero inside a non-selected value
//!   cannot affect the result. Later tests after the first TRUE are not
//!   evaluated either.
//! - "If … no logical tests are found to be TRUE, [IFS] returns the #N/A
//!   error." When every complete pair's test is FALSE, the result is `#N/A`.
//! - "If a `logical_test` … is evaluated and resolves to a value other than
//!   TRUE or FALSE, this function returns a #VALUE! error." Non-coercible text
//!   is `#VALUE!` via [`to_bool`]. A `Blank` test coerces to `FALSE` (the
//!   frozen `to_bool`/`IF` rule); an **error** value in a test propagates its
//!   own kind (leftmost first).
//!
//! # Dangling trailing test (odd argument count) — REFUSED (loud, whole call)
//! A trailing `logical_test` with no `value_if_true` is the documented
//! "You've entered too few arguments for this function" case, which Excel
//! rejects at *formula entry*, so its runtime behavior in a malformed /
//! third-party file is entirely unobserved. `IFS` refuses the **whole call**
//! with `#UNSUPPORTED!` up front — *before evaluating any argument* — rather
//! than guess how a structurally invalid call evaluates. An earlier complete
//! pair matching does not rescue it: that branch is equally unpinned (we cannot
//! know Excel would not reject the call before ever evaluating it). One farm
//! probe settles the true behavior. See
//! `docs/plans/2026-07-15-lane5-probe-needed.md` (L5-5).

use xl_value::{ErrorKind, Value, to_bool};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `IFS(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let count = args.count();
    // Registry enforces min_args = 2 (at least one test/value pair).
    // An ODD argument count means a trailing `logical_test` with no paired
    // `value_if_true` — Excel's "You've entered too few arguments" case, which
    // it rejects at *formula entry*, so the runtime behavior of such a malformed
    // / third-party file is entirely unobserved. Refuse the WHOLE call up front
    // rather than guess how a structurally invalid call evaluates: an earlier
    // complete pair matching does NOT rescue it, because that branch is equally
    // unpinned (we cannot know Excel wouldn't reject before evaluating). L5-5.
    if !count.is_multiple_of(2) {
        return Value::Error(ErrorKind::Unsupported);
    }
    let num_pairs = count / 2;

    for k in 0..num_pairs {
        let test_index = 2 * k;
        let value_index = 2 * k + 1;
        match to_bool(&args.eval_scalar(test_index)) {
            // First TRUE test wins: force ONLY its paired value (lazy).
            Ok(true) => return args.eval_scalar(value_index),
            Ok(false) => continue,
            // Non-coercible test → #VALUE! (via to_bool); an error-valued test
            // propagates its own kind. Either way, later pairs are not
            // evaluated.
            Err(k) => return Value::Error(k),
        }
    }

    // Even arity, exhausted with no TRUE test → #N/A (documented).
    Value::Error(ErrorKind::Na)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // First TRUE test selects its value. IFS(FALSE, "a", TRUE, "b", TRUE, "c")
    // → "b" (first TRUE at pair 2).
    #[test]
    fn first_true_test_selects_its_value() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(false)),
                    Scalar(txt("a")),
                    Scalar(Value::Bool(true)),
                    Scalar(txt("b")),
                    Scalar(Value::Bool(true)),
                    Scalar(txt("c")),
                ]
            ),
            txt("b")
        );
    }

    // A single TRUE pair returns its value.
    #[test]
    fn single_true_pair() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Bool(true)), Scalar(num(42.0))]),
            num(42.0)
        );
    }

    // Number test coerces via to_bool: nonzero → TRUE, 0 → FALSE.
    #[test]
    fn numeric_test_coercion() {
        // IFS(0, "a", 7, "b") → "b" (0 is FALSE, 7 is TRUE).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.0)),
                    Scalar(txt("a")),
                    Scalar(num(7.0)),
                    Scalar(txt("b"))
                ]
            ),
            txt("b")
        );
    }

    // No TRUE test (even arity) → #N/A.
    #[test]
    fn no_true_test_is_na() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(false)),
                    Scalar(txt("a")),
                    Scalar(num(0.0)),
                    Scalar(txt("b")),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // Lazy: only the selected value is forced. IFS(TRUE, "x", TRUE, <poison>)
    // returns "x" without forcing the second pair's value.
    #[test]
    fn is_lazy_unselected_values_not_evaluated() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(true)),
                    Scalar(txt("x")),
                    Scalar(Value::Bool(true)),
                    Poison
                ]
            ),
            txt("x")
        );
    }

    // Lazy: a later selected value must not force an earlier (false-branch)
    // value. IFS(FALSE, <poison>, TRUE, "y") → "y".
    #[test]
    fn is_lazy_earlier_false_value_not_evaluated() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(false)),
                    Poison,
                    Scalar(Value::Bool(true)),
                    Scalar(txt("y"))
                ]
            ),
            txt("y")
        );
    }

    // A non-coercible text test → #VALUE! (via to_bool).
    #[test]
    fn non_logical_test_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("banana")), Scalar(txt("a"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // An error-valued test propagates its own kind (and stops evaluation).
    #[test]
    fn error_in_test_propagates() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Div0)), Poison]),
            Value::Error(ErrorKind::Div0)
        );
    }

    // A blank test coerces to FALSE (to_bool), so IFS falls through to the next
    // pair. IFS(Blank, "a", TRUE, "b") → "b".
    #[test]
    fn blank_test_is_false() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Blank),
                    Scalar(txt("a")),
                    Scalar(Value::Bool(true)),
                    Scalar(txt("b"))
                ]
            ),
            txt("b")
        );
    }

    // Odd arity (dangling trailing test) is REFUSED up front — even when an
    // earlier complete pair WOULD match. The whole structurally-invalid call is
    // unpinned, so it must not compute a result (L5-5). IFS(TRUE, "a", <test>)
    // → #UNSUPPORTED!, and the poison trailing test is never even evaluated.
    #[test]
    fn odd_arity_refused_upfront_even_with_early_match() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Bool(true)), Scalar(txt("a")), Poison]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // Odd arity with NO earlier match is likewise refused loudly (L5-5).
    #[test]
    fn odd_arity_no_match_refused_loudly() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(false)),
                    Scalar(txt("a")),
                    Scalar(Value::Bool(false))
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

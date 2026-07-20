//! `QUOTIENT` — integer portion of `numerator / denominator`, discarding the
//! remainder.
//!
//! # Provenance
//! Behavior contract: `docs/specs/QUOTIENT.md` (Microsoft Learn QUOTIENT
//! function page, verified 2026-07-11). Coercion via `xl-value`'s
//! [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Returns the integer portion of `numerator / denominator`, discarding
//!   the remainder (QUOTIENT.md §1).
//! - Truncates **toward zero** (not floored toward `-inf`, unlike `MOD`'s
//!   `INT` building block): `QUOTIENT(-10,3)` = `-3`, matching the
//!   documented example, not `-4` (QUOTIENT.md §Examples).
//! - `denominator = 0` -> `#DIV/0!` (general division-by-zero fallout —
//!   the documented page doesn't call this out as an explicit special case
//!   the way MOD's page does, but it is the necessary consequence of
//!   dividing by zero; QUOTIENT.md §3).
//! - Non-numeric, non-coercible argument -> `#VALUE!`, explicitly
//!   documented (QUOTIENT.md §Remarks). `numerator` is coerced (and its
//!   error surfaced) before `denominator`, the same left-to-right
//!   argument-evaluation-order precedent `MOD`/`SUM` already follow.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `QUOTIENT(numerator, denominator)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let numerator = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let denominator = match to_number(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    if denominator == 0.0 {
        return Value::Error(ErrorKind::Div0);
    }
    // QUOTIENT.md §Examples: truncate toward zero, e.g. QUOTIENT(-10,3) =
    // -3 (not -4, which floor toward -inf would give).
    Value::number((numerator / denominator).trunc())
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::{ErrorKind, Value};

    use super::eval;

    #[test]
    fn positive_positive() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(10.0)), Scalar(num(3.0))]),
            num(3.0)
        );
    }

    #[test]
    fn negative_numerator_truncates_toward_zero() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-10.0)), Scalar(num(3.0))]),
            num(-3.0)
        );
    }

    #[test]
    fn negative_denominator_truncates_toward_zero() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(10.0)), Scalar(num(-3.0))]),
            num(-3.0)
        );
    }

    #[test]
    fn even_division() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(5.0)), Scalar(num(2.0))]),
            num(2.0)
        );
    }

    #[test]
    fn denominator_zero_is_div0() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0)), Scalar(num(0.0))]),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn text_coercion() {
        use crate::test_support::txt;
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("10")), Scalar(txt("3"))]),
            num(3.0)
        );
    }

    #[test]
    fn error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Ref)), Scalar(num(3.0))]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }
}

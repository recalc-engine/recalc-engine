//! `SQRT` — the (non-negative) square root of a number.
//!
//! # Provenance
//! Behavior contract: `docs/specs/SQRT.md` (which cites the Microsoft Learn
//! SQRT function page). Coercion is deferred entirely to `xl-value`'s
//! [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Coerce the one argument via scalar numeric coercion (bool -> 1/0,
//!   numeric text -> number, blank -> 0) (SQRT.md §Coercion).
//! - If the coerced number is negative -> `#NUM!` (SQRT has no result in the
//!   reals for a negative radicand; SQRT.md §1).
//! - Otherwise return its non-negative square root via plain `f64::sqrt`
//!   (no fast-math/approximation path — the Recalc design rules) (SQRT.md §1). `SQRT(0)` =
//!   `0`.
//! - Non-numeric, non-coercible text -> `#VALUE!` (from `to_number`); an
//!   error-valued argument propagates as-is, no special containment
//!   (SQRT.md §Error behavior).

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `SQRT(number)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    match to_number(&args.eval_scalar(0)) {
        Ok(n) if n < 0.0 => Value::Error(ErrorKind::Num),
        Ok(n) => Value::number(n.sqrt()),
        Err(k) => Value::Error(k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    #[test]
    fn perfect_squares_and_zero() {
        assert_eq!(eval_direct(eval, vec![Scalar(num(9.0))]), num(3.0));
        assert_eq!(eval_direct(eval, vec![Scalar(num(4.0))]), num(2.0));
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.0))]), num(0.0));
    }

    #[test]
    fn irrational_matches_f64_sqrt() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.0))]),
            num(2.0_f64.sqrt())
        );
    }

    #[test]
    fn negative_is_num_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-1.0))]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn text_coercion() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("16"))]), num(4.0));
    }

    #[test]
    fn error_argument_propagates() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Error(ErrorKind::Div0)
        );
    }
}

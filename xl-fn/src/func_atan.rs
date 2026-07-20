//! `ATAN` — the arctangent (inverse tangent) of a number, in **radians**.
//!
//! # Provenance
//! Microsoft Learn ATAN function page
//! (`https://support.microsoft.com/en-us/office/atan-function-50746fa8-630a-406b-81d0-4a2aed395543`).
//! Coercion is deferred entirely to `xl-value`'s [`to_number`]. ATAN's
//! documented contract is unambiguous: "Returns the arctangent … the returned
//! angle is given in radians in the range -pi/2 to pi/2." It has **no domain
//! restriction** (defined for every real), so — like `SIN`/`COS` — the core is
//! implemented directly rather than queued as a probe.
//!
//! # Semantics implemented
//! - Coerce the one argument via scalar numeric coercion (bool → 1/0, numeric
//!   text → number, blank → 0).
//! - Return `atan(number)` via plain `f64::atan` (no fast-math / approximation
//!   path — the Recalc design rules), always in `(-π/2, π/2)`. `ATAN(0)` = `0`, `ATAN(1)` =
//!   `π/4`. The result is always finite: ATAN has no `#NUM!` path.
//! - Non-numeric / non-coercible text yields `#VALUE!` (from `to_number`); an
//!   error-valued argument propagates as-is, no ATAN-specific containment.

use xl_value::{Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `ATAN(number)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    match to_number(&args.eval_scalar(0)) {
        Ok(n) => Value::number(n.atan()),
        Err(k) => Value::Error(k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::ErrorKind;

    #[test]
    fn atan_of_zero_is_zero() {
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.0))]), num(0.0));
    }

    #[test]
    fn atan_of_one_is_quarter_pi() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0))]),
            num(std::f64::consts::FRAC_PI_4)
        );
    }

    #[test]
    fn matches_f64_atan_all_reals() {
        // No domain restriction — even a huge magnitude is in range.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-2.5))]),
            num((-2.5_f64).atan())
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1e300))]),
            num(1e300_f64.atan())
        );
    }

    #[test]
    fn coerces_numeric_text_and_bool() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("0"))]), num(0.0));
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::bool(true))]),
            num(1.0_f64.atan())
        );
    }

    #[test]
    fn non_numeric_text_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("x"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn error_argument_propagates() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Error(ErrorKind::Div0)
        );
    }
}

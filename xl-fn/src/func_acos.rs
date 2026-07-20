//! `ACOS` — the arccosine (inverse cosine) of a number, in **radians**.
//!
//! # Provenance
//! Microsoft Learn ACOS function page
//! (`https://support.microsoft.com/en-us/office/acos-function-cb73173f-d089-4582-afa1-76e5524b5d5b`).
//! Coercion is deferred entirely to `xl-value`'s [`to_number`]. The domain
//! restriction ("Number … must be from -1 to 1") is documented directly on the
//! Microsoft Learn page and is therefore not a guess — mirroring `SQRT`'s
//! documented negative-radicand `#NUM!` and the sibling `ASIN`.
//!
//! # Semantics implemented
//! - Coerce the one argument via scalar numeric coercion (bool → 1/0, numeric
//!   text → number, blank → 0).
//! - Domain: `number < -1` or `number > 1` → `#NUM!` (arccosine is real only on
//!   `[-1, 1]`; documented).
//! - Otherwise return `acos(number)` via plain `f64::acos` (no fast-math —
//!   the Recalc design rules), always in `[0, π]`. `ACOS(1)` = `0`, `ACOS(0)` = `π/2`,
//!   `ACOS(-1)` = `π`.
//! - Non-numeric / non-coercible text yields `#VALUE!` (from `to_number`); an
//!   error-valued argument propagates as-is.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `ACOS(number)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    match to_number(&args.eval_scalar(0)) {
        Ok(n) if !(-1.0..=1.0).contains(&n) => Value::Error(ErrorKind::Num),
        Ok(n) => Value::number(n.acos()),
        Err(k) => Value::Error(k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    #[test]
    fn acos_of_one_is_zero() {
        assert_eq!(eval_direct(eval, vec![Scalar(num(1.0))]), num(0.0));
    }

    #[test]
    fn acos_at_domain_points() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0))]),
            num(std::f64::consts::FRAC_PI_2)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-1.0))]),
            num(std::f64::consts::PI)
        );
    }

    #[test]
    fn matches_f64_acos_interior() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.5))]),
            num(0.5_f64.acos())
        );
    }

    #[test]
    fn out_of_domain_is_num_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.5))]),
            Value::Error(ErrorKind::Num)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-1.0001))]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn coerces_numeric_text() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("1"))]), num(0.0));
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

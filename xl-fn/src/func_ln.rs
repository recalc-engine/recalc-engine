//! `LN` — natural logarithm (base *e*) of a number.
//!
//! # Provenance
//! Behavior contract: `docs/specs/LN.md` (which cites the Microsoft Learn LN
//! function page). Coercion is deferred entirely to `xl-value`'s
//! [`to_number`]; the domain check (`number <= 0` -> `#NUM!`) is documented
//! directly on the Microsoft Learn page and is not a guess (LN.md §Domain).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Coerce the one argument via scalar numeric coercion (bool -> 1/0,
//!   numeric text -> number, blank -> 0) (LN.md §Coercion).
//! - `number <= 0` (including `0` itself) -> `#NUM!`; natural logarithm is
//!   only defined for positive reals (LN.md §Domain, §1).
//! - Otherwise returns `number.ln()` (natural log base *e*, via `f64::ln`,
//!   no fast-math) (LN.md §1).
//! - Non-numeric, non-coercible text -> `#VALUE!`; an error-valued argument
//!   propagates as-is, no special containment (LN.md §Error behavior).

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `LN(number)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    match to_number(&args.eval_scalar(0)) {
        Ok(n) if n > 0.0 => Value::number(n.ln()),
        Ok(_) => Value::Error(ErrorKind::Num),
        Err(k) => Value::Error(k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    #[test]
    fn ln_of_one_is_zero() {
        assert_eq!(eval_direct(eval, vec![Scalar(num(1.0))]), num(0.0));
        assert_eq!(eval_direct(eval, vec![Scalar(txt("1"))]), num(0.0));
    }

    #[test]
    fn ln_of_e_is_one() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(std::f64::consts::E))]),
            num(1.0)
        );
    }

    #[test]
    fn nonpositive_is_num_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0))]),
            Value::Error(ErrorKind::Num)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-2.0))]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn error_argument_propagates() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Value))]),
            Value::Error(ErrorKind::Value)
        );
    }
}

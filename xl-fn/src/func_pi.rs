//! `PI` — returns the mathematical constant pi.
//!
//! # Provenance
//! Behavior contract: `docs/specs/PI.md` (Microsoft Learn "PI function" page).
//! The source is a single unambiguous sentence — "Returns the number
//! 3.14159265358979, the mathematical constant pi, accurate to 15 digits" — so
//! this is implemented directly rather than queued as an oracle experiment.
//!
//! # Semantics implemented (spec bullet in parentheses)
//! - Takes zero arguments and returns the `f64` nearest pi (PI.md §1). Excel's
//!   `PI()` is the IEEE-754 double closest to pi, which is exactly
//!   [`std::f64::consts::PI`] — the same bit pattern — so no rounding or
//!   tolerance question arises. Arity (`0..=0`) is enforced by the registry, so
//!   `eval` does not inspect the argument list.

use xl_value::Value;

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `PI()` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, _args: &mut dyn CallArgs) -> Value {
    Value::number(std::f64::consts::PI)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::eval_direct;

    #[test]
    fn returns_pi() {
        assert_eq!(
            eval_direct(eval, vec![]),
            Value::number(std::f64::consts::PI)
        );
    }

    #[test]
    fn value_is_nearest_double_to_pi() {
        // The f64 nearest pi, matching Excel's documented 15-digit constant.
        match eval_direct(eval, vec![]) {
            Value::Number(n) => assert_eq!(n, std::f64::consts::PI),
            other => panic!("expected a number, got {other:?}"),
        }
    }
}

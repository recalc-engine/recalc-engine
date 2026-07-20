//! `SIN` — the sine of an angle given in **radians**.
//!
//! # Provenance
//! Behavior contract: `docs/specs/SIN.md`, which cites the Microsoft Learn
//! SIN function page
//! (`https://support.microsoft.com/en-us/office/sin-function-cf0e3432-8b9e-483c-bc55-a76651c95602`).
//! Coercion is deferred entirely to `xl-value`'s [`to_number`]. SIN's
//! documented contract is a single unambiguous sentence — "Returns the sine of
//! the given angle" with the angle in radians — so, like `SQRT`/`EXP`/`PI`,
//! the core is implemented directly rather than queued as a probe.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Coerce the one argument via scalar numeric coercion (bool → 1/0,
//!   numeric text → number, blank → 0) (SIN.md §Coercion).
//! - Return `sin(angle)` via plain `f64::sin` (no fast-math / approximation
//!   path — the Recalc design rules) (SIN.md §1). `SIN(0)` = `0`; `SIN(PI()/2)` = `1`.
//! - The result of `sin` is always in `[-1, 1]`, so it is always finite: SIN
//!   has no `#NUM!` overflow path (unlike `EXP`). A non-numeric / non-coercible
//!   text argument yields `#VALUE!` (from `to_number`); an error-valued
//!   argument propagates as-is, no SIN-specific containment (SIN.md
//!   §Error behavior).
//!
//! # Large-magnitude argument reduction (SIN.md §Tolerance)
//! For a very large `|angle|` the result depends on how the runtime reduces the
//! argument modulo `2π`. Recalc uses `f64::sin` (the platform libm), which
//! performs an accurate Payne–Hanek-style reduction; Excel does likewise. Any
//! residual disagreement is a last-ULP effect well inside the workbook-wide
//! 15-significant-figure float-comparison rule (`TOLERANCES.md`), not a
//! semantic divergence. A tightening of that bound to a SIN-specific ULP figure
//! is a human-gated `TOLERANCES.md` decision, not made here.

use xl_value::{Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `SIN(number)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    match to_number(&args.eval_scalar(0)) {
        Ok(n) => Value::number(n.sin()),
        Err(k) => Value::Error(k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::ErrorKind;

    #[test]
    fn sin_of_zero_is_zero() {
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.0))]), num(0.0));
    }

    #[test]
    fn sin_of_half_pi_is_one() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(std::f64::consts::FRAC_PI_2))]),
            num(1.0)
        );
    }

    #[test]
    fn matches_f64_sin() {
        // Faithful pass-through of the platform libm (radians).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0))]),
            num(1.0_f64.sin())
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-2.5))]),
            num((-2.5_f64).sin())
        );
    }

    #[test]
    fn bounded_result_never_overflows() {
        // Unlike EXP, sin is bounded, so even a huge argument yields a finite
        // Number, never #NUM!.
        match eval_direct(eval, vec![Scalar(num(1e300))]) {
            Value::Number(n) => assert!((-1.0..=1.0).contains(&n)),
            other => panic!("expected a bounded Number, got {other:?}"),
        }
    }

    #[test]
    fn coerces_numeric_text() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("0"))]), num(0.0));
    }

    #[test]
    fn boolean_true_coerces_to_one_radian() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::bool(true))]),
            num(1.0_f64.sin())
        );
    }

    #[test]
    fn non_numeric_text_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("angle"))]),
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

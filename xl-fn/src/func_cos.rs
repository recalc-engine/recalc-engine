//! `COS` — the cosine of an angle given in **radians**.
//!
//! # Provenance
//! Microsoft Learn COS function page
//! (`https://support.microsoft.com/en-us/office/cos-function-0fb808a5-95d6-4553-8148-22aebdce5f05`).
//! Coercion is deferred entirely to `xl-value`'s [`to_number`]. COS's
//! documented contract is a single unambiguous sentence — "Returns the cosine
//! of the given angle" with the angle in radians — so, like `SIN`/`SQRT`/`EXP`,
//! the core is implemented directly rather than queued as a probe.
//!
//! # Semantics implemented
//! - Coerce the one argument via scalar numeric coercion (bool → 1/0, numeric
//!   text → number, blank → 0).
//! - Return `cos(angle)` via plain `f64::cos` (no fast-math / approximation
//!   path — the Recalc design rules). `COS(0)` = `1`.
//! - The result of `cos` is always in `[-1, 1]`, so it is always finite: COS
//!   has no `#NUM!` overflow path. A non-numeric / non-coercible text argument
//!   yields `#VALUE!` (from `to_number`); an error-valued argument propagates
//!   as-is, no COS-specific containment.
//!
//! # Large-magnitude argument reduction
//! For a very large `|angle|` the result depends on the runtime's argument
//! reduction modulo `2π`. Recalc uses `f64::cos` (the platform libm), which
//! performs an accurate Payne–Hanek-style reduction; Excel does likewise. Any
//! residual disagreement is a last-ULP effect well inside the workbook-wide
//! 15-significant-figure float-comparison rule (`TOLERANCES.md`), not a
//! semantic divergence. This mirrors the sibling `SIN` implementation exactly.

use xl_value::{Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `COS(number)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    match to_number(&args.eval_scalar(0)) {
        Ok(n) => Value::number(n.cos()),
        Err(k) => Value::Error(k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::ErrorKind;

    #[test]
    fn cos_of_zero_is_one() {
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.0))]), num(1.0));
    }

    #[test]
    fn matches_f64_cos() {
        // Faithful pass-through of the platform libm (radians).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0))]),
            num(1.0_f64.cos())
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-2.5))]),
            num((-2.5_f64).cos())
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(std::f64::consts::PI))]),
            num(std::f64::consts::PI.cos())
        );
    }

    #[test]
    fn bounded_result_never_overflows() {
        // Unlike EXP, cos is bounded, so even a huge argument yields a finite
        // Number, never #NUM!.
        match eval_direct(eval, vec![Scalar(num(1e300))]) {
            Value::Number(n) => assert!((-1.0..=1.0).contains(&n)),
            other => panic!("expected a bounded Number, got {other:?}"),
        }
    }

    #[test]
    fn coerces_numeric_text() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("0"))]), num(1.0));
    }

    #[test]
    fn boolean_true_coerces_to_one_radian() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::bool(true))]),
            num(1.0_f64.cos())
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

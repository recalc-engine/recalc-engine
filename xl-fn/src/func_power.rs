//! `POWER` — raise a number to a power (`number ^ power`).
//!
//! # Provenance
//! Behavior contract: `docs/specs/POWER.md` (which cites the Microsoft Learn
//! POWER function page, verified 2026-07-07). Numeric coercion is deferred
//! entirely to `xl-value`'s [`to_number`]; the non-finite → `#NUM!` mapping is
//! the crate-wide invariant enforced by [`Value::number`] (see the
//! `number_constructor_enforces_finiteness` test in `xl-value`), not re-derived
//! here.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Coerce both arguments via scalar numeric coercion (bool → 1/0, numeric
//!   text → number, blank → 0); an error-valued argument propagates as-is
//!   (POWER.md §Coercion, §Error behavior).
//! - Compute `base.powf(exp)` and hand the result to [`Value::number`], which
//!   maps a non-finite result to `#NUM!`. This single path covers, matching
//!   Excel's `#NUM!` for out-of-range / non-real results (POWER.md §1):
//!   - **Overflow** (`POWER(10, 1000)` → `+inf` → `#NUM!`).
//!   - **Negative base with a non-integer exponent** (`POWER(-1, 0.5)` → `NaN`
//!     → `#NUM!`) — no real result, exactly as `SQRT(-1)` is `#NUM!`. Note that
//!     the platform `powf` correctly returns the real value for a negative base
//!     with an **integer** exponent (`POWER(-2, 3) = -8`, `POWER(-2, 2) = 4`),
//!     so those are supported.
//! - `POWER(0, 0) = 1` and `POWER(0, positive) = 0` fall out of `powf`
//!   directly (POWER.md §1).
//!
//! # Oracle-resolved
//! - **0 raised to a negative power** (`POWER(0, -1)`, `POWER(0, -2)`): a
//!   division-by-zero singularity. **OXP-111 RESOLVED by
//!   RUN-2026-07-11-oracle01**: `0^negative = #DIV/0!` (both `=0^-1` and
//!   `=POWER(0,-1)`/`=POWER(0,-2)` observed as `#DIV/0!` on the Excel farm;
//!   see `docs/oracle-experiments.md`). This case now returns
//!   `ErrorKind::Div0` directly rather than falling through to `powf`'s `+inf`
//!   → `#NUM!` path.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `POWER(number, power)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let base = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let exp = match to_number(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };

    // 0 raised to a negative power is a division-by-zero singularity. OXP-111
    // RESOLVED by RUN-2026-07-11-oracle01: Excel returns #DIV/0! here (not
    // #NUM!), so short-circuit before powf's +inf could resolve to #NUM!.
    // See module docs.
    if base == 0.0 && exp < 0.0 {
        return Value::Error(ErrorKind::Div0);
    }

    // Ordinary f64::powf (no fast-math). Value::number maps a non-finite result
    // (overflow → ±inf; negative base with a non-integer exponent → NaN) to
    // #NUM!, matching Excel's out-of-range / non-real behavior.
    Value::number(base.powf(exp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    #[test]
    fn positive_base() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.0)), Scalar(num(3.0))]),
            num(8.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.0)), Scalar(num(10.0))]),
            num(1024.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(9.0)), Scalar(num(0.5))]),
            num(3.0)
        );
    }

    #[test]
    fn negative_base_integer_exponent() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-2.0)), Scalar(num(3.0))]),
            num(-8.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-2.0)), Scalar(num(2.0))]),
            num(4.0)
        );
    }

    #[test]
    fn zero_and_identity_cases() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(5.0)), Scalar(num(0.0))]),
            num(1.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0)), Scalar(num(5.0))]),
            num(0.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0)), Scalar(num(0.0))]),
            num(1.0)
        );
    }

    #[test]
    fn negative_base_fractional_exponent_is_num() {
        // No real result → NaN → #NUM! (as SQRT(-1) is #NUM!).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-1.0)), Scalar(num(0.5))]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn overflow_is_num() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(10.0)), Scalar(num(1000.0))]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn zero_to_negative_power_is_div0() {
        // OXP-111 RESOLVED by RUN-2026-07-11-oracle01: 0^negative = #DIV/0!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0)), Scalar(num(-1.0))]),
            Value::Error(ErrorKind::Div0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0)), Scalar(num(-2.0))]),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn text_coercion_and_error_propagation() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("2")), Scalar(txt("3"))]),
            num(8.0)
        );
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Div0)), Scalar(num(2.0))]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }
}

//! `TRUNC` — truncates `number` **toward zero** to `[num_digits]` decimal
//! places (the fractional part beyond that digit is dropped, magnitude never
//! increases).
//!
//! # Provenance
//! Microsoft Learn TRUNC function page
//! (`https://support.microsoft.com/en-us/office/trunc-function-8b86a64c-3127-43db-ba14-aa5ceb292721`).
//! Coercion via `xl-value`'s [`to_number`]. TRUNC and `ROUNDDOWN` perform the
//! **identical** numeric operation (both round toward zero at `num_digits`);
//! the Microsoft page documents this equivalence directly ("TRUNC and INT …
//! TRUNC removes the fractional part … ROUNDDOWN(number, num_digits) with a
//! negative num_digits gives the same result"). So this shares
//! `func_rounddown.rs`'s exact digit-scaling structure and its 15-significant-
//! digit float-artifact correction; the **only** interface difference is that
//! TRUNC's `num_digits` is **optional and defaults to 0** (ROUNDDOWN requires
//! both arguments).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `num_digits` (arg 1, optional) defaults to `0` when omitted, giving the
//!   common `TRUNC(number)` → integer-part form (`TRUNC(8.9)` = `8`,
//!   `TRUNC(-8.9)` = `-8`) (TRUNC.md §Semantics). `args.count()` distinguishes
//!   an omitted second argument from an explicit one.
//! - `num_digits > 0` truncates right of the decimal point; `= 0` to the
//!   integer; `< 0` truncates left of it (tens, hundreds, …) — identical
//!   scaling to `ROUNDDOWN` (`TRUNC(3.14159, 2)` = `3.14`,
//!   `TRUNC(-8.94, -1)` = `0`).
//! - Rounding direction is **always toward zero** (there is no half-way tie).
//! - A **non-integer** `num_digits` is **truncated toward zero** before scaling
//!   — OXP-098 (RESOLVED for the round family by `RUN-2026-07-11-oracle01`;
//!   shared coercion path). A non-finite `num_digits` (`NaN`/`±∞`) stays
//!   `#UNSUPPORTED!` rather than guessed.
//! - Non-numeric, non-coercible argument → `#VALUE!`; either argument erroring
//!   propagates (TRUNC.md §Coercion/§Error behavior).
//!
//! # Float-artifact correction (documented 15-significant-digit precision)
//! Naive `(number * scale).trunc() / scale` is unsound for directed rounding:
//! `trunc` is discontinuous at every integer, so a sub-ULP binary artifact can
//! spuriously drop a whole unit (e.g. `0.29 * 100` = `28.999999999999996`,
//! whose naive `trunc` is `28` → `0.28`, but Excel returns `0.29`). Excel
//! computes with 15 significant decimal digits of precision (Microsoft Learn
//! "Floating-point arithmetic may give inaccurate results"), so
//! [`snap_15_significant`] snaps the scaled magnitude to 15 significant digits
//! — erasing exactly that binary noise — *before* the `trunc`. This is the
//! same correction `func_round`/`func_rounddown`/`func_roundup` apply.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `TRUNC(number, [num_digits])` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let number = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    // num_digits is optional and defaults to 0 (the integer-truncation form).
    let raw_digits = if args.count() > 1 {
        match to_number(&args.eval_scalar(1)) {
            Ok(n) => n,
            Err(k) => return Value::Error(k),
        }
    } else {
        0.0
    };
    // OXP-098 (RESOLVED for the round family; shared coercion path): a
    // fractional num_digits truncates toward zero. A non-finite num_digits was
    // not probed -> stays #UNSUPPORTED! rather than guessed.
    if !raw_digits.is_finite() {
        return Value::Error(ErrorKind::Unsupported);
    }
    let truncated = raw_digits.trunc();
    // Clamp into i32 range before the cast; anything past it is already far
    // beyond any representable f64 power-of-ten scale, so it lands in the same
    // scale-overflow path as a merely-large-but-in-range digit count.
    let digits = truncated.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;

    match truncate_toward_zero(number, digits) {
        Ok(v) => Value::number(v),
        Err(k) => Value::Error(k),
    }
}

/// Truncates `number` **toward zero** to `digits` decimal places, or an
/// [`ErrorKind`] for a deferred/overflow edge. Identical to `ROUNDDOWN`'s
/// directed-rounding kernel (see that module's docs).
fn truncate_toward_zero(number: f64, digits: i32) -> Result<f64, ErrorKind> {
    if number == 0.0 {
        return Ok(0.0);
    }
    let scale = 10f64.powi(digits);
    // A scale factor that over/underflows past f64's representable range
    // (beyond ~±308) is deferred rather than returning a silently-wrong or
    // unvalidated result — mirrors ROUND's OXP-096 handling.
    if !scale.is_finite() || scale == 0.0 {
        return Err(ErrorKind::Unsupported);
    }
    let sign = if number.is_sign_negative() { -1.0 } else { 1.0 };
    let scaled = number.abs() * scale;
    if !scaled.is_finite() {
        return Err(ErrorKind::Num);
    }
    // Erase the sub-ULP binary artifact of the scaling before the `trunc`.
    let magnitude = snap_15_significant(scaled).trunc();
    // Directed rounding to an exact-zero magnitude is `0`, sign-independent —
    // keep it `+0.0` rather than combining a sign into `-0.0`.
    let result = if magnitude == 0.0 {
        0.0
    } else {
        sign * magnitude / scale
    };
    if result.is_finite() {
        Ok(result)
    } else {
        Err(ErrorKind::Num)
    }
}

/// Snaps a finite, non-negative magnitude to **15 significant decimal digits**,
/// erasing the sub-ULP binary residue that would otherwise flip a directed
/// (`trunc`) rounding at an integer boundary. Excel carries 15 significant
/// digits of precision (Microsoft Learn "Floating-point arithmetic may give
/// inaccurate results"), so this is the documented precision model, not a
/// heuristic tolerance. Identical to the sibling round-family helpers.
///
/// `{:.14e}` formats one integer digit plus 14 fractional digits = 15
/// significant digits, then the round-trip parse yields the nearest `f64` to
/// that 15-digit decimal.
fn snap_15_significant(v: f64) -> f64 {
    if v == 0.0 {
        return 0.0;
    }
    let s = format!("{v:.14e}");
    s.parse::<f64>().unwrap_or(v)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    /// The default (omitted num_digits) form truncates to the integer part,
    /// toward zero, both signs — the overwhelmingly common corpus shape.
    #[test]
    fn omitted_num_digits_truncates_to_integer() {
        assert_eq!(eval_direct(eval, vec![Scalar(num(8.9))]), num(8.0));
        assert_eq!(eval_direct(eval, vec![Scalar(num(-8.9))]), num(-8.0));
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.49))]), num(0.0));
        // An explicit 0 matches the omitted default.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(8.9)), Scalar(num(0.0))]),
            num(8.0)
        );
    }

    /// Microsoft Learn TRUNC documented examples.
    #[test]
    #[allow(clippy::approx_constant)]
    fn ms_learn_documented_examples() {
        // TRUNC(8.9) = 8 ; TRUNC(-8.9) = -8 ; TRUNC(0.45) = 0.
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.45))]), num(0.0));
        // TRUNC(PI, 2) = 3.14.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.14159)), Scalar(num(2.0))]),
            num(3.14)
        );
    }

    /// Positive num_digits truncates right of the point; negative left of it.
    #[test]
    #[allow(clippy::approx_constant)]
    fn positive_and_negative_num_digits() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.14159)), Scalar(num(3.0))]),
            num(3.141)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-3.14159)), Scalar(num(1.0))]),
            num(-3.1)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(31415.92654)), Scalar(num(-2.0))]),
            num(31400.0)
        );
        // Magnitude smaller than the scale truncates to zero.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(42.0)), Scalar(num(-3.0))]),
            num(0.0)
        );
    }

    /// Float-artifact cases: naive `(x*scale).trunc()/scale` under-rounds
    /// these; the 15-significant-digit snap fixes them.
    #[test]
    fn float_artifacts_do_not_undershoot() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.29)), Scalar(num(2.0))]),
            num(0.29)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.4)), Scalar(num(1.0))]),
            num(1.4)
        );
    }

    /// OXP-098 (RESOLVED for the round family; shared coercion path): a
    /// fractional num_digits truncates toward zero before scaling.
    #[test]
    #[allow(clippy::approx_constant)]
    fn oxp098_fractional_num_digits_truncates_toward_zero() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.14159)), Scalar(num(2.9))]),
            num(3.14)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.14159)), Scalar(num(-1.9))]),
            num(0.0)
        );
    }

    /// Zero truncates to zero at any digit count.
    #[test]
    fn zero_input() {
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.0))]), num(0.0));
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0)), Scalar(num(-3.0))]),
            num(0.0)
        );
    }

    /// Coercion: numeric text / boolean flow through `to_number`; non-numeric
    /// text is `#VALUE!`; an incoming error propagates from either argument.
    #[test]
    fn coercion_and_errors() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("8.9"))]), num(8.0));
        assert_eq!(eval_direct(eval, vec![Scalar(Value::Bool(true))]), num(1.0));
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Error(ErrorKind::Div0)
        );
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(3.9)), Scalar(Value::Error(ErrorKind::Ref))]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }
}

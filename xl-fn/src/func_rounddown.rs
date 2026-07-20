//! `ROUNDDOWN` — rounds `number` **toward zero** to `num_digits` decimal
//! places (magnitude always rounds down / truncates at that digit).
//!
//! # Provenance
//! Behavior contract: `docs/specs/ROUNDDOWN.md` (Microsoft Learn ROUNDDOWN
//! function page). Coercion via `xl-value`'s [`to_number`]. Shares
//! `func_round.rs`'s exact digit-scaling structure; the *only* difference is
//! the rounding direction (toward zero here, vs. half-away in `ROUND`).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `num_digits > 0` rounds right of the decimal point; `= 0` to the
//!   nearest integer; `< 0` rounds left of the decimal point (tens,
//!   hundreds, ...) — identical scaling to `ROUND` (ROUNDDOWN.md §2-4).
//! - Rounding direction is **always toward zero** (the magnitude never
//!   increases — it truncates at `num_digits`): `ROUNDDOWN(3.9,0)` = `3`,
//!   `ROUNDDOWN(-3.9,0)` = `-3`, `ROUNDDOWN(3.14159,2)` = `3.14`
//!   (ROUNDDOWN.md §5). There is **no** half-way tie — the direction is
//!   unconditional — so `ROUND`'s decimal-tie helper is not needed here.
//! - Non-numeric, non-coercible argument -> `#VALUE!`; either argument
//!   erroring propagates (ROUNDDOWN.md §Coercion/§Error behavior).
//!
//! # Non-integer `num_digits` (OXP-098, RESOLVED, shared with `ROUND`)
//! A fractional `num_digits` is **truncated toward zero** before scaling,
//! confirmed for `ROUND` by oracle run `RUN-2026-07-11-oracle01`
//! (`ROUND(3.14159,2.9)` = `3.14`, `3.14159,-1.9` = `0`); `ROUNDDOWN` shares
//! the identical `num_digits` coercion path, so the same rule applies
//! (ROUNDDOWN.md §Coercion). A non-finite `num_digits` (`NaN`/`±∞`) was not
//! probed and stays `#UNSUPPORTED!` rather than guessed (the Recalc design rules
//! Principle 2).
//!
//! # Float-artifact correction (documented 15-significant-digit precision)
//! Naive `(number * scale).trunc() / scale` is **unsound** for directed
//! rounding: `trunc` is discontinuous at *every* integer, so a sub-ULP binary
//! artifact spuriously drops a whole unit (e.g. `0.29 * 100` =
//! `28.999999999999996`, whose naive `trunc` is `28` -> `0.28`, but Excel
//! returns `0.29`; likewise `1.4 * 10` = `13.999999999999998` -> naive `13`
//! -> `1.3` vs Excel `1.4`). Excel computes with **15 significant decimal
//! digits** of precision (Microsoft Learn "Floating-point arithmetic may give
//! inaccurate results", and `implementation-plan.md` §2 hit-list
//! "15-significant-digit rounding"), so [`snap_15_significant`] snaps the
//! scaled magnitude to 15 significant digits — erasing exactly that binary
//! noise — *before* the `trunc`. This reproduces every Microsoft Learn
//! documented example and the common decimal cases (`ROUNDDOWN(0.29,2)` =
//! `0.29`). See the OXP note below for the one residual edge left to the
//! oracle.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `ROUNDDOWN(number, num_digits)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let number = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let raw_digits = match to_number(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    // OXP-098 (RESOLVED for ROUND by RUN-2026-07-11-oracle01; shared coercion
    // path): a fractional num_digits is truncated toward zero. A non-finite
    // num_digits was not probed -> stays #UNSUPPORTED! rather than guessed.
    if !raw_digits.is_finite() {
        return Value::Error(ErrorKind::Unsupported);
    }
    let truncated = raw_digits.trunc();
    // Clamp into i32 range before the cast; anything past it is already far
    // beyond any representable f64 power-of-ten scale, so it lands in the
    // same scale-overflow path as a merely-large-but-in-range digit count.
    let digits = truncated.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;

    match round_down_toward_zero(number, digits) {
        Ok(v) => Value::number(v),
        Err(k) => Value::Error(k),
    }
}

/// Rounds `number` **toward zero** to `digits` decimal places, or an
/// [`ErrorKind`] for a deferred/overflow edge. See the module docs.
fn round_down_toward_zero(number: f64, digits: i32) -> Result<f64, ErrorKind> {
    if number == 0.0 {
        return Ok(0.0);
    }
    let scale = 10f64.powi(digits);
    // A scale factor that over/underflows past f64's representable range
    // (beyond ~±308) is deferred rather than returning a silently-wrong or
    // unvalidated result — mirrors `ROUND`'s OXP-096 handling.
    if !scale.is_finite() || scale == 0.0 {
        return Err(ErrorKind::Unsupported);
    }
    let sign = if number.is_sign_negative() { -1.0 } else { 1.0 };
    let scaled = number.abs() * scale;
    if !scaled.is_finite() {
        return Err(ErrorKind::Num);
    }
    // Erase the sub-ULP binary artifact of the scaling before the `trunc`
    // (see module docs). `snap_15_significant` receives a finite,
    // non-negative magnitude.
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

/// Snaps a finite, non-negative magnitude to **15 significant decimal
/// digits**, erasing the sub-ULP binary residue that would otherwise flip a
/// directed (`ceil`/`trunc`) rounding at an integer boundary. Excel carries
/// 15 significant digits of precision (Microsoft Learn "Floating-point
/// arithmetic may give inaccurate results"), so this is the documented
/// precision model, not a heuristic tolerance.
///
/// `{:.14e}` formats one integer digit plus 14 fractional digits = 15
/// significant digits, then the round-trip parse yields the nearest `f64` to
/// that 15-digit decimal.
///
/// OXP (unassigned): the precise float-artifact reconciliation for *directed*
/// rounding — that Excel rounds the 15-significant-digit decimal, and exactly
/// how it resolves a tie *at the 15th significant digit itself* (only
/// reachable for inputs carrying >15 significant digits) — is not yet pinned
/// by a repo oracle run; probe `ROUNDDOWN` across a >15-sig-digit grid to
/// upgrade this from documented-behavior to RESOLVED (cf. `ROUND`'s OXP-095).
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

    /// Microsoft Learn ROUNDDOWN documented examples (ROUNDDOWN.md §Examples):
    /// every one truncates the magnitude, positive and negative digits.
    #[test]
    #[allow(clippy::approx_constant)]
    fn ms_learn_documented_examples() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.2)), Scalar(num(0.0))]),
            num(3.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(76.9)), Scalar(num(0.0))]),
            num(76.0)
        );
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
    }

    /// The direction contract from the task brief: toward zero, both signs,
    /// at `num_digits = 0` and `> 0`.
    #[test]
    #[allow(clippy::approx_constant)]
    fn rounds_toward_zero_both_signs() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.9)), Scalar(num(0.0))]),
            num(3.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-3.9)), Scalar(num(0.0))]),
            num(-3.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.14159)), Scalar(num(2.0))]),
            num(3.14)
        );
        // A value already at the cut does not move; nor does one that would
        // round *up* under `ROUND` — `ROUNDDOWN` never rounds up.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.999)), Scalar(num(0.0))]),
            num(2.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-2.5)), Scalar(num(0.0))]),
            num(-2.0)
        );
    }

    /// Negative `num_digits` truncates left of the decimal point (tens,
    /// hundreds, ...).
    #[test]
    fn negative_num_digits_truncates_left_of_point() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(4299.0)), Scalar(num(-2.0))]),
            num(4200.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-149.0)), Scalar(num(-1.0))]),
            num(-140.0)
        );
        // Magnitude smaller than the scale truncates to zero.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(42.0)), Scalar(num(-2.0))]),
            num(0.0)
        );
    }

    /// Float-artifact cases: naive `(x*scale).trunc()/scale` spuriously
    /// under-rounds these; the 15-significant-digit snap fixes them. `0.29*100`
    /// = `28.999999999999996` (naive trunc -> `0.28`); `1.4*10` =
    /// `13.999999999999998` (naive trunc -> `1.3`).
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
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.1 + 0.2)), Scalar(num(1.0))]),
            num(0.3)
        );
        // Genuinely truncates: 0.045 stored slightly below 0.045; to 2 places
        // toward zero it is 0.04 regardless.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.045)), Scalar(num(2.0))]),
            num(0.04)
        );
    }

    /// OXP-098 (RESOLVED for ROUND by RUN-2026-07-11-oracle01; shared coercion
    /// path): a fractional num_digits is truncated toward zero before scaling
    /// (`2.9` -> `2`, `-1.9` -> `-1`), matching `ROUND(3.14159,-1.9)` = `0`.
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

    /// Zero rounds to zero at any digit count.
    #[test]
    fn zero_input() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0)), Scalar(num(2.0))]),
            num(0.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0)), Scalar(num(-3.0))]),
            num(0.0)
        );
    }

    /// Coercion: numeric text and boolean flow through `to_number`;
    /// non-numeric text is `#VALUE!`; an incoming error propagates.
    #[test]
    fn coercion_and_errors() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("3.9")), Scalar(num(0.0))]),
            num(3.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Bool(true)), Scalar(num(0.0))]),
            num(1.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("abc")), Scalar(num(0.0))]),
            Value::Error(ErrorKind::Value)
        );
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Div0)), Scalar(num(0.0))]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    // Note: the eval's non-finite `num_digits` guard mirrors `func_round`'s
    // defensive check but is not unit-tested here — a non-finite value can't be
    // injected through `num()` (the `Value::number` invariant maps ±inf/NaN to
    // `#NUM!` first), exactly as `func_round` leaves it untested.
}

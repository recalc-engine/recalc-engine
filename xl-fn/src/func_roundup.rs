//! `ROUNDUP` — rounds `number` **away from zero** to `num_digits` decimal
//! places (magnitude always rounds up).
//!
//! # Provenance
//! Behavior contract: `docs/specs/ROUNDUP.md` (Microsoft Learn ROUNDUP
//! function page). Coercion via `xl-value`'s [`to_number`]. Shares
//! `func_round.rs`'s exact digit-scaling structure; the *only* difference is
//! the rounding direction (away from zero here, vs. half-away in `ROUND`).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `num_digits > 0` rounds right of the decimal point; `= 0` to the
//!   nearest integer; `< 0` rounds left of the decimal point (tens,
//!   hundreds, ...) — identical scaling to `ROUND` (ROUNDUP.md §2-4).
//! - Rounding direction is **always away from zero** (the magnitude never
//!   decreases): `ROUNDUP(3.2,0)` = `4`, `ROUNDUP(-3.2,0)` = `-4`,
//!   `ROUNDUP(3.14159,2)` = `3.15` (ROUNDUP.md §5). There is **no** half-way
//!   tie — the direction is unconditional — so `ROUND`'s decimal-tie helper
//!   is not needed here.
//! - Non-numeric, non-coercible argument -> `#VALUE!`; either argument
//!   erroring propagates (ROUNDUP.md §Coercion/§Error behavior).
//!
//! # Non-integer `num_digits` (OXP-098, RESOLVED, shared with `ROUND`)
//! A fractional `num_digits` is **truncated toward zero** before scaling,
//! confirmed for `ROUND` by oracle run `RUN-2026-07-11-oracle01`
//! (`ROUND(3.14159,2.9)` = `3.14`, `3.14159,-1.9` = `0`); `ROUNDUP` shares the
//! identical `num_digits` coercion path, so the same rule applies (ROUNDUP.md
//! §Coercion). A non-finite `num_digits` (`NaN`/`±∞`) was not probed and
//! stays `#UNSUPPORTED!` rather than guessed (Recalc Principle 2).
//!
//! # Float-artifact correction (documented 15-significant-digit precision)
//! Naive `(number * scale).ceil() / scale` is **unsound** for directed
//! rounding: `ceil` is discontinuous at *every* integer, so a sub-ULP binary
//! artifact spuriously rounds up (e.g. `4.15 * 100` = `415.00000000000006`,
//! whose naive `ceil` is `416` -> `4.16`, but Excel returns `4.15`; likewise
//! `(0.1+0.2) * 10` = `3.0000000000000004` -> naive `4` -> `0.4` vs Excel
//! `0.3`). Excel computes with **15 significant decimal digits** of precision
//! (Microsoft Learn "Floating-point arithmetic may give inaccurate results",
//! and `implementation-plan.md` §2 hit-list "15-significant-digit rounding"),
//! so [`snap_15_significant`] snaps the scaled magnitude to 15 significant
//! digits — erasing exactly that binary noise — *before* the `ceil`. This
//! reproduces every Microsoft Learn documented example and the common
//! decimal cases (`ROUNDUP(0.29,2)` = `0.29`, `ROUNDUP(1.15,2)` = `1.15`).
//! See the OXP note below for the one residual edge left to the oracle.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `ROUNDUP(number, num_digits)` call. See the module docs.
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

    match round_up_away(number, digits) {
        Ok(v) => Value::number(v),
        Err(k) => Value::Error(k),
    }
}

/// Rounds `number` **away from zero** to `digits` decimal places, or an
/// [`ErrorKind`] for a deferred/overflow edge. See the module docs.
fn round_up_away(number: f64, digits: i32) -> Result<f64, ErrorKind> {
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
    // Erase the sub-ULP binary artifact of the scaling before the `ceil`
    // (see module docs). `snap_15_significant` receives a finite,
    // non-negative magnitude.
    let magnitude = snap_15_significant(scaled).ceil();
    // Directed rounding of an exact-zero magnitude is `0`, sign-independent —
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
/// by a repo oracle run; probe `ROUNDUP` across a >15-sig-digit grid to
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

    /// Microsoft Learn ROUNDUP documented examples (ROUNDUP.md §Examples):
    /// every one rounds the magnitude *up*, positive and negative digits.
    #[test]
    #[allow(clippy::approx_constant)]
    fn ms_learn_documented_examples() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.2)), Scalar(num(0.0))]),
            num(4.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(76.9)), Scalar(num(0.0))]),
            num(77.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.14159)), Scalar(num(3.0))]),
            num(3.142)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-3.14159)), Scalar(num(1.0))]),
            num(-3.2)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(31415.92654)), Scalar(num(-2.0))]),
            num(31500.0)
        );
    }

    /// The direction contract from the task brief: away from zero, both signs,
    /// at `num_digits = 0` and `> 0`.
    #[test]
    #[allow(clippy::approx_constant)]
    fn rounds_away_from_zero_both_signs() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.14159)), Scalar(num(2.0))]),
            num(3.15)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-3.2)), Scalar(num(0.0))]),
            num(-4.0)
        );
        // Anything with a nonzero dropped digit rounds up in magnitude...
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.001)), Scalar(num(0.0))]),
            num(3.0)
        );
        // ...but an exact value at the cut does not move.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.0)), Scalar(num(0.0))]),
            num(2.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-1.5)), Scalar(num(0.0))]),
            num(-2.0)
        );
    }

    /// Negative `num_digits` rounds up left of the decimal point (tens,
    /// hundreds, ...).
    #[test]
    fn negative_num_digits_rounds_left_of_point() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(4231.0)), Scalar(num(-2.0))]),
            num(4300.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-142.0)), Scalar(num(-1.0))]),
            num(-150.0)
        );
        // Exact multiple of the scale does not move.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(4200.0)), Scalar(num(-2.0))]),
            num(4200.0)
        );
    }

    /// Float-artifact cases: naive `(x*scale).ceil()/scale` spuriously
    /// over-rounds these; the 15-significant-digit snap fixes them. `4.15*100`
    /// = `415.00000000000006` (naive ceil -> `4.16`); `(0.1+0.2)*10` =
    /// `3.0000000000000004` (naive ceil -> `0.4`); `0.29`/`1.15` likewise.
    #[test]
    fn float_artifacts_do_not_overshoot() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(4.15)), Scalar(num(2.0))]),
            num(4.15)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.15)), Scalar(num(2.0))]),
            num(1.15)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.29)), Scalar(num(2.0))]),
            num(0.29)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.1)), Scalar(num(1.0))]),
            num(1.1)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.1 + 0.2)), Scalar(num(1.0))]),
            num(0.3)
        );
    }

    /// OXP-098 (RESOLVED for ROUND by RUN-2026-07-11-oracle01; shared coercion
    /// path): a fractional num_digits is truncated toward zero before scaling
    /// (`2.9` -> `2`, `-1.9` -> `-1`).
    #[test]
    #[allow(clippy::approx_constant)]
    fn oxp098_fractional_num_digits_truncates_toward_zero() {
        // 3.14159 up to 2.9->2 digits: 3.15 (away from zero at 2 places).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.14159)), Scalar(num(2.9))]),
            num(3.15)
        );
        // -1.9 -> -1: round up to the nearest ten away from zero.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(44.0)), Scalar(num(-1.9))]),
            num(50.0)
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

    /// Coercion: numeric text and boolean/blank flow through `to_number`;
    /// non-numeric text is `#VALUE!`.
    #[test]
    fn coercion_and_errors() {
        // Numeric text argument.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("3.2")), Scalar(num(0.0))]),
            num(4.0)
        );
        // Boolean number -> 1, digits 0: ROUNDUP(TRUE,0) = 1.
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Bool(true)), Scalar(num(0.0))]),
            num(1.0)
        );
        // Non-numeric text -> #VALUE!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("abc")), Scalar(num(0.0))]),
            Value::Error(ErrorKind::Value)
        );
        // An incoming error propagates.
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

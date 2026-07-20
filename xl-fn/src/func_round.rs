//! `ROUND` — rounds `number` to `num_digits` decimal places, half away from
//! zero.
//!
//! # Provenance
//! Behavior contract: `docs/specs/ROUND.md` (Microsoft Learn ROUND function
//! page). Coercion via `xl-value`'s [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `num_digits > 0` rounds right of the decimal point; `= 0` to the
//!   nearest integer; `< 0` rounds left of the decimal point (tens,
//!   hundreds, ...) (ROUND.md §2-4).
//! - Rounding mode is **half away from zero**: `ROUND(2.5,0)` = `3`,
//!   `ROUND(-2.5,0)` = `-3` (ROUND.md §5) — computed via `f64::round`, whose
//!   documented behavior *is* "round half away from zero".
//! - Non-numeric, non-coercible argument -> `#VALUE!`; either argument
//!   erroring propagates (ROUND.md §Coercion/§Error behavior).
//!
//! # Oracle-resolved edges (ROUND.md §"Oracle experiments needed")
//! Three questions ROUND.md flagged as unconfirmed are now **resolved** by
//! oracle run `RUN-2026-07-11-oracle01` (observed Excel values, not guesses):
//! - **Non-integer `num_digits`** (`OXP-098`, RESOLVED): a fractional
//!   `num_digits` is **truncated toward zero** before rounding, confirmed by
//!   `ROUND(3.14159,2.9)` = `3.14` (`2.9` -> `2`) and `ROUND(3.14159,-1.9)`
//!   = `0` (`-1.9` -> `-1`). A non-finite `num_digits` (`NaN`/`±∞`) was not
//!   probed and remains `#UNSUPPORTED!` rather than guessed.
//! - **Binary/decimal half-way ambiguity** (`OXP-095`, RESOLVED): Excel
//!   rounds the **decimal literal as displayed**, half away from zero — it
//!   does *not* round the raw stored `f64`. Confirmed by `ROUND(1.005,2)` =
//!   `1.01`, `ROUND(1.015,2)` = `1.02` (naive IEEE-754 scaling floors these
//!   to `1.00`/`1.01`), plus `ROUND(2.675,2)` = `2.68` and `ROUND(0.15,1)` =
//!   `0.2`. [`halfway_ambiguous`] detects *exactly* the class where a value's
//!   shortest round-trip decimal literal terminates one digit past the
//!   rounding position with a trailing `5` while its true binary value does
//!   not land there — a genuine decimal tie — and [`decimal_tie_round_away`]
//!   then rounds that displayed literal away from zero. Values that *are*
//!   exactly representable ties (`2.5`, `-2.5`, every documented example) are
//!   computed directly since both readings already agree.
//! - **Extreme `num_digits` magnitude** (`OXP-096`, RESOLVED): the
//!   practically useful range is confirmed exact — `ROUND(5,-1)` = `10`,
//!   `ROUND(1.23456789,20)` = `1.23456789`, `ROUND(123,-20)` = `0`,
//!   `ROUND(1,300)` = `1`, `ROUND(1,-300)` = `0`. A magnitude so extreme that
//!   `10^num_digits` cannot be represented as a finite, nonzero `f64` (beyond
//!   ~`±308`, past every probed value) was not observed and stays
//!   `#UNSUPPORTED!` rather than returning a silently-wrong or unvalidated
//!   result.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `ROUND(number, num_digits)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let number = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let raw_digits = match to_number(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    // OXP-098 (RESOLVED, RUN-2026-07-11-oracle01): a fractional num_digits is
    // truncated toward zero (`ROUND(3.14159,2.9)` = 3.14, `ROUND(3.14159,-1.9)`
    // = 0). A non-finite num_digits was not probed -> stays #UNSUPPORTED!.
    if !raw_digits.is_finite() {
        return Value::Error(ErrorKind::Unsupported);
    }
    let truncated = raw_digits.trunc();
    // Clamp into i32 range before the cast; anything past it is already far
    // beyond any representable f64 power-of-ten scale, so it lands in the
    // same OXP-096 overflow path as a merely-large-but-in-range digit count.
    let digits = truncated.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;

    match round_half_away(number, digits) {
        Ok(v) => Value::number(v),
        Err(k) => Value::Error(k),
    }
}

/// Rounds `number` to `digits` decimal places, half away from zero, or an
/// [`ErrorKind`] for an oracle-deferred/overflow edge. See the module docs.
fn round_half_away(number: f64, digits: i32) -> Result<f64, ErrorKind> {
    if number == 0.0 {
        return Ok(0.0);
    }
    let scale = 10f64.powi(digits);
    // OXP-096 (RESOLVED, RUN-2026-07-11-oracle01): the whole practically useful
    // range is exact; only a scale factor that over/underflows past f64's
    // representable range (beyond ~±308, past every probed value) is deferred.
    if !scale.is_finite() || scale == 0.0 {
        return Err(ErrorKind::Unsupported);
    }
    // OXP-095 (RESOLVED, RUN-2026-07-11-oracle01): a genuine binary/decimal
    // half-way tie rounds the *displayed decimal literal* away from zero.
    if halfway_ambiguous(number, digits) {
        return decimal_tie_round_away(number, digits);
    }
    let scaled = number * scale;
    if !scaled.is_finite() {
        return Err(ErrorKind::Num);
    }
    // `f64::round` is documented "round half away from zero" — exactly
    // Excel's documented ROUND rule.
    let rounded = scaled.round();
    let result = rounded / scale;
    if result.is_finite() {
        Ok(result)
    } else {
        Err(ErrorKind::Num)
    }
}

/// Rounds a genuine binary/decimal half-way tie (`OXP-095`) the way Excel
/// does: on the *displayed decimal literal*, half away from zero. Only called
/// when [`halfway_ambiguous`] is `true`, which guarantees `digits >= 0` and
/// that `number`'s shortest round-trip decimal has exactly `digits + 1`
/// fractional digits ending in a tie `5`.
///
/// Resolved by `RUN-2026-07-11-oracle01`: `ROUND(1.005,2)` = `1.01`,
/// `ROUND(1.015,2)` = `1.02`, `ROUND(2.675,2)` = `2.68`, `ROUND(0.15,1)` =
/// `0.2` — every probed tie rounds the literal's trailing `5` away from zero,
/// confirming Excel does not round the raw (sub-tie) `f64`.
fn decimal_tie_round_away(number: f64, digits: i32) -> Result<f64, ErrorKind> {
    let n = number.abs();
    let s = format!("{n}");
    // Guaranteed by `halfway_ambiguous`: a fractional part is present.
    let (int_part, frac) = match s.split_once('.') {
        Some(parts) => parts,
        None => return Err(ErrorKind::Unsupported),
    };
    let keep = digits as usize;
    // `floor(|number| * 10^digits)`, read exactly off the decimal string by
    // dropping the trailing tie digit — no binary residue enters here.
    let mut floor_str = String::with_capacity(int_part.len() + keep);
    floor_str.push_str(int_part);
    floor_str.push_str(&frac[..keep]);
    let floor_scaled: u128 = match floor_str.parse() {
        Ok(m) => m,
        // Pathologically long literal beyond u128; not among the probed cases,
        // so stay explicit rather than guess.
        Err(_) => return Err(ErrorKind::Unsupported),
    };
    // Half away from zero: the dropped digit is exactly `5`, so round up in
    // magnitude, then restore the sign and scale back down.
    let magnitude = (floor_scaled + 1) as f64 / 10f64.powi(digits);
    let result = if number.is_sign_negative() {
        -magnitude
    } else {
        magnitude
    };
    if result.is_finite() {
        Ok(result)
    } else {
        Err(ErrorKind::Num)
    }
}

/// `true` iff rounding `number` to `digits` decimal places is the flagged
/// binary/decimal half-way ambiguity (`OXP-095`): the shortest round-trip
/// decimal literal of `number` terminates *exactly* one digit past the
/// rounding position with a trailing `5` (a genuine decimal tie, e.g.
/// `1.005` at `digits=2`), **and** the `f64`'s true value does not land
/// exactly on that decimal (distinguishing it from an exactly-representable
/// tie like `2.5`, for which this returns `false` — naive `f64` rounding is
/// authoritative there since both plausible readings agree).
fn halfway_ambiguous(number: f64, digits: i32) -> bool {
    // digits < 0 rounds within the integer part, which every f64 up to 2^53
    // represents exactly — no binary/decimal residue is possible there, so
    // this ambiguity class cannot occur.
    if digits < 0 {
        return false;
    }
    let digits = digits as usize;
    let n = number.abs();
    let shortest = format!("{n}");
    let frac = match shortest.split_once('.') {
        Some((_, f)) => f,
        None => return false, // an exact integer input: nothing ambiguous to round.
    };
    if frac.len() != digits + 1 || !frac.ends_with('5') {
        return false;
    }
    // The *true* exact expansion, to see whether the binary value terminates
    // cleanly at `frac.len()` digits (exact, like 2.5) or merely approximates
    // it (inexact, like 1.005). Rust's fixed-precision `{:.N}` formatting is
    // *rounded*, not truncated — a too-small `N` can carry a long run of
    // trailing `9`s in the residue up into a clean-looking `...5000...0`,
    // masking exactly the ambiguity being tested for (this bit Recalc during
    // development: `{:.15}` on `1.005` prints `1.005000000000000`, hiding the
    // residue that `{:.20}` reveals as `1.00499999999999989342`). So the
    // precision here is not "a bit more than needed" but a **provable upper
    // bound**: every finite `f64`'s fractional part terminates within 1074
    // decimal digits (the smallest subnormal is `2^-1074`), so requesting
    // that many guarantees the printed digits are the exact expansion, not a
    // rounded approximation of it, for every possible input magnitude.
    const EXACT_DECIMAL_DIGITS: usize = 1074;
    let precise = format!("{n:.EXACT_DECIMAL_DIGITS$}");
    let precise_frac = precise.split_once('.').map_or("", |(_, f)| f);
    precise_frac[frac.len()..].chars().any(|c| c != '0')
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num};

    /// OXP-095 (RUN-2026-07-11-oracle01): a genuine binary/decimal half-way
    /// tie rounds the *displayed decimal literal* away from zero, not the raw
    /// sub-tie `f64`. Naive IEEE-754 scaling floors `1.005`/`1.015` to
    /// `1.00`/`1.01`; Excel returns `1.01`/`1.02`.
    #[test]
    fn oxp095_decimal_literal_ties_round_away() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.005)), Scalar(num(2.0))]),
            num(1.01)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.015)), Scalar(num(2.0))]),
            num(1.02)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.675)), Scalar(num(2.0))]),
            num(2.68)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.15)), Scalar(num(1.0))]),
            num(0.2)
        );
    }

    /// OXP-096 (RUN-2026-07-11-oracle01): the practically useful num_digits
    /// range is exact, including past all significant digits of the input.
    #[test]
    fn oxp096_extreme_num_digits_magnitude() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(5.0)), Scalar(num(-1.0))]),
            num(10.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.23456789)), Scalar(num(20.0))]),
            num(1.23456789)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(123.0)), Scalar(num(-20.0))]),
            num(0.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0)), Scalar(num(300.0))]),
            num(1.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0)), Scalar(num(-300.0))]),
            num(0.0)
        );
    }

    /// OXP-098 (RUN-2026-07-11-oracle01): a fractional num_digits is truncated
    /// toward zero before rounding (`2.9` -> `2`, `-1.9` -> `-1`). The probe's
    /// `3.14159` is the oracle literal, not an approximation of PI.
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
}

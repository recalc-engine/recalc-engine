//! `FLOOR` — rounds `number` down to the nearest multiple of `significance`.
//!
//! # Provenance
//! Microsoft Learn FLOOR function page
//! (`https://support.microsoft.com/en-us/office/floor-function-14bb497c-24f2-4e04-b327-b0b4de5a8886`).
//! Coercion via `xl-value`'s [`to_number`].
//!
//! # Full sign rule — OXP-223 (RESOLVED, RUN-2026-07-20-oracle01)
//! FLOOR's negative-number and significance-sign behavior is the classic
//! "quirky" corner. The pinned Excel 16.0 build was probed across every sign
//! combination (`=FLOOR(-2.5,-2)`, `=FLOOR(2.5,-2)`, `=FLOOR(-2.5,2)`,
//! `=FLOOR(-2.5,-0.5)`, `=FLOOR(2.5,0)`, `=FLOOR(0,0)`) and decides:
//!
//! | case | rule | probe |
//! |---|---|---|
//! | `significance == 0`, `number == 0` | `0` | `FLOOR(0,0) = 0` |
//! | `significance == 0`, `number != 0` | `#DIV/0!` | `FLOOR(2.5,0) = #DIV/0!` |
//! | `number > 0`, `significance < 0` (sign mismatch) | `#NUM!` | `FLOOR(2.5,-2) = #NUM!` |
//! | all other (nonzero) sign combos | `floor(number/significance)·significance` | see below |
//!
//! The unified value branch is `floor(number / significance) · significance`
//! where `floor` is mathematical floor (toward −∞), which reproduces every
//! probed value: `FLOOR(-2.5,2) = -4` (negative number, positive significance —
//! rounds **down, away from zero**), `FLOOR(-2.5,-2) = -2` and
//! `FLOOR(-2.5,-0.5) = -2.5` (both negative — rounds **toward zero**), and the
//! positive/positive core (`FLOOR(3.7,2) = 2`). Only the `number > 0 &&
//! significance < 0` mismatch is a `#NUM!` error; the `number < 0 &&
//! significance > 0` mismatch is a value (down, away from zero). Provenance:
//! OXP-223.
//!
//! # Float-artifact correction (documented 15-significant-digit precision)
//! `number / significance` carries the usual binary-representation noise — e.g.
//! `2.6 / 0.2` = `12.999999999999998`, whose naive `floor` is `12` (→ `2.4`),
//! but `2.6` **is** `13 · 0.2` so Excel returns `2.6`. Excel computes with 15
//! significant decimal digits (Microsoft Learn "Floating-point arithmetic may
//! give inaccurate results"), so [`snap_15_significant`] snaps the quotient to
//! 15 significant digits — erasing exactly that noise — before the `floor`, the
//! same correction the `ROUND`/`ROUNDDOWN`/`TRUNC` family applies. Bit-exact
//! agreement with Excel's stored multiple is not claimed for a fractional
//! `significance` (a last-ULP effect well inside the workbook 15-sig rule); the
//! served values match every documented positive example to 15 sig figs.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `FLOOR(number, significance)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let number = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let significance = match to_number(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };

    // OXP-223 (RUN-2026-07-20-oracle01): the pinned sign rule.
    //  * significance == 0 → #DIV/0!, except FLOOR(0,0) == 0.
    //  * number > 0 with significance < 0 (sign mismatch) → #NUM!.
    //  * every other combination is the value branch below.
    if significance == 0.0 {
        return if number == 0.0 {
            Value::number(0.0)
        } else {
            Value::Error(ErrorKind::Div0)
        };
    }
    if number > 0.0 && significance < 0.0 {
        return Value::Error(ErrorKind::Num);
    }
    if number == 0.0 {
        return Value::number(0.0);
    }

    let quotient = number / significance;
    // Extreme-magnitude overflow (|number/significance| beyond f64) — the scaled
    // result cannot be finite; not the sign question OXP-223 pinned.
    if !quotient.is_finite() {
        return Value::Error(ErrorKind::Num);
    }
    // Snap the quotient to 15 significant digits before the floor toward −∞ (see
    // module docs), then scale back to the multiple. The snap handles a signed
    // quotient (the `{:.14e}` format carries the sign).
    let steps = snap_15_significant(quotient).floor();
    let result = steps * significance;
    if result.is_finite() {
        Value::number(result)
    } else {
        Value::Error(ErrorKind::Num)
    }
}

/// Snaps a finite (optionally signed) value to **15 significant decimal
/// digits**, erasing the sub-ULP binary residue that would otherwise flip the
/// `floor` at an integer boundary. The `{:.14e}` format carries the sign, so a
/// negative quotient (a `number < 0` case) is snapped by magnitude symmetrically
/// (`-12.999…998` → `-13.0`). Excel carries 15 significant digits of precision
/// (Microsoft Learn "Floating-point arithmetic may give inaccurate results"),
/// so this is the documented precision model, not a heuristic tolerance —
/// identical to the round-family helpers.
///
/// `{:.14e}` formats one integer digit plus 14 fractional digits = 15
/// significant digits, then the round-trip parse yields the nearest `f64`.
fn snap_15_significant(v: f64) -> f64 {
    if v == 0.0 {
        return 0.0;
    }
    let s = format!("{v:.14e}");
    s.parse::<f64>().unwrap_or(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    /// Relative-closeness assertion for a fractional-`significance` multiple,
    /// whose last ULP is not bit-pinned (15-sig fidelity — see module docs).
    fn assert_close(got: Value, want: f64) {
        match got {
            Value::Number(n) => {
                let rel = if want == 0.0 {
                    n.abs()
                } else {
                    ((n - want) / want).abs()
                };
                assert!(rel < 1e-13, "got {n}, want {want} (rel {rel:e})");
            }
            other => panic!("expected a Number, got {other:?}"),
        }
    }

    /// Positive/positive core: greatest multiple of significance ≤ number.
    #[test]
    fn positive_core_integer_significance() {
        // FLOOR(3.7, 2) = 2 ; FLOOR(3.7, 1) = 3 ; FLOOR(2.5, 3) = 0 ; exact.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.7)), Scalar(num(2.0))]),
            num(2.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.7)), Scalar(num(1.0))]),
            num(3.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.5)), Scalar(num(3.0))]),
            num(0.0)
        );
        // Exact multiple stays put.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(6.0)), Scalar(num(2.0))]),
            num(6.0)
        );
    }

    /// Fractional significance with the 15-sig snap (documented examples).
    #[test]
    fn positive_core_fractional_significance() {
        // FLOOR(1.58, 0.1) = 1.5 ; FLOOR(0.234, 0.01) = 0.23 ; FLOOR(2.6, 0.2)
        // = 2.6 (the float-artifact case the snap fixes).
        assert_close(
            eval_direct(eval, vec![Scalar(num(1.58)), Scalar(num(0.1))]),
            1.5,
        );
        assert_close(
            eval_direct(eval, vec![Scalar(num(0.234)), Scalar(num(0.01))]),
            0.23,
        );
        assert_close(
            eval_direct(eval, vec![Scalar(num(2.6)), Scalar(num(0.2))]),
            2.6,
        );
    }

    /// Zero number floors to zero for any served (positive) significance.
    #[test]
    fn zero_number() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0)), Scalar(num(0.1))]),
            num(0.0)
        );
    }

    /// OXP-223 (RUN-2026-07-20-oracle01): negative `number` rounds toward −∞ for
    /// a positive significance (down, away from zero) and toward zero for a
    /// negative significance.
    #[test]
    fn negative_number_sign_rule() {
        // neg number, pos significance: FLOOR(-2.5,2) = -4 (down, away from 0).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-2.5)), Scalar(num(2.0))]),
            num(-4.0)
        );
        // both negative: FLOOR(-2.5,-2) = -2 (toward zero).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-2.5)), Scalar(num(-2.0))]),
            num(-2.0)
        );
        // both negative, fractional exact multiple: FLOOR(-2.5,-0.5) = -2.5.
        assert_close(
            eval_direct(eval, vec![Scalar(num(-2.5)), Scalar(num(-0.5))]),
            -2.5,
        );
    }

    /// OXP-223: significance == 0 → #DIV/0! (except FLOOR(0,0) = 0); a
    /// positive-number/negative-significance sign mismatch → #NUM!.
    #[test]
    fn significance_zero_and_sign_mismatch() {
        // significance == 0 with a nonzero number → #DIV/0!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.5)), Scalar(num(0.0))]),
            Value::Error(ErrorKind::Div0)
        );
        // FLOOR(0,0) = 0 (the one significance-zero value).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0)), Scalar(num(0.0))]),
            num(0.0)
        );
        // sign mismatch: positive number, negative significance → #NUM!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.5)), Scalar(num(-2.0))]),
            Value::Error(ErrorKind::Num)
        );
    }

    /// Coercion and error propagation.
    #[test]
    fn coercion_and_errors() {
        // Numeric text / bool coerce through to_number.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("3.7")), Scalar(num(2.0))]),
            num(2.0)
        );
        // Non-numeric text -> #VALUE!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("x")), Scalar(num(2.0))]),
            Value::Error(ErrorKind::Value)
        );
        // Error in either argument propagates (before the OXP-223 guard).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Div0)), Scalar(num(2.0))]
            ),
            Value::Error(ErrorKind::Div0)
        );
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(3.7)), Scalar(Value::Error(ErrorKind::Ref))]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }
}

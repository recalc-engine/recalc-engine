//! `CEILING` — rounds `number` up to the nearest multiple of `significance`.
//!
//! # Provenance
//! Microsoft Learn CEILING function page
//! (`https://support.microsoft.com/en-us/office/ceiling-function-0a5cd7c8-0720-4f0a-bd2c-c943e510899f`).
//! Coercion via `xl-value`'s [`to_number`]. `CEILING` is the round-**up** sibling
//! of `FLOOR`; it shares FLOOR's 15-significant-digit quotient snap and sign
//! rule, differing only in the rounding direction (toward +∞) and in the
//! `significance == 0` outcome.
//!
//! # Full sign rule — OXP-229 (RESOLVED, RUN-2026-07-20-oracle01)
//! The pinned Excel 16.0 build was probed across every sign combination
//! (`=CEILING(-2.5,-2)`, `=CEILING(2.5,-2)`, `=CEILING(-2.5,2)`,
//! `=CEILING(-2.5,-0.5)`, `=CEILING(2.5,0)`, `=CEILING(0,0)`) and decides:
//!
//! | case | rule | probe |
//! |---|---|---|
//! | `significance == 0` (any `number`) | `0` | `CEILING(2.5,0) = 0`, `CEILING(0,0) = 0` |
//! | `number > 0`, `significance < 0` (sign mismatch) | `#NUM!` | `CEILING(2.5,-2) = #NUM!` |
//! | all other (nonzero-significance) sign combos | `ceil(number/significance)·significance` | see below |
//!
//! **Two asymmetries vs FLOOR (OXP-223):** (1) `significance == 0` returns `0`
//! for *every* `number` (FLOOR gives `#DIV/0!` for a nonzero number); (2) the
//! value branch rounds **up** (toward +∞), so `CEILING(-2.5,2) = -2` (toward
//! zero) where `FLOOR(-2.5,2) = -4`, and `CEILING(-2.5,-2) = -4` (away from
//! zero) where `FLOOR(-2.5,-2) = -2`. The `number > 0 && significance < 0`
//! sign-mismatch `#NUM!` is shared with FLOOR. Probed values reproduced:
//! `CEILING(-2.5,-0.5) = -2.5`, `CEILING(3.2,2) = 4`, `CEILING(0.234,0.01) =
//! 0.24`, `CEILING(2.4,0.2) = 2.4000000000000004` (Excel's own `12·0.2` f64
//! product — not a clean `2.4`). Provenance: OXP-229.
//!
//! # Float-artifact correction (documented 15-significant-digit precision)
//! `number / significance` carries binary-representation noise; [`snap_15`]
//! snaps the quotient to 15 significant digits before the `ceil`, erasing
//! exactly the residue that would otherwise flip the `ceil` at an integer
//! boundary — the same correction `FLOOR`/`ROUND` apply. Bit-exact agreement
//! with Excel's stored multiple is not claimed for a fractional `significance`
//! (a last-ULP effect inside the 15-sig rule); served values match every probed
//! example to 15 sig figs (and the `2.4000000000000004` case bit-exactly).

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `CEILING(number, significance)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let number = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let significance = match to_number(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };

    // OXP-229 (RUN-2026-07-20-oracle01): the pinned sign rule.
    //  * significance == 0 → 0 for any number (asymmetric with FLOOR's #DIV/0!).
    //  * number > 0 with significance < 0 (sign mismatch) → #NUM!.
    //  * every other combination rounds up (toward +∞) below.
    if significance == 0.0 {
        return Value::number(0.0);
    }
    if number > 0.0 && significance < 0.0 {
        return Value::Error(ErrorKind::Num);
    }
    if number == 0.0 {
        return Value::number(0.0);
    }

    let quotient = number / significance;
    // Extreme-magnitude overflow — the scaled result cannot be finite.
    if !quotient.is_finite() {
        return Value::Error(ErrorKind::Num);
    }
    // Snap the quotient to 15 significant digits before the ceil toward +∞, then
    // scale back to the multiple (the signed `{:.14e}` format, as in FLOOR).
    let steps = snap_15(quotient).ceil();
    let result = steps * significance;
    if result.is_finite() {
        Value::number(result)
    } else {
        Value::Error(ErrorKind::Num)
    }
}

/// Snaps a finite (optionally signed) value to **15 significant decimal
/// digits**, erasing the sub-ULP binary residue that would otherwise flip the
/// `ceil` at an integer boundary. Excel carries 15 significant digits of
/// precision (Microsoft Learn "Floating-point arithmetic may give inaccurate
/// results"), so this is the documented precision model — identical to the
/// `FLOOR`/`ROUND` family's snap.
fn snap_15(v: f64) -> f64 {
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

    /// Positive/positive core: least multiple of significance ≥ number.
    #[test]
    fn positive_core() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.2)), Scalar(num(2.0))]),
            num(4.0)
        );
        assert_close(
            eval_direct(eval, vec![Scalar(num(0.234)), Scalar(num(0.01))]),
            0.24,
        );
        // Excel's own 12·0.2 f64 product, bit-exact (not a clean 2.4).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.4)), Scalar(num(0.2))]),
            num(2.4000000000000004)
        );
        // Exact multiple stays put.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(6.0)), Scalar(num(2.0))]),
            num(6.0)
        );
    }

    /// OXP-229: sign rule — up toward +∞; negative/negative rounds away from 0,
    /// negative/positive rounds toward 0.
    #[test]
    fn negative_number_sign_rule() {
        // both negative: CEILING(-2.5,-2) = -4 (away from zero).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-2.5)), Scalar(num(-2.0))]),
            num(-4.0)
        );
        // neg number, pos significance: CEILING(-2.5,2) = -2 (toward zero).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-2.5)), Scalar(num(2.0))]),
            num(-2.0)
        );
        // both negative fractional exact multiple: CEILING(-2.5,-0.5) = -2.5.
        assert_close(
            eval_direct(eval, vec![Scalar(num(-2.5)), Scalar(num(-0.5))]),
            -2.5,
        );
    }

    /// OXP-229: significance == 0 → 0 for any number (asymmetric with FLOOR);
    /// positive-number / negative-significance mismatch → #NUM!.
    #[test]
    fn significance_zero_and_sign_mismatch() {
        // significance == 0 → 0 (both a nonzero and a zero number).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.5)), Scalar(num(0.0))]),
            num(0.0)
        );
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

    #[test]
    fn coercion_and_errors() {
        // Numeric text / bool coerce through to_number.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("3.2")), Scalar(num(2.0))]),
            num(4.0)
        );
        // Non-numeric text -> #VALUE!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("x")), Scalar(num(2.0))]),
            Value::Error(ErrorKind::Value)
        );
        // Error in either argument propagates.
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
                vec![Scalar(num(3.2)), Scalar(Value::Error(ErrorKind::Ref))]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }
}

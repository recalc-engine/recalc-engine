//! `PMT` — the periodic payment for an annuity (a loan or investment) at a
//! constant interest rate.
//!
//! # Provenance
//! Behavior contract: `docs/specs/PMT.md` (which cites the Microsoft Learn PMT
//! function page). Every argument is coerced to a scalar `f64` through
//! `xl-value`'s [`to_number`], exactly as [`crate::func_abs`] does for its one
//! argument; the financial optional-argument handling mirrors
//! [`crate::func_npv`]'s scalar-first pattern.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - **Signature.** `PMT(rate, nper, pv, [fv], [type])` (PMT.md §Signature).
//!   `rate`, `nper`, `pv` are required; `fv` defaults to `0` and `type` defaults
//!   to `0`. An omitted or out-of-range optional argument reads as
//!   [`Value::Blank`], which [`to_number`] maps to `0.0` — i.e. exactly the
//!   documented defaults — so no separate `count()`/`shape()` probe is needed.
//! - **Formula (annuity payment).** With `p = (1 + rate)^nper` computed by the
//!   ordinary `f64::powf` (no fast-math / FMA): when `rate == 0`,
//!   `pmt = -(pv + fv) / nper`; otherwise
//!   `pmt = -(pv*p + fv) * rate / ((p - 1) * (1 + rate*type))`. This is the
//!   standard annuity-payment closed form; it reproduces every worked example on
//!   the MS Learn page (PMT.md §Worked examples), which is how the formula and
//!   its **sign convention** were verified: a *positive* `pv` loan yields a
//!   *negative* payment (cash outflow), e.g.
//!   `PMT(0.08/12, 10, 10000) → ($1,037.03)`.
//! - **Coercion / error propagation.** Each argument is coerced with
//!   [`to_number`] (number passes through; `TRUE`/`FALSE` → `1`/`0`; numeric
//!   text → its number; blank → `0`). A non-coercible text argument → `#VALUE!`;
//!   an error-valued argument propagates as-is, in left-to-right order (PMT.md
//!   §Coercion, §Error behavior).
//! - **`type` flag (OXP-118 — RESOLVED, RUN-2026-07-11-oracle01).** `type` is a
//!   beginning/end-of-period flag: Excel treats any **nonzero** value as `1`
//!   (beginning of period). The farm probe `=PMT(0.05,10,10000,0,2)` returned
//!   `-1233.3769044329204`, which is exactly the `type == 1` result — *not* the
//!   literal `1 + rate*2` arithmetic — so a nonzero `type` collapses to `1`
//!   rather than being read as a raw multiplier (PMT.md §type flag).
//! - **Degenerate division by zero (OXP-119 — RESOLVED, RUN-2026-07-11-oracle01).**
//!   `nper == 0` (both branches) and `rate == -1` with `type == 1` (the
//!   `1 + rate*type == 0` factor) make the denominator zero. The farm probes
//!   `=PMT(0.05,0,10000)` and `=PMT(-1,10,0,500,1)` both returned `#NUM!`, so a
//!   zero denominator yields `#NUM!` (PMT.md §Degenerate inputs).
//! - **Overflow.** A genuinely non-finite *computed* payment (e.g. `(1+rate)^nper`
//!   overflowing to `±inf` and yielding `nan`) → `#NUM!` via [`Value::number`]
//!   (PMT.md §Error behavior / value invariant).

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `PMT(rate, nper, pv, [fv], [type])` call. See the module docs for
/// the semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Coerce every argument to a scalar f64, propagating the first error in
    // left-to-right order. Omitted `fv`/`type` read as Blank → 0.0, i.e. the
    // documented defaults.
    let rate = match to_number(&args.eval_scalar(0)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let nper = match to_number(&args.eval_scalar(1)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let pv = match to_number(&args.eval_scalar(2)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let fv = match to_number(&args.eval_scalar(3)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let ty = match to_number(&args.eval_scalar(4)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };

    // `type` is a beginning/end-of-period flag: Excel treats any nonzero value
    // as 1 (beginning), confirmed by the farm for `type == 2` — the result
    // matches `type == 1`, not `1 + rate*2` (OXP-118, RUN-2026-07-11-oracle01).
    let ty = if ty != 0.0 { 1.0 } else { 0.0 };

    let pmt = if rate == 0.0 {
        // nper == 0 → division by zero → #NUM! (OXP-119).
        if nper == 0.0 {
            return Value::Error(ErrorKind::Num);
        }
        -(pv + fv) / nper
    } else {
        // Ordinary f64::powf — no fast-math / FMA path.
        let p = (1.0 + rate).powf(nper);
        let denom = (p - 1.0) * (1.0 + rate * ty);
        // A zero denominator (nper == 0, or 1 + rate*type == 0 when rate == -1
        // and type == 1) is a division by zero that Excel reports as #NUM!
        // (OXP-119). A non-finite denominator (overflow) is *not* caught here and
        // falls through to Value::number's #NUM!, the documented overflow path.
        if denom == 0.0 {
            return Value::Error(ErrorKind::Num);
        }
        -(pv * p + fv) * rate / denom
    };

    // A non-finite computed payment (overflow) → #NUM! per the value invariant.
    Value::number(pmt)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    /// Pull the `f64` out of a numeric result (panics on anything else) so the
    /// irrational annuity results can be compared with a tolerance, not by bits.
    fn as_num(v: Value) -> f64 {
        match v {
            Value::Number(n) => n,
            other => panic!("expected a number, got {other:?}"),
        }
    }

    // The published worked examples are rounded to whole cents ("($1,037.03)"),
    // so they are checked at cent tolerance; the exact (rate == 0) and
    // formula-invariant relationships are checked far tighter.
    const CENT: f64 = 0.005;
    const TIGHT: f64 = 1e-9;

    #[test]
    fn canonical_ms_example_type0() {
        // MS Learn Example 1: =PMT(A2/12, A3, A4) with 8% annual, 10 months,
        // $10,000 loan → ($1,037.03). Positive pv loan → negative payment.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.08 / 12.0)),
                Scalar(num(10.0)),
                Scalar(num(10000.0)),
            ],
        ));
        assert!((got - (-1037.03)).abs() < CENT, "got {got}");
        assert!(
            got < 0.0,
            "positive-pv loan payment must be negative, got {got}"
        );
    }

    #[test]
    fn canonical_ms_example_type1_beginning_of_period() {
        // MS Learn Example 2: =PMT(A2/12, A3, A4, , 1) — same loan, payments at
        // the beginning of the period → ($1,030.16).
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.08 / 12.0)),
                Scalar(num(10.0)),
                Scalar(num(10000.0)),
                Omitted, // fv defaults to 0
                Scalar(num(1.0)),
            ],
        ));
        assert!((got - (-1030.16)).abs() < CENT, "got {got}");
    }

    #[test]
    fn type1_is_type0_over_one_plus_rate() {
        // The exact formula invariant linking the two conventions:
        // pmt(type=1) = pmt(type=0) / (1 + rate). Checked tightly (no rounding).
        let rate = 0.08 / 12.0;
        let t0 = as_num(eval_direct(
            eval,
            vec![Scalar(num(rate)), Scalar(num(10.0)), Scalar(num(10000.0))],
        ));
        let t1 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(rate)),
                Scalar(num(10.0)),
                Scalar(num(10000.0)),
                Scalar(num(0.0)),
                Scalar(num(1.0)),
            ],
        ));
        assert!(
            t1 > t0,
            "beginning-of-period payment is smaller in magnitude"
        );
        assert!((t1 * (1.0 + rate) - t0).abs() < TIGHT, "t0 {t0}, t1 {t1}");
    }

    #[test]
    fn ms_example_with_future_value() {
        // MS Learn Example 3: =PMT(A9/12, A10*12, 0, A11) — 6% annual over
        // 18 years (216 months) to save $50,000 → ($129.08).
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.06 / 12.0)),
                Scalar(num(216.0)),
                Scalar(num(0.0)),
                Scalar(num(50000.0)),
            ],
        ));
        assert!((got - (-129.08)).abs() < CENT, "got {got}");
    }

    #[test]
    fn rate_zero_is_exact_straight_line() {
        // rate == 0: pmt = -(pv + fv) / nper, an exact rational — no powf.
        // PMT(0, 10, 1000) = -100 exactly.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.0)), Scalar(num(10.0)), Scalar(num(1000.0))],
            ),
            num(-100.0)
        );
        // With a future value: PMT(0, 10, 1000, 500) = -(1500)/10 = -150 exactly.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.0)),
                    Scalar(num(10.0)),
                    Scalar(num(1000.0)),
                    Scalar(num(500.0)),
                ],
            ),
            num(-150.0)
        );
    }

    #[test]
    fn sign_flips_with_pv_sign() {
        // A negative pv (money received) produces a positive payment.
        let got = as_num(eval_direct(
            eval,
            vec![Scalar(num(0.05)), Scalar(num(12.0)), Scalar(num(-10000.0))],
        ));
        assert!(got > 0.0, "negative pv → positive payment, got {got}");
    }

    #[test]
    fn numeric_text_and_logical_coerce() {
        // Arguments coerce through to_number: "1000" → 1000, TRUE → 1 for type.
        // PMT(0, 10, "1000") = -100; TRUE as type=1 must be accepted (== 1.0).
        let got = as_num(eval_direct(
            eval,
            vec![Scalar(num(0.0)), Scalar(num(10.0)), Scalar(txt("1000"))],
        ));
        assert!((got - (-100.0)).abs() < TIGHT, "got {got}");

        let with_bool_type = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.08 / 12.0)),
                Scalar(num(10.0)),
                Scalar(num(10000.0)),
                Scalar(num(0.0)),
                Scalar(Value::Bool(true)), // TRUE → 1.0 → beginning of period
            ],
        ));
        assert!(
            (with_bool_type - (-1030.16)).abs() < CENT,
            "got {with_bool_type}"
        );
    }

    #[test]
    fn arg_error_propagates() {
        // An error in any argument propagates as the result.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Div0)),
                    Scalar(num(10.0)),
                    Scalar(num(10000.0)),
                ],
            ),
            Value::Error(ErrorKind::Div0)
        );
        // Non-coercible text → #VALUE!.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.05)), Scalar(num(10.0)), Scalar(txt("abc"))],
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn type_nonzero_collapses_to_one_oxp_118() {
        // OXP-118 (RUN-2026-07-11-oracle01): =PMT(0.05,10,10000,0,2) →
        // -1233.3769044329204, which is exactly the type == 1 result — Excel
        // reads `type` as a boolean flag (nonzero → beginning of period), not as
        // a raw `1 + rate*type` multiplier.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.05)),
                Scalar(num(10.0)),
                Scalar(num(10000.0)),
                Scalar(num(0.0)),
                Scalar(num(2.0)),
            ],
        ));
        assert!(
            (got - (-1233.3769044329204)).abs() < TIGHT,
            "got {got}, want -1233.3769044329204"
        );
        // Identical to the type == 1 result on the same inputs.
        let t1 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.05)),
                Scalar(num(10.0)),
                Scalar(num(10000.0)),
                Scalar(num(0.0)),
                Scalar(num(1.0)),
            ],
        ));
        assert!((got - t1).abs() < TIGHT, "type 2 must equal type 1");
    }

    #[test]
    fn nper_zero_is_num_oxp_119() {
        // OXP-119 (RUN-2026-07-11-oracle01): =PMT(0.05,0,10000) → #NUM!.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.05)), Scalar(num(0.0)), Scalar(num(10000.0))],
            ),
            Value::Error(ErrorKind::Num)
        );
        // Same #NUM! on the rate == 0 straight-line branch.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.0)), Scalar(num(0.0)), Scalar(num(10000.0))],
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn rate_minus_one_type_one_is_num_oxp_119() {
        // OXP-119 (RUN-2026-07-11-oracle01): =PMT(-1,10,0,500,1) → #NUM! —
        // rate == -1 with type == 1 makes (1 + rate*type) == 0 (zero denominator).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(-1.0)),
                    Scalar(num(10.0)),
                    Scalar(num(0.0)),
                    Scalar(num(500.0)),
                    Scalar(num(1.0)),
                ],
            ),
            Value::Error(ErrorKind::Num)
        );
    }
}

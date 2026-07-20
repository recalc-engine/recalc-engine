//! `IPMT` — the interest portion of a given period's payment on a
//! constant-rate, constant-payment annuity (loan or investment).
//!
//! # Provenance
//! Behavior contract: `docs/specs/IPMT.md`, which cites the Microsoft Learn
//! IPMT page (`IPMT(rate, per, nper, pv, [fv], [type])`). The two published
//! worked examples are pinned as unit tests below:
//! - `=IPMT(0.1/12, 1, 3*12, 8000)` → `-66.67` (interest in month 1), and
//! - `=IPMT(0.1, 3, 3, 8000)` → `-292.45` (interest in the final year).
//!
//! Both reproduce exactly under the derivation here, which fixes the **sign
//! convention**: cash paid out is negative, so the interest charged on a
//! positive `pv` (money received) comes back **negative**.
//!
//! # Definitions (derived from the MS annuity identities; see IPMT.md)
//! With `P = (1+rate)^nper`, the standard annuity payment (Excel `PMT`) is
//! ```text
//!   pmt = -(pv*P + fv) * rate / ((P - 1) * (1 + rate*type))        (rate != 0)
//! ```
//! and the balance still owed after `k` payments (the magnitude of Excel `FV`,
//! i.e. `pv` grown by interest less the payments made) is
//! ```text
//!   bal(k) = pv*(1+rate)^k + pmt*(1 + rate*type) * ((1+rate)^k - 1) / rate.
//! ```
//! The interest charged for period `per` is `rate` applied to the balance
//! outstanding at the **start** of that period, paid out (hence negated):
//! ```text
//!   type == 0 (end):     ipmt = -bal(per-1) * rate
//!   type == 1 (begin):   ipmt = -bal(per-1) * rate / (1 + rate)    (per >= 2)
//!   type == 1, per == 1: ipmt = 0
//! ```
//! The `/(1+rate)` factor for an annuity-due (and the zero first period) is
//! **derived, not guessed**: for a 2-period 10%/1000 begin-annuity, the
//! independently hand-computed interest in period 2 is `-47.62`, which the
//! naive `-bal*rate = -52.38` misses but `-bal*rate/(1+rate)` reproduces
//! exactly (IPMT.md §annuity-due). When `rate == 0` no interest accrues, so the
//! result is `0` (this also sidesteps the `/rate` in `bal`).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - **Argument coercion.** `rate, per, nper, pv` are required; `fv` and `type`
//!   are optional and default to `0`. Every argument is pulled with
//!   [`to_number`] (bool→1/0, numeric text→number, blank→0); an **omitted**
//!   trailing argument reads as [`Value::Blank`] and thus coerces to the `0`
//!   default. An error in any argument **propagates** (IPMT.md §Error behavior).
//! - **`per` range** (THE validation point). Per the MS page, `per` "must be in
//!   the range 1 to nper"; outside `1..=nper` yields `#NUM!` (IPMT.md §per).
//! - **Sign convention.** `-bal(per-1)*rate`: interest on a received (positive)
//!   `pv` is a payment out, so it is negative — matching the two MS examples.
//! - **Overflow.** A non-finite result (e.g. `(1+rate)^nper` overflowing to
//!   `inf`) becomes `#NUM!` via [`Value::number`] (IPMT.md §Error behavior).
//!
//! # OXP resolutions (RUN-2026-07-11-oracle01)
//! - **OXP-150 — non-integer `per` (RESOLVED, end-of-period).**
//!   `=IPMT(0.1,1.5,3,1000)` returned `-85.25412441989381`: Excel evaluates a
//!   fractional in-range `per` with the *same* closed form — the fractional
//!   exponent simply flows through `(1+rate)^(per-1)`; no truncation, no `#NUM!`.
//!   Supported for the probed `type == 0` path. A fractional `per` *with*
//!   annuity-due timing (`type == 1`) was **not** probed and stays
//!   `#UNSUPPORTED!` rather than assume the same closed form.
//! - **OXP-151 — `type` outside `{0, 1}` (RESOLVED).**
//!   `=IPMT(0.1,1,3,1000,0,2)` returned `0` — the `type == 1`, `per == 1`
//!   result — so `type` is a boolean flag (any nonzero value → `1`, beginning of
//!   period), not a raw multiplier.
//! - **OXP-152 — annuity-due (`type == 1`) confirmation (RESOLVED).** The farm
//!   pinned the derived `/(1+rate)` interest factor and the `per == 1 ⇒ 0` first
//!   period: `=IPMT(0.1,2,2,1000,0,1)` → `-47.61904761904761` and
//!   `=IPMT(0.1,1,2,1000,0,1)` → `0`.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `IPMT(rate, per, nper, pv, [fv], [type])` call. See the module
/// docs for the derivation, sign convention, and OXP deferrals.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Required, in order; optional fv/type default to 0 (omitted → Blank → 0).
    // Any error argument propagates immediately.
    let rate = match to_number(&args.eval_scalar(0)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let per = match to_number(&args.eval_scalar(1)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let nper = match to_number(&args.eval_scalar(2)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let pv = match to_number(&args.eval_scalar(3)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let fv = match to_number(&args.eval_scalar(4)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let typ = match to_number(&args.eval_scalar(5)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };

    // `type` is a beginning/end-of-period flag: Excel treats any nonzero value
    // as 1 (beginning), confirmed by the farm for `type == 2` — the result
    // matches `type == 1` (OXP-151, RUN-2026-07-11-oracle01).
    let type_flag = if typ != 0.0 { 1.0 } else { 0.0 };

    // `per` must lie in 1..=nper (MS: "must be in the range 1 to nper").
    if !(per >= 1.0 && per <= nper) {
        return Value::Error(ErrorKind::Num);
    }
    // A fractional in-range `per` is evaluated with the same closed form: the
    // fractional exponent flows through (1+rate)^(per-1). Confirmed for the
    // end-of-period path (OXP-150, RUN-2026-07-11-oracle01). Fractional `per`
    // with annuity-due timing (type == 1) was not probed → refuse rather than
    // assume the same form.
    if per.fract() != 0.0 && type_flag != 0.0 {
        return Value::Error(ErrorKind::Unsupported);
    }

    // rate == 0: a zero-rate annuity accrues no interest in any period. Handle
    // up front — it is the correct answer and avoids the /rate in `bal` below.
    if rate == 0.0 {
        return Value::number(0.0);
    }

    // Standard annuity payment (Excel PMT), used to roll the balance forward.
    let p = (1.0 + rate).powf(nper);
    let pmt = -(pv * p + fv) * rate / ((p - 1.0) * (1.0 + rate * type_flag));

    // Balance owed at the start of period `per` = after (per-1) payments.
    let k = per - 1.0;
    let pk = (1.0 + rate).powf(k);
    let bal = pv * pk + pmt * (1.0 + rate * type_flag) * (pk - 1.0) / rate;

    // Interest = rate on the outstanding balance, paid out (negated). An
    // annuity-due beginning-of-period payment discounts the charge by one
    // period, and its very first payment accrues no interest at all.
    let ipmt = if type_flag == 1.0 {
        if per == 1.0 {
            0.0
        } else {
            -bal * rate / (1.0 + rate)
        }
    } else {
        -bal * rate
    };

    // Non-finite (overflow) → #NUM! per the value invariant.
    Value::number(ipmt)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::{ErrorKind, Value};

    /// Pull the `f64` out of a numeric result (panics on anything else) so
    /// results can be compared with a tolerance, not by exact bits.
    fn as_num(v: Value) -> f64 {
        match v {
            Value::Number(n) => n,
            other => panic!("expected a number, got {other:?}"),
        }
    }

    /// Half-a-cent tolerance: the MS page publishes its example results rounded
    /// to cents (`($66.67)`, `($292.45)`), so agreement to < $0.005 pins us to
    /// the displayed values.
    const CENT: f64 = 0.005;
    /// Tight tolerance for identities computed at full `f64` precision.
    const TOL: f64 = 1e-9;

    /// The standard annuity payment (Excel PMT), recomputed independently so the
    /// interest+principal identity test does not lean on the evaluator.
    fn pmt(rate: f64, nper: f64, pv: f64, fv: f64, typ: f64) -> f64 {
        if rate == 0.0 {
            -(pv + fv) / nper
        } else {
            let p = (1.0 + rate).powf(nper);
            -(pv * p + fv) * rate / ((p - 1.0) * (1.0 + rate * typ))
        }
    }

    #[test]
    fn ms_example_first_month() {
        // MS Learn: =IPMT(0.1/12, 1, 3*12, 8000) → ($66.67): the interest in the
        // first month is charged on the full 8000 balance and paid OUT (negative).
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1 / 12.0)),
                Scalar(num(1.0)),
                Scalar(num(36.0)),
                Scalar(num(8000.0)),
            ],
        ));
        assert!((got - (-66.67)).abs() < CENT, "got {got}");
        // Period-1, type-0 closed form: exactly -pv*rate.
        assert!((got - (-8000.0 * (0.1 / 12.0))).abs() < TOL, "got {got}");
    }

    #[test]
    fn ms_example_final_year() {
        // MS Learn: =IPMT(0.1, 3, 3, 8000) → ($292.45): interest in the final
        // (3rd of 3) annual period.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(num(3.0)),
                Scalar(num(3.0)),
                Scalar(num(8000.0)),
            ],
        ));
        assert!((got - (-292.45)).abs() < CENT, "got {got}");
    }

    #[test]
    fn per_one_end_is_minus_pv_times_rate() {
        // type=0, per=1: interest = -pv*rate exactly, regardless of nper/pmt.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(num(1.0)),
                Scalar(num(3.0)),
                Scalar(num(1000.0)),
            ],
        ));
        assert!((got - (-100.0)).abs() < TOL, "got {got}");
    }

    #[test]
    fn annuity_due_first_period_is_zero() {
        // OXP-152 A2 (RUN-2026-07-11-oracle01): =IPMT(0.1,1,2,1000,0,1) → 0.
        // type=1 (begin) & per=1: the first payment is made before any interest
        // accrues, so IPMT is exactly 0.
        let got = eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(num(1.0)),
                Scalar(num(2.0)),
                Scalar(num(1000.0)),
                Scalar(num(0.0)),
                Scalar(num(1.0)),
            ],
        );
        assert_eq!(got, num(0.0), "annuity-due period 1 must be exactly 0");
    }

    #[test]
    fn annuity_due_second_period_hand_derived() {
        // OXP-152 A1 (RUN-2026-07-11-oracle01): =IPMT(0.1,2,2,1000,0,1) →
        // -47.61904761904761 (farm-confirmed, matching the hand derivation).
        // type=1, rate=0.1, nper=2, pv=1000: the begin-annuity payment is
        // -523.81, leaving 476.19 principal that accrues 47.62 of interest over
        // period 1 — so IPMT(period 2) = -47.62 (independently derived by hand).
        // This is the case the naive `-bal*rate` (= -52.38) gets wrong; the
        // /(1+rate) annuity-due factor is what reproduces -47.62.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(num(2.0)),
                Scalar(num(2.0)),
                Scalar(num(1000.0)),
                Scalar(num(0.0)),
                Scalar(num(1.0)),
            ],
        ));
        assert!((got - (-47.61904761904762)).abs() < 1e-6, "got {got}");
        // And emphatically NOT the un-discounted -52.38.
        assert!(
            (got - (-52.38)).abs() > 1.0,
            "must apply the annuity-due factor"
        );
    }

    #[test]
    fn interest_plus_principal_equals_payment_over_all_periods() {
        // Interest+principal identity (IPMT+PPMT = PMT each period). Summed over
        // all periods with fv=0, total principal repaid is -pv, so
        //   Σ IPMT(per) = nper*PMT + pv.
        // PMT is recomputed independently, so this checks the balance recursion
        // without leaning on the evaluator's own pmt.
        let (rate, nper, pv, fv, typ) = (0.1, 3.0, 8000.0, 0.0, 0.0);
        let mut sum = 0.0;
        for per in 1..=(nper as i64) {
            sum += as_num(eval_direct(
                eval,
                vec![
                    Scalar(num(rate)),
                    Scalar(num(per as f64)),
                    Scalar(num(nper)),
                    Scalar(num(pv)),
                ],
            ));
        }
        let want = nper * pmt(rate, nper, pv, fv, typ) + pv;
        assert!((sum - want).abs() < 1e-6, "sum {sum}, want {want}");
    }

    #[test]
    fn zero_rate_is_zero_interest() {
        // No interest accrues at rate 0, in any period.
        let got = eval_direct(
            eval,
            vec![
                Scalar(num(0.0)),
                Scalar(num(2.0)),
                Scalar(num(4.0)),
                Scalar(num(1000.0)),
            ],
        );
        assert_eq!(got, num(0.0));
    }

    #[test]
    fn per_below_range_is_num() {
        // per = 0 < 1 → #NUM!.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.1)),
                    Scalar(num(0.0)),
                    Scalar(num(3.0)),
                    Scalar(num(1000.0)),
                ]
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn per_above_nper_is_num() {
        // per = 4 > nper = 3 → #NUM!.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.1)),
                    Scalar(num(4.0)),
                    Scalar(num(3.0)),
                    Scalar(num(1000.0)),
                ]
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn non_integer_per_end_of_period_oxp_150() {
        // OXP-150 (RUN-2026-07-11-oracle01): =IPMT(0.1,1.5,3,1000) →
        // -85.25412441989381. A fractional in-range per (type 0) is evaluated
        // with the same closed form — no truncation, no #NUM!.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(num(1.5)),
                Scalar(num(3.0)),
                Scalar(num(1000.0)),
            ],
        ));
        assert!(
            (got - (-85.25412441989381)).abs() < 1e-6,
            "got {got}, want -85.25412441989381"
        );
    }

    #[test]
    fn fractional_per_annuity_due_is_unsupported() {
        // A fractional per with annuity-due timing (type == 1) was NOT probed by
        // RUN-2026-07-11-oracle01 → refuse rather than assume the same closed
        // form (never-guess).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.1)),
                    Scalar(num(1.5)),
                    Scalar(num(3.0)),
                    Scalar(num(1000.0)),
                    Scalar(num(0.0)),
                    Scalar(num(1.0)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn type_nonzero_collapses_to_one_oxp_151() {
        // OXP-151 (RUN-2026-07-11-oracle01): =IPMT(0.1,1,3,1000,0,2) → 0, i.e.
        // the type == 1, per == 1 result. `type` is a boolean flag: any nonzero
        // value is read as 1 (beginning of period), not a raw multiplier.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.1)),
                    Scalar(num(1.0)),
                    Scalar(num(3.0)),
                    Scalar(num(1000.0)),
                    Scalar(num(0.0)),
                    Scalar(num(2.0)),
                ]
            ),
            num(0.0)
        );
    }

    #[test]
    fn error_argument_propagates() {
        // An error in any argument propagates (here: pv).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.1)),
                    Scalar(num(1.0)),
                    Scalar(num(3.0)),
                    Scalar(Value::Error(ErrorKind::Div0)),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }
}

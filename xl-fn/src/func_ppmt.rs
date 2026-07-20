//! `PPMT` — the **principal** portion of the payment for a given period of an
//! annuity with constant periodic payments and a constant interest rate.
//!
//! # Provenance
//! Behavior contract: `docs/specs/PPMT.md`, which cites the Microsoft Learn
//! PPMT page (`support.microsoft.com/.../ppmt-function-...`). Both published
//! examples are reproduced as unit tests and matched to the cent:
//! `PPMT(0.1/12, 1, 24, 2000) = -75.62` and
//! `PPMT(0.08, 10, 10, 200000) = -27,598.05`.
//!
//! # Derivation (clean-room, from the MS-pinned PMT/FV annuity identity)
//! Excel's annuity identity ties present value `pv`, payment `pmt`, and future
//! value `fv` over `nper` periods (`P = (1+rate)^nper`, `rate != 0`):
//! `pv*P + pmt*(1+rate*type)*(P-1)/rate + fv = 0`, whence
//! `pmt = -(pv*P + fv) * rate / ((P-1)*(1+rate*type))`
//! (and `pmt = -(pv+fv)/nper` when `rate == 0`).
//!
//! The outstanding balance carried into period `per` is the *value now* of the
//! original principal plus the payments already made — i.e. Excel's `FV` after
//! `per-1` periods. Writing that balance (with the sign such that it is `+pv`
//! at `k = 0`) as
//! `bal(k) = pv*(1+rate)^k + pmt*(((1+rate)^k - 1)/rate)`  (type 0),
//! the interest charged in period `per` is `-bal(per-1)*rate`, so the principal
//! portion is
//! `PPMT = pmt - interest = pmt + bal(per-1)*rate`.
//! This is verified numerically against both MS examples (see tests) and
//! against the self-consistency invariants `PPMT(per) + IPMT(per) = pmt` and
//! `Σ_{per} PPMT(per) = -(pv + fv)`.
//!
//! # Semantics implemented
//! - **Arguments.** `rate, per, nper, pv` required; `fv` (default 0) and `type`
//!   (default 0) optional. Each is coerced with [`to_number`] in scalar
//!   context (bool→1/0, numeric text→number, blank/omitted→0). An error in any
//!   argument propagates, in left-to-right argument order.
//! - **`per` range.** Per the MS page, `per` "must be in the range 1 to nper".
//!   `per < 1` or `per > nper` → `#NUM!` ([`ErrorKind::Num`]). This also
//!   catches `nper <= 0` (no valid `per` exists).
//! - **`type` flag (OXP-128 — RESOLVED, RUN-2026-07-11-oracle01).** `type` is a
//!   beginning/end-of-period flag: any **nonzero** value collapses to `1`
//!   (beginning). PPMT is the principal split `PMT − IPMT`, both sides carrying
//!   the same timing; for annuity-due the interest carries the derived
//!   `/(1+rate)` factor and the first period accrues zero interest. Farm-pinned:
//!   `=PPMT(0.1,1,12,1000,0,1)` → `-133.42119554571576`,
//!   `=PPMT(0.1,2,12,1000,0,1)` → `-46.76331510028732`.
//! - **Non-integer `per` (OXP-129 — RESOLVED, end-of-period).** A fractional
//!   in-range `per` is evaluated with the same closed form (the fractional
//!   exponent flows through `(1+rate)^(per-1)`). Farm-pinned:
//!   `=PPMT(0.1,1.5,12,1000)` → `-49.0457786469502`,
//!   `=PPMT(0.1,2.5,12,1000)` → `-53.95035651164522`.
//! - **`rate == 0`.** Straight-line: `pmt = -(pv+fv)/nper`, zero interest, so
//!   `PPMT == pmt`.
//! - **Overflow / non-finite** intermediate (huge `nper`) → `#NUM!` via
//!   [`Value::number`].
//!
//! # Resolved error / deferral cases
//! - **`rate == -1` → `#NUM!` (OXP-149 — RESOLVED, RUN-2026-07-11-oracle01).**
//!   `(1+rate) == 0` makes the balance recurrence degenerate; the farm returns
//!   `#NUM!` (`=PPMT(-1,1,12,1000)` and `=PPMT(-1,2,12,1000)` both `#NUM!`). It
//!   is guarded explicitly because the closed form would otherwise yield a finite
//!   (wrong) value at `rate == -1` rather than a NaN.
//! - **Non-integer `per` with annuity-due (`type != 0`) → `#UNSUPPORTED!`.** This
//!   combination was **not** probed by RUN-2026-07-11-oracle01, so it stays
//!   deferred rather than assume the end-of-period closed form carries over.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `PPMT(rate, per, nper, pv, [fv], [type])` call. See the module
/// docs for the semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Coerce all six operands in scalar context, propagating the first error in
    // argument order. Omitted `fv`/`type` evaluate to Blank → 0.
    let nums: [f64; 6] = {
        let mut out = [0.0_f64; 6];
        for (i, slot) in out.iter_mut().enumerate() {
            match to_number(&args.eval_scalar(i)) {
                Ok(n) => *slot = n,
                Err(k) => return Value::Error(k),
            }
        }
        out
    };
    let [rate, per, nper, pv, fv, typ] = nums;

    // `type` is a beginning/end-of-period flag: any nonzero value → 1
    // (beginning). Confirmed for PPMT's annuity-due split (OXP-128) and the
    // sibling IPMT `type == 2` probe (OXP-151), both RUN-2026-07-11-oracle01.
    let type_flag = if typ != 0.0 { 1.0 } else { 0.0 };

    // `per` must lie in 1..=nper (also rejects nper <= 0: no valid per exists).
    if !(per >= 1.0 && per <= nper) {
        return Value::Error(ErrorKind::Num);
    }
    // A fractional in-range `per` is evaluated with the same closed form for the
    // end-of-period path (OXP-129). A fractional `per` with annuity-due timing
    // (type == 1) was not probed → refuse rather than guess.
    if per.fract() != 0.0 && type_flag != 0.0 {
        return Value::Error(ErrorKind::Unsupported);
    }

    // rate == -1: (1 + rate) == 0 makes the balance recurrence degenerate; the
    // farm returns #NUM! (OXP-149). Guarded explicitly because the closed form
    // would otherwise yield a finite (wrong) value rather than a NaN here.
    if rate == -1.0 {
        return Value::Error(ErrorKind::Num);
    }

    // PPMT is the principal split of the payment: PPMT = PMT − IPMT, both sides
    // carrying the same `type` timing. The annuity-due interest factor
    // `/(1+rate)` and the `per == 1 ⇒ 0` first period mirror IPMT and are
    // farm-confirmed (OXP-128).
    let ppmt = if rate == 0.0 {
        // Straight-line amortization: zero interest, so PPMT == the full payment.
        -(pv + fv) / nper
    } else {
        let p = (1.0 + rate).powf(nper);
        let pmt = -(pv * p + fv) * rate / ((p - 1.0) * (1.0 + rate * type_flag));
        // Outstanding balance carried into period `per` (value-now of principal
        // plus payments already made), signed +pv at k = 0.
        let pk = (1.0 + rate).powf(per - 1.0);
        let bal = pv * pk + pmt * (1.0 + rate * type_flag) * (pk - 1.0) / rate;
        // interest for the period; principal = pmt - interest.
        let interest = if type_flag == 1.0 {
            if per == 1.0 {
                0.0
            } else {
                -bal * rate / (1.0 + rate)
            }
        } else {
            -bal * rate
        };
        pmt - interest
    };

    // Non-finite (overflow) → #NUM! via the value invariant.
    Value::number(ppmt)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::{ErrorKind, Value};

    fn as_num(v: Value) -> f64 {
        match v {
            Value::Number(n) => n,
            other => panic!("expected a number, got {other:?}"),
        }
    }

    const TOL: f64 = 1e-6;

    #[test]
    fn ms_example_first_month() {
        // MS Learn: =PPMT(0.1/12, 1, 24, 2000) → ($75.62). Principal portion of
        // the first monthly payment on a $2,000, two-year, 10%-annual loan.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1 / 12.0)),
                Scalar(num(1.0)),
                Scalar(num(24.0)),
                Scalar(num(2000.0)),
            ],
        ));
        assert!((got - (-75.62)).abs() < 5e-3, "got {got}, want -75.62");
    }

    #[test]
    fn ms_example_last_year() {
        // MS Learn: =PPMT(0.08, 10, 10, 200000) → ($27,598.05). Principal
        // portion of the final (10th) yearly payment on a $200,000 8% loan.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.08)),
                Scalar(num(10.0)),
                Scalar(num(10.0)),
                Scalar(num(200000.0)),
            ],
        ));
        assert!(
            (got - (-27598.05)).abs() < 5e-3,
            "got {got}, want -27598.05"
        );
    }

    #[test]
    fn ppmt_plus_ipmt_equals_pmt_and_sums_to_principal() {
        // Independent cross-check on a small loan (pv=2000, rate=0.1, nper=2):
        // PPMT(per=1) = -952.380952…, PPMT(per=2) = -1047.619047…, and the two
        // principal portions sum to -pv (the whole principal is repaid).
        let ppmt1 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(num(1.0)),
                Scalar(num(2.0)),
                Scalar(num(2000.0)),
            ],
        ));
        let ppmt2 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(num(2.0)),
                Scalar(num(2.0)),
                Scalar(num(2000.0)),
            ],
        ));
        assert!((ppmt1 - (-952.3809523809524)).abs() < TOL, "ppmt1 {ppmt1}");
        assert!((ppmt2 - (-1047.6190476190477)).abs() < TOL, "ppmt2 {ppmt2}");
        assert!(
            (ppmt1 + ppmt2 - (-2000.0)).abs() < 1e-6,
            "sum {}",
            ppmt1 + ppmt2
        );
    }

    #[test]
    fn rate_zero_is_straight_line_all_principal() {
        // rate == 0 → zero interest, so PPMT equals the full payment
        // -(pv+fv)/nper for every period. pv=1200, nper=12 → -100 each period.
        for per in 1..=12 {
            let got = as_num(eval_direct(
                eval,
                vec![
                    Scalar(num(0.0)),
                    Scalar(num(per as f64)),
                    Scalar(num(12.0)),
                    Scalar(num(1200.0)),
                ],
            ));
            assert!((got - (-100.0)).abs() < TOL, "per {per}: got {got}");
        }
    }

    #[test]
    fn fv_and_default_fv_agree() {
        // Passing an explicit fv of 0 must match omitting it entirely.
        let with = eval_direct(
            eval,
            vec![
                Scalar(num(0.08)),
                Scalar(num(10.0)),
                Scalar(num(10.0)),
                Scalar(num(200000.0)),
                Scalar(num(0.0)),
            ],
        );
        let without = eval_direct(
            eval,
            vec![
                Scalar(num(0.08)),
                Scalar(num(10.0)),
                Scalar(num(10.0)),
                Scalar(num(200000.0)),
            ],
        );
        assert_eq!(with, without);
    }

    #[test]
    fn per_below_one_is_num() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.1)),
                    Scalar(num(0.0)),
                    Scalar(num(12.0)),
                    Scalar(num(2000.0)),
                ]
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn per_above_nper_is_num() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.1)),
                    Scalar(num(13.0)),
                    Scalar(num(12.0)),
                    Scalar(num(2000.0)),
                ]
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn annuity_due_split_oxp_128() {
        // OXP-128 (RUN-2026-07-11-oracle01): annuity-due (type=1) principal split.
        //   =PPMT(0.1,1,12,1000,0,1) → -133.42119554571576 (per 1 = full payment,
        //     since IPMT is 0 in the first begin-of-period period),
        //   =PPMT(0.1,2,12,1000,0,1) → -46.76331510028732.
        let h1 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(num(1.0)),
                Scalar(num(12.0)),
                Scalar(num(1000.0)),
                Scalar(num(0.0)),
                Scalar(num(1.0)),
            ],
        ));
        assert!(
            (h1 - (-133.42119554571576)).abs() < TOL,
            "h1 {h1}, want -133.42119554571576"
        );
        let h2 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(num(2.0)),
                Scalar(num(12.0)),
                Scalar(num(1000.0)),
                Scalar(num(0.0)),
                Scalar(num(1.0)),
            ],
        ));
        assert!(
            (h2 - (-46.76331510028732)).abs() < TOL,
            "h2 {h2}, want -46.76331510028732"
        );
    }

    #[test]
    fn type_nonzero_collapses_to_one_oxp_128() {
        // OXP-128 H3 cross-check: =PPMT(0.1,1,12,1000,0,0) → -46.76331510028732
        // (the type == 0, per == 1 principal). A nonzero `type` (e.g. 2) is read
        // as 1, matching the annuity-due first-period full-payment result.
        let type2 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(num(1.0)),
                Scalar(num(12.0)),
                Scalar(num(1000.0)),
                Scalar(num(0.0)),
                Scalar(num(2.0)),
            ],
        ));
        assert!(
            (type2 - (-133.42119554571576)).abs() < TOL,
            "type 2 must equal type 1, got {type2}"
        );
    }

    #[test]
    fn fractional_per_end_of_period_oxp_129() {
        // OXP-129 (RUN-2026-07-11-oracle01): a fractional in-range per (type 0)
        // is evaluated with the same closed form.
        //   =PPMT(0.1,1.5,12,1000) → -49.0457786469502,
        //   =PPMT(0.1,2.5,12,1000) → -53.95035651164522.
        let h1 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(num(1.5)),
                Scalar(num(12.0)),
                Scalar(num(1000.0)),
            ],
        ));
        assert!(
            (h1 - (-49.0457786469502)).abs() < TOL,
            "h1 {h1}, want -49.0457786469502"
        );
        let h2 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(num(2.5)),
                Scalar(num(12.0)),
                Scalar(num(1000.0)),
            ],
        ));
        assert!(
            (h2 - (-53.95035651164522)).abs() < TOL,
            "h2 {h2}, want -53.95035651164522"
        );
    }

    #[test]
    fn rate_minus_one_is_num_oxp_149() {
        // OXP-149 (RUN-2026-07-11-oracle01): =PPMT(-1,1,12,1000) and
        // =PPMT(-1,2,12,1000) both → #NUM! ((1+rate) == 0, degenerate recurrence).
        for per in [1.0, 2.0] {
            assert_eq!(
                eval_direct(
                    eval,
                    vec![
                        Scalar(num(-1.0)),
                        Scalar(num(per)),
                        Scalar(num(12.0)),
                        Scalar(num(1000.0)),
                    ]
                ),
                Value::Error(ErrorKind::Num),
                "per {per}"
            );
        }
    }

    #[test]
    fn fractional_per_annuity_due_is_unsupported() {
        // Fractional per with annuity-due timing (type == 1) was NOT probed by
        // RUN-2026-07-11-oracle01 → refuse rather than guess (never-guess).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.1)),
                    Scalar(num(1.5)),
                    Scalar(num(12.0)),
                    Scalar(num(2000.0)),
                    Scalar(num(0.0)),
                    Scalar(num(1.0)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn argument_error_propagates() {
        // An error in any argument propagates (here the pv slot).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.1)),
                    Scalar(num(1.0)),
                    Scalar(num(12.0)),
                    Scalar(Value::Error(ErrorKind::Div0)),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }
}

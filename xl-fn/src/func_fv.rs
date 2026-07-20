//! `FV` — the **future value** of an annuity (a loan or investment) at a
//! constant interest rate: the cash balance attained after the last payment.
//!
//! # Provenance
//! Behavior contract: `docs/specs/FV.md`, which cites the Microsoft Learn FV
//! function page. FV is the future-value member of the same time-value-of-money
//! (TVM) family as [`crate::func_pmt`], [`crate::func_pv`], [`crate::func_ppmt`],
//! and [`crate::func_ipmt`]: they all solve the one pinned annuity identity for a
//! different unknown, share the `rate == 0` degenerate branch, and share the
//! `type` 0/1 beginning/end-of-period flag (farm-confirmed for the family —
//! PMT OXP-118, IPMT OXP-151, PPMT OXP-128, all RUN-2026-07-11-oracle01). Every
//! argument is coerced to a scalar `f64` through `xl-value`'s [`to_number`],
//! exactly as [`crate::func_pmt`] does.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - **Signature.** `FV(rate, nper, pmt, [pv], [type])` (FV.md §Signature).
//!   `rate`, `nper`, `pmt` are required; `pv` defaults to `0` and `type` defaults
//!   to `0`. An omitted optional argument reads as [`Value::Blank`], which
//!   [`to_number`] maps to `0.0` — exactly the documented defaults — so no
//!   separate `count()`/`shape()` probe is needed.
//! - **Formula (future value).** With `p = (1 + rate)^nper` computed by the
//!   ordinary `f64::powf` (no fast-math / FMA): when `rate == 0`,
//!   `fv = -(pv + pmt*nper)`; otherwise
//!   `fv = -(pv*p + pmt*(1 + rate*type)*(p - 1)/rate)`. This is the standard
//!   future-value closed form obtained by solving the family's annuity identity
//!   `pv*p + pmt*(1 + rate*type)*(p - 1)/rate + fv = 0` for `fv`; it reproduces
//!   the MS Learn worked examples (FV.md §Worked examples), which pin the formula
//!   and its **sign convention**: a negative `pmt` (cash paid out each period)
//!   yields a positive `fv` (the balance accumulated), e.g.
//!   `FV(0.06/12, 10, -200, -500, 1) → $2,581.40`.
//! - **Coercion / error propagation.** Each argument is coerced with
//!   [`to_number`] (number passes through; `TRUE`/`FALSE` → `1`/`0`; numeric text
//!   → its number; blank → `0`). A non-coercible text argument → `#VALUE!`; an
//!   error-valued argument propagates as-is, in left-to-right order (FV.md
//!   §Coercion, §Error behavior).
//! - **`type` flag (family — OXP-118, RUN-2026-07-11-oracle01).** `type` is a
//!   beginning/end-of-period flag: Excel treats any **nonzero** value as `1`
//!   (beginning of period), not as a raw `1 + rate*type` multiplier. The PMT farm
//!   probe `=PMT(0.05,10,10000,0,2)` returned exactly the `type == 1` result
//!   (OXP-118), and the sibling IPMT/PPMT probes (OXP-151/128) confirmed the same
//!   collapse; FV shares the identical `type` factor, so a nonzero `type`
//!   collapses to `1` here too (FV.md §type flag).
//! - **Non-finite / overflow → `#NUM!` (value invariant).** FV has no interior
//!   division except `/rate`, which the `rate == 0` branch removes, so the only
//!   non-finite path is a genuine overflow: `p = (1 + rate)^nper` growing to
//!   `±inf` for an absurd `nper`. [`Value::number`] maps such a non-finite result
//!   to `#NUM!` — the same documented backstop the whole family uses for overflow
//!   (FV.md §Error behavior).

use xl_value::{Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `FV(rate, nper, pmt, [pv], [type])` call. See the module docs for
/// the semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Coerce every argument to a scalar f64, propagating the first error in
    // left-to-right order. Omitted `pv`/`type` read as Blank → 0.0, i.e. the
    // documented defaults.
    let rate = match to_number(&args.eval_scalar(0)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let nper = match to_number(&args.eval_scalar(1)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let pmt = match to_number(&args.eval_scalar(2)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let pv = match to_number(&args.eval_scalar(3)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let ty = match to_number(&args.eval_scalar(4)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };

    // `type` is a beginning/end-of-period flag: any nonzero value collapses to 1
    // (beginning), a family property farm-confirmed for PMT (OXP-118) and the
    // sibling IPMT/PPMT probes (OXP-151/128), all RUN-2026-07-11-oracle01.
    let ty = if ty != 0.0 { 1.0 } else { 0.0 };

    let fv = if rate == 0.0 {
        // Straight-line: the future value is just the (negated) undiscounted sum
        // of the present value and the payments. No division, always exact.
        -(pv + pmt * nper)
    } else {
        // Ordinary f64::powf — no fast-math / FMA path.
        let p = (1.0 + rate).powf(nper);
        let annuity = pmt * (1.0 + rate * ty) * (p - 1.0) / rate;
        -(pv * p + annuity)
    };

    // A non-finite computed future value (overflow) → #NUM! per the value
    // invariant.
    Value::number(fv)
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

    // The published worked examples are rounded to whole cents, so they are
    // checked at cent tolerance; the exact (rate == 0) and formula-invariant
    // relationships are checked far tighter.
    const CENT: f64 = 0.005;
    const TIGHT: f64 = 1e-9;
    const TOL: f64 = 1e-6;

    #[test]
    fn canonical_ms_examples() {
        // MS Learn FV examples (all published to the cent):
        // 1. =FV(0.06/12, 10, -200, -500, 1) → $2,581.40 (begin-of-period).
        let e1 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.06 / 12.0)),
                Scalar(num(10.0)),
                Scalar(num(-200.0)),
                Scalar(num(-500.0)),
                Scalar(num(1.0)),
            ],
        ));
        assert!((e1 - 2581.40).abs() < CENT, "got {e1}");

        // 2. =FV(0.12/12, 12, -1000) → $12,682.50.
        let e2 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.12 / 12.0)),
                Scalar(num(12.0)),
                Scalar(num(-1000.0)),
            ],
        ));
        assert!((e2 - 12682.50).abs() < CENT, "got {e2}");

        // 3. =FV(0.11/12, 35, -2000, , 1) → $82,846.25 (begin-of-period, pv = 0).
        let e3 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.11 / 12.0)),
                Scalar(num(35.0)),
                Scalar(num(-2000.0)),
                Omitted,
                Scalar(num(1.0)),
            ],
        ));
        assert!((e3 - 82846.25).abs() < CENT, "got {e3}");
        assert!(
            e3 > 0.0,
            "negative payments accumulate a positive FV, got {e3}"
        );
    }

    #[test]
    fn rate_zero_is_exact_straight_line() {
        // rate == 0: fv = -(pv + pmt*nper), an exact rational — no powf.
        // FV(0, 10, 100) = -(0 + 1000) = -1000 exactly.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.0)), Scalar(num(10.0)), Scalar(num(100.0))],
            ),
            num(-1000.0)
        );
        // With a present value: FV(0, 10, 100, 50) = -(50 + 1000) = -1050 exactly.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.0)),
                    Scalar(num(10.0)),
                    Scalar(num(100.0)),
                    Scalar(num(50.0)),
                ],
            ),
            num(-1050.0)
        );
    }

    #[test]
    fn type1_is_type0_times_one_plus_rate() {
        // For the payment stream (pv = 0) the annuity-due future value is the
        // ordinary-annuity value scaled by exactly (1 + rate): the begin-of-period
        // (1 + rate*type) factor becomes (1 + rate). Checked tightly (no rounding).
        let rate = 0.05;
        let t0 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(rate)),
                Scalar(num(10.0)),
                Scalar(num(100.0)),
                Scalar(num(0.0)),
                Scalar(num(0.0)),
            ],
        ));
        let t1 = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(rate)),
                Scalar(num(10.0)),
                Scalar(num(100.0)),
                Scalar(num(0.0)),
                Scalar(num(1.0)),
            ],
        ));
        assert!((t1 - t0 * (1.0 + rate)).abs() < TIGHT, "t0 {t0}, t1 {t1}");
    }

    #[test]
    fn type_nonzero_collapses_to_one_oxp_118() {
        // Family OXP-118 (RUN-2026-07-11-oracle01): Excel reads `type` as a
        // boolean flag — any nonzero value behaves as 1, not as a raw
        // `1 + rate*type` multiplier. So FV(...,type=2) must equal FV(...,type=1)
        // exactly on identical inputs.
        let t2 = eval_direct(
            eval,
            vec![
                Scalar(num(0.05)),
                Scalar(num(10.0)),
                Scalar(num(100.0)),
                Scalar(num(0.0)),
                Scalar(num(2.0)),
            ],
        );
        let t1 = eval_direct(
            eval,
            vec![
                Scalar(num(0.05)),
                Scalar(num(10.0)),
                Scalar(num(100.0)),
                Scalar(num(0.0)),
                Scalar(num(1.0)),
            ],
        );
        assert_eq!(t2, t1, "nonzero type must collapse to type == 1");
    }

    #[test]
    fn loan_repaid_by_pmt_has_zero_future_value() {
        // Family cross-check: a loan of `pv` repaid by the annuity payment
        // PMT(rate, nper, pv) leaves a zero balance, so
        // FV(rate, nper, PMT(rate, nper, pv), pv) == 0. This ties FV, PMT, and the
        // shared annuity identity together across evaluators.
        let (rate, nper, pv) = (0.05, 12.0, 10000.0);
        let pmt = as_num(eval_direct(
            crate::func_pmt::eval,
            vec![Scalar(num(rate)), Scalar(num(nper)), Scalar(num(pv))],
        ));
        let residual = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(rate)),
                Scalar(num(nper)),
                Scalar(num(pmt)),
                Scalar(num(pv)),
            ],
        ));
        assert!(
            residual.abs() < TOL,
            "residual FV should be ~0, got {residual}"
        );
    }

    #[test]
    fn sign_flips_with_pmt_sign() {
        // A positive payment (cash received each period) produces a negative FV.
        let got = as_num(eval_direct(
            eval,
            vec![Scalar(num(0.05)), Scalar(num(12.0)), Scalar(num(250.0))],
        ));
        assert!(got < 0.0, "positive payment → negative FV, got {got}");
    }

    #[test]
    fn numeric_text_and_logical_coerce() {
        // Arguments coerce through to_number: "-1000" → -1000, TRUE → 1 for type.
        // FV(0, 10, "-100") = -(0 + -1000) = 1000.
        let got = as_num(eval_direct(
            eval,
            vec![Scalar(num(0.0)), Scalar(num(10.0)), Scalar(txt("-100"))],
        ));
        assert!((got - 1000.0).abs() < TIGHT, "got {got}");

        // TRUE in the `type` slot must behave as 1.0 (beginning of period): equal
        // to an explicit type == 1 on the same inputs.
        let with_bool = eval_direct(
            eval,
            vec![
                Scalar(num(0.05)),
                Scalar(num(10.0)),
                Scalar(num(100.0)),
                Scalar(num(0.0)),
                Scalar(Value::Bool(true)),
            ],
        );
        let with_one = eval_direct(
            eval,
            vec![
                Scalar(num(0.05)),
                Scalar(num(10.0)),
                Scalar(num(100.0)),
                Scalar(num(0.0)),
                Scalar(num(1.0)),
            ],
        );
        assert_eq!(with_bool, with_one, "TRUE type must equal type == 1");
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
                    Scalar(num(100.0)),
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
}

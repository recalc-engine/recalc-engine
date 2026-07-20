//! `PV` — the **present value** of an annuity (a loan or investment) at a
//! constant interest rate: the lump sum that a series of future payments is
//! worth right now.
//!
//! # Provenance
//! Behavior contract: `docs/specs/PV.md`, which cites the Microsoft Learn PV
//! function page. PV is the present-value member of the same time-value-of-money
//! (TVM) family as [`crate::func_pmt`], [`crate::func_fv`], [`crate::func_ppmt`],
//! and [`crate::func_ipmt`]: they all solve the one pinned annuity identity for a
//! different unknown, share the `rate == 0` degenerate branch, and share the
//! `type` 0/1 beginning/end-of-period flag (farm-confirmed for the family —
//! PMT OXP-118, IPMT OXP-151, PPMT OXP-128, all RUN-2026-07-11-oracle01). Every
//! argument is coerced to a scalar `f64` through `xl-value`'s [`to_number`],
//! exactly as [`crate::func_pmt`] does.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - **Signature.** `PV(rate, nper, pmt, [fv], [type])` (PV.md §Signature).
//!   `rate`, `nper`, `pmt` are required; `fv` defaults to `0` and `type` defaults
//!   to `0`. An omitted optional argument reads as [`Value::Blank`], which
//!   [`to_number`] maps to `0.0` — exactly the documented defaults — so no
//!   separate `count()`/`shape()` probe is needed.
//! - **Formula (present value).** With `p = (1 + rate)^nper` computed by the
//!   ordinary `f64::powf` (no fast-math / FMA): when `rate == 0`,
//!   `pv = -(pmt*nper + fv)`; otherwise
//!   `pv = -(fv + pmt*(1 + rate*type)*(p - 1)/rate) / p`. This is the standard
//!   present-value closed form obtained by solving the family's annuity identity
//!   `pv*p + pmt*(1 + rate*type)*(p - 1)/rate + fv = 0` for `pv`; it reproduces
//!   the MS Learn worked example (PV.md §Worked example), which pins the formula
//!   and its **sign convention**: a positive `pmt` (cash received each period)
//!   yields a negative `pv` (the cash you pay out now), e.g.
//!   `PV(0.08/12, 240, 500) → ($59,777.15)`.
//! - **Coercion / error propagation.** Each argument is coerced with
//!   [`to_number`] (number passes through; `TRUE`/`FALSE` → `1`/`0`; numeric text
//!   → its number; blank → `0`). A non-coercible text argument → `#VALUE!`; an
//!   error-valued argument propagates as-is, in left-to-right order (PV.md
//!   §Coercion, §Error behavior).
//! - **`type` flag (family — OXP-118, RUN-2026-07-11-oracle01).** `type` is a
//!   beginning/end-of-period flag: Excel treats any **nonzero** value as `1`
//!   (beginning of period), not as a raw `1 + rate*type` multiplier. The PMT farm
//!   probe `=PMT(0.05,10,10000,0,2)` returned exactly the `type == 1` result
//!   (OXP-118), and the sibling IPMT/PPMT probes (OXP-151/128) confirmed the same
//!   collapse; PV shares the identical `type` factor, so a nonzero `type`
//!   collapses to `1` here too (PV.md §type flag).
//! - **Non-finite / overflow → `#NUM!` (value invariant).** The `rate != 0`
//!   branch divides by `p = (1 + rate)^nper`. That factor is `0` when
//!   `rate == -1` (with `nper > 0`), and can overflow to `±inf` for an absurd
//!   `nper`; either way the computed `pv` is non-finite and [`Value::number`]
//!   maps it to `#NUM!` — the same documented backstop the whole family uses for
//!   overflow (PV.md §Error behavior). This is the deterministic consequence of
//!   the closed form plus the frozen value invariant, not a separately probed
//!   PV-specific semantic (see PV.md §Degenerate inputs).

use xl_value::{Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `PV(rate, nper, pmt, [fv], [type])` call. See the module docs for
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
    let pmt = match to_number(&args.eval_scalar(2)) {
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

    // `type` is a beginning/end-of-period flag: any nonzero value collapses to 1
    // (beginning), a family property farm-confirmed for PMT (OXP-118) and the
    // sibling IPMT/PPMT probes (OXP-151/128), all RUN-2026-07-11-oracle01.
    let ty = if ty != 0.0 { 1.0 } else { 0.0 };

    let pv = if rate == 0.0 {
        // Straight-line: the present value is just the (negated) undiscounted sum
        // of the payments and the closing balance. No division, always exact.
        -(pmt * nper + fv)
    } else {
        // Ordinary f64::powf — no fast-math / FMA path.
        let p = (1.0 + rate).powf(nper);
        let annuity = pmt * (1.0 + rate * ty) * (p - 1.0) / rate;
        // Division by p = (1 + rate)^nper. When p == 0 (rate == -1, nper > 0) or
        // overflows, the result is non-finite → #NUM! via Value::number below.
        -(fv + annuity) / p
    };

    // A non-finite computed present value (overflow, or the rate == -1 zero
    // divisor) → #NUM! per the value invariant.
    Value::number(pv)
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

    // The published worked example is rounded to whole cents ("($59,777.15)"),
    // so it is checked at cent tolerance; the exact (rate == 0) and
    // formula-invariant relationships are checked far tighter.
    const CENT: f64 = 0.005;
    const TIGHT: f64 = 1e-9;
    const TOL: f64 = 1e-6;

    #[test]
    fn canonical_ms_example() {
        // MS Learn PV example: a $500/month, 20-year (240-month) annuity at 8%
        // annual → =PV(0.08/12, 12*20, 500) → ($59,777.15). A positive payment
        // received yields a negative present value (cash paid out now).
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.08 / 12.0)),
                Scalar(num(240.0)),
                Scalar(num(500.0)),
            ],
        ));
        assert!((got - (-59777.15)).abs() < CENT, "got {got}");
        assert!(got < 0.0, "positive payment → negative PV, got {got}");
    }

    #[test]
    fn rate_zero_is_exact_straight_line() {
        // rate == 0: pv = -(pmt*nper + fv), an exact rational — no powf.
        // PV(0, 10, 100) = -1000 exactly.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.0)), Scalar(num(10.0)), Scalar(num(100.0))],
            ),
            num(-1000.0)
        );
        // With a future value: PV(0, 10, 100, 50) = -(1000 + 50) = -1050 exactly.
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
        // For the payment stream (fv = 0) the annuity-due present value is the
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
        // `1 + rate*type` multiplier. So PV(...,type=2) must equal PV(...,type=1)
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
    fn pv_is_the_inverse_of_fv() {
        // PV and FV solve the same annuity identity for opposite unknowns, so
        // FV(rate, nper, pmt, PV(rate, nper, pmt, fv, type), type) == fv. This
        // cross-checks the two evaluators against each other (family consistency).
        let (rate, nper, pmt, fv, ty) = (0.05, 12.0, -250.0, 1000.0, 0.0);
        let pv = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(rate)),
                Scalar(num(nper)),
                Scalar(num(pmt)),
                Scalar(num(fv)),
                Scalar(num(ty)),
            ],
        ));
        let recovered = as_num(eval_direct(
            crate::func_fv::eval,
            vec![
                Scalar(num(rate)),
                Scalar(num(nper)),
                Scalar(num(pmt)),
                Scalar(num(pv)),
                Scalar(num(ty)),
            ],
        ));
        assert!(
            (recovered - fv).abs() < TOL,
            "recovered {recovered}, want {fv}"
        );
    }

    #[test]
    fn sign_flips_with_pmt_sign() {
        // A negative payment (cash paid out each period) produces a positive PV.
        let got = as_num(eval_direct(
            eval,
            vec![Scalar(num(0.05)), Scalar(num(12.0)), Scalar(num(-250.0))],
        ));
        assert!(got > 0.0, "negative payment → positive PV, got {got}");
    }

    #[test]
    fn numeric_text_and_logical_coerce() {
        // Arguments coerce through to_number: "100" → 100, TRUE → 1 for type.
        // PV(0, 10, "100") = -1000.
        let got = as_num(eval_direct(
            eval,
            vec![Scalar(num(0.0)), Scalar(num(10.0)), Scalar(txt("100"))],
        ));
        assert!((got - (-1000.0)).abs() < TIGHT, "got {got}");

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

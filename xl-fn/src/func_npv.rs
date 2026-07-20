//! `NPV` — net present value of a series of cash flows at a constant discount
//! rate.
//!
//! # Provenance
//! Behavior contract: `docs/specs/NPV.md` (which cites the Microsoft Learn NPV
//! function page). Cash-flow coercion is deferred to `xl-value`
//! ([`coerce_number_arg`] with the two [`CoercionMode`]s) exactly as
//! [`crate::func_sum`] does; the rate is coerced with [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - **Period-1 discounting (THE correctness point).** `NPV(rate, v1, v2, …)`
//!   = Σ_{i=1..n} `v_i / (1 + rate)^i`. The *first* cash flow `value1` is
//!   discounted by **one** period (`i = 1`), not zero — the MS page states
//!   "The NPV investment begins one period before the date of the value1 cash
//!   flow" and the values "occur at the end of each period" (NPV.md §1). So
//!   `NPV(0.1, 100) = 100 / 1.1`, never `100`.
//! - **Rate** (arg 0) coerces under scalar rules via [`to_number`] (bool→1/0,
//!   numeric text→number, blank→0); an error rate propagates (NPV.md §Rate).
//! - **Cash-flow series** (args 1..): an ordered stream. A *direct scalar*
//!   argument coerces under [`CoercionMode::Scalar`] (matching the SUM family).
//!   A *range/array* argument contributes only its numeric cells, **in order**;
//!   empty cells, text, and logical values inside a range/array are ignored and
//!   do **not** consume a period — the page states "only numbers in that array
//!   or reference are counted. Empty cells, logical values, text, or error
//!   values in the array or reference are ignored" (NPV.md §Values, §Coercion).
//!   The period index `i` advances by one per **included** numeric cash flow,
//!   in argument-then-row order.
//! - **`rate == -1`** makes `(1 + rate) = 0`, a division by zero. **OXP-124
//!   RESOLVED**: `NPV(-1, 100) = #DIV/0!` (observed on the Excel farm), so
//!   this returns `ErrorKind::Div0` directly (NPV.md §rate=-1) rather than
//!   falling through to `#NUM!` via the value invariant.
//! - **Error propagation.** An error in the rate or in any cash flow propagates
//!   (NPV.md §Error behavior). The MS Remarks literally say error values *in an
//!   array or reference* are "ignored", but **OXP-126 RESOLVED**
//!   (RUN-2026-07-11-oracle01): `NPV(0.1, 100, #N/A, 200) = #N/A` on the Excel
//!   farm — a cash-flow error **propagates**, matching the SUM-family contract
//!   and contradicting the literal doc wording.
//! - **Direct scalar logical/text.** **OXP-125 RESOLVED**
//!   (RUN-2026-07-11-oracle01): `NPV(0.1, TRUE, "5") = 5.0413223140495855` on
//!   the Excel farm (`= 1/1.1 + 5/1.1^2`), so direct logical/numeric-text cash
//!   flows **coerce** under the SUM-family scalar rule (`TRUE` → 1, `"5"` → 5)
//!   and are counted — the page's "…are ignored" Remark does not apply to
//!   direct scalar arguments.
//! - Overflow (a non-finite running total) becomes `#NUM!` via [`Value::number`]
//!   (NPV.md §Error behavior / value invariant).

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg, to_number};

use crate::args::{CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Evaluate an `NPV(rate, value1, [value2], …)` call. See the module docs for
/// the semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Arg 0 is the discount rate; coerce under scalar rules, propagate errors.
    let rate = match to_number(&args.eval_scalar(0)) {
        Ok(r) => r,
        Err(k) => return Value::Error(k),
    };

    // rate == -1 → (1 + rate) == 0 → division by zero. OXP-124 RESOLVED:
    // NPV(-1, 100) = #DIV/0! on the Excel farm. Guard here so the divisor is
    // always non-zero below.
    let base = 1.0 + rate;
    if base == 0.0 {
        return Value::Error(ErrorKind::Div0);
    }

    let mut acc = 0.0_f64;
    // Period index of the *next* included cash flow; the first counted number is
    // discounted by (1 + rate)^1, so pre-increment before each add.
    let mut period: i32 = 0;

    for i in 1..args.count() {
        // RFC 0010: a single-cell REFERENCE cash flow aggregates like a range —
        // text/logical/blank ignored and the period index NOT advanced for them
        // — while a scalar LITERAL cash flow coerces (SUM-family rule). The rate
        // (arg 0, above) is a plain scalar and is unaffected.
        match eff_shape(args, i) {
            EffShape::Omitted => {}
            EffShape::ScalarLiteral => {
                let v = args.eval_scalar_array_arg(i);
                match coerce_number_arg(&v, CoercionMode::Scalar) {
                    NumericArg::Number(n) => {
                        period += 1;
                        acc += n / base.powi(period);
                    }
                    NumericArg::Skip => {}
                    NumericArg::Error(k) => return Value::Error(k),
                }
            }
            EffShape::Aggregate => {
                let mut err: Option<ErrorKind> = None;
                let acc_ref = &mut acc;
                let period_ref = &mut period;
                args.for_each_cell(i, &mut |v| {
                    match coerce_number_arg(v, CoercionMode::RangeAggregate) {
                        NumericArg::Number(n) => {
                            *period_ref += 1;
                            *acc_ref += n / base.powi(*period_ref);
                            ControlFlow::Continue(())
                        }
                        NumericArg::Skip => ControlFlow::Continue(()),
                        // Stop the scan at the first error rather than visiting
                        // and ignoring the rest of the range. OXP-191 (RUN-
                        // 2026-07-13) CONFIRMS Excel PROPAGATES a range-cell
                        // error: NPV(0.1, {-10000,#N/A,4200,6800}) = #N/A (both
                        // the range form and the scalar-args form), overturning
                        // the MS-page "errors in the array are ignored" remark —
                        // so the OXP-126 scalar pin generalizes to range cells.
                        NumericArg::Error(k) => {
                            err = Some(k);
                            ControlFlow::Break(())
                        }
                    }
                });
                if let Some(k) = err {
                    return Value::Error(k);
                }
            }
        }
    }

    // Non-finite running total (overflow) → #NUM! per the value invariant.
    Value::number(acc)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    /// Pull the `f64` out of a numeric result (panics on anything else) so
    /// irrational results can be compared with a tolerance, not by exact bits.
    fn as_num(v: Value) -> f64 {
        match v {
            Value::Number(n) => n,
            other => panic!("expected a number, got {other:?}"),
        }
    }

    const TOL: f64 = 1e-6;

    #[test]
    fn canonical_ms_example() {
        // MS Learn: =NPV(0.1, -10000, 3000, 4200, 6800) → $1,188.44 (the initial
        // -10000 is value1, discounted one period).
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(num(-10000.0)),
                Scalar(num(3000.0)),
                Scalar(num(4200.0)),
                Scalar(num(6800.0)),
            ],
        ));
        assert!((got - 1188.4434123352207).abs() < TOL, "got {got}");
    }

    #[test]
    fn first_value_is_discounted_one_period() {
        // THE gotcha: value1 sits at the end of period 1, so it is divided by
        // (1 + rate)^1 — NOT left undiscounted.
        let got = as_num(eval_direct(
            eval,
            vec![Scalar(num(0.1)), Scalar(num(100.0))],
        ));
        assert!((got - (100.0 / 1.1)).abs() < TOL, "got {got}");
        // And emphatically not the raw cash flow.
        assert!((got - 100.0).abs() > 1.0, "must be discounted, got {got}");
    }

    #[test]
    fn two_values_sum_of_discounts() {
        // 100/1.1 + 200/1.1^2.
        let got = as_num(eval_direct(
            eval,
            vec![Scalar(num(0.1)), Scalar(num(100.0)), Scalar(num(200.0))],
        ));
        let want = 100.0 / 1.1 + 200.0 / 1.1_f64.powi(2);
        assert!((got - want).abs() < TOL, "got {got}, want {want}");
    }

    #[test]
    fn range_skips_text_blank_and_period_advances_only_on_numbers() {
        // Cash flows [100, "x", <blank>, TRUE, 200] in a range: only 100 and 200
        // count, so the result is 100/1.1 + 200/1.1^2 — the ignored cells do NOT
        // push 200 out to period 5.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Range(vec![
                    num(100.0),
                    txt("x"),
                    Value::Blank,
                    Value::Bool(true),
                    num(200.0),
                ]),
            ],
        ));
        let want = 100.0 / 1.1 + 200.0 / 1.1_f64.powi(2);
        assert!((got - want).abs() < TOL, "got {got}, want {want}");
    }

    #[test]
    fn period_index_spans_arguments_in_order() {
        // A scalar then a range: 100 is period 1, the range's 200 is period 2,
        // 300 is period 3 — the counter carries across arguments.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.05)),
                Scalar(num(100.0)),
                Range(vec![num(200.0), txt("skip"), num(300.0)]),
            ],
        ));
        let want = 100.0 / 1.05 + 200.0 / 1.05_f64.powi(2) + 300.0 / 1.05_f64.powi(3);
        assert!((got - want).abs() < TOL, "got {got}, want {want}");
    }

    #[test]
    fn scalar_coercion_text_and_logical() {
        // Direct scalars coerce (SUM-family rule): "100" → 100 at period 1,
        // TRUE → 1 at period 2.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(txt("100")),
                Scalar(Value::Bool(true)),
            ],
        ));
        let want = 100.0 / 1.1 + 1.0 / 1.1_f64.powi(2);
        assert!((got - want).abs() < TOL, "got {got}, want {want}");
    }

    #[test]
    fn oxp_125_direct_scalar_true_and_numeric_text() {
        // RUN-2026-07-11-oracle01 / OXP-125: =NPV(0.1, TRUE, "5") observed as
        // 5.0413223140495855. Direct logical/numeric-text scalars coerce under
        // the SUM-family rule (TRUE → 1 at period 1, "5" → 5 at period 2) and
        // are counted: 1/1.1 + 5/1.1^2.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(0.1)),
                Scalar(Value::Bool(true)),
                Scalar(txt("5")),
            ],
        ));
        assert!((got - 5.0413223140495855).abs() < TOL, "got {got}");
    }

    #[test]
    fn oxp_126_direct_scalar_error_propagates() {
        // RUN-2026-07-11-oracle01 / OXP-126: =NPV(0.1, 100, #N/A, 200) observed
        // as #N/A. A cash-flow error propagates (SUM-family contract), despite
        // the MS Remark that error values "are ignored".
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.1)),
                    Scalar(num(100.0)),
                    Scalar(Value::Error(ErrorKind::Na)),
                    Scalar(num(200.0)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn rate_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Div0)), Scalar(num(100.0))]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn cash_flow_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.1)),
                    Range(vec![num(100.0), Value::Error(ErrorKind::Value), num(200.0)])
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn rate_minus_one_is_div0() {
        // (1 + rate) == 0. OXP-124 RESOLVED: NPV(-1, 100) = #DIV/0!.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(-1.0)), Scalar(num(100.0)), Scalar(num(200.0))]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn no_numeric_cash_flows_is_zero() {
        // Nothing numeric participates → 0 (the empty discounted sum).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.1)), Range(vec![txt("a"), Value::Blank])]
            ),
            num(0.0)
        );
    }

    // RFC 0010: a single-cell *reference* cash flow holding text is IGNORED (the
    // range rule) and does NOT consume a period, so `NPV(0.1, text_ref, 100)` =
    // 100/1.1 (the 100 stays at period 1), instead of `#VALUE!`.
    #[test]
    fn rfc0010_cash_flow_text_reference_is_ignored() {
        let got = as_num(eval_direct(
            eval,
            vec![Scalar(num(0.1)), CellRef(txt("x")), Scalar(num(100.0))],
        ));
        assert!((got - (100.0 / 1.1)).abs() < TOL, "got {got}");
    }

    // RFC 0010: a scalar *literal* cash flow still coerces — non-numeric text
    // typed directly is `#VALUE!` (the sharp contrast with the ignored
    // reference above).
    #[test]
    fn rfc0010_scalar_literal_cash_flow_still_coerces() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.1)), Scalar(txt("x"))]),
            Value::Error(ErrorKind::Value)
        );
    }
}

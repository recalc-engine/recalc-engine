//! `IRR` — the internal rate of return of a series of periodic cash flows.
//!
//! # Provenance
//! Behavior contract: `docs/specs/IRR.md`. Farm-pinned target values:
//! **OXP-155**, run **RUN-2026-07-11-oracle01** (Excel farm). Cash-flow
//! coercion is deferred to `xl-value` ([`coerce_number_arg`] with the two
//! [`CoercionMode`]s) exactly as [`crate::func_npv`] does; the optional `guess`
//! is coerced with [`to_number`]. IRR is the rate `r` for which the (time-0
//! indexed) net present value of the cash flows is zero, so this module reuses
//! the NPV summation shape — but note the **index base differs** (see below).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - **Signature.** `IRR(values, [guess])` (registered arity `1..=2`). `values`
//!   is an ordered series of cash flows; `guess` is the starting rate for the
//!   iterative solver and **defaults to `0.1`** — confirmed by the farm:
//!   `IRR(A1:A5)` and `IRR(A1:A5, 0.1)` return the identical value (IRR.md
//!   §Signature, OXP-155).
//! - **What IRR solves (THE correctness point).** IRR finds `r` such that
//!   `NPV(r) = Σ_{i=0..n-1} values_i / (1 + r)^i = 0`. Unlike Excel's `NPV`
//!   worksheet function (where `value1` sits at the *end* of period 1 and is
//!   discounted once), IRR treats the **first** cash flow as occurring at
//!   **time 0** — undiscounted (`i = 0`, divisor `(1+r)^0 = 1`). This time-0
//!   convention is why `IRR({-100,30,40,50,60}) ≈ 0.2489` (IRR.md §1).
//! - **Sign-change requirement.** A valid IRR needs at least one **positive**
//!   and one **negative** cash flow; otherwise `NPV(r)` has no real root in
//!   `r > -1` and Excel returns `#NUM!`. The farm confirms `IRR({100,200,300})`
//!   (all positive) = `#NUM!` (IRR.md §Sign change, OXP-155). This precheck also
//!   short-circuits the all-positive series before Newton would diverge to
//!   `+∞`.
//! - **Iterative solver (Newton-Raphson).** Starting from `guess`, iterate
//!   `r ← r − NPV(r)/NPV'(r)` where
//!   `NPV'(r) = −Σ_{i=1..n-1} i·values_i / (1 + r)^(i+1)`. Excel documents an
//!   iterative technique that "cycles through the calculation … until the result
//!   is accurate within 0.00001 percent", giving up after **20** tries with
//!   `#NUM!`. We mirror the 20-iteration cap ([`MAX_ITER`]) and converge when the
//!   **rate step** `|Δr| = |next − r|` falls below [`STEP_TOLERANCE`] — a
//!   **scale-invariant** criterion (see next bullet), returning the freshly
//!   stepped iterate. All OXP-155 targets converge in ≤ 8 iterations
//!   (IRR.md §Solver).
//! - **Scale invariance (THE fix — OXP-155 review C1/C2).** IRR is mathematically
//!   scale-invariant: multiplying every cash flow by a constant `k` leaves the
//!   rate unchanged, because `NPV(r)` and `NPV'(r)` both scale by `k`, so the
//!   Newton step `NPV/NPV'` — and hence *every* iterate — is identical. The
//!   convergence *test* must share that property. The earlier absolute
//!   `|NPV(r)| < 1e-11` rule did **not**: for ordinary-dollar magnitudes like
//!   `{-100000,30000,40000,50000,60000}` the fixed-point NPV noise floor (~1.1e-11)
//!   exceeds `1e-11`, so the solver exhausted all 20 iterations and wrongly
//!   returned `#NUM!` — even though the rate is the same `0.2489…` as the `×1`
//!   farm vector `{-100,30,40,50,60}` (larger scales 1e4/1e6/1e9 fail worse). This
//!   silently broke IRR on most real-magnitude corpus cells. Testing the **rate
//!   step** [`STEP_TOLERANCE`] instead — exactly as the sibling
//!   [`crate::func_xirr`] already does — fixes it: the stopping rule depends only
//!   on the rate, which is invariant under cash-flow scaling. The
//!   [`MAX_ITER`]-exhaustion fallback uses a *normalized* residual
//!   `|NPV(r)| / Σ|flowᵢ|` ([`RESIDUAL_TOLERANCE`]), scale-invariant for the same
//!   numerator/denominator-scale-together reason.
//! - **Guess-dependent root selection (bug-for-bug).** A cash-flow polynomial
//!   may have several real roots; Newton returns whichever root's basin contains
//!   `guess`. This is **not** forced to a canonical root — it falls out of the
//!   iteration. The farm pins the classic two-root case `{-1,5,-6}` (roots at
//!   `r=1` and `r=2`): the default `guess=0.1` finds `r≈1`, while `guess=1.5`
//!   finds `r≈2` (IRR.md §Multiple roots, OXP-155).
//! - **`values` coercion.** A range/array contributes only its **numeric** cells,
//!   **in order** (row-major); blank, text, and logical cells are skipped and do
//!   not occupy a period — the same `CoercionMode::RangeAggregate` rule
//!   [`crate::func_npv`] applies to its cash-flow ranges. A direct scalar
//!   `values` coerces under `CoercionMode::Scalar` (SUM-family), though a lone
//!   scalar can never satisfy the sign-change requirement.
//! - **Error in `values` → `#VALUE!` (OXP-192/194, RUN-2026-07-13).** An error
//!   cell in the cash-flow **range** yields `#VALUE!`, NOT the cell's own error:
//!   Excel numeric-validates the whole array, so `#N/A`, `#DIV/0!`, and `#REF!`
//!   all map to `#VALUE!` (the range scan stops at the first error). This is the
//!   IRR-specific divergence from NPV, which *propagates* the specific error
//!   kind (OXP-191). **Unpinned edges** (OXP probed multi-cell cell-ranges only,
//!   so these keep the cell's own error kind, not `#VALUE!`): a direct scalar /
//!   single-cell / 1×1-range `values` argument (see `eval`'s `Scalar` arm), an
//!   error inside an **array-constant literal** `values` (`=IRR({-100,#N/A,200})`
//!   — reachable, rides the range pin), and an error **`guess`** (propagates its
//!   own kind before any iteration).
//! - **Non-convergence.** If the solver blows up (a non-finite `NPV`/`NPV'`, a
//!   zero derivative, or exhausting [`MAX_ITER`] without the rate step reaching
//!   [`STEP_TOLERANCE`] and without the final iterate passing the scale-invariant
//!   normalized-residual check [`RESIDUAL_TOLERANCE`]), the result is `#NUM!`
//!   (IRR.md §Non-convergence).
//!
//! # Array-position arguments (M2 lane 6 follow-up, 2026-09-04)
//! An argument in a range/array position is evaluated under the consumed-array
//! gate (RFC-0011; `docs/plans/2026-07-14-consumed-array-eval-spec.md` §2).
//! A materialized multi-cell array reaching this function is **refused** with a
//! loud `#UNSUPPORTED!` plus an engine diagnostic (spec §4, born-refusing
//! boundary): only the SUM/SUMPRODUCT consumers are oracle-pinned (OXP-201), and
//! the legacy alternative — a silent, host-row-dependent implicit intersection —
//! is a "never silently wrong" violation. Plain ranges are unchanged.

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg, to_number};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Default starting rate when `guess` is omitted. Farm-confirmed (OXP-155):
/// `IRR(values)` == `IRR(values, 0.1)`.
const DEFAULT_GUESS: f64 = 0.1;

/// Maximum Newton iterations before giving up with `#NUM!`. Mirrors Excel's
/// documented "20 tries" cap for the IRR iterative solver.
const MAX_ITER: u32 = 20;

/// Convergence threshold on the Newton **step** `|Δr| = |next − r|`, relative to
/// the current rate magnitude (`STEP_TOLERANCE · (1 + |r|)`). Testing the *rate
/// step* rather than the *absolute NPV residual* makes convergence
/// **scale-invariant** — IRR is unchanged when every cash flow is multiplied by a
/// constant, so the stopping rule must not depend on the cash-flow magnitude
/// either (see the module docs' scale-invariance bullet for the bug the old
/// absolute `|NPV(r)| < 1e-11` rule caused on real-dollar vectors). This mirrors
/// the sibling [`crate::func_xirr`]'s `STEP_TOLERANCE` exactly. Newton converges
/// quadratically here, so the bound is reached in a handful of iterations and the
/// returned `next` iterate sits essentially on the true root — every OXP-155
/// target is reproduced to ≤ 2.8e-12 (Excel's own guess-to-guess iteration
/// artifact is ~2.7e-12). Tighter than Excel's documented ~1e-7; tightening a
/// convergence bound only improves fidelity (OXP-155, review C1/C2).
const STEP_TOLERANCE: f64 = 1e-12;

/// Normalized residual bound accepted only if the step tolerance was not reached
/// within [`MAX_ITER`]: the final iterate is a genuine root iff
/// `|NPV(r)| / Σ|flowᵢ| < RESIDUAL_TOLERANCE`. Dividing by the sum of absolute
/// cash flows keeps this fallback **scale-invariant** too (numerator and
/// denominator scale together). It only guards against reporting a non-root as a
/// solution; a real IRR converges via the step test far inside [`MAX_ITER`].
const RESIDUAL_TOLERANCE: f64 = 1e-10;

/// The time-0-indexed net present value and its derivative at `rate` for the
/// ordered `flows`:
/// - `NPV(r)  = Σ_{i=0..n-1} flows_i / (1 + r)^i`
/// - `NPV'(r) = −Σ_{i=1..n-1} i · flows_i / (1 + r)^(i+1)`
///
/// The `i = 0` cash flow is undiscounted (divisor `(1+r)^0 = 1`) and contributes
/// nothing to the derivative. Integer exponents use ordinary `f64::powi` (no
/// fast-math / FMA), in a stable left-to-right traversal order. If `rate == -1`
/// the divisors are zero and the sums become non-finite; the caller treats a
/// non-finite `NPV`/`NPV'` as non-convergence (`#NUM!`).
fn npv_and_deriv(flows: &[f64], rate: f64) -> (f64, f64) {
    let base = 1.0 + rate;
    let mut npv = 0.0_f64;
    let mut dnpv = 0.0_f64;
    for (i, &v) in flows.iter().enumerate() {
        // (1 + rate)^i; for i == 0 this is 1.0 for any base (incl. 0.0).
        let inv = base.powi(i as i32);
        npv += v / inv;
        if i > 0 {
            // derivative term: −i · v / (1 + rate)^(i+1) = −(i · v) / (inv · base).
            dnpv -= (i as f64) * v / (inv * base);
        }
    }
    (npv, dnpv)
}

/// Evaluate an `IRR(values, [guess])` call. See the module docs for the
/// semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Collect the cash flows from `values` (arg 0), in order. A range/array
    // yields only numeric cells (blank/text/logical skipped); a direct scalar
    // coerces under the SUM-family scalar rule. An error cell propagates.
    let mut flows: Vec<f64> = Vec::new();
    match args.shape(0) {
        // No `values` at all → no cash flows → #NUM! via the sign-change check.
        ArgShape::Omitted => {}
        // Array position: evaluate under the array-context gate, so an operator
        // expression over a range materializes (and the scalar coercion refuses
        // it loudly — unpinned for IRR) instead of being implicit-intersected.
        ArgShape::Scalar => {
            match coerce_number_arg(&args.eval_scalar_array_arg(0), CoercionMode::Scalar) {
                NumericArg::Number(n) => flows.push(n),
                NumericArg::Skip => {}
                // OXP (unassigned): a direct single-cell / 1×1-range `values` error
                // keeps its own kind here, UNPINNED — OXP-192/194 mapped errors to
                // `#VALUE!` only for multi-cell ranges. A lone `values` can never
                // satisfy the sign-change requirement anyway (→ `#NUM!`), so this is
                // a degenerate corner; probe `=IRR(A1)` with A1=`#N/A` before
                // aligning it with the range rule.
                NumericArg::Error(k) => return Value::Error(k),
            }
        }
        ArgShape::Range | ArgShape::Array => {
            let mut range_err: Option<ErrorKind> = None;
            let flows_ref = &mut flows;
            let err_ref = &mut range_err;
            args.for_each_cell(0, &mut |v| {
                match coerce_number_arg(v, CoercionMode::RangeAggregate) {
                    NumericArg::Number(n) => {
                        flows_ref.push(n);
                        ControlFlow::Continue(())
                    }
                    NumericArg::Skip => ControlFlow::Continue(()),
                    // OXP-192/194 (RUN-2026-07-13): Excel maps ANY error cell in
                    // the values array to #VALUE! — it numeric-validates the
                    // whole array, so #N/A, #DIV/0!, and #REF! all yield #VALUE!,
                    // NOT the cell's own error. (Contrast NPV, which propagates
                    // the specific error kind — OXP-191.) Stop at the first error.
                    NumericArg::Error(_k) => {
                        *err_ref = Some(ErrorKind::Value);
                        ControlFlow::Break(())
                    }
                }
            });
            if let Some(k) = range_err {
                return Value::Error(k);
            }
        }
    }

    // `guess` (arg 1) defaults to 0.1 when omitted; otherwise coerce, propagating
    // an error. An omitted position reads as ArgShape::Omitted (not Blank→0), so
    // the 0.1 default is honored rather than collapsing to a 0 guess.
    let guess = match args.shape(1) {
        ArgShape::Omitted => DEFAULT_GUESS,
        _ => match to_number(&args.eval_scalar(1)) {
            Ok(g) => g,
            Err(k) => return Value::Error(k),
        },
    };

    // A real IRR requires at least one positive and one negative cash flow;
    // otherwise NPV(r) has no root in r > -1 and Excel returns #NUM! (OXP-155:
    // all-positive {100,200,300} = #NUM!). This also guards Newton from
    // diverging to +∞ on an all-positive series. Zeros count as neither sign.
    let has_pos = flows.iter().any(|&v| v > 0.0);
    let has_neg = flows.iter().any(|&v| v < 0.0);
    if !(has_pos && has_neg) {
        return Value::Error(ErrorKind::Num);
    }

    // Newton-Raphson from `guess`. Convergence is tested on the **rate step**
    // |next − rate| (scale-invariant), and the freshly stepped `next` iterate is
    // returned; the root basin (hence which root of a multi-root series is found)
    // is fixed by `guess`, unchanged by the stopping rule.
    let mut rate = guess;
    for _ in 0..MAX_ITER {
        let (npv, dnpv) = npv_and_deriv(&flows, rate);
        // A non-finite NPV (e.g. the rate wandered to the r = -1 pole) or a zero /
        // non-finite derivative means Newton cannot take a step → #NUM!.
        if !npv.is_finite() || dnpv == 0.0 || !dnpv.is_finite() {
            return Value::Error(ErrorKind::Num);
        }
        let next = rate - npv / dnpv;
        if !next.is_finite() {
            return Value::Error(ErrorKind::Num);
        }
        // Scale-invariant convergence: the step is negligible relative to the rate
        // magnitude. Return the stepped iterate (quadratic convergence puts it
        // essentially on the true root).
        if (next - rate).abs() <= STEP_TOLERANCE * (1.0 + rate.abs()) {
            return Value::number(next);
        }
        rate = next;
    }

    // Exhausted the iteration cap without the step converging. Accept only if the
    // final rate is a genuine root by the scale-invariant *normalized* residual
    // |NPV(r)| / Σ|flowᵢ|; otherwise report non-convergence. (Σ|flowᵢ| > 0 is
    // guaranteed — the sign-change check above required a non-zero flow.)
    let (npv, _) = npv_and_deriv(&flows, rate);
    let sum_abs: f64 = flows.iter().map(|v| v.abs()).sum();
    if npv.is_finite() && sum_abs > 0.0 && npv.abs() <= RESIDUAL_TOLERANCE * sum_abs {
        Value::number(rate)
    } else {
        Value::Error(ErrorKind::Num)
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    /// Pull the `f64` out of a numeric result (panics on anything else) so the
    /// iterative IRR results can be compared with a tolerance, not by bits.
    fn as_num(v: Value) -> f64 {
        match v {
            Value::Number(n) => n,
            other => panic!("expected a number, got {other:?}"),
        }
    }

    /// Unit-test assertion bound. The *declared corpus* tolerance stays `1e-7`
    /// (Excel's documented IRR convergence accuracy — see `docs/specs/IRR.md` and
    /// the PROPOSED `TOLERANCES.md` row); these unit tests assert the far tighter
    /// **`1e-10`** actually achieved (every farm target is reproduced to ≤ 2.8e-12).
    /// Tightening a test bound is always allowed and closes the silent-regression
    /// gap the old `1e-7` bound left open (OXP-155, review C2).
    const TOL: f64 = 1e-10;

    // Farm cash-flow vectors (OXP-155, RUN-2026-07-11-oracle01).
    fn vec_a() -> Vec<Value> {
        vec![num(-100.0), num(30.0), num(40.0), num(50.0), num(60.0)]
    }
    fn vec_b() -> Vec<Value> {
        vec![
            num(-1000.0),
            num(500.0),
            num(400.0),
            num(300.0),
            num(200.0),
            num(100.0),
        ]
    }
    fn vec_d() -> Vec<Value> {
        vec![num(-1.0), num(5.0), num(-6.0)]
    }

    #[test]
    fn farm_a_default_guess() {
        // OXP-155: IRR(A1:A5) → 0.24888335662133043 (default guess 0.1).
        let got = as_num(eval_direct(eval, vec![Range(vec_a())]));
        assert!((got - 0.24888335662133043).abs() < TOL, "got {got}");
    }

    #[test]
    fn farm_a_explicit_guess_matches_default() {
        // OXP-155: IRR(A1:A5, 0.1) → 0.24888335662133043 — the default guess IS
        // 0.1, so the explicit and omitted forms return the identical value.
        let default = eval_direct(eval, vec![Range(vec_a())]);
        let explicit = eval_direct(eval, vec![Range(vec_a()), Scalar(num(0.1))]);
        assert_eq!(default, explicit, "omitted guess must equal explicit 0.1");
        assert!((as_num(explicit) - 0.24888335662133043).abs() < TOL);
    }

    #[test]
    fn farm_a_guess_half_same_root() {
        // OXP-155: IRR(A1:A5, 0.5) → 0.24888335662407135. A different guess
        // converges to a very slightly different point on the SAME root (~0.2489).
        let got = as_num(eval_direct(eval, vec![Range(vec_a()), Scalar(num(0.5))]));
        assert!((got - 0.24888335662407135).abs() < TOL, "got {got}");
        // Same root as the default-guess call, to the declared tolerance.
        let default = as_num(eval_direct(eval, vec![Range(vec_a())]));
        assert!(
            (got - default).abs() < TOL,
            "guess 0.5 must find the same root"
        );
    }

    #[test]
    fn farm_b() {
        // OXP-155: IRR(B1:B6) → 0.20271969394349076.
        let got = as_num(eval_direct(eval, vec![Range(vec_b())]));
        assert!((got - 0.20271969394349076).abs() < TOL, "got {got}");
    }

    #[test]
    fn scale_invariance_real_magnitude_cashflows() {
        // OXP-155 review C1 — REGRESSION GUARD for the shipped bug: IRR is
        // mathematically scale-invariant, so multiplying every cash flow by a
        // constant must NOT change the rate. The superseded absolute
        // `|NPV(r)| < 1e-11` criterion returned #NUM! on ordinary dollar
        // magnitudes (their NPV noise floor exceeds 1e-11); the scale-invariant
        // step criterion returns the SAME rate as the ×1 farm vector at every
        // scale. `IRR({-100,30,40,50,60})` == the OXP-155 A target.
        const UNIT_RATE: f64 = 0.24888335662133043; // OXP-155 A (default guess)
        let unit = as_num(eval_direct(eval, vec![Range(vec_a())]));
        assert!((unit - UNIT_RATE).abs() < 1e-9, "unit rate {unit}");
        // ×1e3 is the exact bug repro {-100000,30000,40000,50000,60000}; ×1e6 and
        // ×1e9 fail worse under the old absolute rule. All must match to ≤ 1e-9.
        for &k in &[1e3_f64, 1e6, 1e9] {
            let scaled = vec![
                num(-100.0 * k),
                num(30.0 * k),
                num(40.0 * k),
                num(50.0 * k),
                num(60.0 * k),
            ];
            let got = as_num(eval_direct(eval, vec![Range(scaled)]));
            assert!(
                (got - UNIT_RATE).abs() < 1e-9,
                "scale {k}: got {got}, want {UNIT_RATE} (scale-invariance)"
            );
            assert!(
                (got - unit).abs() < 1e-9,
                "scale {k}: got {got} must equal the ×1 rate {unit}"
            );
        }
    }

    #[test]
    fn farm_c_all_positive_is_num() {
        // OXP-155: IRR(C1:C3) with {100,200,300} (no sign change) → #NUM!.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(100.0), num(200.0), num(300.0)])]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn farm_d_default_guess_finds_root_one() {
        // OXP-155: IRR({-1,5,-6}) with the default guess 0.1 finds the r=1 root →
        // 0.99999999999996 (Excel's near-1 iterate).
        let got = as_num(eval_direct(eval, vec![Range(vec_d())]));
        assert!((got - 0.99999999999996).abs() < TOL, "got {got}");
        // Emphatically the r≈1 basin, not the r≈2 root.
        assert!(
            (got - 1.0).abs() < 1e-6,
            "must land on the r=1 root, got {got}"
        );
    }

    #[test]
    fn farm_d_guess_1_5_finds_root_two() {
        // OXP-155: IRR({-1,5,-6}, 1.5) finds the OTHER root at r=2 →
        // 1.9999999999999987. The guess selects the root basin (bug-for-bug);
        // the root is NOT canonicalized.
        let got = as_num(eval_direct(eval, vec![Range(vec_d()), Scalar(num(1.5))]));
        assert!((got - 1.9999999999999987).abs() < TOL, "got {got}");
        assert!(
            (got - 2.0).abs() < 1e-6,
            "must land on the r=2 root, got {got}"
        );
    }

    #[test]
    fn guess_selects_distinct_roots() {
        // The two farm D-vector calls must return genuinely different roots — the
        // guess-dependent root selection, not a fixed canonical answer.
        let r1 = as_num(eval_direct(eval, vec![Range(vec_d())]));
        let r2 = as_num(eval_direct(eval, vec![Range(vec_d()), Scalar(num(1.5))]));
        assert!(
            (r2 - r1).abs() > 0.5,
            "guess must pick different roots: {r1} vs {r2}"
        );
    }

    #[test]
    fn range_skips_text_blank_logical_and_preserves_order() {
        // The A cash flows interleaved with text/blank/logical cells: only the
        // numbers count, in order, so the result equals IRR(A). Confirms both
        // range coercion (skip non-numbers) and order preservation.
        let interleaved = vec![
            num(-100.0),
            txt("note"),
            num(30.0),
            Value::Blank,
            num(40.0),
            Value::Bool(true),
            num(50.0),
            num(60.0),
        ];
        let got = as_num(eval_direct(eval, vec![Range(interleaved)]));
        let plain = as_num(eval_direct(eval, vec![Range(vec_a())]));
        assert!((got - plain).abs() < TOL, "got {got}, want {plain}");
        assert!((got - 0.24888335662133043).abs() < TOL, "got {got}");
    }

    #[test]
    fn all_negative_is_num() {
        // No positive cash flow → no real IRR → #NUM! (sign-change requirement).
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(-100.0), num(-30.0), num(-40.0)])]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn single_value_is_num() {
        // A lone cash flow cannot have both signs → #NUM!.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(-100.0)])]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn empty_values_is_num() {
        // A range with no numeric cells → no cash flows → #NUM!.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![txt("a"), Value::Blank])]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn error_in_values_maps_to_value_oxp192_194() {
        // OXP-192/194 (RUN-2026-07-13): an error cell in the cash-flow RANGE
        // yields #VALUE! — Excel numeric-validates the array, so ANY error kind
        // (#N/A, #DIV/0!, #REF!) maps to #VALUE!, not the cell's own error.
        for kind in [ErrorKind::Div0, ErrorKind::Na, ErrorKind::Ref] {
            assert_eq!(
                eval_direct(
                    eval,
                    vec![Range(vec![num(-100.0), Value::Error(kind), num(200.0)])]
                ),
                Value::Error(ErrorKind::Value),
                "error {kind:?} in values must map to #VALUE!"
            );
        }
    }

    #[test]
    fn error_guess_propagates() {
        // An error `guess` propagates before any iteration.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec_a()), Scalar(Value::Error(ErrorKind::Na))]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn numeric_text_guess_coerces() {
        // A numeric-text guess coerces (to_number): "0.5" behaves as 0.5 and
        // reproduces the guess-0.5 farm target.
        let got = as_num(eval_direct(eval, vec![Range(vec_a()), Scalar(txt("0.5"))]));
        assert!((got - 0.24888335662407135).abs() < TOL, "got {got}");
    }

    #[test]
    fn array_values_supported() {
        // `values` as an array constant behaves like a range of the same cells.
        let got = as_num(eval_direct(eval, vec![Array(vec_a())]));
        assert!((got - 0.24888335662133043).abs() < TOL, "got {got}");
    }
}

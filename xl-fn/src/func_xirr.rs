//! `XIRR` — internal rate of return for a schedule of cash flows on **specific
//! dates** (the rate that makes [`XNPV`](crate::func_xnpv) equal zero).
//!
//! # Provenance
//! Behavior contract: `docs/specs/XIRR.md`, which cites the public Microsoft
//! Learn XIRR page
//! (<https://support.microsoft.com/office/xirr-function-de1242ec-6477-445b-b11b-a303ad9adc9d>).
//! Farm-pinned numerics: `RUN-2026-07-11-oracle01`, experiment **OXP-157**
//! (`tools/oracle/out/results/OXP-157.*.sidecar.json`). The XNPV kernel and the
//! cash-flow/date collection are reused verbatim from
//! [`crate::func_xnpv`]; numeric coercion is `xl-value`'s frozen [`to_number`].
//! Clean-room from the Microsoft page and the oracle only.
//!
//! # Definition & solver
//! `XIRR(values, dates, [guess])` returns the `rate` solving
//! `Σ value_i / (1 + rate)^((date_i − date_0)/365) = 0` (the same fixed-365
//! day-count and first-date base as XNPV). It is found by **Newton–Raphson**
//! from `guess` (default `0.1`), using [`crate::func_xnpv::xnpv_sum`] as the
//! objective and [`crate::func_xnpv::xnpv_deriv`] as its analytic derivative,
//! for at most [`MAX_ITER`] steps.
//!
//! # Semantics implemented (pinned vs inferred)
//! - **Default `guess = 0.1`** when the third argument is omitted (Microsoft
//!   page). A supplied `guess` coerces via [`to_number`].
//! - **Sign-change requirement.** The `values` must contain at least one
//!   positive and one negative amount, else `#NUM!` (Microsoft page: "If … the
//!   numbers … do not contain at least one positive and one negative value,
//!   XIRR returns the `#NUM!` error value").
//! - **Length / date-order / coercion** are exactly XNPV's (shared
//!   [`collect_pairs`](crate::func_xnpv::collect_pairs)): unequal counts →
//!   `#NUM!`; a date before the first (starting) date → `#NUM!`; each value via
//!   [`to_number`], each date truncated toward zero to a serial.
//! - **Non-convergence** (no root found within [`MAX_ITER`], a non-finite
//!   iterate, or a degenerate derivative) → `#NUM!` (Microsoft page: "If XIRR
//!   can't find a result that works after 100 tries, the `#NUM!` error value is
//!   returned").
//!
//! # Guess-dependent convergence (FARM-PINNED, OXP-157)
//! The pinned farm values are guess-dependent by ~1.5e-9:
//! `XIRR(A, B)` and `XIRR(A, B, 0.1)` → `0.3733625352382659`, while
//! `XIRR(A, B, 0.5)` → `0.37336253374814987`. Both are within Excel's own
//! convergence tolerance of the *same* root; Excel simply stops at slightly
//! different final iterates depending on the starting `guess`. This solver
//! converges tightly to that root (to ~1e-12), landing within **1e-7** of each
//! pinned value — the accuracy bar this function is asserted and (pending human
//! sign-off) tolerated at. See the PROPOSED TOLERANCE note in
//! `docs/specs/XIRR.md`; XIRR does **not** bit-reproduce Excel's per-guess
//! stopping iterate.
//!
//! # Multi-root schedules DEFER LOUDLY (OXP-168 — ratified checkpoint)
//! When a schedule has **multiple** real roots, Excel's XIRR is not a clean
//! root-finder: the start `guess` selects which root — or which *non-root
//! artifact* — it lands on, and that behavior is not pinned beyond single data
//! points. `RUN-2026-07-11-oracle01` experiment **OXP-168** probed
//! `values = {-1, 5, -6}`, `dates = {43831, 44196, 44561}` (fixed-365 exponents
//! `0,1,2`), whose XNPV has two genuine roots, `r = 1` and `r = 2`:
//! - **High guess** (`1.5`, `1.9`, `2.5`) → Excel converges to `r ≈ 2`
//!   (pinned `1.9999999925494194` for `1.5`).
//! - **Low guess** (default / `0.1`) → Excel returns `≈ 2.98e-9` (`~0`), which is
//!   **not a root** (XNPV there is `-2`) — an artifact of Excel's own iteration
//!   bailing out near `0`.
//!
//! An earlier revision returned the genuine root `r = 1` for the low-guess case
//! (explicit, but a *silent* divergence from Excel's pinned output — invisible to a
//! conformance run, which corrupts the fidelity measurement itself). The
//! **ratified decision (2026-07-13, option (c))** is to **defer loudly**:
//! detect the multi-root risk with **Descartes' rule of signs** (more than one
//! sign change in the date-ordered cash flows ⇒ possible multiple positive
//! roots) and return `#UNSUPPORTED!` rather than any root. Reproducing Excel's
//! guess-dependent landing (including the non-root `~0` artifact) would require
//! reverse-engineering its broken iteration/stopping, which OXP-168 does not pin
//! beyond single outputs; guessing it is forbidden (Principle 2). The guard is a
//! deterministic O(n) over-approximation — a **single** sign change ⇒ a unique
//! positive root ⇒ the solver runs and keeps matching Excel (the dominant
//! real-world shape). Coverage given up (multi-root schedules we sometimes
//! matched, e.g. the high-guess `r ≈ 2` cases) buys a trustworthy fidelity
//! number; narrowing the guard back needs a probe **grid** (many schedules ×
//! guesses), not a guess — file a queued OXP if multi-root XIRR shows up in the
//! corpus at measurable frequency. See [`sign_changes_by_date`] and the
//! `oxp168_multi_root_defers_loudly` test.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;
use crate::func_xnpv::{collect_pairs, xnpv_deriv, xnpv_sum};

/// Default `guess` when the third argument is omitted (Microsoft XIRR page).
const DEFAULT_GUESS: f64 = 0.1;

/// Maximum Newton–Raphson iterations before declaring non-convergence
/// (Microsoft page: "after 100 tries … `#NUM!`").
const MAX_ITER: usize = 100;

/// Convergence threshold on the Newton step, relative to the current rate
/// magnitude. Newton converges quadratically here, so this is reached in a
/// handful of steps and pins the root to ~1e-12 — far tighter than the 1e-7
/// accuracy the pinned values are compared at.
const STEP_TOLERANCE: f64 = 1e-12;

/// Residual bound accepted if the step tolerance was not hit within
/// [`MAX_ITER`] (guards against reporting a non-root as a solution).
const RESIDUAL_TOLERANCE: f64 = 1e-6;

/// Evaluate an `XIRR(values, dates, [guess])` call. See the module docs for the
/// solver and the guess-dependent convergence note.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Args 0 (values) and 1 (dates): shared XNPV collection + validation.
    let cashflows = match collect_pairs(args, 0, 1) {
        Ok(p) => p,
        Err(k) => return Value::Error(k),
    };

    // XIRR needs a sign change to have a real root (Microsoft page).
    let has_positive = cashflows.iter().any(|&(v, _)| v > 0.0);
    let has_negative = cashflows.iter().any(|&(v, _)| v < 0.0);
    if !(has_positive && has_negative) {
        return Value::Error(ErrorKind::Num);
    }

    // Multi-root guard — ratified XIRR checkpoint (2026-07-13, OXP-168).
    // Descartes' rule of signs: the XNPV equation in `x = 1/(1+r)` has as many
    // positive roots as the sign of its date-ordered coefficients changes,
    // possibly fewer by an even number. MORE THAN ONE sign change ⇒ the basin
    // may hold multiple real roots, and OXP-168 pinned that Excel's own
    // iteration can land on a NON-root artifact (`2.98e-9`, where XNPV = -2)
    // that we cannot reproduce without reverse-engineering its broken stopping
    // — a forbidden guess (Principle 2). Rather than silently return a
    // *different* root than Excel, defer LOUDLY. This is a deterministic O(n)
    // over-approximation: a single sign change ⇒ a unique positive root ⇒ the
    // solver runs and keeps matching Excel (the overwhelming real-world shape).
    // Coverage given up (multi-root schedules we sometimes matched) buys a
    // trustworthy fidelity number; narrowing it back needs a probe grid, not a
    // guess. See the module "Guess-dependent ROOT SELECTION" note.
    if sign_changes_by_date(&cashflows) > 1 {
        return Value::Error(ErrorKind::Unsupported);
    }

    // Arg 2 is the optional guess (default 0.1 when omitted).
    let guess = match args.shape(2) {
        ArgShape::Omitted => DEFAULT_GUESS,
        _ => match to_number(&args.eval_scalar(2)) {
            Ok(g) => g,
            Err(k) => return Value::Error(k),
        },
    };

    let base_date = cashflows[0].1;
    match solve(guess, &cashflows, base_date) {
        Some(rate) => Value::number(rate),
        None => Value::Error(ErrorKind::Num),
    }
}

/// Count sign changes in the **date-ordered** cash-flow values (zeros skipped).
/// This is the sign-change count of the XNPV polynomial's coefficients, so by
/// Descartes' rule of signs it upper-bounds the number of positive real roots.
/// `> 1` ⇒ the schedule may be multi-root and XIRR defers loudly (see `eval`).
fn sign_changes_by_date(cashflows: &[(f64, i64)]) -> usize {
    let mut ordered: Vec<(f64, i64)> = cashflows.to_vec();
    ordered.sort_by_key(|&(_, d)| d);
    let mut changes = 0usize;
    let mut last_positive: Option<bool> = None;
    for &(v, _) in &ordered {
        if v == 0.0 {
            continue;
        }
        let positive = v > 0.0;
        if last_positive.is_some_and(|prev| prev != positive) {
            changes += 1;
        }
        last_positive = Some(positive);
    }
    changes
}

/// Newton–Raphson root-find of [`xnpv_sum`] from `guess`, returning the
/// converged `rate` or `None` (→ `#NUM!`) if it does not settle on a root within
/// [`MAX_ITER`] steps.
fn solve(guess: f64, cashflows: &[(f64, i64)], base_date: i64) -> Option<f64> {
    let mut rate = guess;
    for _ in 0..MAX_ITER {
        let f = xnpv_sum(rate, cashflows, base_date);
        let df = xnpv_deriv(rate, cashflows, base_date);
        // A zero/NaN/inf derivative makes the quotient non-finite, caught below.
        let next = rate - f / df;
        if !next.is_finite() {
            return None;
        }
        if (next - rate).abs() <= STEP_TOLERANCE * (1.0 + rate.abs()) {
            return Some(next);
        }
        rate = next;
    }
    // Did not hit the step tolerance in MAX_ITER; accept only a genuine root.
    if xnpv_sum(rate, cashflows, base_date).abs() <= RESIDUAL_TOLERANCE {
        Some(rate)
    } else {
        None
    }
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

    /// The farm cash-flow schedule (RUN-2026-07-11-oracle01, OXP-157):
    /// A = {-10000, 2750, 4250, 3250, 2750}, B = {39448, 39508, 39751, 39859, 39904}.
    fn values() -> Vec<Value> {
        vec![
            num(-10000.0),
            num(2750.0),
            num(4250.0),
            num(3250.0),
            num(2750.0),
        ]
    }
    fn dates() -> Vec<Value> {
        vec![
            num(39448.0),
            num(39508.0),
            num(39751.0),
            num(39859.0),
            num(39904.0),
        ]
    }

    /// XIRR is iterative: the pinned values are matched within 1e-7 (Excel's
    /// convergence). See the PROPOSED TOLERANCE note in `docs/specs/XIRR.md`
    /// (human checkpoint — not yet an approved TOLERANCES.md row).
    const XIRR_TOL: f64 = 1e-7;

    // ---- FARM-PINNED targets (RUN-2026-07-11-oracle01, OXP-157) -------------

    #[test]
    fn xirr_default_guess_matches_farm() {
        // =XIRR(A1:A5, B1:B5) -> 0.3733625352382659 (default guess 0.1)
        let got = as_num(eval_direct(eval, vec![Range(values()), Range(dates())]));
        assert!(
            (got - 0.3733625352382659).abs() < XIRR_TOL,
            "got {got}, want 0.3733625352382659"
        );
    }

    #[test]
    fn xirr_explicit_guess_0_1_matches_farm() {
        // =XIRR(A1:A5, B1:B5, 0.1) -> 0.3733625352382659
        let got = as_num(eval_direct(
            eval,
            vec![Range(values()), Range(dates()), Scalar(num(0.1))],
        ));
        assert!(
            (got - 0.3733625352382659).abs() < XIRR_TOL,
            "got {got}, want 0.3733625352382659"
        );
    }

    #[test]
    fn xirr_guess_0_5_matches_farm() {
        // =XIRR(A1:A5, B1:B5, 0.5) -> 0.37336253374814987 (a slightly different
        // converged iterate; within 1e-7 of the same root our solver returns).
        let got = as_num(eval_direct(
            eval,
            vec![Range(values()), Range(dates()), Scalar(num(0.5))],
        ));
        assert!(
            (got - 0.37336253374814987).abs() < XIRR_TOL,
            "got {got}, want 0.37336253374814987"
        );
    }

    #[test]
    fn xirr_root_makes_xnpv_zero() {
        // The returned rate is a genuine XNPV root: XNPV(rate, A, B) ≈ 0.
        let rate = as_num(eval_direct(eval, vec![Range(values()), Range(dates())]));
        let base = 39448_i64;
        let cashflows: Vec<(f64, i64)> = vec![
            (-10000.0, 39448),
            (2750.0, 39508),
            (4250.0, 39751),
            (3250.0, 39859),
            (2750.0, 39904),
        ];
        let residual = crate::func_xnpv::xnpv_sum(rate, &cashflows, base);
        assert!(residual.abs() < 1e-6, "XNPV at XIRR root = {residual}");
    }

    // ---- validation / error paths -------------------------------------------

    // ---- OXP-168: guess-dependent root selection ----------------------------

    /// The OXP-168 multi-root schedule: values `{-1, 5, -6}`, dates
    /// `{43831, 44196, 44561}` (fixed-365 exponents 0,1,2). XNPV has two genuine
    /// roots, `r = 1` and `r = 2`.
    fn oxp168_values() -> Vec<Value> {
        vec![num(-1.0), num(5.0), num(-6.0)]
    }
    fn oxp168_dates() -> Vec<Value> {
        vec![num(43831.0), num(44196.0), num(44561.0)]
    }
    fn xirr_oxp168(guess: Option<f64>) -> Value {
        let mut args = vec![Range(oxp168_values()), Range(oxp168_dates())];
        if let Some(g) = guess {
            args.push(Scalar(num(g)));
        }
        eval_direct(eval, args)
    }

    /// OXP-168 (RUN-2026-07-11) + ratified XIRR checkpoint (2026-07-13,
    /// option (c)): the multi-root schedule `{-1, 5, -6}` has TWO sign changes
    /// (−,+,−), so the Descartes multi-root guard defers it **loudly** at every
    /// guess — we no longer silently return a root that differs from Excel's.
    /// Excel's outputs on this schedule (`≈2` for high guesses, the non-root
    /// artifact `≈2.98e-9` for the default/low guess — XNPV there is `-2`) are
    /// NOT reproduced: OXP-168 pins them for only this one schedule, so matching
    /// them would be a forbidden guess. `#UNSUPPORTED!` is the explicit, never-
    /// silently-wrong answer until a probe grid characterizes Excel's iteration.
    #[test]
    fn oxp168_multi_root_defers_loudly() {
        for guess in [None, Some(0.1), Some(1.5), Some(1.9), Some(2.5)] {
            assert_eq!(
                xirr_oxp168(guess),
                Value::Error(ErrorKind::Unsupported),
                "guess {guess:?}: multi-root schedule must defer loudly"
            );
        }
    }

    /// The guard is a sign-change over-approximation, so it must NOT fire on a
    /// single-sign-change (unique-root) schedule — those still solve and keep
    /// matching Excel. `{-1, 5}` (−,+) has one sign change → a genuine root.
    #[test]
    fn single_sign_change_schedule_still_solves() {
        let args = vec![
            Range(vec![num(-1.0), num(5.0)]),
            Range(vec![num(43831.0), num(44196.0)]),
        ];
        let got = as_num(eval_direct(eval, args));
        // XNPV(-1 + 5/(1+r) = 0) → r = 4 (exponent 0 and 1 over 365 days).
        assert!((got - 4.0).abs() < XIRR_TOL, "got {got}, want ≈4.0");
    }

    #[test]
    fn xirr_no_sign_change_is_num_error() {
        // All-positive cash flows have no real IRR → #NUM!.
        let all_pos = vec![
            num(1000.0),
            num(2750.0),
            num(4250.0),
            num(3250.0),
            num(2750.0),
        ];
        let got = eval_direct(eval, vec![Range(all_pos), Range(dates())]);
        assert_eq!(got, Value::Error(ErrorKind::Num));
    }

    #[test]
    fn xirr_length_mismatch_is_num_error() {
        let mut d = dates();
        d.pop();
        let got = eval_direct(eval, vec![Range(values()), Range(d)]);
        assert_eq!(got, Value::Error(ErrorKind::Num));
    }

    #[test]
    fn xirr_date_before_start_is_num_error() {
        let mut d = dates();
        d[2] = num(39000.0); // before base date 39448
        let got = eval_direct(eval, vec![Range(values()), Range(d)]);
        assert_eq!(got, Value::Error(ErrorKind::Num));
    }

    #[test]
    fn xirr_value_error_propagates() {
        let mut v = values();
        v[1] = Value::Error(ErrorKind::Div0);
        let got = eval_direct(eval, vec![Range(v), Range(dates())]);
        assert_eq!(got, Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn xirr_guess_error_propagates() {
        let got = eval_direct(
            eval,
            vec![
                Range(values()),
                Range(dates()),
                Scalar(Value::Error(ErrorKind::Value)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Value));
    }
}

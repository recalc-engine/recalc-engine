//! `XNPV` — net present value of a schedule of cash flows that occur on
//! **specific dates** (not equal periods), discounted at a single annual rate.
//!
//! # Provenance
//! Behavior contract: `docs/specs/XNPV.md`, which cites the public Microsoft
//! Learn XNPV page
//! (<https://support.microsoft.com/office/xnpv-function-1b42bbf6-370f-4532-a0eb-d67c16b664b7>).
//! Farm-pinned numerics: `RUN-2026-07-11-oracle01`, experiment **OXP-156**
//! (`tools/oracle/out/results/OXP-156.*.sidecar.json`). Date-serial handling is
//! this crate's own integer arithmetic (see below); numeric coercion is
//! `xl-value`'s frozen [`to_number`]. Clean-room from the Microsoft page and the
//! oracle only.
//!
//! # Formula (the correctness point)
//! `XNPV(rate, values, dates)` = Σ_i `value_i / (1 + rate)^((date_i − date_0)/365)`
//! where **`date_0` is the *first* date listed** (not the smallest) and the year
//! fraction uses a **fixed 365-day** denominator. Because only the *difference*
//! `date_i − date_0` enters, the workbook's 1900/1904 date system is irrelevant
//! (the epoch offset cancels), so this function ignores the [`EvalContext`] date
//! system and never converts a serial to a calendar date — it needs day counts,
//! not `(y, m, d)`.
//!
//! # Semantics implemented (with pinned vs inferred provenance)
//! - **`rate = 0` → `#NUM!` (FARM-PINNED, OXP-156).** Mathematically
//!   `(1 + 0)^t = 1`, so a naive reading would return the plain cash-flow sum
//!   (here `3000`). The pinned Excel build instead returns `#NUM!`, and
//!   Recalc Principle 2 requires reproducing the *observed* value rather than
//!   rationalizing it away. We gate `rate ≤ 0 → #NUM!` as the minimal rule
//!   consistent with every pin (`rate = 0 → #NUM!`; `rate = 0.05` and
//!   `rate = 0.09` → finite values). `rate < 0` is **not** independently
//!   farm-probed; it shares the "rate ≤ 0 invalid" reading pending a probe.
//! - **`values` / `dates` are equal-length, position-paired sequences.** They
//!   are read in row-major order with blanks surfaced positionally (so pairing
//!   never drifts). Mismatched lengths → `#NUM!` (Microsoft XNPV page: "If
//!   `values` and `dates` contain a different number of values, XNPV returns the
//!   `#NUM!` error value"). An empty pairing → `#NUM!`.
//! - **A date earlier than the first (starting) date → `#NUM!`** (Microsoft
//!   page: "If any number in `dates` precedes the starting date, XNPV returns
//!   the `#NUM!` error value"). Dates otherwise need not be sorted.
//! - **Coercion.** Each `value` cell coerces with [`to_number`] (blank → 0,
//!   numeric text → its number, logical → 1/0); an error cell propagates. Each
//!   `date` cell coerces with [`to_number`] then **truncates toward zero** to a
//!   whole-day serial (all valid Excel date serials are non-negative, where
//!   truncation and flooring coincide). Non-numeric mixed-range edges are not
//!   independently farm-probed.
//! - Overflow (a non-finite running total) becomes `#NUM!` via [`Value::number`].

use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// The fixed day-count denominator XNPV/XIRR use for the year fraction
/// (`(date_i − date_0) / 365`). Microsoft XNPV page; not a configurable basis.
pub(crate) const DAY_COUNT: f64 = 365.0;

/// The largest `f64` magnitude a date argument may have before integer
/// truncation loses precision; beyond it no valid Excel date is reachable, so it
/// is `#NUM!`. Mirrors [`crate::date_common`]'s safety rail.
const DATE_MAGNITUDE_LIMIT: f64 = 1e15;

/// Evaluate an `XNPV(rate, values, dates)` call. See the module docs for the
/// pinned semantics (notably the farm-pinned `rate = 0 → #NUM!`).
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Arg 0 is the discount rate; scalar coercion, propagate errors.
    let rate = match to_number(&args.eval_scalar(0)) {
        Ok(r) => r,
        Err(k) => return Value::Error(k),
    };

    // OXP-156 (RUN-2026-07-11-oracle01): rate = 0 → #NUM! on the farm. Gate
    // rate ≤ 0 (the minimal rule consistent with the pins). A NaN rate cannot
    // occur (a `Number` value is always finite), so `<=` is total here.
    if rate <= 0.0 {
        return Value::Error(ErrorKind::Num);
    }

    // Args 1 (values) and 2 (dates): equal-length, position-paired.
    let cashflows = match collect_pairs(args, 1, 2) {
        Ok(p) => p,
        Err(k) => return Value::Error(k),
    };

    let base_date = cashflows[0].1;
    Value::number(xnpv_sum(rate, &cashflows, base_date))
}

/// The discounted sum `Σ value_i / (1 + rate)^((date_i − base_date)/365)`,
/// accumulated in listed order. Shared with `XIRR`, which drives this to zero.
///
/// This is the raw numeric kernel: it applies **no** `rate` gate (XIRR must
/// evaluate it at rates the XNPV worksheet function would reject), so callers
/// that need the `rate ≤ 0 → #NUM!` rule apply it themselves.
pub(crate) fn xnpv_sum(rate: f64, cashflows: &[(f64, i64)], base_date: i64) -> f64 {
    let base = 1.0 + rate;
    let mut acc = 0.0_f64;
    for &(value, date) in cashflows {
        let t = (date - base_date) as f64 / DAY_COUNT;
        acc += value / base.powf(t);
    }
    acc
}

/// The derivative of [`xnpv_sum`] with respect to `rate`:
/// `d/drate Σ v_i (1+rate)^(-t_i) = -Σ v_i · t_i · (1+rate)^(-(t_i+1))`.
/// Used by `XIRR`'s Newton–Raphson step.
pub(crate) fn xnpv_deriv(rate: f64, cashflows: &[(f64, i64)], base_date: i64) -> f64 {
    let base = 1.0 + rate;
    let mut acc = 0.0_f64;
    for &(value, date) in cashflows {
        let t = (date - base_date) as f64 / DAY_COUNT;
        acc += -value * t / base.powf(t + 1.0);
    }
    acc
}

/// Collect the `values`/`dates` arguments into position-paired
/// `(value, date_serial)` cash flows, applying the shared XNPV/XIRR validation:
/// equal length (else `#NUM!`), non-empty (else `#NUM!`), and no date earlier
/// than the first/starting date (else `#NUM!`, per the Microsoft page). The
/// returned `Vec` is never empty, so callers may index `[0]` for the base date.
pub(crate) fn collect_pairs(
    args: &mut dyn CallArgs,
    values_index: usize,
    dates_index: usize,
) -> Result<Vec<(f64, i64)>, ErrorKind> {
    let raw_values = collect_cells(args, values_index)?;
    let raw_dates = collect_cells(args, dates_index)?;

    // Microsoft XNPV/XIRR page: differing counts → #NUM!.
    if raw_values.len() != raw_dates.len() {
        return Err(ErrorKind::Num);
    }
    if raw_values.is_empty() {
        return Err(ErrorKind::Num);
    }

    let mut pairs = Vec::with_capacity(raw_values.len());
    for (value_cell, date_cell) in raw_values.iter().zip(raw_dates.iter()) {
        let value = to_number(value_cell)?;
        let serial = coerce_date_serial(date_cell)?;
        pairs.push((value, serial));
    }

    // The first listed date is the discounting base; any date before it → #NUM!.
    let base_date = pairs[0].1;
    if pairs.iter().any(|&(_, date)| date < base_date) {
        return Err(ErrorKind::Num);
    }

    Ok(pairs)
}

/// Materialize a range/array argument's cells in row-major order, blanks
/// surfaced positionally so the values↔dates pairing cannot drift. An unbounded
/// whole-column range (which the dense walk refuses) surfaces as
/// `#UNSUPPORTED!`.
fn collect_cells(args: &mut dyn CallArgs, index: usize) -> Result<Vec<Value>, ErrorKind> {
    let mut out = Vec::new();
    let result = args.for_each_row(index, &mut |row| {
        out.extend(row.iter().cloned());
        ControlFlow::Continue(())
    });
    result.map(|()| out)
}

/// Coerce a date cell to a whole-day serial: scalar numeric coercion, then
/// truncate toward zero. A magnitude past the safety rail is `#NUM!`; an error
/// value propagates. (For all valid Excel dates the serial is non-negative, so
/// truncate-toward-zero and floor agree.)
fn coerce_date_serial(value: &Value) -> Result<i64, ErrorKind> {
    let n = to_number(value)?;
    if !n.is_finite() || n.abs() >= DATE_MAGNITUDE_LIMIT {
        return Err(ErrorKind::Num);
    }
    Ok(n.trunc() as i64)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    /// Pull the `f64` out of a numeric result (panics on anything else) so the
    /// irrational discounted sum can be compared with a tolerance.
    fn as_num(v: Value) -> f64 {
        match v {
            Value::Number(n) => n,
            other => panic!("expected a number, got {other:?}"),
        }
    }

    /// The farm cash-flow schedule (RUN-2026-07-11-oracle01, OXP-156/157):
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

    /// XNPV is a closed-form discounted sum, so it is checked at ≤1e-9 (no
    /// tolerance row needed; see `docs/specs/XNPV.md`).
    const TOL: f64 = 1e-9;

    // ---- FARM-PINNED targets (RUN-2026-07-11-oracle01, OXP-156) -------------

    #[test]
    fn xnpv_rate_9pct_matches_farm() {
        // =XNPV(0.09, A1:A5, B1:B5) -> 2086.647602031535
        let got = as_num(eval_direct(
            eval,
            vec![Scalar(num(0.09)), Range(values()), Range(dates())],
        ));
        assert!(
            (got - 2086.647602031535).abs() < TOL,
            "got {got}, want 2086.647602031535"
        );
    }

    #[test]
    fn xnpv_rate_5pct_matches_farm() {
        // =XNPV(0.05, A1:A5, B1:B5) -> 2472.9824192453066
        let got = as_num(eval_direct(
            eval,
            vec![Scalar(num(0.05)), Range(values()), Range(dates())],
        ));
        assert!(
            (got - 2472.9824192453066).abs() < TOL,
            "got {got}, want 2472.9824192453066"
        );
    }

    #[test]
    fn xnpv_rate_zero_is_num_error() {
        // =XNPV(0, A1:A5, B1:B5) -> #NUM!  (FARM-PINNED, OXP-156). NOT the plain
        // cash-flow sum (3000) — the observed error is reproduced verbatim.
        let got = eval_direct(
            eval,
            vec![Scalar(num(0.0)), Range(values()), Range(dates())],
        );
        assert_eq!(got, Value::Error(ErrorKind::Num));
    }

    // ---- rate gate / error propagation --------------------------------------

    #[test]
    fn xnpv_negative_rate_is_num_error() {
        // rate ≤ 0 → #NUM! (rate = 0 is the farm-pinned point; rate < 0 shares
        // the same "rate ≤ 0 invalid" reading, not independently probed).
        let got = eval_direct(
            eval,
            vec![Scalar(num(-0.1)), Range(values()), Range(dates())],
        );
        assert_eq!(got, Value::Error(ErrorKind::Num));
    }

    #[test]
    fn xnpv_rate_error_propagates() {
        let got = eval_direct(
            eval,
            vec![
                Scalar(Value::Error(ErrorKind::Div0)),
                Range(values()),
                Range(dates()),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn xnpv_value_error_propagates() {
        let mut v = values();
        v[2] = Value::Error(ErrorKind::Na);
        let got = eval_direct(eval, vec![Scalar(num(0.09)), Range(v), Range(dates())]);
        assert_eq!(got, Value::Error(ErrorKind::Na));
    }

    // ---- length / date-order validation (Microsoft XNPV page) ---------------

    #[test]
    fn xnpv_length_mismatch_is_num_error() {
        // 5 values, 4 dates → #NUM!.
        let mut d = dates();
        d.pop();
        let got = eval_direct(eval, vec![Scalar(num(0.09)), Range(values()), Range(d)]);
        assert_eq!(got, Value::Error(ErrorKind::Num));
    }

    #[test]
    fn xnpv_date_before_start_is_num_error() {
        // A later date earlier than the first (starting) date → #NUM!.
        let mut d = dates();
        d[3] = num(39000.0); // < base date 39448
        let got = eval_direct(eval, vec![Scalar(num(0.09)), Range(values()), Range(d)]);
        assert_eq!(got, Value::Error(ErrorKind::Num));
    }

    // ---- coercion edges ------------------------------------------------------

    #[test]
    fn xnpv_dates_truncate_toward_zero() {
        // A fractional date serial (a time-of-day component) truncates to the
        // whole day, so 39508.9 pairs identically to 39508.
        let mut d = dates();
        d[1] = num(39508.9);
        let got = as_num(eval_direct(
            eval,
            vec![Scalar(num(0.09)), Range(values()), Range(d)],
        ));
        assert!(
            (got - 2086.647602031535).abs() < TOL,
            "got {got}, want 2086.647602031535"
        );
    }

    #[test]
    fn xnpv_numeric_text_value_coerces() {
        // A numeric-text value cell coerces via to_number and participates.
        let mut v = values();
        v[1] = txt("2750");
        let got = as_num(eval_direct(
            eval,
            vec![Scalar(num(0.09)), Range(v), Range(dates())],
        ));
        assert!(
            (got - 2086.647602031535).abs() < TOL,
            "got {got}, want 2086.647602031535"
        );
    }
}

//! `YEARFRAC` — the fraction of a year between two dates, on one of five
//! day-count bases used in accounting/bond conventions.
//!
//! # Provenance
//! Behavior contract: `docs/specs/YEARFRAC.md`, which cites the public Microsoft
//! Learn YEARFRAC page
//! (<https://support.microsoft.com/en-us/office/yearfrac-function-3844141e-c76d-4143-82b6-208454ddc6a8>,
//! verified 2026-07-11). Serial ↔ `(year, month, day)` conversion (including the
//! 1900 fake-leap-day seam) is [`crate::datecore`]; date-argument coercion is
//! `xl-value`'s via [`crate::date_common`]. The 30/360 day-count rules mirror the
//! oracle-pinned DAYS360 US/European rules documented in `docs/specs/DAYS360.md`
//! (`OXP-099`/`OXP-100`, RUN-2026-07-11-oracle01) — reproduced here clean-room
//! from that spec (the DAYS360 module's helpers are private; this crate shares
//! only `datecore`/`date_common`). Clean-room from the Microsoft pages only.
//!
//! # Signature
//! `YEARFRAC(start_date, end_date, [basis])` — arity 2..=3; `basis` defaults to
//! `0`.
//!
//! # `basis` (arg 2, optional) — day-count convention
//! Coerced to a number (scalar `to_number`) and **truncated toward zero**; the
//! Microsoft page: *"If basis < 0 or if basis > 4, YEARFRAC returns the #NUM!
//! error value."* So a truncated `basis` outside `0..=4` → `#NUM!`.
//!
//! | `basis` | convention | status |
//! |---|---|---|
//! | 0 (or omitted) | US (NASD) 30/360 | **implemented** — DAYS360 US day count / 360 |
//! | 1 | Actual/actual | **deferred** → `#UNSUPPORTED!` (see below) |
//! | 2 | Actual/360 | **implemented** — `(end − start)` serial days / 360 |
//! | 3 | Actual/365 | **implemented** — `(end − start)` serial days / 365 |
//! | 4 | European 30/360 | **implemented** — DAYS360 European day count / 360 |
//!
//! ## Why basis 1 (Actual/actual) is deferred — not guessed
//! The Microsoft page labels basis 1 "Actual/actual" but documents **no
//! algorithm** for choosing the year-length denominator when the interval spans
//! a year boundary or multiple years (the notorious leap-year-averaging edge).
//! The page's own worked example — `YEARFRAC(1/1/2012, 7/30/2012, 1)` =
//! `0.57650273` = `211/366` — shows the denominator is `366` here (2012 is a leap
//! year and the whole period lies within it), but that is a single data point,
//! not a rule: it does not tell us how Excel picks the denominator for a
//! period crossing 2011→2012, or spanning 2011→2014. Reproducing those from
//! memory would be a guess (Recalc Principle 2 forbids "plausible Excel
//! behavior" from training memory), so basis 1 returns `#UNSUPPORTED!` pending an
//! oracle probe of its denominator rule.
//!
//! ## Basis 0 / 4 fidelity caveat
//! Basis 0/4 reuse the *DAYS360* US/European day count (`docs/specs/DAYS360.md`),
//! which was oracle-pinned for **DAYS360**, not independently for YEARFRAC. The
//! standard 30/360 conventions are shared, and the DAYS360 month-end edges that
//! were *not* probed (the US end-side February month-end) stay deferred here too
//! (they surface `#UNSUPPORTED!`), so YEARFRAC never guesses where DAYS360 did
//! not. Confirming YEARFRAC-basis-0 ≡ DAYS360-US/360 (and basis-4 ≡
//! DAYS360-European/360) on the February/31st edges is a flagged oracle probe.
//!
//! # Order / errors
//! Arguments resolve left-to-right (`start`, `end`, then `basis`); the first
//! error wins. An error date/basis argument propagates; an out-of-range serial →
//! `#NUM!`; serial 0 ("January 0, 1900") → `#UNSUPPORTED!` (`OXP-090`), all via
//! [`crate::date_common`]. Truncated `basis` outside `0..=4` → `#NUM!`.
//! `start == end` → `0` for every valid basis (the fraction is zero regardless of
//! convention), checked before the basis-1 deferral. `start > end` is
//! **deferred** → `#UNSUPPORTED!` (the sign/ordering behavior is undocumented and
//! unprobed).

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::date_common::{coerce_serial, map_date_error};
use crate::datecore::{DateSystem, serial_to_ymd};

/// A resolved calendar date's `(year, month, day)` components. Threaded as one
/// value through the 30/360 helpers so their argument lists stay small (clippy
/// `too_many_arguments`).
type Ymd = (i32, u32, u32);

/// Evaluate a `YEARFRAC(start_date, end_date, [basis])` call. See the module docs
/// for the per-basis conventions and the basis-1 deferral.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let system = ctx.date_system();

    // Resolve the two dates left-to-right; each yields its serial (for the
    // actual-day bases and the ordering checks) and its (y, m, d) (for 30/360).
    let (s1, ymd1) = match resolve(&args.eval_scalar(0), system) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };
    let (s2, ymd2) = match resolve(&args.eval_scalar(1), system) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };

    // basis: scalar numeric coercion, truncate toward zero. An omitted/blank
    // third argument coerces to 0 (the default basis).
    let basis = match to_number(&args.eval_scalar(2)) {
        Ok(n) => n.trunc(),
        Err(k) => return Value::Error(k),
    };
    // "If basis < 0 or if basis > 4, YEARFRAC returns the #NUM! error value."
    // (evaluated on the truncated basis; the range test also rejects NaN/±inf).
    if !(0.0..=4.0).contains(&basis) {
        return Value::Error(ErrorKind::Num);
    }
    let basis = basis as i64;

    // Equal dates → 0 for every valid basis (the fraction is zero regardless of
    // day-count convention), BEFORE the basis-1 deferral.
    if s1 == s2 {
        return Value::number(0.0);
    }
    // OXP (unassigned): YEARFRAC with start_date > end_date — the sign/ordering
    // behavior (negative fraction? absolute value? #NUM!?) is undocumented on the
    // Microsoft page and unprobed. Defer rather than guess.
    if s1 > s2 {
        return Value::Error(ErrorKind::Unsupported);
    }

    match basis {
        // US (NASD) 30/360: DAYS360 US day count / 360.
        0 => match thirty_360(ymd1, ymd2, Method::Us, system) {
            Ok(days) => Value::number(days as f64 / 360.0),
            Err(k) => Value::Error(k),
        },
        // OXP (unassigned): YEARFRAC basis 1 (Actual/actual) denominator rule for
        // year-spanning / multi-year intervals — undocumented, deferred (module
        // docs). Never guessed.
        1 => Value::Error(ErrorKind::Unsupported),
        // Actual/360, Actual/365: raw serial-day difference over the fixed
        // denominator. `s2 > s1` here (equal/greater handled above).
        2 => Value::number((s2 - s1) as f64 / 360.0),
        3 => Value::number((s2 - s1) as f64 / 365.0),
        // European 30/360: DAYS360 European day count / 360.
        4 => match thirty_360(ymd1, ymd2, Method::European, system) {
            Ok(days) => Value::number(days as f64 / 360.0),
            Err(k) => Value::Error(k),
        },
        _ => unreachable!("basis was range-checked into 0..=4"),
    }
}

/// Coerce a date argument to a serial (scalar path, floored) and resolve it to
/// `(year, month, day)` in `system`, mapping a [`crate::datecore::DateError`] to
/// its Excel error kind. Returns the serial alongside the calendar components so
/// callers can use the raw serial (actual-day bases, ordering checks) or the
/// `(y, m, d)` (30/360 bases). Floor and truncate-toward-zero coincide on the
/// positive valid-date serial domain.
fn resolve(value: &Value, system: DateSystem) -> Result<(i64, Ymd), ErrorKind> {
    let serial = coerce_serial(value)?;
    let (y, m, d) = serial_to_ymd(serial, system).map_err(map_date_error)?;
    Ok((serial, (y, m, d)))
}

/// Which 30/360 day-count method to apply.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    /// US (NASD) 30/360 — the DAYS360 US rule (basis 0).
    Us,
    /// European 30/360 — the DAYS360 European rule (basis 4).
    European,
}

/// The 30/360 day count `(y2−y1)·360 + (m2−m1)·30 + (d2a−d1a)`, after the
/// method-specific day adjustments — the numerator of basis 0/4 (denominator
/// 360). Reproduces the DAYS360 US/European rules clean-room from
/// `docs/specs/DAYS360.md`, including DAYS360's deferral of the unprobed US
/// end-side February month-end (`Err(ErrorKind::Unsupported)`).
fn thirty_360(
    (y1, m1, d1): Ymd,
    (y2, m2, d2): Ymd,
    method: Method,
    system: DateSystem,
) -> Result<i64, ErrorKind> {
    let (d1a, d2a) = match method {
        Method::European => european_adjust(d1, d2),
        Method::Us => us_adjust((y1, m1, d1), (y2, m2, d2), system)?,
    };
    Ok((y2 as i64 - y1 as i64) * 360 + (m2 as i64 - m1 as i64) * 30 + (d2a - d1a))
}

/// European day adjustment: the 31st of any month becomes the 30th (both dates).
/// Nothing else is touched (no February handling) — DAYS360.md "European".
fn european_adjust(d1: u32, d2: u32) -> (i64, i64) {
    let a = if d1 == 31 { 30 } else { d1 };
    let b = if d2 == 31 { 30 } else { d2 };
    (a as i64, b as i64)
}

/// US (NASD) day adjustment — the DAYS360 pinned clauses, deferring the unprobed
/// US end-side month-end-below-the-30th edge (DAYS360.md `OXP-099`/`OXP-100`):
/// - a start on the last day of its month → 30 (pinned incl. February start);
/// - an end on the 31st with the adjusted start on the 30th → 30, else the 31st
///   stays 31 (the `31-Dec` example disproves the prose's roll-to-next-month);
/// - an end month-end **below** the 30th (a February end-side month-end) is
///   unprobed → `#UNSUPPORTED!`, never guessed by symmetry.
fn us_adjust(
    (y1, m1, d1): Ymd,
    (y2, m2, d2): Ymd,
    system: DateSystem,
) -> Result<(i64, i64), ErrorKind> {
    let start_is_month_end = d1 == days_in_month(y1, m1, system);
    let end_is_month_end = d2 == days_in_month(y2, m2, system);

    // OXP-099 end-side February month-end (below the 30th) — unprobed for
    // DAYS360, so equally deferred for YEARFRAC basis 0. Never guessed.
    if end_is_month_end && d2 < 30 {
        return Err(ErrorKind::Unsupported);
    }

    let d1a: i64 = if start_is_month_end { 30 } else { d1 as i64 };
    let d2a: i64 = if d2 == 31 && d1a == 30 { 30 } else { d2 as i64 };
    Ok((d1a, d2a))
}

/// Number of days in month `month` of `year`, in the workbook's date system
/// (February is 29 in a leap year, and in the 1900 system Excel treats 1900 as a
/// leap year — the phantom 29-Feb-1900). `month` is always `1..=12`.
fn days_in_month(year: i32, month: u32, system: DateSystem) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let phantom_1900 = system == DateSystem::Excel1900 && year == 1900;
            if phantom_1900 || is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => unreachable!("month component from serial_to_ymd is always 1..=12"),
    }
}

/// Proleptic-Gregorian leap-year test (the Excel-1900 phantom is handled
/// separately in [`days_in_month`]).
fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datecore::date_to_serial;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    /// A `Value::Number` holding the 1900-system serial of the given calendar
    /// date (the default `eval_direct` date system), derived through
    /// `date_to_serial` so the tests are robust to serial arithmetic.
    fn serial(y: i64, m: i64, d: i64) -> Value {
        num(date_to_serial(y, m, d, DateSystem::Excel1900).unwrap() as f64)
    }

    fn yearfrac(args: Vec<crate::test_support::TestArg>) -> Value {
        eval_direct(super::eval, args)
    }

    /// Extract the `f64` from a `Value::Number`, panicking otherwise.
    fn as_num(v: &Value) -> f64 {
        match v {
            Value::Number(n) => *n,
            other => panic!("expected a number, got {other:?}"),
        }
    }

    /// Assert two fractions are exactly equal (each side is a single IEEE-754
    /// division of integer operands — correctly rounded, so bit-identical).
    fn assert_frac(out: &Value, expected: f64) {
        assert_eq!(as_num(out), expected, "got {out:?}, expected {expected}");
    }

    // ---- Microsoft Learn worked examples (verified 2026-07-11) --------------

    #[test]
    fn ms_example_basis0_us_30_360() {
        // =YEARFRAC(1/1/2012, 7/30/2012) [basis omitted → 0] = 0.58055556.
        // US 30/360 day count: (7-1)·30 + (30-1) = 209; 209/360 = 0.58055556.
        let out = yearfrac(vec![
            Scalar(serial(2012, 1, 1)),
            Scalar(serial(2012, 7, 30)),
        ]);
        assert_frac(&out, 209.0 / 360.0);
    }

    #[test]
    fn ms_example_basis3_actual_365() {
        // =YEARFRAC(1/1/2012, 7/30/2012, 3) = 0.57808219 = 211/365.
        // Actual days between the two dates (2012 is a leap year) = 211.
        let out = yearfrac(vec![
            Scalar(serial(2012, 1, 1)),
            Scalar(serial(2012, 7, 30)),
            Scalar(num(3.0)),
        ]);
        assert_frac(&out, 211.0 / 365.0);
        // Sanity: the actual-day numerator is exactly 211.
        assert_eq!(
            date_to_serial(2012, 7, 30, DateSystem::Excel1900).unwrap()
                - date_to_serial(2012, 1, 1, DateSystem::Excel1900).unwrap(),
            211
        );
    }

    #[test]
    fn basis2_actual_360() {
        // Same dates, Actual/360: 211/360 (shares the 211-day numerator verified
        // against the MS basis-3 example; /360 instead of /365).
        let out = yearfrac(vec![
            Scalar(serial(2012, 1, 1)),
            Scalar(serial(2012, 7, 30)),
            Scalar(num(2.0)),
        ]);
        assert_frac(&out, 211.0 / 360.0);
    }

    #[test]
    fn basis2_actual_360_simple() {
        // Jan 1 → Jan 31, 2011: exactly 30 actual days. 30/360 = 1/12.
        let out = yearfrac(vec![
            Scalar(serial(2011, 1, 1)),
            Scalar(serial(2011, 1, 31)),
            Scalar(num(2.0)),
        ]);
        assert_frac(&out, 30.0 / 360.0);
    }

    #[test]
    fn basis3_actual_365_simple() {
        // Jan 1 → Jan 31, 2011: 30 actual days. 30/365.
        let out = yearfrac(vec![
            Scalar(serial(2011, 1, 1)),
            Scalar(serial(2011, 1, 31)),
            Scalar(num(3.0)),
        ]);
        assert_frac(&out, 30.0 / 365.0);
    }

    // ---- basis 0 vs basis 4 divergence on a 31st end date -------------------

    #[test]
    fn basis0_vs_basis4_on_31st_end() {
        // (1-Jan-2011, 31-Dec-2011):
        //   US (basis 0): end 31-Dec is a month-end but not rolled; start 1 < 30
        //     leaves the 31st in place → (12-1)·30 + (31-1) = 360; 360/360 = 1.
        //   European (basis 4): 31 → 30 → (12-1)·30 + (30-1) = 359; 359/360.
        let us = yearfrac(vec![
            Scalar(serial(2011, 1, 1)),
            Scalar(serial(2011, 12, 31)),
            Scalar(num(0.0)),
        ]);
        assert_frac(&us, 360.0 / 360.0);

        let eu = yearfrac(vec![
            Scalar(serial(2011, 1, 1)),
            Scalar(serial(2011, 12, 31)),
            Scalar(num(4.0)),
        ]);
        assert_frac(&eu, 359.0 / 360.0);
    }

    #[test]
    fn basis0_start_on_31st_becomes_30() {
        // (31-Jan-2011, 15-Mar-2011), US 30/360: start 31 → 30.
        // (3-1)·30 + (15-30) = 60 - 15 = 45; 45/360 = 0.125.
        let out = yearfrac(vec![
            Scalar(serial(2011, 1, 31)),
            Scalar(serial(2011, 3, 15)),
            Scalar(num(0.0)),
        ]);
        assert_frac(&out, 45.0 / 360.0);
    }

    // ---- basis truncation toward zero ---------------------------------------

    #[test]
    fn basis_truncates_toward_zero() {
        // basis 3.9 truncates to 3 (Actual/365), matching the plain-3 result.
        let out = yearfrac(vec![
            Scalar(serial(2011, 1, 1)),
            Scalar(serial(2011, 1, 31)),
            Scalar(num(3.9)),
        ]);
        assert_frac(&out, 30.0 / 365.0);
    }

    // ---- basis out of range → #NUM! -----------------------------------------

    #[test]
    fn basis_above_four_is_num_error() {
        let out = yearfrac(vec![
            Scalar(serial(2012, 1, 1)),
            Scalar(serial(2012, 7, 30)),
            Scalar(num(5.0)),
        ]);
        assert_eq!(out, Value::Error(ErrorKind::Num));
    }

    #[test]
    fn basis_negative_is_num_error() {
        let out = yearfrac(vec![
            Scalar(serial(2012, 1, 1)),
            Scalar(serial(2012, 7, 30)),
            Scalar(num(-1.0)),
        ]);
        assert_eq!(out, Value::Error(ErrorKind::Num));
    }

    #[test]
    fn invalid_basis_beats_equal_dates() {
        // basis > 4 → #NUM! even when start == end (basis validated first).
        let out = yearfrac(vec![
            Scalar(serial(2011, 6, 15)),
            Scalar(serial(2011, 6, 15)),
            Scalar(num(9.0)),
        ]);
        assert_eq!(out, Value::Error(ErrorKind::Num));
    }

    // ---- start == end → 0 ----------------------------------------------------

    #[test]
    fn equal_dates_are_zero_each_basis() {
        for basis in [0.0, 2.0, 3.0, 4.0] {
            let out = yearfrac(vec![
                Scalar(serial(2011, 6, 15)),
                Scalar(serial(2011, 6, 15)),
                Scalar(num(basis)),
            ]);
            assert_eq!(out, num(0.0), "basis {basis}");
        }
    }

    #[test]
    fn equal_dates_zero_short_circuits_before_basis1_deferral() {
        // start == end → 0 even for the (otherwise deferred) basis 1: the
        // fraction is zero regardless of the day-count convention.
        let out = yearfrac(vec![
            Scalar(serial(2011, 6, 15)),
            Scalar(serial(2011, 6, 15)),
            Scalar(num(1.0)),
        ]);
        assert_eq!(out, num(0.0));
    }

    // ---- basis 1 (Actual/actual) deferral -----------------------------------

    #[test]
    fn basis1_actual_actual_is_deferred() {
        // The MS page shows YEARFRAC(1/1/2012, 7/30/2012, 1) = 0.57650273
        // (= 211/366), but documents no denominator algorithm for the general
        // (year-spanning / multi-year) case, so basis 1 is #UNSUPPORTED! rather
        // than guessed. See the module docs / docs/specs/YEARFRAC.md.
        let out = yearfrac(vec![
            Scalar(serial(2012, 1, 1)),
            Scalar(serial(2012, 7, 30)),
            Scalar(num(1.0)),
        ]);
        assert_eq!(out, Value::Error(ErrorKind::Unsupported));
    }

    // ---- start > end deferral ------------------------------------------------

    #[test]
    fn start_after_end_is_deferred() {
        // Reversed order: undocumented/unprobed → #UNSUPPORTED! (not guessed).
        let out = yearfrac(vec![
            Scalar(serial(2012, 7, 30)),
            Scalar(serial(2012, 1, 1)),
            Scalar(num(0.0)),
        ]);
        assert_eq!(out, Value::Error(ErrorKind::Unsupported));
    }

    // ---- error / coercion behavior ------------------------------------------

    #[test]
    fn error_date_argument_propagates() {
        let out = yearfrac(vec![
            Scalar(Value::Error(ErrorKind::Div0)),
            Scalar(serial(2012, 7, 30)),
            Scalar(num(0.0)),
        ]);
        assert_eq!(out, Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn non_numeric_text_date_is_value_error() {
        // A non-numeric text date → #VALUE! via the frozen to_number contract
        // (date-literal text parsing is not done here — OXP-001).
        let out = yearfrac(vec![Scalar(txt("not a date")), Scalar(serial(2012, 7, 30))]);
        assert_eq!(out, Value::Error(ErrorKind::Value));
    }

    #[test]
    fn january_zero_serial_defers() {
        // Serial 0 in the 1900 system ("January 0, 1900") → #UNSUPPORTED! (OXP-090).
        let out = yearfrac(vec![
            Scalar(num(0.0)),
            Scalar(serial(2012, 7, 30)),
            Scalar(num(2.0)),
        ]);
        assert_eq!(out, Value::Error(ErrorKind::Unsupported));
    }

    #[test]
    fn negative_serial_is_num_error() {
        let out = yearfrac(vec![
            Scalar(num(-5.0)),
            Scalar(serial(2012, 7, 30)),
            Scalar(num(2.0)),
        ]);
        assert_eq!(out, Value::Error(ErrorKind::Num));
    }

    #[test]
    fn us_end_side_february_month_end_defers() {
        // basis 0, end on a February month-end below the 30th (28-Feb) with an
        // earlier start: the DAYS360 end-side February edge is unprobed, so it
        // stays #UNSUPPORTED! here too (never guessed by symmetry).
        let out = yearfrac(vec![
            Scalar(serial(2011, 1, 15)),
            Scalar(serial(2011, 2, 28)),
            Scalar(num(0.0)),
        ]);
        assert_eq!(out, Value::Error(ErrorKind::Unsupported));
    }

    #[test]
    fn european_february_month_end_is_computed_not_deferred() {
        // basis 4 has no February handling, so a February end-side month-end is
        // computed normally: (2-1)·30 + (28-15) = 43; 43/360.
        let out = yearfrac(vec![
            Scalar(serial(2011, 1, 15)),
            Scalar(serial(2011, 2, 28)),
            Scalar(num(4.0)),
        ]);
        assert_frac(&out, 43.0 / 360.0);
    }
}

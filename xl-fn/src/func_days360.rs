//! `DAYS360` — days between two dates on a 360-day (twelve 30-day months) year,
//! the day-count basis used in some accounting/bond conventions.
//!
//! # Provenance
//! Behavior contract: `docs/specs/DAYS360.md`, which cites the public Microsoft
//! Learn DAYS360 page
//! (<https://support.microsoft.com/office/days360-function-b9a509fd-49ef-407e-94df-0cbda5718c2a>).
//! Serial ↔ `(year, month, day)` conversion (including the 1900 leap-year bug)
//! is [`crate::datecore`]; date-argument coercion is `xl-value`'s via
//! [`crate::date_common`]. Clean-room from the Microsoft page only.
//!
//! # Formula
//! After the method-specific day adjustments below,
//! `result = (y2 - y1)·360 + (m2 - m1)·30 + (d2 - d1)`.
//!
//! # `method` argument (arg 2, optional)
//! Omitted or `FALSE` → US (NASD) method; `TRUE` → European. Coerced with
//! `xl-value`'s [`to_bool`] (the frozen boolean contract: numbers `0`→`FALSE`
//! else `TRUE`; `"TRUE"`/`"FALSE"` text; `Blank`→`FALSE`; an error propagates).
//! `args.count()` distinguishes an omitted third argument from a supplied one.
//!
//! # Day adjustments — pinned vs deferred
//! The European clause is stated plainly and fully; the US clause on the public
//! page is the infamous one whose *prose* is contradicted by the page's own
//! worked example, so only the example-verified parts are pinned.
//!
//! ## European (`method = TRUE`) — fully pinned
//! Page: "Starting dates and ending dates that occur on the 31st day of a month
//! become equal to the 30th day of the same month." So `d1 == 31 → 30` and
//! `d2 == 31 → 30`; nothing else is touched. Fully pinned, no deferral.
//!
//! ## US / NASD (`method = FALSE` or omitted)
//! Page prose (verbatim): *"If the starting date is the last day of a month, it
//! becomes equal to the 30th day of the same month. If the ending date is the
//! last day of a month and the starting date is earlier than the 30th day of a
//! month, the ending date becomes equal to the 1st day of the next month;
//! otherwise the ending date becomes equal to the 30th day of the same month."*
//!
//! Pinned clauses (clearly stated **and** consistent with the page's example
//! grid, which lists `DAYS360(1-Jan-11, 31-Dec-11) = 360`):
//! - **Start on the 31st → 30.** The 31st is unambiguously "the last day of a
//!   month"; this is the common case and is required for the example
//!   `DAYS360(1-Jan-11, 31-Dec-11) = 360` (the start rule must *not* fire for
//!   `1-Jan`).
//! - **End on the 31st with the (adjusted) start on the 30th → 30** (prose's
//!   "otherwise … the 30th day"). When the start is *earlier* than the 30th the
//!   31st **stays** the 31st — pinned directly by the example
//!   `DAYS360(1-Jan-11, 31-Dec-11) = 360`, which is only reproducible if the
//!   `31-Dec` end is left as day 31 (giving `330 + 30`), **not** rolled to the
//!   "1st day of the next month" the prose describes (which would give `30`).
//!   The example therefore *disproves* the prose's roll-to-next-month clause for
//!   the 31st, so it is not implemented.
//!
//! Oracle-resolved edges (RUN-2026-07-11-oracle01):
//! - **OXP-099 — a US *start* on a month-end that is not the 31st (February
//!   28/29).** RESOLVED: the "last day of a month → 30" clause **does** fire for
//!   a February *start* — `DAYS360(2011-02-28, 2011-03-31)` = 30 and
//!   `DAYS360(2012-02-29, 2012-03-31)` = 30 (start → 30, end 31 → 30). The
//!   symmetric *end*-side February month-end (a month-end below the 30th on the
//!   **end** date, e.g. `DAYS360(2011-01-15, 2011-02-28)`) was **not** probed by
//!   the run, so it **remains deferred** to `#UNSUPPORTED!` rather than assumed
//!   symmetric — never guessed.
//! - **OXP-100 — an end date on the last day of a 30-day month (Apr/Jun/Sep/Nov
//!   30) with the start earlier than the 30th.** RESOLVED: the end does **not**
//!   roll to the "1st day of the next month" — `DAYS360(2011-01-15, 2011-04-30)`
//!   = 105, i.e. the 30-day-month-end is used as day 30 directly. The prose's
//!   roll-to-next-month clause is therefore not implemented for either the 30th
//!   or (per the `31-Dec` example) the 31st.
//!
//! # Errors
//! An error `start_date`/`end_date` propagates; an out-of-range serial → `#NUM!`;
//! serial 0 ("January 0, 1900") → `#UNSUPPORTED!` (`OXP-090`), all via
//! [`crate::date_common`]. A non-`"TRUE"`/`"FALSE"` text `method` → `#VALUE!`
//! (the frozen [`to_bool`] rule).

use xl_value::{ErrorKind, Value, to_bool};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::date_common::{coerce_serial, map_date_error};
use crate::datecore::{DateSystem, serial_to_ymd};

/// Evaluate a `DAYS360(start_date, end_date, [method])` call. See the module docs
/// for the pinned-vs-deferred day-adjustment rules.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let system = ctx.date_system();

    let (y1, m1, d1) = match resolve(&args.eval_scalar(0), system) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };
    let (y2, m2, d2) = match resolve(&args.eval_scalar(1), system) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };

    // method: omitted (count < 3) → US; otherwise boolean-coerce arg 2.
    let european = if args.count() >= 3 {
        match to_bool(&args.eval_scalar(2)) {
            Ok(b) => b,
            Err(k) => return Value::Error(k),
        }
    } else {
        false
    };

    let (d1a, d2a) = if european {
        european_adjust(d1, d2)
    } else {
        match us_adjust(y1, m1, d1, y2, m2, d2, system) {
            Ok(pair) => pair,
            Err(k) => return Value::Error(k),
        }
    };

    let result = (y2 as i64 - y1 as i64) * 360 + (m2 as i64 - m1 as i64) * 30 + (d2a - d1a);
    Value::number(result as f64)
}

/// Coerce a date argument to a serial (scalar path, floored) and resolve it to
/// `(year, month, day)` in `system`, mapping a [`crate::datecore::DateError`] to
/// its Excel error kind.
fn resolve(value: &Value, system: DateSystem) -> Result<(i32, u32, u32), ErrorKind> {
    let serial = coerce_serial(value)?;
    serial_to_ymd(serial, system).map_err(map_date_error)
}

/// European day adjustment: the 31st of any month becomes the 30th (both dates).
fn european_adjust(d1: u32, d2: u32) -> (i64, i64) {
    let a = if d1 == 31 { 30 } else { d1 };
    let b = if d2 == 31 { 30 } else { d2 };
    (a as i64, b as i64)
}

/// US (NASD) day adjustment — the pinned 31st-day clauses, deferring the two
/// unverifiable month-end edges (see module docs, `OXP-099`/`OXP-100`).
fn us_adjust(
    y1: i32,
    m1: u32,
    d1: u32,
    y2: i32,
    m2: u32,
    d2: u32,
    system: DateSystem,
) -> Result<(i64, i64), ErrorKind> {
    let start_is_month_end = d1 == days_in_month(y1, m1, system);
    let end_is_month_end = d2 == days_in_month(y2, m2, system);

    // OXP-099 (RUN-2026-07-11-oracle01): a START on the last day of a month
    // becomes the 30th, now confirmed for a February start (the only month-end
    // below the 30th) — DAYS360(2011-02-28,2011-03-31)=30,
    // DAYS360(2012-02-29,2012-03-31)=30. The symmetric END-side February
    // month-end (a month-end below the 30th on the *end* date) was NOT probed by
    // the run, so it stays deferred rather than guessed.
    if end_is_month_end && d2 < 30 {
        return Err(ErrorKind::Unsupported); // OXP-099 end-side February — unprobed
    }

    // Start on the last day of its month → 30: the pinned 31st clause plus the
    // oracle-confirmed February 28/29 last day (and, by the same rule, the
    // phantom 29-Feb-1900 and any 30-day-month-end start, which is a no-op).
    let d1a: i64 = if start_is_month_end { 30 } else { d1 as i64 };

    // OXP-100 (RUN-2026-07-11-oracle01): an END on the last day of a 30-day
    // month with an earlier start does NOT roll to the 1st of the next month —
    // DAYS360(2011-01-15,2011-04-30)=105 keeps the end as day 30. So the prose's
    // "1st day of next month" clause is not implemented; the "otherwise → 30th"
    // clause below only ever touches day 31 (when the adjusted start is the 30th;
    // an earlier start leaves the 31st in place, per the 31-Dec example).
    let d2a: i64 = if d2 == 31 && d1a == 30 { 30 } else { d2 as i64 };

    Ok((d1a, d2a))
}

/// Number of days in month `month` of `year`, in the workbook's date system.
///
/// February is 29 in a leap year; additionally, in the 1900 date system Excel
/// treats 1900 itself as a leap year (the Lotus-1-2-3 phantom 29-Feb-1900,
/// serial 60), so February 1900 has 29 days there. `month` is always `1..=12`
/// (it comes from [`serial_to_ymd`]).
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

/// Proleptic-Gregorian leap-year test (used for real February lengths; the
/// Excel-1900 phantom is handled separately in [`days_in_month`]).
fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datecore::date_to_serial;
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::{ErrorKind, Value};

    /// A `Value::Number` holding the 1900-system serial of the given calendar
    /// date (the default date system `eval_direct` uses). Derived through
    /// `date_to_serial` rather than a hand-typed serial so the test is robust to
    /// serial arithmetic.
    fn serial(y: i64, m: i64, d: i64) -> Value {
        num(date_to_serial(y, m, d, DateSystem::Excel1900).unwrap() as f64)
    }

    /// Convenience: call DAYS360 with two dates and an optional method arg.
    fn days360(args: Vec<crate::test_support::TestArg>) -> Value {
        eval_direct(super::eval, args)
    }

    // ---- Microsoft Learn worked examples (the page's example grid) ----------

    #[test]
    fn ms_example_1jan_1feb_is_30() {
        // DAYS360(1-Jan-11, 1-Feb-11) = 30. Neither date is a month-end.
        let out = days360(vec![Scalar(serial(2011, 1, 1)), Scalar(serial(2011, 2, 1))]);
        assert_eq!(out, num(30.0));
    }

    #[test]
    fn ms_example_30jan_1feb_is_1() {
        // DAYS360(30-Jan-11, 1-Feb-11) = 1.
        let out = days360(vec![
            Scalar(serial(2011, 1, 30)),
            Scalar(serial(2011, 2, 1)),
        ]);
        assert_eq!(out, num(1.0));
    }

    #[test]
    fn ms_example_full_year_is_360() {
        // DAYS360(1-Jan-11, 31-Dec-11) = 360. This is the example that pins the
        // 31st-end/early-start behavior: the 31-Dec end STAYS day 31 (giving
        // 330 + 30), it is NOT rolled to the "1st of next month".
        let out = days360(vec![
            Scalar(serial(2011, 1, 1)),
            Scalar(serial(2011, 12, 31)),
        ]);
        assert_eq!(out, num(360.0));
    }

    // ---- US 31st-day start adjustment ---------------------------------------

    #[test]
    fn us_start_on_31st_becomes_30() {
        // DAYS360(31-Jan-11, 15-Mar-11): start 31 → 30.
        // (0)·360 + (3-1)·30 + (15-30) = 60 - 15 = 45.
        let out = days360(vec![
            Scalar(serial(2011, 1, 31)),
            Scalar(serial(2011, 3, 15)),
        ]);
        assert_eq!(out, num(45.0));
    }

    // ---- European vs US divergence on a 31st end date -----------------------

    #[test]
    fn european_vs_us_on_31st_end_with_early_start() {
        // DAYS360(15-Jan-11, 31-Mar-11):
        //   US   : end 31 stays 31 (start 15 < 30) → 60 + 16 = 76.
        //   Euro : end 31 → 30                      → 60 + 15 = 75.
        let us = days360(vec![
            Scalar(serial(2011, 1, 15)),
            Scalar(serial(2011, 3, 31)),
        ]);
        assert_eq!(us, num(76.0), "US method");

        let eu = days360(vec![
            Scalar(serial(2011, 1, 15)),
            Scalar(serial(2011, 3, 31)),
            Scalar(Value::Bool(true)),
        ]);
        assert_eq!(eu, num(75.0), "European method");
    }

    #[test]
    fn method_number_coerces_via_to_bool() {
        // A non-zero numeric method (2) coerces to TRUE → European (75), matching
        // to_bool's numeric rule.
        let eu = days360(vec![
            Scalar(serial(2011, 1, 15)),
            Scalar(serial(2011, 3, 31)),
            Scalar(num(2.0)),
        ]);
        assert_eq!(eu, num(75.0));
    }

    // ---- month / year rollover ----------------------------------------------

    #[test]
    fn exact_year_rollover_is_360() {
        // DAYS360(15-Jan-11, 15-Jan-12) = one 360-day year.
        let out = days360(vec![
            Scalar(serial(2011, 1, 15)),
            Scalar(serial(2012, 1, 15)),
        ]);
        assert_eq!(out, num(360.0));
    }

    // ---- error propagation ---------------------------------------------------

    #[test]
    fn error_argument_propagates() {
        let out = days360(vec![
            Scalar(Value::Error(ErrorKind::Div0)),
            Scalar(serial(2011, 2, 1)),
        ]);
        assert_eq!(out, Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn text_method_is_value_error() {
        // A non-"TRUE"/"FALSE" text method → #VALUE! (frozen to_bool rule).
        let out = days360(vec![
            Scalar(serial(2011, 1, 15)),
            Scalar(serial(2011, 3, 31)),
            Scalar(Value::text("yes")),
        ]);
        assert_eq!(out, Value::Error(ErrorKind::Value));
    }

    // ---- OXP-099: US February *start* month-end → 30 (RUN-2026-07-11-oracle01)

    #[test]
    fn oxp099_us_february_start_month_end_becomes_30() {
        // =DAYS360("2011-02-28","2011-03-31") -> 30 (non-leap Feb-end start → 30,
        // Mar 31 end → 30): (3-2)·30 + (30-30) = 30.
        assert_eq!(
            days360(vec![
                Scalar(serial(2011, 2, 28)),
                Scalar(serial(2011, 3, 31)),
            ]),
            num(30.0)
        );
        // =DAYS360("2012-02-29","2012-03-31") -> 30 (leap Feb-end start → 30).
        assert_eq!(
            days360(vec![
                Scalar(serial(2012, 2, 29)),
                Scalar(serial(2012, 3, 31)),
            ]),
            num(30.0)
        );
    }

    #[test]
    fn us_february_month_end_end_still_defers() {
        // The END-side February month-end was NOT probed by the run, so it
        // stays #UNSUPPORTED! (never guessed by symmetry with the start rule).
        let out = days360(vec![
            Scalar(serial(2011, 1, 15)),
            Scalar(serial(2011, 2, 28)),
        ]);
        assert_eq!(out, Value::Error(ErrorKind::Unsupported));
    }

    #[test]
    fn european_february_month_end_is_computed_not_deferred() {
        // The European method has no month-end handling at all, so a February
        // month-end is computed normally.
        // DAYS360(28-Feb-11, 15-Apr-11, TRUE) = (4-2)·30 + (15-28) = 60 - 13 = 47.
        let out = days360(vec![
            Scalar(serial(2011, 2, 28)),
            Scalar(serial(2011, 4, 15)),
            Scalar(Value::Bool(true)),
        ]);
        assert_eq!(out, num(47.0));
    }

    // ---- OXP-100: US 30-day-month-end end, early start (RUN-2026-07-11-oracle01)

    #[test]
    fn oxp100_us_30day_month_end_end_with_early_start_no_rollover() {
        // =DAYS360("2011-01-15","2011-04-30") -> 105: the 30-Apr end is used as
        // day 30 (NOT rolled to 1-May): (4-1)·30 + (30-15) = 105.
        assert_eq!(
            days360(vec![
                Scalar(serial(2011, 1, 15)),
                Scalar(serial(2011, 4, 30)),
            ]),
            num(105.0)
        );
    }

    #[test]
    fn european_30day_month_end_end_is_computed() {
        // European: 30-Apr is not the 31st, so no adjustment.
        // DAYS360(15-Jan-11, 30-Apr-11, TRUE) = (4-1)·30 + (30-15) = 90 + 15 = 105.
        let out = days360(vec![
            Scalar(serial(2011, 1, 15)),
            Scalar(serial(2011, 4, 30)),
            Scalar(Value::Bool(true)),
        ]);
        assert_eq!(out, num(105.0));
    }

    #[test]
    fn us_30day_month_end_end_with_late_start_is_computed() {
        // Start 31-Jan → 30 (>= 30), so the OXP-100 clause does NOT fire and the
        // end (30-Apr) is a prose no-op: computed normally.
        // start 31→30; (4-1)·30 + (30-30) = 90.
        let out = days360(vec![
            Scalar(serial(2011, 1, 31)),
            Scalar(serial(2011, 4, 30)),
        ]);
        assert_eq!(out, num(90.0));
    }

    #[test]
    fn us_30day_month_end_start_is_computed_not_deferred() {
        // A 30-day-month-end START (30-Apr) is a prose no-op → computed.
        // DAYS360(30-Apr-11, 31-May-11): end 31 stays 31 (start 30 == 30 → 30th
        // clause fires: 31 → 30). (5-4)·30 + (30-30) = 30.
        let out = days360(vec![
            Scalar(serial(2011, 4, 30)),
            Scalar(serial(2011, 5, 31)),
        ]);
        assert_eq!(out, num(30.0));
    }

    // ---- out-of-range / January-zero coercion errors ------------------------

    #[test]
    fn january_zero_serial_defers() {
        // Serial 0 in the 1900 system ("January 0, 1900") → #UNSUPPORTED! (OXP-090).
        let out = days360(vec![Scalar(num(0.0)), Scalar(serial(2011, 2, 1))]);
        assert_eq!(out, Value::Error(ErrorKind::Unsupported));
    }

    #[test]
    fn negative_serial_is_num_error() {
        let out = days360(vec![Scalar(num(-5.0)), Scalar(serial(2011, 2, 1))]);
        assert_eq!(out, Value::Error(ErrorKind::Num));
    }
}

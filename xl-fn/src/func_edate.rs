//! `EDATE` — a date `months` months before/after `start_date`, keeping the
//! same day-of-month (clamped to the shifted month's last day when that
//! month is shorter).
//!
//! # Provenance
//! Behavior contract: `docs/specs/EDATE.md` (which cites the Microsoft Learn
//! EDATE page). This is `EOMONTH`'s sibling: it reuses the identical
//! month-arithmetic core in [`crate::datecore`] —
//! [`crate::datecore::eomonth_serial`], [`crate::datecore::serial_to_ymd`],
//! [`crate::datecore::date_to_serial`] — and the same `xl-value` coercion via
//! [`crate::date_common`]. Only the final day-of-month selection differs from
//! [`crate::func_eomonth`]: EOMONTH always picks the shifted month's last
//! day; EDATE picks the *start date's* day, clamped down to the shifted
//! month's last day when that month is shorter.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `start_date` is a serial (scalar-coerced, floored to a whole day);
//!   `months` is an integer offset, positive or negative (EDATE.md §1-3).
//!   The date system comes from the [`EvalContext`].
//! - Returns the same day-of-month as `start_date`, shifted by `months`
//!   whole months, **clamped** to the shifted month's last day when that
//!   month has fewer days (EDATE.md §4 — Microsoft's own examples only cover
//!   the unclamped case, but the clamp is required to keep the result a
//!   valid calendar date at all, and matches `EOMONTH`'s month-length
//!   arithmetic exactly): `EDATE(DATE(2020,1,31),1)` → 2020-02-29 (2020 is a
//!   real leap year), not an overflow into March.
//! - The clamp ceiling is computed by delegating to [`eomonth_serial`] for
//!   the shifted month — the identical machinery `EOMONTH` uses — so the
//!   1900 fake-leap-day seam (Feb 1900 = 29 days) is inherited for free:
//!   `EDATE(DATE(1900,1,31),1)` lands on the phantom serial 60, exactly like
//!   `EOMONTH(DATE(1900,1,1),1)` does.
//! - A `start_date` before the epoch/past 9999, or a shift that pushes the
//!   resulting month out of range, → `#NUM!`; a `start_date` of serial 0
//!   ("January 0, 1900") → `#UNSUPPORTED!` (`OXP-090`, the same deferral
//!   `EOMONTH` uses) (EDATE.md §Error behavior).
//!
//! # Non-integer `months` — RESOLVED (`OXP-136`, RUN-2026-07-11-oracle01)
//! EDATE **truncates `months` toward zero** (`TRUNC`), matching `EOMONTH`
//! (`OXP-092`) and **not** flooring: `EDATE("2020-01-15",-1.5)` = 43814
//! (2019-12-15, months → −1), **not** −2 (2019-11-15 = 43784); and the positive
//! `EDATE("2020-01-31",1.5)` = `EDATE("2020-01-31",1.9)` = 43890 (2020-02-29,
//! months → 1). Truncation happens in [`coerce_int_trunc`]. (`DATE` floors
//! instead — the month-offset path and the year/month/day path genuinely
//! differ; each is pinned from its own probe.)

use xl_value::Value;

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::date_common::{coerce_int_trunc, coerce_serial, map_date_error};
use crate::datecore::{date_to_serial, eomonth_serial, serial_to_ymd};

/// Evaluate an `EDATE(start_date, months)` call. See the module docs.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let start = match coerce_serial(&args.eval_scalar(0)) {
        Ok(s) => s,
        Err(k) => return Value::Error(k),
    };
    let months = match coerce_int_trunc(&args.eval_scalar(1)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let system = ctx.date_system();

    // The day-of-month to preserve, read off the (validated) start serial.
    let day = match serial_to_ymd(start, system) {
        Ok((_, _, d)) => d,
        Err(e) => return Value::Error(map_date_error(e)),
    };

    // Last day of the shifted month — EOMONTH's exact machinery — gives both
    // the target (year, month) and the clamp ceiling in one call, inheriting
    // the 1900 phantom-day seam for free.
    let last_of_shifted = match eomonth_serial(start, months, system) {
        Ok(s) => s,
        Err(e) => return Value::Error(map_date_error(e)),
    };
    let (year, month, last_day) = match serial_to_ymd(last_of_shifted, system) {
        Ok(t) => t,
        Err(e) => return Value::Error(map_date_error(e)),
    };

    let clamped_day = day.min(last_day);
    match date_to_serial(year as i64, month as i64, clamped_day as i64, system) {
        Ok(serial) => Value::number(serial as f64),
        Err(e) => Value::Error(map_date_error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datecore::DateSystem;
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::ErrorKind;

    /// Serial for a real (verifiable) calendar date, via the same
    /// `date_to_serial` the implementation itself is built on — mirrors the
    /// pattern `datecore`'s own tests use to derive expected serials.
    fn s(year: i64, month: i64, day: i64) -> f64 {
        date_to_serial(year, month, day, DateSystem::Excel1900).unwrap() as f64
    }

    #[test]
    fn same_day_forward_one_month() {
        // EDATE.md example: EDATE(15-Jan-11,1) = 15-Feb-11 (shape confirmed
        // here on 2020-01-15 -> 2020-02-15, an ordinary same-day shift).
        let v = eval_direct(eval, vec![Scalar(num(s(2020, 1, 15))), Scalar(num(1.0))]);
        assert_eq!(v, num(s(2020, 2, 15)));
    }

    #[test]
    fn leap_year_day_clamp() {
        // EDATE(2020-01-31, 1): Feb 2020 (a real leap year) has only 29 days,
        // so day 31 clamps down to 29 rather than overflowing into March.
        let v = eval_direct(eval, vec![Scalar(num(s(2020, 1, 31))), Scalar(num(1.0))]);
        assert_eq!(v, num(s(2020, 2, 29)));
    }

    #[test]
    fn non_leap_year_day_clamp() {
        // EDATE(2021-01-31, 1): Feb 2021 is not a leap year -> clamps to 28.
        let v = eval_direct(eval, vec![Scalar(num(s(2021, 1, 31))), Scalar(num(1.0))]);
        assert_eq!(v, num(s(2021, 2, 28)));
    }

    #[test]
    fn negative_months() {
        // EDATE.md example: EDATE(15-Jan-11,-1) = 15-Dec-10 (shape check).
        let v = eval_direct(eval, vec![Scalar(num(s(2020, 1, 15))), Scalar(num(-1.0))]);
        assert_eq!(v, num(s(2019, 12, 15)));
    }

    #[test]
    fn negative_months_with_day_clamp() {
        // EDATE(2020-03-31, -1): Feb 2020 has 29 days -> clamps to 29.
        let v = eval_direct(eval, vec![Scalar(num(s(2020, 3, 31))), Scalar(num(-1.0))]);
        assert_eq!(v, num(s(2020, 2, 29)));
    }

    #[test]
    fn year_rollover_forward() {
        // EDATE(2020-12-15, 1) = 2021-01-15.
        let v = eval_direct(eval, vec![Scalar(num(s(2020, 12, 15))), Scalar(num(1.0))]);
        assert_eq!(v, num(s(2021, 1, 15)));
    }

    #[test]
    fn year_rollover_backward_multi_year() {
        // EDATE(2020-01-15, -13) crosses a year boundary by more than 12
        // months = 2018-12-15.
        let v = eval_direct(eval, vec![Scalar(num(s(2020, 1, 15))), Scalar(num(-13.0))]);
        assert_eq!(v, num(s(2018, 12, 15)));
    }

    #[test]
    fn zero_months_is_identity() {
        let v = eval_direct(eval, vec![Scalar(num(s(2020, 6, 10))), Scalar(num(0.0))]);
        assert_eq!(v, num(s(2020, 6, 10)));
    }

    #[test]
    fn feb_1900_phantom_day_clamp() {
        // Start day 31 (Jan 1900 has 31 real days), shifted +1 month lands on
        // Feb 1900 which Excel's 1900 leap-year bug gives 29 days (the same
        // headline case EOMONTH.md documents) -> clamps to the fictitious
        // serial 60, i.e. "1900-02-29".
        let jan31 = date_to_serial(1900, 1, 31, DateSystem::Excel1900).unwrap();
        let v = eval_direct(eval, vec![Scalar(num(jan31 as f64)), Scalar(num(1.0))]);
        assert_eq!(v, num(60.0));
    }

    // ---- OXP-136: months TRUNCATES toward zero (RUN-2026-07-11-oracle01) -----

    #[test]
    fn oxp136_positive_fractional_months_truncate() {
        // =EDATE("2020-01-31",1.5) -> 43890 and =EDATE("2020-01-31",1.9) -> 43890
        // (months -> 1; Jan 31 + 1 month clamps to Feb 29 2020).
        let start = s(2020, 1, 31);
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(start)), Scalar(num(1.5))]),
            num(43890.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(start)), Scalar(num(1.9))]),
            num(43890.0)
        );
        assert_eq!(num(43890.0), num(s(2020, 2, 29)));
    }

    #[test]
    fn oxp136_negative_fractional_months_truncate_toward_zero() {
        // =EDATE("2020-01-15",-1.5) -> 43814 (months -> -1, 2019-12-15; NOT -2
        // which is 2019-11-15 = 43784).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(s(2020, 1, 15))), Scalar(num(-1.5))]),
            num(43814.0)
        );
        assert_eq!(num(43814.0), num(s(2019, 12, 15)));
    }

    #[test]
    fn start_date_error_propagates() {
        let v = eval_direct(
            eval,
            vec![Scalar(Value::Error(ErrorKind::Div0)), Scalar(num(1.0))],
        );
        assert_eq!(v, Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn months_error_propagates() {
        let v = eval_direct(
            eval,
            vec![
                Scalar(num(s(2020, 1, 15))),
                Scalar(Value::Error(ErrorKind::Ref)),
            ],
        );
        assert_eq!(v, Value::Error(ErrorKind::Ref));
    }

    #[test]
    fn january_zero_start_date_is_oracle_deferred() {
        // Serial 0 in the 1900 system ("January 0, 1900") is OXP-090,
        // exactly as EOMONTH defers it.
        let v = eval_direct(eval, vec![Scalar(num(0.0)), Scalar(num(1.0))]);
        assert_eq!(v, Value::Error(ErrorKind::Unsupported));
    }
}

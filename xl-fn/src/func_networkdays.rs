//! `NETWORKDAYS` — the count of whole *working* days (Mon–Fri, excluding
//! weekends and an optional holiday list) in the inclusive span between two
//! dates.
//!
//! # Provenance
//! Clean-room from Microsoft's public NETWORKDAYS pages (the same discipline as
//! the sibling `WORKDAY` — the clean-room rule "no code copied from GPL implementations",
//! clean-room only, public sources only):
//! - `https://support.microsoft.com/en-us/office/networkdays-function-48e717bf-a7a3-495f-969e-5005e3eb18e7`
//!   (verified 2026-07-11) — "Returns the number of whole working days between
//!   start_date and end_date. Working days exclude weekends and any dates
//!   identified in holidays." Documented example: `NETWORKDAYS(10/1/2012,
//!   3/1/2013)` = 110; with one holiday 109; with three holidays 107.
//! - `https://learn.microsoft.com/en-us/office/vba/api/excel.worksheetfunction.networkdays`
//!   (VBA reference, verified 2026-07-11) — "If any argument is not a valid
//!   date, NetworkDays returns the #VALUE! error value."
//!
//! Behavior contract: `docs/specs/NETWORKDAYS.md`. The serial↔calendar
//! validation, the 1900 leap-year seam, the Saturday/Sunday weekend test, and
//! the holiday-argument parsing are the **same machinery** as
//! [`crate::func_workday`], reusing [`crate::datecore`]/[`crate::date_common`];
//! the weekend anchor (serial 1 = Sunday) is the one `WEEKDAY`/`WORKDAY` pin.
//!
//! # Semantics implemented
//! - `NETWORKDAYS(start_date, end_date, [holidays])`; `holidays` optional.
//! - **Inclusive count.** The result is the number of whole working days in the
//!   closed interval `[start_date, end_date]` — **both endpoints are counted**
//!   when they are working days. So `NETWORKDAYS(Mon, Mon)` = 1 (a single
//!   weekday) and `NETWORKDAYS(Mon, Fri)` = 5 (a full Mon–Fri week).
//! - **Weekends are Saturday and Sunday.** Identical to `WORKDAY`: the MS page
//!   says only "exclude weekends" without naming the days, and Saturday/Sunday
//!   is NETWORKDAYS's fixed weekend (the configurable weekend is the separate
//!   `NETWORKDAYS.INTL`, out of scope). The weekday is derived from the raw
//!   serial with **serial 1 (1900-01-01) = Sunday** (see [`is_weekend`]), so
//!   the phantom 1900-02-29 does not perturb the weekly cycle.
//! - **holidays** (optional): a range/array/scalar of date serials excluded from
//!   the count exactly like weekends, streamed with
//!   [`CallArgs::for_each_cell`] (materialized cells only). Each numeric cell's
//!   time-of-day component is dropped (floored to a whole day, mirroring the
//!   endpoints). A holiday outside `[start_date, end_date]`, on a weekend, or
//!   repeated has **no additional effect** — counting walks each calendar day
//!   once and tests membership, so duplicates and weekend/out-of-range holidays
//!   deduplicate automatically. A **blank** holiday cell is ignored (stated
//!   choice; `for_each_cell` elides range blanks and an explicit `Blank` is
//!   skipped too). An **error** holiday cell propagates. A **non-numeric**
//!   (text/logical) holiday cell → `#VALUE!` — the same rule the oracle pinned
//!   for `WORKDAY` (RUN-2026-07-11-oracle01, `OXP-138`): a text date-literal or
//!   logical in `holidays` is not coerced; Excel rejects the call with
//!   `#VALUE!`. `holidays` omitted (`args.count() < 3`) → no holidays.
//! - **Coercion / errors.**
//!   - `start_date`, `end_date` — scalar numeric coercion, floored to a whole
//!     day ([`coerce_serial`]); a non-numeric/non-parseable value is `#VALUE!`
//!     (the documented "not a valid date" case — dates entered as text); a
//!     magnitude past the safety rail is `#NUM!`. The floored serial must
//!     resolve to a real date in the active date system (out-of-range → `#NUM!`;
//!     serial 0 in 1900 → `#UNSUPPORTED!`, `OXP-090`), identical to
//!     `WORKDAY`/`WEEKDAY`/`YEAR`.
//!   - An **error** in any argument propagates (leftmost first: `start_date`,
//!     then `end_date`, then a holiday cell).
//! - **`end_date < start_date` (the reversed span) is DEFERRED.** Excel is
//!   widely believed to return a *negative* count here, but **neither** MS
//!   source documents the sign (both the support page and the VBA reference are
//!   silent on start-after-end), and no oracle probe has resolved it. Per
//!   Recalc Principle 2 ("never silently wrong"; guessing is forbidden) this
//!   is returned as `#UNSUPPORTED!` rather than a guessed negative — see the
//!   deferral marker below and `docs/specs/NETWORKDAYS.md`.

use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::date_common::{coerce_serial, map_date_error};
use crate::datecore::serial_to_ymd;

/// Is `serial` a Saturday or Sunday? Anchored so serial 1 (1900-01-01) is
/// Sunday, matching [`crate::func_weekday`]/[`crate::func_workday`] (serial
/// 43831 = 2020-01-01, a real-world Wednesday, is the cross-check). `dow`: 0 =
/// Sunday … 6 = Saturday. Computed on the raw serial, so the phantom 1900-02-29
/// (a label on a real sequential serial) does not perturb the weekly cycle.
fn is_weekend(serial: i64) -> bool {
    let dow = (serial - 1).rem_euclid(7);
    dow == 0 || dow == 6
}

/// Coerce and validate one date endpoint: scalar serial coercion (floors the
/// time-of-day component), then confirm it resolves to a real date in the active
/// date system. `Err` carries the Excel error to surface (`#VALUE!` for a
/// non-date, `#NUM!` for an out-of-range serial, `#UNSUPPORTED!` for serial 0 in
/// 1900), letting the caller propagate leftmost-first.
fn eval_endpoint(
    ctx: &EvalContext,
    args: &mut dyn CallArgs,
    index: usize,
) -> Result<i64, ErrorKind> {
    let serial = coerce_serial(&args.eval_scalar(index))?;
    match serial_to_ymd(serial, ctx.date_system()) {
        Ok(_) => Ok(serial),
        Err(e) => Err(map_date_error(e)),
    }
}

/// Evaluate a `NETWORKDAYS(start_date, end_date, [holidays])` call. See the
/// module docs.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // start_date / end_date: floored serials that must each be a real date. An
    // error value (or a non-date / out-of-range serial) propagates leftmost
    // first: start_date, then end_date.
    let start = match eval_endpoint(ctx, args, 0) {
        Ok(s) => s,
        Err(k) => return Value::Error(k),
    };
    let end = match eval_endpoint(ctx, args, 1) {
        Ok(s) => s,
        Err(k) => return Value::Error(k),
    };

    // holidays (optional): materialize the skip set. Blanks ignored; errors
    // propagate; non-numeric cells → #VALUE! (the WORKDAY OXP-138 rule). All
    // holiday cells are validated *before* the reversed-span deferral so a
    // genuine argument error always surfaces, matching Excel's evaluate-args-
    // first order.
    let mut holidays: Vec<i64> = Vec::new();
    let mut holiday_err: Option<ErrorKind> = None;
    if args.count() >= 3 {
        args.for_each_cell(2, &mut |v| match v {
            // Drop any time-of-day component, mirroring the endpoints.
            Value::Number(n) => {
                holidays.push(n.floor() as i64);
                ControlFlow::Continue(())
            }
            // Stated choice: a blank holiday cell contributes nothing.
            Value::Blank => ControlFlow::Continue(()),
            // An error holiday cell propagates.
            Value::Error(k) => {
                holiday_err = Some(*k);
                ControlFlow::Break(())
            }
            // BC-6 (RFC-0012): a lambda holiday cell is an unsupported *type*,
            // NOT the OXP-138 text/logical case — refuse with `#UNSUPPORTED!`.
            // OXP-138 probed text/number/logical only (no lambda), so no oracle
            // pins `#VALUE!` here; default to `#UNSUPPORTED!` per Principle 2.
            Value::Lambda(_) => {
                holiday_err = Some(ErrorKind::Unsupported);
                ControlFlow::Break(())
            }
            // OXP-138 (shared with WORKDAY): a text/logical holiday cell is not
            // coerced — Excel rejects the whole call with #VALUE!.
            _ => {
                holiday_err = Some(ErrorKind::Value);
                ControlFlow::Break(())
            }
        });
        if let Some(k) = holiday_err {
            return Value::Error(k);
        }
    }

    // Reversed span: Excel's sign for start_date > end_date is undocumented and
    // unprobed, so it is refused rather than guessed (Recalc Principle 2).
    // OXP (unassigned): end_date < start_date returns what? The community holds
    // it is a negative count (hypothesis: negate the inclusive working-day count
    // of [end_date, start_date]), but neither the MS support page nor the VBA
    // reference states the sign, and no farm run has confirmed it. Probe:
    // =NETWORKDAYS(DATE(2020,1,10),DATE(2020,1,6)) — hypothesis -5. Until run,
    // this path returns #UNSUPPORTED!.
    if end < start {
        return Value::Error(ErrorKind::Unsupported);
    }

    // Inclusive count over [start, end]: each calendar day that is neither a
    // weekend nor a listed holiday is one working day. Walking day-by-day and
    // testing membership makes duplicate / weekend / out-of-range holidays
    // deduplicate for free. Both endpoints are validated in range, so the walk
    // is bounded by the representable serial domain (~3M days at most).
    let mut count: i64 = 0;
    for serial in start..=end {
        if !is_weekend(serial) && !holidays.contains(&serial) {
            count += 1;
        }
    }
    Value::number(count as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::ErrorKind;

    // DATE-derived serials in the 1900 system, hand-verifiable from the pinned
    // anchor serial 43831 = 2020-01-01 (a Wednesday):
    //   43833 = 2020-01-03  Friday
    //   43834 = 2020-01-04  Saturday
    //   43835 = 2020-01-05  Sunday
    //   43836 = 2020-01-06  Monday
    //   43837 = 2020-01-07  Tuesday
    //   43838 = 2020-01-08  Wednesday
    //   43840 = 2020-01-10  Friday
    //   43843 = 2020-01-13  Monday
    const FRI_2020_01_03: i64 = 43833;
    const SAT_2020_01_04: i64 = 43834;
    const MON_2020_01_06: i64 = 43836;
    const WED_2020_01_08: i64 = 43838;
    const FRI_2020_01_10: i64 = 43840;
    const MON_2020_01_13: i64 = 43843;

    // ---- weekend anchor -----------------------------------------------------

    #[test]
    fn weekend_anchor_against_known_dates() {
        assert!(!is_weekend(FRI_2020_01_03)); // Friday
        assert!(is_weekend(SAT_2020_01_04)); // Saturday
        assert!(is_weekend(SAT_2020_01_04 + 1)); // Sunday
        assert!(!is_weekend(MON_2020_01_06)); // Monday
        assert!(is_weekend(1)); // serial 1 = 1900-01-01 = Sunday
    }

    // ---- inclusive counting -------------------------------------------------

    #[test]
    fn same_day_weekday_is_one() {
        // NETWORKDAYS(Mon, Mon): a single weekday, counted inclusively.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(MON_2020_01_06 as f64)),
                Scalar(num(MON_2020_01_06 as f64)),
            ],
        );
        assert_eq!(got, num(1.0));
    }

    #[test]
    fn same_day_weekend_is_zero() {
        // NETWORKDAYS(Sat, Sat): the lone day is a weekend → no working days.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(SAT_2020_01_04 as f64)),
                Scalar(num(SAT_2020_01_04 as f64)),
            ],
        );
        assert_eq!(got, num(0.0));
    }

    #[test]
    fn full_week_mon_to_fri_is_five() {
        // NETWORKDAYS(Mon 01-06, Fri 01-10): Mon,Tue,Wed,Thu,Fri = 5.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(MON_2020_01_06 as f64)),
                Scalar(num(FRI_2020_01_10 as f64)),
            ],
        );
        assert_eq!(got, num(5.0));
    }

    #[test]
    fn span_crossing_a_weekend_excludes_sat_sun() {
        // NETWORKDAYS(Fri 01-03, Mon 01-06): Fri, [Sat, Sun excluded], Mon = 2.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(FRI_2020_01_03 as f64)),
                Scalar(num(MON_2020_01_06 as f64)),
            ],
        );
        assert_eq!(got, num(2.0));
    }

    // ---- holidays -----------------------------------------------------------

    #[test]
    fn holiday_inside_range_reduces_count() {
        // NETWORKDAYS(Mon 01-06, Fri 01-10, {Wed 01-08}): 5 weekdays − 1 = 4.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(MON_2020_01_06 as f64)),
                Scalar(num(FRI_2020_01_10 as f64)),
                Range(vec![num(WED_2020_01_08 as f64)]),
            ],
        );
        assert_eq!(got, num(4.0));
    }

    #[test]
    fn holiday_outside_range_has_no_effect() {
        // NETWORKDAYS(Mon 01-06, Fri 01-10, {Mon 01-13}): the holiday is past
        // end_date, so the count is the full week = 5.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(MON_2020_01_06 as f64)),
                Scalar(num(FRI_2020_01_10 as f64)),
                Range(vec![num(MON_2020_01_13 as f64)]),
            ],
        );
        assert_eq!(got, num(5.0));
    }

    #[test]
    fn holiday_on_a_weekend_has_no_effect() {
        // A holiday that lands on a Saturday (already excluded) changes nothing.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(FRI_2020_01_03 as f64)),
                Scalar(num(MON_2020_01_06 as f64)),
                Range(vec![num(SAT_2020_01_04 as f64)]),
            ],
        );
        assert_eq!(got, num(2.0));
    }

    #[test]
    fn duplicate_holiday_counts_once() {
        // The same holiday listed twice removes the day exactly once: 5 − 1 = 4.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(MON_2020_01_06 as f64)),
                Scalar(num(FRI_2020_01_10 as f64)),
                Range(vec![num(WED_2020_01_08 as f64), num(WED_2020_01_08 as f64)]),
            ],
        );
        assert_eq!(got, num(4.0));
    }

    #[test]
    fn blank_holiday_cell_is_ignored() {
        // A blank in the holiday range contributes nothing (stated choice).
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(MON_2020_01_06 as f64)),
                Scalar(num(FRI_2020_01_10 as f64)),
                Range(vec![Value::Blank, num(WED_2020_01_08 as f64)]),
            ],
        );
        assert_eq!(got, num(4.0));
    }

    // ---- reversed span (deferred) ------------------------------------------

    #[test]
    fn end_before_start_is_deferred_unsupported() {
        // OXP (unassigned): the sign of a reversed span is undocumented on MS
        // Learn and unprobed, so it is refused rather than guessed negative.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(FRI_2020_01_10 as f64)),
                Scalar(num(MON_2020_01_06 as f64)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Unsupported));
    }

    // ---- coercion / error propagation --------------------------------------

    #[test]
    fn non_integer_dates_truncate_toward_the_day() {
        // Fractional serials floor to the whole day (time-of-day dropped),
        // mirroring WORKDAY's endpoint handling: Mon.9 .. Fri.9 is still 5.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(MON_2020_01_06 as f64 + 0.9)),
                Scalar(num(FRI_2020_01_10 as f64 + 0.9)),
            ],
        );
        assert_eq!(got, num(5.0));
    }

    #[test]
    fn start_date_error_propagates() {
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(Value::Error(ErrorKind::Div0)),
                Scalar(num(MON_2020_01_06 as f64)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn end_date_error_propagates() {
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(MON_2020_01_06 as f64)),
                Scalar(Value::Error(ErrorKind::Na)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Na));
    }

    #[test]
    fn date_like_text_start_now_coerces() {
        // OXP-160 (RUN-2026-07-11-oracle01): a date-shaped text start
        // ("2020-01-06") now coerces through the frozen `to_number` contract to
        // its serial (43836 = MON_2020_01_06), so the call computes like the
        // all-numeric one instead of deferring #UNSUPPORTED! (the old OXP-001
        // hold). NETWORKDAYS("2020-01-06", Fri 01-10) = Mon..Fri = 5.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(Value::text("2020-01-06")),
                Scalar(num(FRI_2020_01_10 as f64)),
            ],
        );
        assert_eq!(got, num(5.0));
        // Equivalent to the same call with the numeric serial.
        let numeric = eval_direct(
            super::eval,
            vec![
                Scalar(num(MON_2020_01_06 as f64)),
                Scalar(num(FRI_2020_01_10 as f64)),
            ],
        );
        assert_eq!(got, numeric);
    }

    #[test]
    fn holiday_error_propagates() {
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(MON_2020_01_06 as f64)),
                Scalar(num(FRI_2020_01_10 as f64)),
                Range(vec![Value::Error(ErrorKind::Na)]),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Na));
    }

    #[test]
    fn non_numeric_holiday_is_value_error() {
        // OXP-138 (shared with WORKDAY, RUN-2026-07-11-oracle01): a text
        // date-literal holiday cell is not coerced — Excel returns #VALUE!.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(MON_2020_01_06 as f64)),
                Scalar(num(FRI_2020_01_10 as f64)),
                Range(vec![Value::text("2020-01-08")]),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Value));
        // A logical holiday cell also errors #VALUE! (mirrors the probe's TRUE).
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(MON_2020_01_06 as f64)),
                Scalar(num(FRI_2020_01_10 as f64)),
                Range(vec![Value::Bool(true)]),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Value));
    }

    // ---- domain edges -------------------------------------------------------

    #[test]
    fn out_of_range_start_is_num() {
        // A negative start serial is not a valid date.
        let got = eval_direct(
            super::eval,
            vec![Scalar(num(-5.0)), Scalar(num(MON_2020_01_06 as f64))],
        );
        assert_eq!(got, Value::Error(ErrorKind::Num));
    }
}

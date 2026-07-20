//! `WORKDAY` — the serial date a whole number of *working* days (Mon–Fri,
//! excluding weekends and an optional holiday list) before or after a start
//! date.
//!
//! # Provenance
//! Clean-room from Microsoft's public WORKDAY page (the same discipline as
//! every other function in this crate — the clean-room rule "no code copied from GPL
//! implementations", clean-room only, public sources only):
//! `https://support.microsoft.com/en-us/office/workday-function-f764a5b7-05fc-4494-9486-60d494efbf33`
//! (verified 2026-07-11). Behavior contract: `docs/specs/WORKDAY.md`. The
//! serial↔calendar validation and the 1900 leap-year seam reuse
//! [`crate::datecore`]/[`crate::date_common`] (identical domain rules to
//! `YEAR`/`MONTH`/`DAY`/`EOMONTH`); the weekend test is derived from the raw
//! **serial number**, reusing the anchor `WEEKDAY` already pinned.
//!
//! # Semantics implemented
//! - `WORKDAY(start_date, days, [holidays])`; `holidays` optional.
//! - **Weekends are Saturday and Sunday.** Microsoft's WORKDAY page states
//!   "working days exclude weekends" without naming the days; Saturday/Sunday
//!   is the fixed WORKDAY weekend (the configurable weekend is a separate
//!   function, `WORKDAY.INTL`, out of scope here). The weekday of a serial is
//!   `(serial - 1) mod 7` with **serial 1 (1900-01-01) = Sunday** — the exact
//!   anchor [`crate::func_weekday`] pins and cross-checks against serial 43831
//!   (2020-01-01, a real-world Wednesday). So `dow == 0` (Sunday) or `dow == 6`
//!   (Saturday) is a weekend. This is computed on the serial, not the calendar
//!   date, so the phantom 1900-02-29 (a date *label* on a real sequential
//!   serial) does not perturb the weekly cycle — matching `WEEKDAY`.
//! - **Stepping.** Starting from `start_date`'s serial, step one day at a time
//!   in the sign direction of `days` (positive → forward/future, negative →
//!   backward/past), and each time the stepped serial is a *working* day
//!   (neither weekend nor a listed holiday) decrement the remaining count. When
//!   the count reaches zero the current serial is the result. **The start date
//!   itself is never counted** (stepping advances before the first test), so
//!   `WORKDAY(Fri, 1)` skips the weekend and lands on the next Monday.
//! - `days = 0` returns `start_date`'s own serial unchanged (the zero-step
//!   result of the loop; no day is counted). Flagged as a minor unconfirmed
//!   edge in the spec's Oracle-experiments section but computed as the direct
//!   identity, not an arbitrary pick.
//! - **holidays** (optional): a range/array/scalar of date serials that are
//!   skipped exactly like weekends. Streamed with [`CallArgs::for_each_cell`]
//!   (materialized cells only). Each numeric cell's time-of-day component is
//!   dropped (floored to a whole day, mirroring `start_date`) and the serial is
//!   added to the skip set. **A blank holiday cell is ignored** (`for_each_cell`
//!   already elides range blanks; an explicit `Blank` is skipped too) — the
//!   documented, stated choice. An **error** holiday cell propagates. A
//!   **non-numeric** (text/logical) holiday cell → `#VALUE!` — RESOLVED by the
//!   oracle (RUN-2026-07-11-oracle01, `OXP-138`):
//!   `WORKDAY(DATE(2008,10,1),151,{"2008-11-27";42000;TRUE})` = `#VALUE!`, so a
//!   text date-literal or logical in `holidays` is **not** coerced to a serial —
//!   Excel rejects the whole call with `#VALUE!`. `holidays` omitted
//!   (`args.count() < 3`) → no holidays.
//! - **Coercion / errors.**
//!   - `start_date` — scalar numeric coercion, floored to a whole day
//!     ([`coerce_serial`]); a non-numeric/non-parseable value is `#VALUE!`; a
//!     magnitude past the safety rail is `#NUM!`. The floored serial must
//!     resolve to a real date in the active date system (out-of-range → `#NUM!`;
//!     serial 0 in 1900 → `#UNSUPPORTED!`, `OXP-090`), identical to
//!     `YEAR`/`WEEKDAY`.
//!   - `days` — scalar numeric coercion; a **non-integer** `days` **truncates
//!     toward zero** (`TRUNC`) — RESOLVED by the oracle
//!     (RUN-2026-07-11-oracle01, `OXP-137`): `WORKDAY(DATE(2020,1,3),1.9)` =
//!     43836 (days → 1; the positive probe cannot distinguish truncate from
//!     floor, and `days` is a month-offset-family argument like `EOMONTH`/
//!     `EDATE`'s `months`, which the oracle pinned as truncate-toward-zero, so
//!     [`coerce_int_trunc`] is used). A magnitude too large to be any date is
//!     `#NUM!`.
//!   - An **error** in any argument propagates (leftmost first: `start_date`,
//!     then `days`, then a holiday cell).
//!   - A landed result outside the representable date domain (before the epoch,
//!     past 9999-12-31) is `#NUM!`.

use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::date_common::{coerce_int_trunc, coerce_serial, map_date_error};
use crate::datecore::serial_to_ymd;

/// Excel's largest representable serial is `2_958_465` (9999-12-31 in the 1900
/// system; the 1904 system's maximum is smaller). Once day-by-day stepping
/// carries the running serial outside `0..=SERIAL_CEILING` the result cannot be
/// a valid date, so we stop rather than iterate up to `|days|` (which may be
/// ≈1e15) times; the out-of-range serial then maps to `#NUM!` through
/// [`serial_to_ymd`] below. This bounds the loop to at most ~3M iterations of
/// cheap integer work regardless of how large `days` is.
const SERIAL_CEILING: i64 = 2_958_465;

/// Is `serial` a Saturday or Sunday? Anchored so serial 1 (1900-01-01) is
/// Sunday, matching [`crate::func_weekday`] (see the module docs for the
/// cross-check against serial 43831 = 2020-01-01, a Wednesday). `dow`: 0 =
/// Sunday … 6 = Saturday.
fn is_weekend(serial: i64) -> bool {
    let dow = (serial - 1).rem_euclid(7);
    dow == 0 || dow == 6
}

/// Evaluate a `WORKDAY(start_date, days, [holidays])` call. See the module docs.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // start_date: scalar serial coercion (floors the time-of-day component),
    // then validate it is a real date in the active date system. An error value
    // propagates as #VALUE!/etc.; an out-of-range serial is #NUM!.
    let start = match coerce_serial(&args.eval_scalar(0)) {
        Ok(s) => s,
        Err(k) => return Value::Error(k),
    };
    if let Err(e) = serial_to_ymd(start, ctx.date_system()) {
        return Value::Error(map_date_error(e));
    }

    // days: integer coercion, non-integer truncated toward zero (OXP-137); too
    // large → #NUM!; an error value propagates.
    let days = match coerce_int_trunc(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };

    // holidays (optional): materialize the skip set. Blanks ignored; errors
    // propagate; non-numeric cells defer (OXP-138).
    let mut holidays: Vec<i64> = Vec::new();
    let mut holiday_err: Option<ErrorKind> = None;
    if args.count() >= 3 {
        args.for_each_cell(2, &mut |v| match v {
            // Drop any time-of-day component, mirroring start_date.
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
            // OXP-138: a text/logical holiday cell is not coerced — Excel
            // rejects the whole call with #VALUE!.
            _ => {
                holiday_err = Some(ErrorKind::Value); // OXP-138
                ControlFlow::Break(())
            }
        });
        if let Some(k) = holiday_err {
            return Value::Error(k);
        }
    }

    // Step day-by-day in the sign direction, counting only working days. The
    // start date itself is not counted (we advance before testing). `days == 0`
    // leaves the loop body unentered → the start serial is returned.
    let step = if days >= 0 { 1 } else { -1 };
    let mut remaining = days.unsigned_abs();
    let mut serial = start;
    while remaining > 0 {
        serial += step;
        if !(0..=SERIAL_CEILING).contains(&serial) {
            // Stepped outside every representable serial; the true result is
            // further out still. Stop — the out-of-range serial maps to #NUM!.
            break;
        }
        if !is_weekend(serial) && !holidays.contains(&serial) {
            remaining -= 1;
        }
    }

    // Validate the landed (or bailed-out) serial. In-range → the result serial;
    // out-of-range → #NUM! (serial 0 in 1900 is unreachable here — it is a
    // Saturday, hence never a counted landing — but the mapping is shared for
    // safety).
    match serial_to_ymd(serial, ctx.date_system()) {
        Ok(_) => Value::number(serial as f64),
        Err(e) => Value::Error(map_date_error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::ErrorKind;

    // DATE-derived serials in the 1900 system, all hand-verifiable from the
    // pinned anchor serial 43831 = 2020-01-01 (a Wednesday):
    //   43833 = 2020-01-03  Friday
    //   43834 = 2020-01-04  Saturday
    //   43835 = 2020-01-05  Sunday
    //   43836 = 2020-01-06  Monday
    //   43837 = 2020-01-07  Tuesday
    //   43843 = 2020-01-13  Monday
    const WED_2020_01_01: i64 = 43831;
    const FRI_2020_01_03: i64 = 43833;
    const SAT_2020_01_04: i64 = 43834;
    const SUN_2020_01_05: i64 = 43835;
    const MON_2020_01_06: i64 = 43836;
    const TUE_2020_01_07: i64 = 43837;
    const MON_2020_01_13: i64 = 43843;

    // ---- weekday anchor (serial 1 = Sunday), verified against known dates ----

    #[test]
    fn weekend_anchor_against_known_dates() {
        // The pinned anchor: 2020-01-01 is a Wednesday → a working day.
        assert!(!is_weekend(WED_2020_01_01));
        assert!(!is_weekend(FRI_2020_01_03)); // Friday
        assert!(is_weekend(SAT_2020_01_04)); // Saturday
        assert!(is_weekend(SUN_2020_01_05)); // Sunday
        assert!(!is_weekend(MON_2020_01_06)); // Monday
        // Serial 1 is 1900-01-01, defined as Sunday → a weekend.
        assert!(is_weekend(1));
    }

    // ---- core stepping ------------------------------------------------------

    #[test]
    fn friday_plus_one_skips_weekend_to_monday() {
        // WORKDAY(Fri, 1) == next Mon: the start (Friday) is not counted, Sat
        // and Sun are skipped, Monday is the first working day.
        let got = eval_direct(
            super::eval,
            vec![Scalar(num(FRI_2020_01_03 as f64)), Scalar(num(1.0))],
        );
        assert_eq!(got, num(MON_2020_01_06 as f64));
    }

    #[test]
    fn monday_plus_five_is_next_monday() {
        // WORKDAY(Mon, 5): Tue,Wed,Thu,Fri (4), skip Sat/Sun, next Mon is the 5th.
        let got = eval_direct(
            super::eval,
            vec![Scalar(num(MON_2020_01_06 as f64)), Scalar(num(5.0))],
        );
        assert_eq!(got, num(MON_2020_01_13 as f64));
    }

    #[test]
    fn negative_days_counts_backward() {
        // WORKDAY(Mon, -1): skip Sun/Sat, land on the previous Friday.
        let got = eval_direct(
            super::eval,
            vec![Scalar(num(MON_2020_01_06 as f64)), Scalar(num(-1.0))],
        );
        assert_eq!(got, num(FRI_2020_01_03 as f64));
    }

    #[test]
    fn zero_days_returns_start() {
        let got = eval_direct(
            super::eval,
            vec![Scalar(num(MON_2020_01_06 as f64)), Scalar(num(0.0))],
        );
        assert_eq!(got, num(MON_2020_01_06 as f64));
    }

    // ---- holidays -----------------------------------------------------------

    #[test]
    fn holiday_shifts_result_forward() {
        // WORKDAY(Fri, 1, {Mon}): the next working day (Monday) is a holiday, so
        // the result shifts to Tuesday.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(FRI_2020_01_03 as f64)),
                Scalar(num(1.0)),
                Range(vec![num(MON_2020_01_06 as f64)]),
            ],
        );
        assert_eq!(got, num(TUE_2020_01_07 as f64));
    }

    #[test]
    fn holiday_off_the_path_has_no_effect() {
        // A holiday that is never landed on (a Saturday, already a weekend)
        // leaves the result unchanged: WORKDAY(Fri, 1, {Sat}) == Mon.
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(FRI_2020_01_03 as f64)),
                Scalar(num(1.0)),
                Range(vec![num(SAT_2020_01_04 as f64)]),
            ],
        );
        assert_eq!(got, num(MON_2020_01_06 as f64));
    }

    #[test]
    fn blank_holiday_cell_is_ignored() {
        // A blank in the holiday range contributes nothing (stated choice).
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(FRI_2020_01_03 as f64)),
                Scalar(num(1.0)),
                Range(vec![Value::Blank, num(MON_2020_01_06 as f64)]),
            ],
        );
        // Monday is still a holiday → Tuesday, blank simply skipped.
        assert_eq!(got, num(TUE_2020_01_07 as f64));
    }

    // ---- coercion / error propagation --------------------------------------

    #[test]
    fn start_date_error_propagates() {
        let got = eval_direct(
            super::eval,
            vec![Scalar(Value::Error(ErrorKind::Div0)), Scalar(num(1.0))],
        );
        assert_eq!(got, Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn days_error_propagates() {
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(FRI_2020_01_03 as f64)),
                Scalar(Value::Error(ErrorKind::Value)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Value));
    }

    #[test]
    fn holiday_error_propagates() {
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(FRI_2020_01_03 as f64)),
                Scalar(num(1.0)),
                Range(vec![Value::Error(ErrorKind::Na)]),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Na));
    }

    #[test]
    fn oxp137_non_integer_days_truncates_toward_zero() {
        // OXP-137 (RUN-2026-07-11-oracle01): =WORKDAY(DATE(2020,1,3),1.9) ->
        // 43836 (days -> 1; Friday + 1 working day skips the weekend to Monday).
        let got = eval_direct(
            super::eval,
            vec![Scalar(num(FRI_2020_01_03 as f64)), Scalar(num(1.9))],
        );
        assert_eq!(got, num(MON_2020_01_06 as f64));
        assert_eq!(MON_2020_01_06, 43836);
        // 1.5 also truncates to 1 (same landing).
        let got = eval_direct(
            super::eval,
            vec![Scalar(num(FRI_2020_01_03 as f64)), Scalar(num(1.5))],
        );
        assert_eq!(got, num(MON_2020_01_06 as f64));
    }

    #[test]
    fn oxp138_non_numeric_holiday_is_value_error() {
        // OXP-138 (RUN-2026-07-11-oracle01): a text/logical holiday cell is not
        // coerced — Excel returns #VALUE!. A text date-literal:
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(FRI_2020_01_03 as f64)),
                Scalar(num(1.0)),
                Range(vec![Value::text("2020-01-06")]),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Value));
        // A logical holiday cell also errors #VALUE! (mirrors the probe's TRUE).
        let got = eval_direct(
            super::eval,
            vec![
                Scalar(num(FRI_2020_01_03 as f64)),
                Scalar(num(1.0)),
                Range(vec![Value::Bool(true)]),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Value));
    }

    // ---- domain edges -------------------------------------------------------

    #[test]
    fn result_before_epoch_is_num() {
        // Stepping far backward past serial 1 leaves the representable domain.
        let got = eval_direct(
            super::eval,
            vec![Scalar(num(MON_2020_01_06 as f64)), Scalar(num(-100_000.0))],
        );
        assert_eq!(got, Value::Error(ErrorKind::Num));
    }

    #[test]
    fn huge_days_is_num_without_hanging() {
        // A |days| far beyond the domain must terminate quickly (loop bound) and
        // report #NUM!, not iterate 1e12 times.
        let got = eval_direct(
            super::eval,
            vec![Scalar(num(MON_2020_01_06 as f64)), Scalar(num(1e12))],
        );
        assert_eq!(got, Value::Error(ErrorKind::Num));
    }

    #[test]
    fn out_of_range_start_is_num() {
        // A negative start serial is not a valid date.
        let got = eval_direct(super::eval, vec![Scalar(num(-5.0)), Scalar(num(1.0))]);
        assert_eq!(got, Value::Error(ErrorKind::Num));
    }
}

//! `DATE` — construct a date serial from year/month/day, with Excel's overflow
//! normalization and the 1900 leap-year bug.
//!
//! # Provenance
//! Behavior contract: `docs/specs/DATE.md` (which cites the Microsoft Learn DATE
//! page and Microsoft's 1900-leap-year-bug note). The serial construction lives
//! in [`crate::datecore::date_to_serial`]; coercion is `xl-value`'s via
//! [`crate::date_common`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Three required numeric arguments, scalar-coerced (DATE.md §Coercion).
//! - **Year resolution** (DATE.md §2): a `year` in `0..=1899` has `1900` added
//!   (`DATE(105,1,2)` → year 2005); `1900..=9999` are literal. Applied here
//!   uniformly; the spec flags the 1904-system interaction as oracle-pending
//!   (`OXP-093`) but the year *argument interpretation* is a documented DATE
//!   behavior, so it is applied and noted rather than deferred.
//! - **Month/day overflow normalization** (DATE.md §3/§4): `month`/`day` outside
//!   the natural range roll into adjacent months/years — `DATE(2020,13,1)` =
//!   2021-01-01, `DATE(2020,0,1)` = 2019-12-01, `DATE(2020,1,32)` = 2020-02-01.
//!   Done in serial space by [`date_to_serial`](crate::datecore::date_to_serial),
//!   which is what makes `DATE(1900,2,29)` land on serial 60 (the fake leap day,
//!   DATE.md §5).
//! - Result before the epoch or past 9999-12-31 → `#NUM!`. A result that lands
//!   on the "January 0, 1900" serial is serial `0`, a plain number — RESOLVED
//!   (`OXP-090`, RUN-2026-07-11-oracle01): `DATE(1900,1,0)` = `0` (DATE.md
//!   §Error behavior).
//!
//! # Non-integer arguments — RESOLVED (`OXP-091`, RUN-2026-07-11-oracle01)
//! DATE **floors** each of year/month/day toward negative infinity (`INT`),
//! not truncate-toward-zero and not round. Pinned by the oracle:
//! `DATE(2020,-1.5,1)` = 43739 (month floored to −2 → 2019-10-01),
//! `DATE(2020,1,-1.5)` = 43828 (day floored to −2), while the `.9` probes
//! (`DATE(2020.9,1,1)` = `DATE(2020,1.9,1)` = `DATE(2020,1,1.9)` = 43831)
//! confirm floor and rule out rounding (which would carry `2020.9` to 2021).
//! The floor happens in [`coerce_int_floor`]; overflow normalization then runs
//! on the floored integers exactly as for integer inputs.
//!
//! # 1904 date system — RESOLVED (`OXP-093`, RUN-2026-07-11-oracle01)
//! The `year 0..=1899 → +1900` quirk applies **uniformly** in the 1904 date
//! system too (it is a property of the year *argument*, epoch-independent):
//! on a 1904-flagged workbook `DATE(105,1,2)` = 36892 (2005-01-02 in 1904
//! serials) and `DATE(5,1,1)` = 366 (1905-01-01). This was already the applied
//! behavior; the oracle confirms it.

use xl_value::Value;

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::date_common::{coerce_int_floor, map_date_error};
use crate::datecore::{DateError, date_to_serial};

/// Resolve DATE's `year` argument per the 0..1899 → +1900 quirk (DATE.md §2).
fn resolve_year(year: i64) -> i64 {
    if (0..=1899).contains(&year) {
        year + 1900
    } else {
        year
    }
}

/// Evaluate a `DATE(year, month, day)` call. See the module docs.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Coerce all three; any error propagates, any non-integer floors toward
    // negative infinity (OXP-091), any absurd magnitude is out-of-range → #NUM!.
    let year = match coerce_int_floor(&args.eval_scalar(0)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let month = match coerce_int_floor(&args.eval_scalar(1)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let day = match coerce_int_floor(&args.eval_scalar(2)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };

    match date_to_serial(resolve_year(year), month, day, ctx.date_system()) {
        Ok(serial) => Value::number(serial as f64),
        // OXP-090 (RUN-2026-07-11-oracle01): a result landing on "January 0,
        // 1900" is serial 0, a plain number — `=DATE(1900,1,0)` = 0.
        Err(DateError::JanuaryZero) => Value::number(0.0),
        Err(e) => Value::Error(map_date_error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::CallArgs;
    use crate::datecore::DateSystem;
    use crate::test_support::{TestArg::*, TestArgs, eval_direct, num};
    use xl_value::ErrorKind;

    /// Invoke `eval` under an explicit date system (for the 1904 probes).
    fn eval_in(system: DateSystem, args: Vec<crate::test_support::TestArg>) -> Value {
        let ctx = EvalContext::with_date_system(system);
        let mut ta = TestArgs::new(args);
        eval(&ctx, &mut ta as &mut dyn CallArgs)
    }

    // ---- integer sanity (unchanged documented overflow normalization) -------

    #[test]
    fn integer_args_and_overflow_normalization() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(2020.0)), Scalar(num(1.0)), Scalar(num(1.0))]
            ),
            num(43831.0)
        );
        // DATE(2020,13,1) = 2021-01-01; DATE(105,1,2) = 2005-01-02 (year +1900).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(2020.0)), Scalar(num(13.0)), Scalar(num(1.0))]
            ),
            num(44197.0)
        );
    }

    // ---- OXP-091: non-integer args FLOOR (RUN-2026-07-11-oracle01) -----------

    #[test]
    fn oxp091_fractional_year_floors() {
        // =DATE(2020.9,1,1) -> 43831 (year floors to 2020; rules out rounding
        // to 2021).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(2020.9)), Scalar(num(1.0)), Scalar(num(1.0))]
            ),
            num(43831.0)
        );
    }

    #[test]
    fn oxp091_fractional_month_and_day_floor() {
        // =DATE(2020,1.9,1) -> 43831 ; =DATE(2020,1,1.9) -> 43831.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(2020.0)), Scalar(num(1.9)), Scalar(num(1.0))]
            ),
            num(43831.0)
        );
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(2020.0)), Scalar(num(1.0)), Scalar(num(1.9))]
            ),
            num(43831.0)
        );
    }

    #[test]
    fn oxp091_negative_month_floors_toward_neg_infinity() {
        // =DATE(2020,-1.5,1) -> 43739 (2019-10-01): month floors to -2, NOT
        // truncate-toward-zero (-1, which would give 2019-11-01 = 43770).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(2020.0)), Scalar(num(-1.5)), Scalar(num(1.0))]
            ),
            num(43739.0)
        );
    }

    #[test]
    fn oxp091_negative_day_floors_toward_neg_infinity() {
        // =DATE(2020,1,-1.5) -> 43828: day floors to -2 (43828), NOT -1 (43829).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(2020.0)), Scalar(num(1.0)), Scalar(num(-1.5))]
            ),
            num(43828.0)
        );
    }

    // ---- OXP-093: the +1900 year quirk holds in the 1904 system too ---------

    #[test]
    fn oxp093_year_plus_1900_applies_in_1904_system() {
        // =DATE(105,1,2) on a 1904 workbook -> 36892 (2005-01-02 in 1904 serials).
        assert_eq!(
            eval_in(
                DateSystem::Excel1904,
                vec![Scalar(num(105.0)), Scalar(num(1.0)), Scalar(num(2.0))]
            ),
            num(36892.0)
        );
        // =DATE(5,1,1) on a 1904 workbook -> 366 (1905-01-01).
        assert_eq!(
            eval_in(
                DateSystem::Excel1904,
                vec![Scalar(num(5.0)), Scalar(num(1.0)), Scalar(num(1.0))]
            ),
            num(366.0)
        );
    }

    // ---- OXP-090: DATE landing on serial 0 ("January 0, 1900") --------------

    #[test]
    fn oxp090_date_january_zero_is_serial_zero() {
        // =DATE(1900,1,0) -> 0 (a plain number, not an error).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(1900.0)), Scalar(num(1.0)), Scalar(num(0.0))]
            ),
            num(0.0)
        );
    }

    // ---- error propagation --------------------------------------------------

    #[test]
    fn argument_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Ref)),
                    Scalar(num(1.0)),
                    Scalar(num(1.0))
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }
}

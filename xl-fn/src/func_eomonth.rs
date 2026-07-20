//! `EOMONTH` — the last day of the month `months` before/after a start date.
//!
//! # Provenance
//! Behavior contract: `docs/specs/EOMONTH.md` (which cites the Microsoft Learn
//! EOMONTH page). The month arithmetic lives in
//! [`crate::datecore::eomonth_serial`]; coercion is `xl-value`'s via
//! [`crate::date_common`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `start_date` is a serial (scalar-coerced, floored to a whole day);
//!   `months` is an integer offset (EOMONTH.md §1-4). The date system comes
//!   from the [`EvalContext`].
//! - Returns the last day of the shifted month, computed as "first of the
//!   following month, minus one day" in serial space — so **Feb 1900 is 29 days
//!   long**: `EOMONTH(DATE(1900,1,1),1)` = serial 60, the fake leap day
//!   (EOMONTH.md §6, the headline item for this function).
//! - A `start_date` before the epoch/past 9999 → `#NUM!`; a `start_date` of
//!   serial 0 ("January 0, 1900") → `#UNSUPPORTED!` (`OXP-090`); a *result*
//!   outside the range → `#NUM!` (EOMONTH.md §Error behavior).
//!
//! # Non-integer `months` — RESOLVED (`OXP-092`, RUN-2026-07-11-oracle01)
//! EOMONTH **truncates `months` toward zero** (`TRUNC`), not floor:
//! `EOMONTH(DATE(2020,1,15),1.9)` = 43890 (Feb 2020's last day, months → 1) and
//! `EOMONTH(DATE(2020,1,15),-1.9)` = 43830 (Dec 2019's last day, months → −1,
//! **not** −2 which would give Nov 2019's 43799). Truncation happens in
//! [`coerce_int_trunc`]. (This is the opposite direction from `DATE`, which
//! floors — the two Excel code paths genuinely differ; each is pinned from its
//! own probe.)

use xl_value::Value;

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::date_common::{coerce_int_trunc, coerce_serial, map_date_error};
use crate::datecore::eomonth_serial;

/// Evaluate an `EOMONTH(start_date, months)` call. See the module docs.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let start = match coerce_serial(&args.eval_scalar(0)) {
        Ok(s) => s,
        Err(k) => return Value::Error(k),
    };
    let months = match coerce_int_trunc(&args.eval_scalar(1)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    match eomonth_serial(start, months, ctx.date_system()) {
        Ok(serial) => Value::number(serial as f64),
        Err(e) => Value::Error(map_date_error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datecore::{DateSystem, date_to_serial};
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::ErrorKind;

    fn s(year: i64, month: i64, day: i64) -> f64 {
        date_to_serial(year, month, day, DateSystem::Excel1900).unwrap() as f64
    }

    #[test]
    fn integer_months_shape() {
        // EOMONTH(2020-01-15, 1) = last day of Feb 2020 (leap) = 2020-02-29.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(s(2020, 1, 15))), Scalar(num(1.0))]),
            num(s(2020, 2, 29))
        );
    }

    // ---- OXP-092: months TRUNCATES toward zero (RUN-2026-07-11-oracle01) -----

    #[test]
    fn oxp092_positive_fractional_months_truncate() {
        // =EOMONTH(DATE(2020,1,15),1.9) -> 43890 (months -> 1; Feb 2020 EOM).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(s(2020, 1, 15))), Scalar(num(1.9))]),
            num(43890.0)
        );
        assert_eq!(num(43890.0), num(s(2020, 2, 29)));
    }

    #[test]
    fn oxp092_negative_fractional_months_truncate_toward_zero() {
        // =EOMONTH(DATE(2020,1,15),-1.9) -> 43830 (months -> -1, Dec 2019 EOM;
        // NOT -2 which is Nov 2019 EOM = 43799).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(s(2020, 1, 15))), Scalar(num(-1.9))]),
            num(43830.0)
        );
        assert_eq!(num(43830.0), num(s(2019, 12, 31)));
    }

    #[test]
    fn months_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(s(2020, 1, 15))),
                    Scalar(Value::Error(ErrorKind::Na))
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }
}

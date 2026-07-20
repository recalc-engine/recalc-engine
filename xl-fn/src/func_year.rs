//! `YEAR` — the calendar year of a date serial.
//!
//! # Provenance
//! Behavior contract: `docs/specs/YEAR.md` (which cites the Microsoft Learn YEAR
//! page). The serial↔calendar conversion — including the 1900 leap-year bug —
//! lives in [`crate::datecore`]; coercion is `xl-value`'s [`to_number`](xl_value::to_number)
//! via [`crate::date_common`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Coerce the one argument in scalar context and floor to a whole day, then
//!   return its calendar year (YEAR.md §1). The date system (1900/1904) comes
//!   from the [`EvalContext`] (YEAR.md §1, workbook state).
//! - The 1900 fake-leap-day seam is respected: `YEAR(60)` = `1900` (YEAR.md §3).
//! - Negative / out-of-range serial → `#NUM!` (YEAR.md §Error behavior);
//!   `YEAR(-1)` = `#NUM!`.
//! - **Serial 0** ("January 0, 1900") — RESOLVED (`OXP-090`,
//!   RUN-2026-07-11-oracle01): `YEAR(0)` = `1900`. A blank argument coerces to
//!   serial 0 and hence also reads as year 1900.

use xl_value::Value;

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::date_common::{coerce_serial, map_date_error};
use crate::datecore::{DateError, serial_to_ymd};

/// Evaluate a `YEAR(serial_number)` call. See the module docs.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let serial = match coerce_serial(&args.eval_scalar(0)) {
        Ok(s) => s,
        Err(k) => return Value::Error(k),
    };
    match serial_to_ymd(serial, ctx.date_system()) {
        Ok((y, _m, _d)) => Value::number(f64::from(y)),
        // OXP-090 (RUN-2026-07-11-oracle01): serial 0 = "January 0, 1900",
        // YEAR = 1900. Intercepted here so the shared datecore signal still
        // defers for the unprobed date functions.
        Err(DateError::JanuaryZero) => Value::number(1900.0),
        Err(e) => Value::Error(map_date_error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::Scalar, eval_direct, num};
    use xl_value::ErrorKind;

    #[test]
    fn oxp090_serial_zero_and_negative() {
        // RUN-2026-07-11-oracle01: =YEAR(0) → 1900; =YEAR(-1) → #NUM!.
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.0))]), num(1900.0));
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-1.0))]),
            Value::Error(ErrorKind::Num)
        );
    }
}

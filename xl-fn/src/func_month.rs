//! `MONTH` — the calendar month (1-12) of a date serial.
//!
//! # Provenance
//! Behavior contract: `docs/specs/MONTH.md` (which cites the Microsoft Learn
//! MONTH page). Conversion (incl. the 1900 leap-year bug) is
//! [`crate::datecore`]; coercion is `xl-value`'s via [`crate::date_common`].
//!
//! # Semantics implemented
//! - Coerce the argument, floor to a whole day, return the month `1..=12`
//!   (MONTH.md §1), per the [`EvalContext`]'s date system.
//! - `MONTH(60)` = `2` — the fake 1900-02-29 is month 2 (MONTH.md §3).
//! - Out-of-range/negative → `#NUM!`; serial 0 → `1` — RESOLVED (`OXP-090`,
//!   RUN-2026-07-11-oracle01): `MONTH(0)` = `1` (the "January 0, 1900"
//!   special), like `YEAR` (MONTH.md §Error behavior).

use xl_value::Value;

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::date_common::{coerce_serial, map_date_error};
use crate::datecore::{DateError, serial_to_ymd};

/// Evaluate a `MONTH(serial_number)` call. See the module docs.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let serial = match coerce_serial(&args.eval_scalar(0)) {
        Ok(s) => s,
        Err(k) => return Value::Error(k),
    };
    match serial_to_ymd(serial, ctx.date_system()) {
        Ok((_y, m, _d)) => Value::number(f64::from(m)),
        // OXP-090 (RUN-2026-07-11-oracle01): serial 0 = "January 0, 1900",
        // MONTH = 1. Intercepted so the shared signal still defers elsewhere.
        Err(DateError::JanuaryZero) => Value::number(1.0),
        Err(e) => Value::Error(map_date_error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::Scalar, eval_direct, num};

    #[test]
    fn oxp090_serial_zero_is_month_one() {
        // RUN-2026-07-11-oracle01: =MONTH(0) → 1.
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.0))]), num(1.0));
    }
}

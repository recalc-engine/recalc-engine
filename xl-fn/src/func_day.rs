//! `DAY` — the day-of-month (1-31) of a date serial.
//!
//! # Provenance
//! Behavior contract: `docs/specs/DAY.md` (which cites the Microsoft Learn DAY
//! page). Conversion (incl. the 1900 leap-year bug) is [`crate::datecore`];
//! coercion is `xl-value`'s via [`crate::date_common`].
//!
//! # Semantics implemented
//! - Coerce the argument, floor to a whole day, return the day `1..=31`
//!   (DAY.md §1), per the [`EvalContext`]'s date system.
//! - `DAY(60)` = `29` — the fake 1900-02-29 (DAY.md §2).
//! - Out-of-range/negative → `#NUM!`; serial 0 → `0` — RESOLVED (`OXP-090`,
//!   RUN-2026-07-11-oracle01): `DAY(0)` = `0` (the "January 0, 1900"
//!   special — day component zero), like `YEAR` (DAY.md §Error behavior).

use xl_value::Value;

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::date_common::{coerce_serial, map_date_error};
use crate::datecore::{DateError, serial_to_ymd};

/// Evaluate a `DAY(serial_number)` call. See the module docs.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let serial = match coerce_serial(&args.eval_scalar(0)) {
        Ok(s) => s,
        Err(k) => return Value::Error(k),
    };
    match serial_to_ymd(serial, ctx.date_system()) {
        Ok((_y, _m, d)) => Value::number(f64::from(d)),
        // OXP-090 (RUN-2026-07-11-oracle01): serial 0 = "January 0, 1900",
        // DAY = 0. Intercepted so the shared signal still defers elsewhere.
        Err(DateError::JanuaryZero) => Value::number(0.0),
        Err(e) => Value::Error(map_date_error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::Scalar, eval_direct, num};

    #[test]
    fn oxp090_serial_zero_is_day_zero() {
        // RUN-2026-07-11-oracle01: =DAY(0) → 0.
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.0))]), num(0.0));
    }
}

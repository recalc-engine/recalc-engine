//! `WEEKDAY` — the day of the week of a date serial, numbered per
//! `return_type`.
//!
//! # Provenance
//! No `docs/specs/WEEKDAY.md` exists yet (this batch's task explicitly asked
//! for a clean-room implementation from Microsoft's public docs, the same
//! discipline as every other function in this crate — the Recalc design rules "no code
//! copied from GPL implementations", clean-room only, public sources only):
//! Microsoft Learn's WEEKDAY function page
//! (`https://support.microsoft.com/en-us/office/weekday-function-60e44483-2ed1-439f-8bd0-e404c190949a`).
//! The serial↔calendar validation reuses [`crate::datecore`]/[`crate::date_common`]
//! (the same domain rules as `YEAR`/`MONTH`/`DAY`); the weekday number itself
//! is derived directly from the **serial number**, not the calendar date,
//! since Excel's serial numbering advances exactly one weekday per serial
//! regardless of which calendar date a serial is *displayed* as (the
//! fictitious 1900-02-29 is a phantom date label on a real, sequential
//! serial — the weekday cycle is unaffected by it).
//!
//! # Semantics implemented
//! - `WEEKDAY(serial_number, [return_type])`; `return_type` optional.
//! - Day-of-week is computed as `serial mod 7`, anchored so **serial 1
//!   (1900-01-01) is Sunday**. Cross-checked (not merely assumed) against a
//!   second, independent anchor already pinned elsewhere in this crate:
//!   serial 43831 is `DATE`/`YEAR`/`MONTH`/`DAY`'s own test fixture for
//!   2020-01-01, a date independently known to be a real-world Wednesday;
//!   this module's formula computes serial 43831 to Wednesday, agreeing
//!   with that external fact.
//! - `return_type = 1` (the default, used when the argument is **omitted**
//!   — [`ArgShape::Omitted`], distinct from a *present* argument that merely
//!   evaluates to [`xl_value::Value::Blank`], see below): Sunday=1 ..
//!   Saturday=7.
//! - `return_type = 2`: Monday=1 .. Sunday=7.
//! - `return_type = 3`: Monday=0 .. Sunday=6.
//! - Microsoft's WEEKDAY page documents further `return_type` values
//!   `11..=17` (each a different single-day-numbered-1 rotation), which are
//!   real, valid Excel inputs but **out of this task's implementation
//!   scope** — rather than silently mis-tag them with the domain error
//!   `#NUM!` (a claim they are invalid, which they are not), an unimplemented
//!   documented value returns `#UNSUPPORTED!`.
//! - Any **undocumented** `return_type` (e.g. `0`, `4`-`10`, `18+`,
//!   negative) is a genuine Excel domain error -> `#NUM!`.
//! - A **present-but-blank** `return_type` argument (e.g. a reference to an
//!   empty cell, as opposed to the argument being omitted entirely) is
//!   *not* special-cased to the omitted-default of `1`: it goes through
//!   ordinary scalar numeric coercion like any other value, where
//!   `to_number(Blank)` is `0` (a frozen, already-established `xl-value`
//!   rule — not a fresh guess), and `0` is not a valid `return_type` ->
//!   `#NUM!`. This is distinguished from the omitted case via
//!   [`ArgShape::Omitted`], which `CallArgs::shape` reports only for a
//!   truly-absent argument position.
//! - A non-integer `return_type` **truncates toward zero** (`TRUNC`) —
//!   RESOLVED by the oracle (RUN-2026-07-11-oracle01, `OXP-097`):
//!   `WEEKDAY(DATE(2020,1,1),1.9)` = 4 (type → 1) and
//!   `WEEKDAY(DATE(2020,1,1),2.1)` = 3 (type → 2). Only positive `return_type`
//!   values are meaningful (the documented set is `1..=3`, `11..=17`), so
//!   truncate-toward-zero and floor agree on every valid input. A non-finite
//!   `return_type` is an undocumented value -> `#NUM!`.
//! - `serial_number` coercion/range rules are identical to `YEAR`/`MONTH`/
//!   `DAY`: negative/out-of-range -> `#NUM!`; serial `0` -> `#UNSUPPORTED!`
//!   (`OXP-090`).

use xl_value::{ErrorKind, Value, to_number};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;
use crate::date_common::{coerce_serial, map_date_error};
use crate::datecore::serial_to_ymd;

/// Evaluate a `WEEKDAY(serial_number, [return_type])` call. See the module
/// docs.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let serial = match coerce_serial(&args.eval_scalar(0)) {
        Ok(s) => s,
        Err(k) => return Value::Error(k),
    };
    // Validate the serial resolves to a real date in the active date system
    // (out-of-range / January-0 rules identical to YEAR/MONTH/DAY), even
    // though only the raw serial number feeds the weekday formula below.
    if let Err(e) = serial_to_ymd(serial, ctx.date_system()) {
        return Value::Error(map_date_error(e));
    }

    let return_type = if matches!(args.shape(1), ArgShape::Omitted) {
        1i64
    } else {
        match to_number(&args.eval_scalar(1)) {
            // OXP-097: a fractional return_type truncates toward zero (`as i64`
            // truncates toward zero); a non-finite value is undocumented → #NUM!.
            Ok(n) if n.is_finite() => n as i64,
            Ok(_) => return Value::Error(ErrorKind::Num),
            Err(k) => return Value::Error(k),
        }
    };

    // Sunday-anchored 1..=7 index: serial 1 (1900-01-01) is Sunday (see the
    // module docs for the cross-check against the 2020-01-01 Wednesday
    // anchor).
    let sunday_based = (serial - 1).rem_euclid(7) + 1;

    let weekday = match return_type {
        1 => sunday_based,
        2 => (sunday_based - 2).rem_euclid(7) + 1,
        3 => (sunday_based - 2).rem_euclid(7),
        // Documented by Microsoft but not implemented in this task's scope.
        11..=17 => return Value::Error(ErrorKind::Unsupported),
        _ => return Value::Error(ErrorKind::Num),
    };
    Value::number(weekday as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num};

    // Serial 43831 = 2020-01-01, a Wednesday (the crate's pinned anchor).
    const WED_2020_01_01: f64 = 43831.0;

    #[test]
    fn default_and_typed_shapes() {
        // Omitted return_type -> type 1 (Sun=1..Sat=7): Wednesday = 4.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(WED_2020_01_01)), Omitted]),
            num(4.0)
        );
        // Type 2 (Mon=1..Sun=7): Wednesday = 3.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(WED_2020_01_01)), Scalar(num(2.0))]),
            num(3.0)
        );
        // Type 3 (Mon=0..Sun=6): Wednesday = 2.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(WED_2020_01_01)), Scalar(num(3.0))]),
            num(2.0)
        );
    }

    // ---- OXP-097: fractional return_type truncates toward zero --------------

    #[test]
    fn oxp097_fractional_return_type_truncates() {
        // =WEEKDAY(DATE(2020,1,1),1.9) -> 4 (type -> 1).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(WED_2020_01_01)), Scalar(num(1.9))]),
            num(4.0)
        );
        // =WEEKDAY(DATE(2020,1,1),2.1) -> 3 (type -> 2).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(WED_2020_01_01)), Scalar(num(2.1))]),
            num(3.0)
        );
    }

    #[test]
    fn undocumented_return_type_is_num() {
        // 0 and 4 are genuine Excel domain errors.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(WED_2020_01_01)), Scalar(num(0.0))]),
            Value::Error(ErrorKind::Num)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(WED_2020_01_01)), Scalar(num(4.0))]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn documented_but_unimplemented_return_type_is_unsupported() {
        // 11..=17 are valid Excel inputs, out of scope -> #UNSUPPORTED!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(WED_2020_01_01)), Scalar(num(11.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

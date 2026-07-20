//! `SLN` — straight-line depreciation of an asset for one period.
//!
//! # Provenance
//! Behavior contract: `docs/specs/SLN.md` (Microsoft Learn SLN function page,
//! verified 2026-07-11: `https://support.microsoft.com/en-us/office/sln-function-cdb666e5-c1c6-40a7-806a-e695edc2f1c8`).
//! Coercion via `xl-value`'s [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Documented formula `SLN(cost, salvage, life) = (cost - salvage) / life`
//!   (SLN.md §1).
//! - Coerce all three arguments via scalar numeric coercion (bool -> 1/0,
//!   numeric text -> number, blank -> 0), left-to-right: `cost` first, then
//!   `salvage`, then `life`; an error-valued argument propagates as-is at the
//!   first one encountered in that order, the same convention `MOD`/`POWER`
//!   follow (SLN.md §Coercion, §Error behavior).
//!
//! # `life == 0` — RESOLVED (`OXP-135`, RUN-2026-07-11-oracle01)
//! `(cost - salvage) / 0` is a division-by-zero singularity. The public SLN
//! page states no error behavior, so the exact error kind was pinned by the
//! oracle: `SLN(10000,1000,0)` = `#DIV/0!` (not `#NUM!`). This case returns
//! [`ErrorKind::Div0`] via an explicit guard rather than letting the non-finite
//! `f64` division resolve through the [`Value::number`] `#NUM!` invariant.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `SLN(cost, salvage, life)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let cost = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let salvage = match to_number(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let life = match to_number(&args.eval_scalar(2)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };

    // life = 0 is a division-by-zero singularity; the oracle pinned the error
    // kind as #DIV/0! (OXP-135, RUN-2026-07-11-oracle01). See module docs.
    if life == 0.0 {
        return Value::Error(ErrorKind::Div0);
    }

    Value::number((cost - salvage) / life)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    #[test]
    fn worked_example() {
        // MS Learn worked example: cost=30000, salvage=7500, life=10 -> 2250.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(30000.0)), Scalar(num(7500.0)), Scalar(num(10.0))]
            ),
            num(2250.0)
        );
    }

    #[test]
    fn task_spec_examples() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(10000.0)), Scalar(num(1000.0)), Scalar(num(5.0))]
            ),
            num(1800.0)
        );
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(100.0)), Scalar(num(0.0)), Scalar(num(4.0))]
            ),
            num(25.0)
        );
    }

    #[test]
    fn oxp135_life_zero_is_div0() {
        // OXP-135 (RUN-2026-07-11-oracle01): =SLN(10000,1000,0) -> #DIV/0!.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(10000.0)), Scalar(num(1000.0)), Scalar(num(0.0))]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn text_coercion() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("10000")), Scalar(txt("1000")), Scalar(txt("5"))]
            ),
            num(1800.0)
        );
    }

    #[test]
    fn bool_and_blank_coercion() {
        // TRUE -> 1, blank -> 0: SLN(TRUE, 0, blank-as-life) would divide by
        // zero, so exercise bool/blank on cost/salvage instead, life fixed.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(true)),
                    Scalar(num(0.0)),
                    Scalar(num(4.0))
                ]
            ),
            num(0.25)
        );
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(100.0)), Scalar(Value::Blank), Scalar(num(4.0))]
            ),
            num(25.0)
        );
    }

    #[test]
    fn error_propagation_left_to_right() {
        // cost's error wins even when salvage/life would also error.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Ref)),
                    Scalar(Value::Error(ErrorKind::Div0)),
                    Scalar(num(4.0)),
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
        // salvage's error surfaces once cost is clean.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(100.0)),
                    Scalar(Value::Error(ErrorKind::Value)),
                    Scalar(num(4.0)),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
        // life's error surfaces once cost/salvage are clean.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(100.0)),
                    Scalar(num(0.0)),
                    Scalar(Value::Error(ErrorKind::Na)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn non_numeric_text_is_value_error() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("abc")), Scalar(num(0.0)), Scalar(num(4.0))]
            ),
            Value::Error(ErrorKind::Value)
        );
    }
}

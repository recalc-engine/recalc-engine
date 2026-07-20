//! `EXP` — e (the base of the natural logarithm) raised to a power.
//!
//! # Provenance
//! Behavior contract: `docs/specs/EXP.md`, which cites the Microsoft Learn
//! EXP function page
//! (`https://support.microsoft.com/en-us/office/exp-function-c578f034-2c45-4c37-bc8c-329660a63abe`).
//! Coercion is deferred entirely to `xl-value`'s [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Coerce the one argument via scalar numeric coercion (bool -> 1/0,
//!   numeric text -> number, blank -> 0) (EXP.md §Coercion).
//! - Return e^number via `f64::exp` (EXP.md §1). `EXP(0)` = `1`,
//!   `EXP(1)` = `e` (EXP.md §Examples).
//! - An error-valued argument, or a non-numeric/non-coercible text argument,
//!   propagates/produces `#VALUE!` per `to_number`'s standard rule — no
//!   EXP-specific containment (EXP.md §Error behavior).
//! - Overflow -> `#NUM!`. `Value::number` already maps any non-finite `f64`
//!   result to `Value::Error(ErrorKind::Num)` (`xl-value`'s frozen
//!   NaN/Inf invariant), so this function relies on that constructor rather
//!   than re-implementing a finiteness check (EXP.md §Error behavior).
//!   **OXP note:** the public MS Learn page documents no overflow / `#NUM!`
//!   behavior for EXP at all, so the *exact* input threshold Excel treats as
//!   overflow (which may not coincide exactly with `f64::exp`'s own overflow
//!   point, e.g. `EXP(1000)`) is not pinned by a public source. This
//!   implementation uses the crate-wide non-finite -> `#NUM!` default that
//!   every other numeric function already relies on, not an EXP-specific
//!   guess; pinning the precise boundary against real Excel is deferred to
//!   an oracle experiment (EXP.md §Oracle experiments needed).

use xl_value::{Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `EXP(number)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    match to_number(&args.eval_scalar(0)) {
        Ok(n) => Value::number(n.exp()),
        Err(k) => Value::Error(k),
    }
}

#[cfg(test)]
mod tests {
    use std::ops::ControlFlow;

    use xl_value::{ErrorKind, Value};

    use super::eval;
    use crate::args::{ArgShape, CallArgs};
    use crate::context::EvalContext;

    /// Minimal single-scalar-argument `CallArgs` mock. `EXP` only ever calls
    /// `eval_scalar(0)`, so every other trait method is unreachable for this
    /// function and panics loudly if hit — that would signal a bug in `eval`
    /// (e.g. suddenly streaming a range) rather than silently misbehaving.
    struct OneArg(Value);

    impl CallArgs for OneArg {
        fn count(&self) -> usize {
            1
        }
        fn shape(&mut self, _index: usize) -> ArgShape {
            ArgShape::Scalar
        }
        fn eval_scalar(&mut self, index: usize) -> Value {
            assert_eq!(index, 0, "EXP only reads argument 0");
            self.0.clone()
        }
        fn for_each_cell(
            &mut self,
            _index: usize,
            _visit: &mut dyn FnMut(&Value) -> ControlFlow<()>,
        ) {
            unreachable!("EXP never streams cells")
        }
        fn dims(&mut self, _index: usize) -> Option<(u32, u32)> {
            unreachable!("EXP never queries dims")
        }
        fn for_each_row(
            &mut self,
            _index: usize,
            _visit: &mut dyn FnMut(&[Value]) -> ControlFlow<()>,
        ) -> Result<(), ErrorKind> {
            unreachable!("EXP never walks rows")
        }
        fn for_each_used_row(
            &mut self,
            _index: usize,
            _visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
        ) -> Result<(), ErrorKind> {
            unreachable!("EXP never walks used rows")
        }
    }

    fn call(v: Value) -> Value {
        let ctx = EvalContext::new();
        let mut args = OneArg(v);
        eval(&ctx, &mut args)
    }

    #[test]
    fn exp_of_zero_is_one() {
        assert_eq!(call(Value::number(0.0)), Value::number(1.0));
    }

    #[test]
    fn exp_of_one_is_e() {
        match call(Value::number(1.0)) {
            Value::Number(n) => assert!(
                (n - std::f64::consts::E).abs() < 1e-12,
                "expected e, got {n}"
            ),
            other => panic!("expected Number, got {other:?}"),
        }
    }

    #[test]
    fn exp_overflow_is_num_error() {
        // EXP(1000) overflows f64's finite range; Value::number maps any
        // non-finite result to #NUM! (see module docs' OXP note on the
        // unpinned exact boundary).
        assert_eq!(call(Value::number(1000.0)), Value::Error(ErrorKind::Num));
    }

    #[test]
    fn exp_coerces_numeric_text() {
        assert_eq!(call(Value::text("0")), Value::number(1.0));
    }

    #[test]
    fn exp_propagates_error_argument() {
        assert_eq!(
            call(Value::Error(ErrorKind::Ref)),
            Value::Error(ErrorKind::Ref)
        );
    }

    #[test]
    fn exp_non_numeric_text_is_value_error() {
        assert_eq!(
            call(Value::text("not a number")),
            Value::Error(ErrorKind::Value)
        );
    }
}

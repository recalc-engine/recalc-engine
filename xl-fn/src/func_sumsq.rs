//! `SUMSQ` — sum the squares of all numbers across the arguments.
//!
//! # Provenance
//! Behavior contract: `docs/specs/SUMSQ.md` (which cites the Microsoft Learn
//! SUMSQ function page). Coercion is deferred entirely to `xl-value`
//! ([`coerce_number_arg`] with the two [`CoercionMode`]s), exactly as
//! [`crate::func_sum`] does — `SUMSQ` is `SUM`'s structural twin, differing
//! only in the reduction step (`acc += x * x` rather than `acc += x`).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Direct **scalar** arguments coerce under [`CoercionMode::Scalar`]: numbers
//!   pass through, `TRUE`/`FALSE` → 1/0, numeric text → its number, blank → 0,
//!   and the coerced value is then squared. This is the classic asymmetry
//!   `SUMSQ(3,"4",TRUE)` = `9 + 16 + 1` = `26` (SUMSQ.md §Coercion,
//!   hit-list). A scalar text that cannot parse as a number is `#VALUE!`.
//! - **Range / array** arguments aggregate under [`CoercionMode::RangeAggregate`]:
//!   only real numbers count (and are squared); blank, boolean, and text cells
//!   (including numeric-looking text) are silently skipped (SUMSQ.md §1,
//!   §Coercion). Array constants follow the same per-element rule.
//! - Omitted arguments contribute 0 and never error (SUMSQ.md §3).
//! - With nothing numeric anywhere the result is 0, never an error (SUMSQ.md
//!   §4).
//! - Any argument that evaluates to an error propagates it; the first error in
//!   left-to-right argument order (and, within a range, the first cell scanned)
//!   wins (SUMSQ.md §Error behavior). The exact short-circuit order when
//!   several arguments hold *different* errors simultaneously is
//!   oracle-deferred (`OXP-082`, shared with SUM/PRODUCT); the single-error
//!   corpus cases are unambiguous.
//! - Overflow (a non-finite running sum-of-squares) becomes `#NUM!` via
//!   [`Value::number`] (SUMSQ.md §Error behavior / value invariant).

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Evaluate a `SUMSQ(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut acc = 0.0_f64;

    for i in 0..args.count() {
        // RFC 0010: a single-cell REFERENCE aggregates like a range (text/logical
        // ignored); only a scalar LITERAL coerces (`SUMSQ("4")` = 16).
        match eff_shape(args, i) {
            EffShape::Omitted => {}
            EffShape::ScalarLiteral => {
                let v = args.eval_scalar_array_arg(i);
                match coerce_number_arg(&v, CoercionMode::Scalar) {
                    NumericArg::Number(n) => acc += n * n,
                    NumericArg::Skip => {}
                    NumericArg::Error(k) => return Value::Error(k),
                }
            }
            EffShape::Aggregate => {
                let mut err: Option<ErrorKind> = None;
                let acc_ref = &mut acc;
                args.for_each_cell(i, &mut |v| {
                    match coerce_number_arg(v, CoercionMode::RangeAggregate) {
                        NumericArg::Number(n) => {
                            *acc_ref += n * n;
                            ControlFlow::Continue(())
                        }
                        NumericArg::Skip => ControlFlow::Continue(()),
                        // Stop the scan at the first error rather than visiting
                        // and ignoring the rest of the range.
                        NumericArg::Error(k) => {
                            err = Some(k);
                            ControlFlow::Break(())
                        }
                    }
                });
                if let Some(k) = err {
                    return Value::Error(k);
                }
            }
        }
    }

    // Non-finite running sum-of-squares (overflow) → #NUM! per the value
    // invariant.
    Value::number(acc)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn sums_squares_of_scalar_numbers() {
        // 3^2 + 4^2 = 9 + 16 = 25.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.0)), Scalar(num(4.0))]),
            num(25.0)
        );
    }

    #[test]
    fn single_argument() {
        // 7^2 = 49.
        assert_eq!(eval_direct(eval, vec![Scalar(num(7.0))]), num(49.0));
    }

    #[test]
    fn scalar_coercion_text_and_logical() {
        // Direct scalars coerce: text "4" -> 4, TRUE -> 1, so 3^2 + 4^2 + 1^2
        // = 9 + 16 + 1 = 26.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(3.0)),
                    Scalar(txt("4")),
                    Scalar(Value::Bool(true))
                ]
            ),
            num(26.0)
        );
    }

    #[test]
    fn range_skips_text_logical_blank() {
        // Inside a range only real numbers participate: 2^2 + 5^2 = 4 + 25 =
        // 29, the text, boolean, and blank are ignored.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(2.0),
                    txt("x"),
                    Value::Bool(true),
                    Value::Blank,
                    num(5.0),
                ])]
            ),
            num(29.0)
        );
    }

    #[test]
    fn array_constant_per_element() {
        // Array constants follow the RangeAggregate per-element rule: text and
        // blank are skipped, 3^2 + 4^2 = 9 + 16 = 25.
        assert_eq!(
            eval_direct(
                eval,
                vec![Array(vec![num(3.0), txt("nope"), num(4.0), Value::Blank])]
            ),
            num(25.0)
        );
    }

    #[test]
    fn empty_range_no_numbers_is_zero() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![txt("a"), Value::Bool(false), Value::Blank])]
            ),
            num(0.0)
        );
    }

    #[test]
    fn empty_range_literally_empty_is_zero() {
        assert_eq!(eval_direct(eval, vec![Range(vec![])]), num(0.0));
    }

    #[test]
    fn no_arguments_is_zero() {
        assert_eq!(eval_direct(eval, vec![]), num(0.0));
    }

    #[test]
    fn error_scalar_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(2.0)),
                    Scalar(Value::Error(ErrorKind::Div0)),
                    Scalar(num(3.0)),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn error_range_cell_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(2.0),
                    Value::Error(ErrorKind::Value),
                    num(5.0)
                ])]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // RFC 0010: a single-cell *reference* to text is ignored (the range rule),
    // so `SUMSQ(num_ref, text_ref)` = num² (3² = 9), not `#VALUE!`; a lone text
    // reference contributes nothing → 0.
    #[test]
    fn rfc0010_text_reference_is_ignored() {
        assert_eq!(
            eval_direct(eval, vec![CellRef(num(3.0)), CellRef(txt("x"))]),
            num(9.0)
        );
        assert_eq!(eval_direct(eval, vec![CellRef(txt("x"))]), num(0.0));
    }

    // RFC 0010: a scalar *literal* still coerces — numeric text `"4"` → 4² = 16.
    #[test]
    fn rfc0010_scalar_literal_still_coerces() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("4"))]), num(16.0));
    }
}

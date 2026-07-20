//! `PRODUCT` — multiply all numbers across the arguments.
//!
//! # Provenance
//! Behavior contract: `docs/specs/PRODUCT.md` (which cites the Microsoft Learn
//! PRODUCT function page). Coercion is deferred entirely to `xl-value`
//! ([`coerce_number_arg`] with the two [`CoercionMode`]s), exactly as
//! [`crate::func_sum`] does — `PRODUCT` is `SUM`'s structural twin, differing
//! only in the reduction operator (`*` rather than `+`).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Direct **scalar** arguments coerce under [`CoercionMode::Scalar`]: numbers
//!   pass through, `TRUE`/`FALSE` → 1/0, numeric text → its number, blank → 0.
//!   This is the same asymmetry as SUM — `PRODUCT(2,"3",TRUE)` = 6 (PRODUCT.md
//!   §Coercion, hit-list). A scalar text that cannot parse as a number is
//!   `#VALUE!`, now oracle-confirmed (`OXP-110`, RESOLVED
//!   `RUN-2026-07-11-oracle01`): observed `=PRODUCT("x")` = `#VALUE!`. Note
//!   this is a coercion *error* on the direct text argument — it short-circuits
//!   *before* the empty-aggregate check below, so `PRODUCT("x")` is `#VALUE!`,
//!   not the empty-aggregate `0`.
//! - **Range / array** arguments aggregate under [`CoercionMode::RangeAggregate`]:
//!   only real numbers participate; blank, boolean, and text cells (including
//!   numeric-looking text) are silently skipped (PRODUCT.md §1, §Coercion).
//!   Array constants follow the same per-element rule.
//! - Any argument that evaluates to an error propagates it; the first error in
//!   left-to-right argument order (and, within a range, the first cell scanned)
//!   wins (PRODUCT.md §Error behavior). The exact short-circuit order when
//!   several arguments hold *different* errors simultaneously is oracle-deferred
//!   (`OXP-082`, shared with SUM); the single-error corpus cases are unambiguous.
//! - **Empty aggregate → 0.** If no numeric value participates across any
//!   argument, this returns `0`. **OXP note:** the public PRODUCT page does not
//!   state the empty-product convention; `0` is implemented to match the
//!   observed Excel SUM-family empty-aggregate behavior. `RUN-2026-07-11-oracle01`
//!   probed only the scalar-text case `=PRODUCT("x")` (which errors before this
//!   path, see above); the intended all-text/all-blank **range** probe
//!   (`=PRODUCT(A1:A3)` → `0`) was *not* run, so the empty-aggregate `= 0`
//!   convention remains implemented-but-not-yet-oracle-verified — still flagged
//!   under `OXP-110`. See PRODUCT.md §Empty aggregate.
//! - Overflow (a non-finite running product) becomes `#NUM!` via
//!   [`Value::number`] (PRODUCT.md §Error behavior / value invariant).

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Evaluate a `PRODUCT(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut acc = 1.0_f64;
    // Track whether any number actually participated: the empty aggregate must
    // return 0, not the multiplicative identity 1 (see the OXP note above).
    let mut any = false;

    for i in 0..args.count() {
        // RFC 0010: a single-cell REFERENCE aggregates like a range (text/logical
        // ignored); only a scalar LITERAL coerces (`PRODUCT("3")` = 3).
        match eff_shape(args, i) {
            EffShape::Omitted => {}
            EffShape::ScalarLiteral => {
                let v = args.eval_scalar_array_arg(i);
                match coerce_number_arg(&v, CoercionMode::Scalar) {
                    NumericArg::Number(n) => {
                        acc *= n;
                        any = true;
                    }
                    NumericArg::Skip => {}
                    NumericArg::Error(k) => return Value::Error(k),
                }
            }
            EffShape::Aggregate => {
                let mut err: Option<ErrorKind> = None;
                let acc_ref = &mut acc;
                let any_ref = &mut any;
                args.for_each_cell(i, &mut |v| {
                    match coerce_number_arg(v, CoercionMode::RangeAggregate) {
                        NumericArg::Number(n) => {
                            *acc_ref *= n;
                            *any_ref = true;
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

    // Empty aggregate (nothing numeric participated) → 0, matching the observed
    // SUM-family behavior (OXP-110, oracle-deferred).
    if !any {
        return Value::number(0.0);
    }

    // Non-finite running product (overflow) → #NUM! per the value invariant.
    Value::number(acc)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn multiplies_scalar_numbers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(2.0)), Scalar(num(3.0)), Scalar(num(4.0))]
            ),
            num(24.0)
        );
    }

    #[test]
    fn single_argument() {
        assert_eq!(eval_direct(eval, vec![Scalar(num(7.0))]), num(7.0));
    }

    #[test]
    fn scalar_coercion_text_and_logical() {
        // Direct scalars coerce: text "3" → 3, TRUE → 1, so 2 * 3 * 1 = 6.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(2.0)),
                    Scalar(txt("3")),
                    Scalar(Value::Bool(true))
                ]
            ),
            num(6.0)
        );
    }

    #[test]
    fn range_skips_text_logical_blank() {
        // Inside a range only real numbers participate: 2 * 5 = 10, the text,
        // boolean, and blank are ignored.
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
            num(10.0)
        );
    }

    #[test]
    fn array_constant_per_element() {
        // Array constants follow the RangeAggregate per-element rule: text and
        // blank are skipped, 3 * 4 = 12.
        assert_eq!(
            eval_direct(
                eval,
                vec![Array(vec![num(3.0), txt("nope"), num(4.0), Value::Blank])]
            ),
            num(12.0)
        );
    }

    #[test]
    fn empty_range_no_numbers_is_zero() {
        // No numeric element participates → empty aggregate → 0 (OXP-110).
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

    // OXP-110 (RUN-2026-07-11-oracle01): a direct non-numeric scalar text
    // coercion-errors to #VALUE! (short-circuiting before the empty-aggregate
    // path). Observed `=PRODUCT("x")` = #VALUE!.
    #[test]
    fn scalar_non_numeric_text_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("x"))]),
            Value::Error(ErrorKind::Value)
        );
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
    // so `PRODUCT(num_ref, text_ref)` = the number (6), not `#VALUE!`.
    #[test]
    fn rfc0010_text_reference_is_ignored() {
        assert_eq!(
            eval_direct(eval, vec![CellRef(num(6.0)), CellRef(txt("x"))]),
            num(6.0)
        );
        // A lone text reference contributes nothing → empty aggregate → 0.
        assert_eq!(eval_direct(eval, vec![CellRef(txt("x"))]), num(0.0));
    }

    // RFC 0010: a scalar *literal* still coerces — non-numeric text `"x"` is
    // `#VALUE!` (contrast the ignored reference above).
    #[test]
    fn rfc0010_scalar_literal_still_coerces() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("x"))]),
            Value::Error(ErrorKind::Value)
        );
    }
}

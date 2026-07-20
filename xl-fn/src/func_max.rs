//! `MAX` — the largest numeric value across all arguments.
//!
//! # Provenance
//! Behavior contract: `docs/specs/MAX.md`, which mirrors `docs/specs/MIN.md`
//! (same write-up, "largest" instead of "smallest") and cites the Microsoft
//! Learn MAX function page. Coercion is deferred entirely to `xl-value`
//! ([`coerce_number_arg`] with the two [`CoercionMode`]s), the same split
//! `SUM`/`MIN` use.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Direct **scalar** arguments coerce under [`CoercionMode::Scalar`]:
//!   numbers pass through, `TRUE`/`FALSE` → 1/0, numeric text → its number
//!   (MAX.md §Coercion). A scalar text that cannot parse as a number is
//!   `#VALUE!`.
//! - **Range / array** arguments aggregate under
//!   [`CoercionMode::RangeAggregate`]: only real numbers participate; blank,
//!   boolean, and text cells inside the range are ignored entirely
//!   (MAX.md §1/§2, §Coercion).
//! - Any argument that evaluates to an error propagates it — first error in
//!   left-to-right argument order / first cell scanned within a range wins,
//!   the same short-circuit policy `SUM`/`MIN` use (`OXP-082`; MAX.md notes
//!   the same "all-error argument set precedence order" open question as
//!   MIN, covered by the same policy).
//! - With no numeric values found anywhere, the result is `0`, never an
//!   error (MAX.md §3, §Error behavior).
//!
//! # Oracle-deferred: scalar blank argument (`OXP-087`)
//! Mirrors `MIN`'s `OXP-086` exactly (see `func_min` module docs): a
//! **scalar** argument evaluating to [`Value::Blank`] is oracle-deferred —
//! does it count as `0` or is it excluded? MAX.md's own docs insist this be
//! verified independently rather than assumed symmetric with MIN, so it
//! gets its own ID, `OXP-087`, even though the reasoning is identical. Per
//! "never guess semantics" this returns `#UNSUPPORTED!`.
//!
//! # `-0`/`+0` tie-break (`OXP-087`, RESOLVED RUN-2026-07-11-oracle01)
//! Running-maximum-scan tie-break policy: first candidate wins ties; a
//! later value only replaces the running maximum when it compares
//! *strictly* greater. The farm ran the probe
//! (`RUN-2026-07-11-oracle01`): `=MAX(-0, 0)` = 0 and `=MAX(0, -0)` = 0 —
//! both yield a zero whose displayed value is `0` regardless of argument
//! order, which the existing strict-greater policy already produces
//! (`-0.0 == 0.0` under the value model), so no behavior change. (Note:
//! the probe fixes the *displayed* result at `0`; it does not distinguish
//! the sign bit of the stored zero, which Excel does not surface here.)

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Evaluate a `MAX(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut max: Option<f64> = None;

    for i in 0..args.count() {
        // RFC 0010: a single-cell REFERENCE aggregates like a range (text/logical
        // ignored, a blank cell skipped — resolving the bare-ref half of
        // OXP-087); only a scalar LITERAL / omitted slot takes the scalar path,
        // where a blank stays oracle-deferred.
        match eff_shape(args, i) {
            // An omitted argument slot evaluates to `Blank` per
            // `CallArgs::eval_scalar`'s contract, so it hits the same
            // oracle-deferred scalar-blank case as a bare reference to an
            // empty cell (`OXP-087`, see the module docs).
            EffShape::Omitted | EffShape::ScalarLiteral => {
                let v = args.eval_scalar_array_arg(i);
                if v.is_blank() {
                    return Value::Error(ErrorKind::Unsupported);
                }
                match coerce_number_arg(&v, CoercionMode::Scalar) {
                    NumericArg::Number(n) => {
                        if max.is_none_or(|m| n > m) {
                            max = Some(n);
                        }
                    }
                    // CoercionMode::Scalar never yields Skip.
                    NumericArg::Skip => {}
                    NumericArg::Error(k) => return Value::Error(k),
                }
            }
            EffShape::Aggregate => {
                let mut err: Option<ErrorKind> = None;
                let max_ref = &mut max;
                args.for_each_cell(i, &mut |v| match coerce_number_arg(
                    v,
                    CoercionMode::RangeAggregate,
                ) {
                    NumericArg::Number(n) => {
                        if max_ref.is_none_or(|m| n > m) {
                            *max_ref = Some(n);
                        }
                        ControlFlow::Continue(())
                    }
                    NumericArg::Skip => ControlFlow::Continue(()),
                    NumericArg::Error(k) => {
                        err = Some(k);
                        ControlFlow::Break(())
                    }
                });
                if let Some(k) = err {
                    return Value::Error(k);
                }
            }
        }
    }

    Value::number(max.unwrap_or(0.0))
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // OXP-087 (RUN-2026-07-11-oracle01): the -0/+0 tie-break yields a
    // displayed 0 regardless of argument order. Observed `=MAX(-0, 0)` = 0.
    #[test]
    fn negative_zero_first_yields_zero() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-0.0)), Scalar(num(0.0))]),
            num(0.0)
        );
    }

    // OXP-087 (RUN-2026-07-11-oracle01): observed `=MAX(0, -0)` = 0.
    #[test]
    fn positive_zero_first_yields_zero() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0)), Scalar(num(-0.0))]),
            num(0.0)
        );
    }

    // RFC 0010 / OXP-087 (bare-ref half): a single-cell *reference* to an empty
    // cell is now SKIPPED (the range rule), not oracle-deferred, so
    // `MAX(blank_ref, 5)` = 5 instead of `#UNSUPPORTED!`.
    #[test]
    fn rfc0010_blank_reference_skipped_resolves_oxp087() {
        assert_eq!(
            eval_direct(eval, vec![CellRef(Value::Blank), Scalar(num(5.0))]),
            num(5.0)
        );
    }

    // RFC 0010: a single-cell *reference* to text/logical is ignored (the range
    // rule), so `MAX(num_ref, text_ref)` = the number, not `#VALUE!`.
    #[test]
    fn rfc0010_text_reference_is_ignored() {
        assert_eq!(
            eval_direct(eval, vec![CellRef(num(3.0)), CellRef(txt("x"))]),
            num(3.0)
        );
    }

    // RFC 0010: the scalar *literal* / omitted blank path is UNCHANGED — a
    // literal blank stays oracle-deferred (`OXP-087`), never guessed.
    #[test]
    fn rfc0010_scalar_literal_blank_still_deferred() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Blank), Scalar(num(5.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

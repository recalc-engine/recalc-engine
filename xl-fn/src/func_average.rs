//! `AVERAGE` — arithmetic mean of all numeric values across the arguments.
//!
//! # Provenance
//! Behavior contract: `docs/specs/AVERAGE.md` (which cites the Microsoft
//! Learn AVERAGE function page). Coercion is deferred entirely to
//! `xl-value` ([`coerce_number_arg`] with the two [`CoercionMode`]s), the
//! same split `SUM` uses.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - The mean is SUM of included values / COUNT of included values, using
//!   the same scalar-vs-range inclusion rules as SUM/COUNT (AVERAGE.md §1).
//! - Direct **scalar** arguments coerce under [`CoercionMode::Scalar`]:
//!   numbers pass through, `TRUE`/`FALSE` → 1/0, numeric text → its number.
//!   A scalar text that cannot parse as a number is `#VALUE!` (AVERAGE.md
//!   §Coercion).
//! - **Range / array** arguments aggregate under
//!   [`CoercionMode::RangeAggregate`]: only real numbers contribute to
//!   *both* the sum and the count; blank, boolean, and text cells
//!   (including numeric-looking text) inside a range never participate —
//!   critically, a range blank is *excluded from the denominator*, not
//!   counted as a zero (AVERAGE.md §2, §3; this is the documented contrast
//!   with SUM's scalar-blank-as-zero rule).
//! - Any argument that evaluates to an error propagates it, first error in
//!   left-to-right argument order / first cell scanned within a range wins
//!   (AVERAGE.md §Error behavior), the same short-circuit policy as `SUM`
//!   (`OXP-082`).
//! - With nothing numeric found anywhere, the result is `#DIV/0!` (never 0
//!   like SUM/MIN/MAX) (AVERAGE.md §4, §Error behavior).
//!
//! # Scalar blank argument (`OXP-083`, RESOLVED RUN-2026-07-11-oracle01)
//! AVERAGE.md's own "Oracle experiments needed" section flagged the *one*
//! place its scalar rule might not simply mirror SUM's: a **scalar**
//! argument that evaluates to [`Value::Blank`] (a bare reference to an
//! empty cell, or an elided argument slot — [`CallArgs::eval_scalar`]
//! documents both evaluate to `Blank`). SUM would coerce this to `0`
//! (`CoercionMode::Scalar` → `to_number(Blank) == 0`), silently entering
//! the denominator; AVERAGE's *range* rule instead excludes blanks from the
//! denominator entirely. The farm ran the probe (`RUN-2026-07-11-oracle01`):
//! `=AVERAGE(A1,5)` = 5, `=AVERAGE(A1,5,10)` = 7.5, and the scalar form
//! equals the range form `=AVERAGE(A1:A1,5)` = 5 — so a **scalar blank is
//! EXCLUDED** (follows AVERAGE's range-blank rule, *not* SUM's zero rule).
//! Implemented by skipping it (see `eval`). Every other scalar shape
//! (number, bool, numeric/non-numeric text, error) is fully supported.

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Evaluate an `AVERAGE(...)` call. See the module docs for the semantics
/// and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut sum = 0.0_f64;
    let mut count = 0u64;

    for i in 0..args.count() {
        // RFC 0010: a single-cell REFERENCE aggregates like a range (its
        // text/logical/blank ignored); only a scalar LITERAL / omitted slot
        // takes the scalar path below. `eff_shape` makes the distinction
        // `shape()` alone cannot.
        match eff_shape(args, i) {
            // An omitted argument slot evaluates to `Blank` per
            // `CallArgs::eval_scalar`'s contract, so it hits the same
            // oracle-deferred scalar-blank case as a bare reference to an
            // empty cell (`OXP-083`, see the module docs) — both are
            // handled uniformly below.
            EffShape::Omitted | EffShape::ScalarLiteral => {
                let v = args.eval_scalar_array_arg(i);
                // OXP-083 (RUN-2026-07-11-oracle01, RESOLVED): a scalar Blank
                // is EXCLUDED from both the sum and the denominator — it does
                // *not* count as 0 the way SUM would. Observed:
                // `=AVERAGE(A1,5)` = 5 (not 2.5), `=AVERAGE(A1,5,10)` = 7.5,
                // and the scalar form matches the range form `=AVERAGE(A1:A1,5)`
                // = 5. So a scalar blank follows AVERAGE's range-blank rule.
                if v.is_blank() {
                    continue;
                }
                match coerce_number_arg(&v, CoercionMode::Scalar) {
                    NumericArg::Number(n) => {
                        sum += n;
                        count += 1;
                    }
                    // CoercionMode::Scalar never yields Skip.
                    NumericArg::Skip => {}
                    NumericArg::Error(k) => return Value::Error(k),
                }
            }
            EffShape::Aggregate => {
                let mut err: Option<ErrorKind> = None;
                let sum_ref = &mut sum;
                let count_ref = &mut count;
                args.for_each_cell(i, &mut |v| {
                    match coerce_number_arg(v, CoercionMode::RangeAggregate) {
                        NumericArg::Number(n) => {
                            *sum_ref += n;
                            *count_ref += 1;
                            ControlFlow::Continue(())
                        }
                        // Blank/text/logical cells inside a range never
                        // enter the denominator (AVERAGE.md §2/§3).
                        NumericArg::Skip => ControlFlow::Continue(()),
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

    if count == 0 {
        return Value::Error(ErrorKind::Div0);
    }
    Value::number(sum / count as f64)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::Value;

    // OXP-083 (RUN-2026-07-11-oracle01): a scalar Blank is EXCLUDED from both
    // sum and denominator (not counted as 0). Observed `=AVERAGE(A1,5)` = 5.
    #[test]
    fn scalar_blank_excluded_two_args() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Blank), Scalar(num(5.0))]),
            num(5.0)
        );
    }

    // OXP-083 (RUN-2026-07-11-oracle01): observed `=AVERAGE(A1,5,10)` = 7.5 —
    // denominator is 2, the blank does not enter it.
    #[test]
    fn scalar_blank_excluded_three_args() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Blank), Scalar(num(5.0)), Scalar(num(10.0))]
            ),
            num(7.5)
        );
    }

    // OXP-083 (RUN-2026-07-11-oracle01): an omitted slot evaluates to Blank
    // and is likewise excluded. Mirrors the range form `=AVERAGE(A1:A1,5)` = 5.
    #[test]
    fn omitted_slot_excluded() {
        assert_eq!(eval_direct(eval, vec![Omitted, Scalar(num(5.0))]), num(5.0));
    }

    // OXP-083 (RUN-2026-07-11-oracle01): the scalar form matches the range
    // form — `=AVERAGE(A1:A1,5)` = 5 with A1 blank.
    #[test]
    fn scalar_blank_matches_range_form() {
        assert_eq!(
            eval_direct(eval, vec![Range(vec![Value::Blank]), Scalar(num(5.0))]),
            num(5.0)
        );
    }

    // RFC 0010: a single-cell *reference* to text is IGNORED (excluded from the
    // denominator, the range rule), not coerced to `#VALUE!`. Here the text ref
    // drops out and only the number counts → 4/1 = 4.
    #[test]
    fn rfc0010_text_reference_is_ignored() {
        assert_eq!(
            eval_direct(eval, vec![CellRef(num(4.0)), CellRef(txt("x"))]),
            num(4.0)
        );
    }

    // RFC 0010: a scalar *literal* still coerces under `CoercionMode::Scalar`,
    // so numeric text `"6"` counts as 6 → (4 + 6)/2 = 5.
    #[test]
    fn rfc0010_scalar_literal_still_coerces() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(4.0)), Scalar(txt("6"))]),
            num(5.0)
        );
    }
}

//! `MEDIAN` — the median (middle value) of all numeric arguments.
//!
//! # Provenance
//! Behavior contract: `docs/specs/MEDIAN.md` (which cites the Microsoft Learn
//! MEDIAN function page). Coercion is deferred entirely to `xl-value`
//! ([`coerce_number_arg`] with the two [`CoercionMode`]s), the same
//! scalar-vs-range split `SUM`/`AVERAGE`/`STDEV` use. MS Learn's MEDIAN remarks
//! ("If an array or reference argument contains text, logical values, or empty
//! cells, those values are ignored; however, cells with the value zero are
//! included") are exactly the range-aggregate rule; a *directly typed* logical
//! or numeric-text argument coerces, mirroring the SUM/AVERAGE family.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Participating numbers are gathered with the SUM/AVERAGE inclusion rules
//!   (MEDIAN.md §1): direct **scalar** args coerce under
//!   [`CoercionMode::Scalar`] (numbers pass, `TRUE`/`FALSE` → 1/0, numeric text
//!   → its number); **range / array** args aggregate under
//!   [`CoercionMode::RangeAggregate`] (only real numbers contribute — blank,
//!   boolean, text, and numeric-looking text inside a range never participate;
//!   a numeric zero *cell* does participate, since it is a real number).
//! - The gathered numbers are sorted; the median is the single middle value for
//!   an **odd** count, or the **average of the two middle values** for an
//!   **even** count (MEDIAN.md §2, the documented even-count rule).
//! - Empty numeric set → `#NUM!` (MEDIAN.md §3): with no numbers there is no
//!   middle value. `Number1` is documented required, so the "nothing numeric
//!   anywhere" case surfaces the numeric error rather than a silent value.
//! - Any argument that evaluates to an error propagates it; the first error in
//!   left-to-right argument order (and, within a range, the first cell scanned)
//!   wins (MEDIAN.md §Error behavior) — the same short-circuit policy as `SUM`
//!   (`OXP-082`).
//!
//! # Numerical method
//! Exact last-ULP agreement with Excel is asserted only for an **odd** count,
//! where the result is one of the input numbers verbatim (a comparison-only
//! selection). The **even** case averages the two middle values `(a + b) / 2`;
//! this single add-then-halve can differ from Excel's own intermediate rounding
//! in the final bit(s), so conformance is asserted at the declared grid
//! tolerance, not bit-exactness. Values are materialized into a `Vec<f64>` so
//! they can be sorted (a second pass over the *values*, not a re-evaluation of
//! the *arguments*).
//!
//! # Oracle-deferred: scalar blank argument
//! Like `AVERAGE`/`STDEV`, a **scalar** argument that evaluates to
//! [`Value::Blank`] (a bare reference to an empty cell, or an elided argument
//! slot) raises the unresolved scalar-blank question: is it coerced to `0` and
//! counted as a data point (SUM's `CoercionMode::Scalar` rule), or excluded
//! like a range blank? `AVERAGE`'s `OXP-083` resolved that question *for
//! AVERAGE* (excluded), but the MAX/MIN specs insist this be verified per
//! function rather than assumed symmetric, and MEDIAN's own even/odd split
//! makes "does a blank shift the middle" a distinct observable. Per "never
//! guess semantics" this returns `#UNSUPPORTED!` rather than picking a reading.
//! Every other scalar shape (number, bool, numeric/non-numeric text, error) is
//! fully supported.
//
// OXP (unassigned): =MEDIAN(A1,1,2,3) with A1 blank — is the blank excluded
// (median of {1,2,3} = 2) or counted as 0 (median of {0,1,2,3} = 1.5)?

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Evaluate a `MEDIAN(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Materialize the participating numbers so the middle value(s) can be
    // selected by sorting, without re-evaluating arguments.
    let mut xs: Vec<f64> = Vec::new();

    for i in 0..args.count() {
        // RFC 0010: a single-cell REFERENCE aggregates like a range (text/logical
        // ignored, a blank cell skipped); only a scalar LITERAL / omitted slot
        // takes the scalar path, where a blank stays oracle-deferred.
        match eff_shape(args, i) {
            // An omitted slot evaluates to `Blank` (like a bare empty-cell
            // reference), hitting the same oracle-deferred scalar-blank case.
            EffShape::Omitted | EffShape::ScalarLiteral => {
                let v = args.eval_scalar_array_arg(i);
                // A scalar Blank is oracle-deferred rather than guessed as
                // either "counts as 0" or "excluded" (see module docs).
                if v.is_blank() {
                    return Value::Error(ErrorKind::Unsupported);
                }
                match coerce_number_arg(&v, CoercionMode::Scalar) {
                    NumericArg::Number(n) => xs.push(n),
                    // CoercionMode::Scalar never yields Skip.
                    NumericArg::Skip => {}
                    NumericArg::Error(k) => return Value::Error(k),
                }
            }
            EffShape::Aggregate => {
                let mut err: Option<ErrorKind> = None;
                let xs_ref = &mut xs;
                args.for_each_cell(i, &mut |v| {
                    match coerce_number_arg(v, CoercionMode::RangeAggregate) {
                        NumericArg::Number(n) => {
                            xs_ref.push(n);
                            ControlFlow::Continue(())
                        }
                        // Blank/text/logical cells inside a range are ignored
                        // (MS Learn MEDIAN remarks); a numeric 0 cell is a real
                        // number and is kept.
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

    let n = xs.len();
    // No numbers anywhere → there is no middle value → #NUM!.
    if n == 0 {
        return Value::Error(ErrorKind::Num);
    }

    // Total order over finite f64 (the value model forbids NaN/Inf in a
    // `Value::Number`, so `total_cmp` here is a plain numeric sort).
    xs.sort_by(f64::total_cmp);

    let mid = n / 2;
    if n % 2 == 1 {
        // Odd count: the single middle element, returned verbatim.
        Value::number(xs[mid])
    } else {
        // Even count: the average of the two middle elements (documented rule).
        Value::number((xs[mid - 1] + xs[mid]) / 2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn odd_count_middle_value() {
        // {1,2,3,4,5} sorted → middle (index 2) = 3.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(1.0),
                    num(2.0),
                    num(3.0),
                    num(4.0),
                    num(5.0),
                ])],
            ),
            num(3.0)
        );
    }

    #[test]
    fn odd_count_unsorted_input() {
        // Unsorted {7,1,3,9,5} → sorted {1,3,5,7,9} → median 5.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(7.0),
                    num(1.0),
                    num(3.0),
                    num(9.0),
                    num(5.0),
                ])],
            ),
            num(5.0)
        );
    }

    #[test]
    fn even_count_averages_two_middles() {
        // {1,2,3,4} → two middles 2 and 3 → (2+3)/2 = 2.5.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), num(2.0), num(3.0), num(4.0)])],
            ),
            num(2.5)
        );
    }

    #[test]
    fn even_count_two_values() {
        // {10,20} → (10+20)/2 = 15.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(10.0)), Scalar(num(20.0))]),
            num(15.0)
        );
    }

    #[test]
    fn single_value_is_itself() {
        // n=1 → the value itself.
        assert_eq!(eval_direct(eval, vec![Scalar(num(42.0))]), num(42.0));
    }

    #[test]
    fn range_skips_text_logical_blank() {
        // Only the real numbers {1,2,3,4} participate → median 2.5; the
        // text/logical/blank cells are ignored per MS Learn.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(1.0),
                    txt("x"),
                    Value::Bool(true),
                    num(2.0),
                    Value::Blank,
                    num(3.0),
                    txt("99"),
                    num(4.0),
                ])],
            ),
            num(2.5)
        );
    }

    #[test]
    fn zero_cell_participates() {
        // A numeric 0 is a real number, kept: {0,1,2} → median 1 (not 1.5,
        // which is what dropping the 0 would give).
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(0.0), num(1.0), num(2.0)])]),
            num(1.0)
        );
    }

    #[test]
    fn scalar_coercion_text_and_logical() {
        // Direct scalars coerce: TRUE→1, "5"→5, 3 → {1,3,5} → median 3.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(true)),
                    Scalar(num(3.0)),
                    Scalar(txt("5"))
                ],
            ),
            num(3.0)
        );
    }

    #[test]
    fn empty_is_num_error() {
        // A range holding only non-numbers contributes nothing → #NUM!.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![txt("a"), Value::Bool(false), Value::Blank])]
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn scalar_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(1.0)),
                    Scalar(Value::Error(ErrorKind::Value)),
                    Scalar(num(2.0)),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn range_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), Value::Error(ErrorKind::Na), num(2.0)])]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn scalar_blank_is_oracle_deferred() {
        // A scalar Blank (elided slot / bare empty-cell ref) is not guessed —
        // it returns #UNSUPPORTED! like AVERAGE/STDEV.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0)), Omitted, Scalar(num(2.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // RFC 0010: a single-cell *reference* to text is ignored (the range rule),
    // so a text ref drops out and the median is over the remaining numbers →
    // {1,2,3} → 2, instead of `#VALUE!`.
    #[test]
    fn rfc0010_text_reference_is_ignored() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    CellRef(txt("x")),
                    Scalar(num(1.0)),
                    Scalar(num(2.0)),
                    Scalar(num(3.0))
                ]
            ),
            num(2.0)
        );
    }

    // RFC 0010 (bare-ref half of the scalar-blank question): a single-cell
    // *reference* to an empty cell is now SKIPPED (the range rule), so
    // `MEDIAN(blank_ref, 1, 2, 3)` = 2 rather than `#UNSUPPORTED!`.
    #[test]
    fn rfc0010_blank_reference_skipped() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    CellRef(Value::Blank),
                    Scalar(num(1.0)),
                    Scalar(num(2.0)),
                    Scalar(num(3.0))
                ]
            ),
            num(2.0)
        );
    }

    // RFC 0010: a scalar *literal* still coerces — numeric text `"5"` → 5, so
    // {1,3,5} → median 3 (the scalar path is unchanged).
    #[test]
    fn rfc0010_scalar_literal_still_coerces() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("5")), Scalar(num(1.0)), Scalar(num(3.0))]
            ),
            num(3.0)
        );
    }
}

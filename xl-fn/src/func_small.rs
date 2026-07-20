//! `SMALL` — the k-th **smallest** numeric value in a data set.
//!
//! # Provenance
//! Behavior contract: `docs/specs/SMALL.md` (which cites the Microsoft Learn
//! SMALL function page). `SMALL` is the mirror of `LARGE` (see `func_large`):
//! the same participating-numbers set and the same `k` coercion/validation, but
//! it counts from the smallest end. The array's number-inclusion rules are
//! deferred to `xl-value` ([`coerce_number_arg`] with the two
//! [`CoercionMode`]s), exactly as `MIN` does; `k` is coerced by [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `SMALL(array, k)` returns the k-th smallest value: `k = 1` is the minimum,
//!   `k = n` (n = the count of numbers) is the maximum (SMALL.md §1).
//! - The participating numbers are gathered from `array` with `MIN`'s inclusion
//!   rules (SMALL.md §Coercion): a **scalar** `array` coerces under
//!   [`CoercionMode::Scalar`] (numbers pass, `TRUE`/`FALSE` → 1/0, numeric text
//!   → its number); a **range / array** aggregates under
//!   [`CoercionMode::RangeAggregate`] (only real numbers participate — blank,
//!   boolean, and text cells are ignored entirely).
//! - `k` is a scalar coerced to a number ([`to_number`]: `TRUE` → 1, numeric
//!   text → its number). `k ≤ 0`, `k >` the count, or an empty array →
//!   `#NUM!` (SMALL.md §2/§3, the documented error rows).
//! - Any argument that evaluates to an error propagates it; the array is scanned
//!   first (its first error wins, the `SUM`/`MIN` short-circuit policy,
//!   `OXP-082`), then `k` is coerced.
//!
//! # Oracle-deferred: non-integer `k`
//! MS Learn documents only `k ≤ 0` and `k >` count; it does **not** say what a
//! *fractional* `k` does. `ROUND`'s `OXP-098` established that a non-integer
//! `num_digits` truncates toward zero, but that resolution was probed for
//! `ROUND` specifically — it is **not** confirmed for `SMALL`, and "plausible by
//! analogy" is not a source under Recalc Principle 2. So a non-integer (or
//! non-finite) `k` returns `#UNSUPPORTED!` rather than a guessed truncation.
//! The integer-`k` core — every documented case — is fully supported. The
//! deferral is flagged with an `OXP (unassigned)` probe note at the `resolve_k`
//! deferral site below.
//!
//! # Oracle-deferred: scalar blank `array`
//! A **scalar** `array` argument that is [`Value::Blank`] (a bare empty-cell
//! reference) hits the same unresolved scalar-blank question as `MAX`/`MIN`:
//! count as `0` or exclude? Deferred to `#UNSUPPORTED!`, mirroring `MIN`.

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg, to_number};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Evaluate a `SMALL(array, k)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Gather the participating numbers from `array` (arg 0), then resolve `k`
    // (arg 1) — the array is scanned first so its errors win (OXP-082 order).
    let mut xs = match collect_array_numbers(args) {
        Ok(xs) => xs,
        Err(k) => return Value::Error(k),
    };

    let count = xs.len();
    let k = match resolve_k(args, count) {
        Ok(k) => k,
        Err(k) => return Value::Error(k),
    };

    // k-th smallest: ascending sort, then the element at 1-based position `k`
    // from the bottom (index k-1). k=1 → index 0 (the minimum); k=count →
    // index count-1 (the maximum). Total order over finite f64 (the value
    // model forbids NaN/Inf in a `Value::Number`, so `total_cmp` is a plain
    // numeric sort here).
    xs.sort_by(f64::total_cmp);
    Value::number(xs[k - 1])
}

/// Collect the participating numbers from the `array` argument (index 0) using
/// `MIN`'s inclusion rules: a scalar coerces (booleans/numeric text included),
/// a range/array keeps only real number cells. Returns the propagated
/// [`ErrorKind`] for an erroring cell/argument, or an oracle-deferred
/// `#UNSUPPORTED!` for a scalar `Blank` (see the module docs).
fn collect_array_numbers(args: &mut dyn CallArgs) -> Result<Vec<f64>, ErrorKind> {
    let mut xs: Vec<f64> = Vec::new();
    match args.shape(0) {
        ArgShape::Omitted | ArgShape::Scalar => {
            let v = args.eval_scalar(0);
            // A scalar Blank array is oracle-deferred (count-as-0 vs exclude),
            // mirroring MAX/MIN — see module docs.
            if v.is_blank() {
                return Err(ErrorKind::Unsupported);
            }
            match coerce_number_arg(&v, CoercionMode::Scalar) {
                NumericArg::Number(n) => xs.push(n),
                // CoercionMode::Scalar never yields Skip.
                NumericArg::Skip => {}
                NumericArg::Error(k) => return Err(k),
            }
        }
        ArgShape::Range | ArgShape::Array => {
            let mut err: Option<ErrorKind> = None;
            let xs_ref = &mut xs;
            args.for_each_cell(
                0,
                &mut |v| match coerce_number_arg(v, CoercionMode::RangeAggregate) {
                    NumericArg::Number(n) => {
                        xs_ref.push(n);
                        ControlFlow::Continue(())
                    }
                    NumericArg::Skip => ControlFlow::Continue(()),
                    NumericArg::Error(k) => {
                        err = Some(k);
                        ControlFlow::Break(())
                    }
                },
            );
            if let Some(k) = err {
                return Err(k);
            }
        }
    }
    Ok(xs)
}

/// Resolve the `k` argument (index 1) to a 1-based index into a `count`-element
/// data set, mirroring `LARGE`'s resolver.
///
/// Returns `Ok(k)` with `1 <= k <= count`, or `Err(ErrorKind)`:
/// - `#VALUE!`/propagated error if `k` cannot coerce to a number.
/// - `#UNSUPPORTED!` for a non-integer or non-finite `k` (oracle-deferred — see
///   the module docs; the ROUND `OXP-098` truncation family is not confirmed
///   for these functions).
/// - `#NUM!` for `k <= 0`, an empty data set (`count == 0`), or `k > count`.
fn resolve_k(args: &mut dyn CallArgs, count: usize) -> Result<usize, ErrorKind> {
    let k_raw = to_number(&args.eval_scalar(1))?;
    // Non-integer / non-finite k: truncate-vs-error is not documented or
    // oracle-confirmed for LARGE/SMALL → defer rather than guess.
    // OXP (unassigned): =SMALL({3,1,2},2.9) and =SMALL({3,1,2},1.5) — is a
    // fractional k truncated toward zero (the ROUND OXP-098 family: 2.9→k=2)
    // or a #NUM!/#VALUE! error? Probe before supporting fractional k.
    if !k_raw.is_finite() || k_raw.fract() != 0.0 {
        return Err(ErrorKind::Unsupported);
    }
    // k <= 0 or empty array → #NUM! (documented). k_raw is a whole number here.
    if k_raw < 1.0 || count == 0 {
        return Err(ErrorKind::Num);
    }
    // Float→int cast saturates at usize::MAX, so an astronomically large whole
    // k lands in the `k > count` #NUM! path below rather than wrapping.
    let k = k_raw as usize;
    if k > count {
        return Err(ErrorKind::Num);
    }
    Ok(k)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn k_one_is_the_min() {
        // SMALL(array, 1) = the smallest value.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(3.0), num(1.0), num(4.0), num(1.0), num(5.0)]),
                    Scalar(num(1.0)),
                ],
            ),
            num(1.0)
        );
    }

    #[test]
    fn k_middle_value() {
        // Sorted ascending {1,1,3,4,5}: the 3rd smallest is 3.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(3.0), num(1.0), num(4.0), num(1.0), num(5.0)]),
                    Scalar(num(3.0)),
                ],
            ),
            num(3.0)
        );
    }

    #[test]
    fn k_equals_n_is_the_max() {
        // SMALL(array, n) = the largest value.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(3.0), num(1.0), num(4.0), num(1.0), num(5.0)]),
                    Scalar(num(5.0)),
                ],
            ),
            num(5.0)
        );
    }

    #[test]
    fn k_zero_is_num_error() {
        // k ≤ 0 → #NUM!.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), num(2.0), num(3.0)]), Scalar(num(0.0))],
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn k_negative_is_num_error() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), num(2.0), num(3.0)]), Scalar(num(-1.0))],
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn k_beyond_count_is_num_error() {
        // k > number of data points → #NUM!.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), num(2.0), num(3.0)]), Scalar(num(4.0))],
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn empty_array_is_num_error() {
        // No numbers in the array → #NUM! regardless of k.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![txt("a"), Value::Bool(true), Value::Blank]),
                    Scalar(num(1.0)),
                ],
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn range_skips_text_logical_blank() {
        // Only {3,1,4,5} participate; the 2nd smallest is 3.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![
                        num(3.0),
                        txt("x"),
                        num(1.0),
                        Value::Bool(true),
                        num(4.0),
                        Value::Blank,
                        num(5.0),
                    ]),
                    Scalar(num(2.0)),
                ],
            ),
            num(3.0)
        );
    }

    #[test]
    fn k_coerces_from_numeric_text() {
        // k given as the text "2" coerces to 2 → 2nd smallest of {3,4,5} = 4.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(3.0), num(4.0), num(5.0)]), Scalar(txt("2"))],
            ),
            num(4.0)
        );
    }

    #[test]
    fn non_integer_k_is_oracle_deferred() {
        // Fractional k: truncate-vs-error is not documented/confirmed for
        // SMALL → #UNSUPPORTED! (not a guessed truncation).
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), num(2.0), num(3.0)]), Scalar(num(2.9))],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn array_error_propagates() {
        // An error cell in the array propagates (scanned before k).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), Value::Error(ErrorKind::Na), num(3.0)]),
                    Scalar(num(1.0)),
                ],
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn k_error_propagates() {
        // A non-coercible text k → #VALUE!.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), num(2.0)]), Scalar(txt("abc"))],
            ),
            Value::Error(ErrorKind::Value)
        );
    }
}

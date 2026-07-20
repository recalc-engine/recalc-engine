//! `STDEV` — estimate of the **sample** standard deviation (the "n-1" method).
//!
//! # Provenance
//! Behavior contract: `docs/specs/STDEV.md` (which cites the Microsoft Learn
//! STDEV function page). Coercion is deferred entirely to `xl-value`
//! ([`coerce_number_arg`] with the two [`CoercionMode`]s), the same
//! scalar-vs-range split `SUM`/`AVERAGE` use — MS Learn's STDEV remarks
//! ("Logical values and text representations of numbers that you type directly
//! into the list of arguments are counted"; "If an argument is an array or
//! reference, only numbers in that array or reference are counted. Empty cells,
//! logical values, text, or error values in the array or reference are
//! ignored") are exactly this asymmetry. Including logicals/text from a
//! *reference* is the sibling function `STDEVA`, not `STDEV`.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Participating numbers are gathered with the SUM/AVERAGE inclusion rules
//!   (STDEV.md §1): direct **scalar** args coerce under
//!   [`CoercionMode::Scalar`] (numbers pass, `TRUE`/`FALSE` → 1/0, numeric text
//!   → its number); **range / array** args aggregate under
//!   [`CoercionMode::RangeAggregate`] (only real numbers contribute — blank,
//!   boolean, text, and numeric-looking text inside a range never participate).
//! - Sample standard deviation: with the gathered values `x₁..xₙ`, `mean =
//!   Σxᵢ / n`, `variance = Σ(xᵢ − mean)² / (n − 1)`, and the result is
//!   `sqrt(variance)` (STDEV.md §2). This is the documented **n-1** method.
//! - `n < 2` → `#DIV/0!`: the documented `n − 1` denominator is `0` (n=1) or
//!   `−1` (n=0); Excel surfaces the degenerate sample as a division-by-zero
//!   error (STDEV.md §3). A single number has no sample spread.
//! - Any argument that evaluates to an error propagates it; the first error in
//!   left-to-right argument order (and, within a range, the first cell scanned)
//!   wins (STDEV.md §Error behavior) — the same short-circuit policy as `SUM`
//!   (`OXP-082`).
//!
//! # Numerical method (STDEV.md §Numerical method)
//! The variance is computed by the **two-pass** algorithm — first accumulate
//! the mean, then accumulate `Σ(xᵢ − mean)²` — never the naive one-pass
//! `Σxᵢ² − (Σxᵢ)²/n`, which suffers catastrophic cancellation when the mean is
//! large relative to the spread. This requires materializing the participating
//! numbers (a second pass over the *values*, not a re-evaluation of the
//! *arguments*, which could re-trigger side effects), so they are collected
//! into a `Vec<f64>` as arguments are forced left-to-right.
//!
//! Exact last-ULP agreement with Excel's own STDEV is **not** claimed: Excel's
//! internal summation order and intermediate rounding are unpublished, so the
//! two-pass result can differ from Excel's in the final bit(s). The module
//! implements the standard stable estimator and does not assert bit-exactness;
//! conformance is asserted only at the declared grid tolerance.
//!
//! **`OXP-117` RESOLVED by RUN-2026-07-11-oracle01.** The oracle observed
//! `STDEV(A1:A10)` = `3.689323936863109` over the set
//! `{2,4,4,4,5,5,7,9,12,13}` (mean 6.5, Σ(x−mean)² = 122.5, variance 122.5/9);
//! the two-pass estimator here reproduces that value to full displayed f64
//! precision (the ≤ `1e-9` grid tolerance holds with room to spare — no
//! tolerance change is required, and none was made). See the
//! `oracle_ten_value_sample` test.
//!
//! # Oracle-deferred: scalar blank argument (`OXP-083`)
//! Like `AVERAGE`, STDEV's `n` sits in a denominator, so a **scalar** argument
//! that evaluates to [`Value::Blank`] (a bare reference to an empty cell, or an
//! elided argument slot) raises the same unresolved question AVERAGE defers: is
//! it coerced to `0` and counted as a data point (SUM's `CoercionMode::Scalar`
//! rule), or excluded like a range blank? The docs do not say. Per the "never
//! guess semantics" rule this returns `#UNSUPPORTED!` rather than picking a
//! reading; it shares AVERAGE's `OXP-083` (`docs/oracle-experiments.md`). Every
//! other scalar shape (number, bool, numeric/non-numeric text, error) is fully
//! supported.

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Evaluate a `STDEV(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Materialize the participating numbers so variance can use the stable
    // two-pass method (mean, then Σ(x−mean)²) without re-evaluating arguments.
    let mut xs: Vec<f64> = Vec::new();

    for i in 0..args.count() {
        // RFC 0010: a single-cell REFERENCE aggregates like a range (text/logical
        // ignored, a blank cell skipped — this is STDEV, not STDEVA); only a
        // scalar LITERAL / omitted slot takes the scalar path, where a blank
        // stays oracle-deferred (OXP-083).
        match eff_shape(args, i) {
            // An omitted slot evaluates to `Blank` (like a bare empty-cell
            // reference), hitting the same oracle-deferred scalar-blank case.
            EffShape::Omitted | EffShape::ScalarLiteral => {
                let v = args.eval_scalar_array_arg(i);
                // OXP-083: a scalar Blank is oracle-deferred rather than
                // guessed as either "counts as 0" or "excluded".
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
                        // (MS Learn STDEV remarks — this is STDEV, not STDEVA).
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
    // n-1 denominator is 0 (n==1) or negative (n==0): a sample of fewer than
    // two numbers has no defined spread → #DIV/0!.
    if n < 2 {
        return Value::Error(ErrorKind::Div0);
    }

    // Pass 1: mean.
    let mean = xs.iter().sum::<f64>() / n as f64;
    // Pass 2: Σ(x − mean)².
    let ss: f64 = xs.iter().map(|&x| (x - mean) * (x - mean)).sum();
    let variance = ss / (n as f64 - 1.0);

    // Value::number maps a non-finite result (overflow) to #NUM! per the
    // value invariant.
    Value::number(variance.sqrt())
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    /// Assert an `eval_direct` result is a `Number` within 1e-9 of `expected`.
    fn assert_close(got: Value, expected: f64) {
        match got {
            Value::Number(r) => assert!(
                (r - expected).abs() < 1e-9,
                "expected ≈ {expected}, got {r}"
            ),
            other => panic!("expected a number ≈ {expected}, got {other:?}"),
        }
    }

    #[test]
    fn classic_eight_value_sample() {
        // {2,4,4,4,5,5,7,9}: mean 5, Σ(x−5)² = 32, variance 32/7, √ ≈ 2.13809.
        // (The textbook set whose *population* σ is 2 but *sample* s ≈ 2.138.)
        assert_close(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(2.0),
                    num(4.0),
                    num(4.0),
                    num(4.0),
                    num(5.0),
                    num(5.0),
                    num(7.0),
                    num(9.0),
                ])],
            ),
            2.138_089_935_299_395,
        );
    }

    #[test]
    fn oracle_ten_value_sample() {
        // OXP-117 RESOLVED (RUN-2026-07-11-oracle01): STDEV(A1:A10) over
        // {2,4,4,4,5,5,7,9,12,13} = 3.689323936863109 (sample s; mean 6.5,
        // Σ(x−mean)² = 122.5, variance 122.5/9). Matched within the ≤1e-9 grid
        // tolerance — no tolerance was weakened.
        assert_close(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(2.0),
                    num(4.0),
                    num(4.0),
                    num(4.0),
                    num(5.0),
                    num(5.0),
                    num(7.0),
                    num(9.0),
                    num(12.0),
                    num(13.0),
                ])],
            ),
            3.689_323_936_863_109,
        );
    }

    #[test]
    fn small_known_set() {
        // {1,2,3,4,5}: mean 3, Σ(x−3)² = 10, variance 10/4 = 2.5, √2.5.
        assert_close(
            eval_direct(
                eval,
                vec![
                    Scalar(num(1.0)),
                    Scalar(num(2.0)),
                    Scalar(num(3.0)),
                    Scalar(num(4.0)),
                    Scalar(num(5.0)),
                ],
            ),
            2.5_f64.sqrt(),
        );
    }

    #[test]
    fn scalar_coercion_text_and_logical_counted() {
        // Direct scalars coerce (MS Learn: typed logicals/text are counted):
        // TRUE→1, "5"→5, 3 → sample {1,5,3}: mean 3, Σ(x−3)²=8, var 4, √4=2.
        assert_close(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(true)),
                    Scalar(txt("5")),
                    Scalar(num(3.0)),
                ],
            ),
            2.0,
        );
    }

    #[test]
    fn range_skips_text_logical_blank() {
        // A range with text/logical/blank mixed in yields the same result as
        // the pure-numeric {2,4,4,4,5,5,7,9}: only real numbers participate.
        assert_close(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(2.0),
                    txt("x"),
                    Value::Bool(true),
                    num(4.0),
                    num(4.0),
                    Value::Blank,
                    num(4.0),
                    num(5.0),
                    txt("99"),
                    num(5.0),
                    num(7.0),
                    num(9.0),
                ])],
            ),
            2.138_089_935_299_395,
        );
    }

    #[test]
    fn single_number_is_div0() {
        // n = 1 → n-1 = 0 → #DIV/0!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(5.0))]),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn empty_sample_is_div0() {
        // A range holding only non-numbers contributes no data points → n = 0.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![txt("a"), Value::Bool(false), Value::Blank])]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn scalar_error_propagates() {
        // An error argument propagates as STDEV's result.
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
        // An error cell inside a range propagates too (first error wins).
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(
                    vec![num(1.0), Value::Error(ErrorKind::Na), num(2.0),]
                )]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn scalar_blank_is_oracle_deferred() {
        // OXP-083: a scalar Blank (elided slot / bare empty-cell ref) is not
        // guessed — it returns #UNSUPPORTED! like AVERAGE.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0)), Omitted, Scalar(num(2.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // RFC 0010: a single-cell *reference* to text is ignored (the range rule),
    // so the text ref drops out and STDEV is over the classic 8-value sample
    // {2,4,4,4,5,5,7,9} ≈ 2.138, instead of `#VALUE!`.
    #[test]
    fn rfc0010_text_reference_is_ignored() {
        assert_close(
            eval_direct(
                eval,
                vec![
                    CellRef(txt("x")),
                    Scalar(num(2.0)),
                    Scalar(num(4.0)),
                    Scalar(num(4.0)),
                    Scalar(num(4.0)),
                    Scalar(num(5.0)),
                    Scalar(num(5.0)),
                    Scalar(num(7.0)),
                    Scalar(num(9.0)),
                ],
            ),
            2.138_089_935_299_395,
        );
    }

    // RFC 0010: a scalar *literal* still coerces — TRUE→1, "5"→5, 3 → sample
    // {1,5,3}: mean 3, Σ(x−3)²=8, var 4, √4=2 (the scalar path is unchanged).
    #[test]
    fn rfc0010_scalar_literal_still_coerces() {
        assert_close(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(true)),
                    Scalar(txt("5")),
                    Scalar(num(3.0)),
                ],
            ),
            2.0,
        );
    }
}

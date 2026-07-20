//! `MINA` — the smallest value across all arguments, counting logicals and
//! text (unlike `MIN`).
//!
//! # Provenance
//! Behavior contract: `docs/specs/MINA.md` (which cites the Microsoft Learn
//! MINA function page, `support.microsoft.com/en-us/office/mina-function-
//! 245a6f46-7ca5-4dc7-ab49-805341bc31d3`, fetched 2026-07-11).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `MINA` and `MIN` are **identical** on the scalar/direct-argument path:
//!   numbers pass through, `TRUE`/`FALSE` → 1/0, numeric text → its number,
//!   non-numeric text → `#VALUE!` (MINA.md §Coercion — "text that cannot be
//!   translated into numbers cause errors" is verbatim MS Learn language
//!   shared with `MIN`'s page). This reuses [`coerce_number_arg`] with
//!   [`CoercionMode::Scalar`], exactly as `func_min` does.
//! - `MINA` and `MIN` **diverge only in range/array aggregation** in how they
//!   treat logicals (MINA.md §Coercion, §Key distinction): a `TRUE` cell
//!   counts as `1` and a `FALSE` cell counts as `0`, whereas `MIN` skips
//!   logicals. A **text** cell is ignored/skipped (numeric-looking or not),
//!   exactly as `MIN` does — it is NOT counted as `0`. A genuinely blank cell
//!   is ignored in both functions. So counting logicals is the *only*
//!   range-side difference between `MINA` and `MIN`.
//! - Any argument that evaluates to an error propagates it — same
//!   short-circuit policy as `MIN`/`SUM` (first error encountered, in
//!   argument order / cell-scan order, wins).
//! - With no values found anywhere, the result is `0`, matching `MIN` and
//!   explicitly documented on the MINA page ("If the arguments contain no
//!   values, MINA returns 0").
//!
//! # MS Learn self-contradiction, RESOLVED by the oracle
//! The live MINA remarks contain two bullets that directly contradict each
//! other: one says text in an array/reference is "ignored" (verbatim MIN/MAX
//! boilerplate), the next says "arguments that contain text or FALSE evaluate
//! as 0". **OXP-127 RESOLVED by RUN-2026-07-11-oracle01: range text is
//! ignored (not 0)** — the oracle observed `MINA({5,"hello"})` = `5` (and its
//! `MAXA` twin `MAXA({-5,"hello"})` = `-5`), so the "ignored" bullet wins and
//! the "evaluate as 0" bullet does not apply to text drawn from a
//! range/array. An earlier revision guessed count-as-0 (a wrong
//! harmonization); the measured result overturns it. See `MINA.md` and
//! `docs/oracle-experiments.md` OXP-127 for the citation.
//!
//! # Scalar blank argument — `OXP-131` RESOLVED (bare ref); omitted still deferred
//! **RESOLVED by RUN-2026-07-11-oracle01:** a **scalar** blank from a *bare
//! reference to an empty cell* is **skipped** (contributes nothing), exactly as
//! a range blank is. The oracle observed `MINA(A1)` = `MINA(A1:A1)` = `0` (A1
//! empty): the single-cell reference and the single-cell range produce the
//! identical result, and the range path already skips the blank — so the scalar
//! path skips it too, rather than guessing count-as-`0`. With no other value the
//! aggregate is empty → `0` (the documented "no values → 0").
//!
//! An **omitted argument slot** (`MINA(,5)`) also evaluates to [`Value::Blank`]
//! but has no single-cell-reference twin and was **not** probed by this run, so
//! it stays deferred to `#UNSUPPORTED!` rather than assume it behaves like the
//! bare reference. (`MIN`'s own scalar-blank question remains `OXP-086`.)

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{ArgShape, CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Coerces a single **range/array** cell for `MINA`'s aggregation.
///
/// Unlike [`CoercionMode::RangeAggregate`] (which `MIN` uses and which skips
/// `Bool`/`Text`/`Blank` alike), `MINA` counts `Bool` (`TRUE` → 1, `FALSE` →
/// 0). It **ignores** a `Text` cell (numeric-looking or not) exactly as `MIN`
/// does, and ignores a genuinely blank cell — so the *only* divergence from
/// `MIN`'s range handling is that logicals are counted. See the module docs
/// for the OXP-127 oracle resolution behind the text rule.
fn mina_cell(value: &Value) -> NumericArg {
    match value {
        Value::Number(n) => NumericArg::Number(*n),
        Value::Bool(b) => NumericArg::Number(if *b { 1.0 } else { 0.0 }),
        // OXP-127 RESOLVED by RUN-2026-07-11-oracle01: text in a range/array
        // is ignored (skipped), NOT counted as 0. `MINA({5,"hello"})` = 5.
        Value::Text(_) => NumericArg::Skip,
        Value::Blank => NumericArg::Skip,
        Value::Error(k) => NumericArg::Error(*k),
        // Same as MIN's RangeAggregate handling: an array constant or an
        // unresolved reference nested inside a range/array argument is not a
        // shape this function resolves itself.
        Value::Array(_) | Value::Ref(_) => NumericArg::Error(ErrorKind::Unsupported),
        // BC-6 (RFC-0012): a lambda is refused, not counted or skipped.
        Value::Lambda(_) => NumericArg::Error(ErrorKind::Unsupported),
    }
}

/// Coerces a single cell from a top-level **array constant** `{...}` `MINA`
/// argument. Unlike a range/reference, an array constant **drops** logicals
/// (and text and blanks) — only numbers contribute; errors propagate. Pinned
/// by **OXP-188 (RUN-2026-07-13)**: `MINA({5,TRUE})` = `5` (the logical is
/// dropped, not counted `1`), mirroring `SUM`'s array-constant rule (OXP-006)
/// and `MAXA`'s. The MINA-vs-`MIN` "count logicals" difference applies to the
/// range/reference form only; here `{...}` behaves exactly like `MIN`.
fn array_constant_cell(value: &Value) -> NumericArg {
    match value {
        Value::Number(n) => NumericArg::Number(*n),
        // OXP-188: a logical inside an array constant is dropped, not 1/0.
        Value::Bool(_) | Value::Text(_) | Value::Blank => NumericArg::Skip,
        Value::Error(k) => NumericArg::Error(*k),
        Value::Array(_) | Value::Ref(_) => NumericArg::Error(ErrorKind::Unsupported),
        // BC-6 (RFC-0012): a lambda in an array constant is refused.
        Value::Lambda(_) => NumericArg::Error(ErrorKind::Unsupported),
    }
}

/// Evaluate a `MINA(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut min: Option<f64> = None;

    for i in 0..args.count() {
        // RFC 0010: a single-cell REFERENCE aggregates like a range (logicals
        // counted, text ignored per OXP-127, a blank cell skipped) — so a bare
        // empty-cell reference now rides the `EffShape::Aggregate` arm and is
        // skipped there, while only a scalar LITERAL / omitted slot reaches the
        // scalar path below (where an *omitted* blank stays deferred).
        let shape = eff_shape(args, i);
        match shape {
            EffShape::Omitted | EffShape::ScalarLiteral => {
                let v = args.eval_scalar_array_arg(i);
                if v.is_blank() {
                    // OXP-131 RESOLVED (RUN-2026-07-11-oracle01): a scalar blank
                    // from a bare empty-cell reference is skipped, exactly as a
                    // range blank — `MINA(A1)` = `MINA(A1:A1)` = 0. An *omitted*
                    // slot (`MINA(,5)`) has no cell-reference twin and was not
                    // probed, so it stays deferred. See the module docs.
                    if shape == EffShape::Omitted {
                        return Value::Error(ErrorKind::Unsupported);
                    }
                    continue;
                }
                // Scalar coercion is identical to MIN's — see module docs.
                match coerce_number_arg(&v, CoercionMode::Scalar) {
                    NumericArg::Number(n) => {
                        if min.is_none_or(|m| n < m) {
                            min = Some(n);
                        }
                    }
                    // CoercionMode::Scalar never yields Skip.
                    NumericArg::Skip => {}
                    NumericArg::Error(k) => return Value::Error(k),
                }
            }
            EffShape::Aggregate => {
                // OXP-188: a top-level array constant drops logicals (see
                // `array_constant_cell`); a range/reference (incl. a single-cell
                // ref riding this arm) counts them, so branch on the underlying
                // shape rather than on `eff_shape`.
                let resolve = if matches!(args.shape(i), ArgShape::Array) {
                    array_constant_cell
                } else {
                    mina_cell
                };
                let mut err: Option<ErrorKind> = None;
                let min_ref = &mut min;
                args.for_each_cell(i, &mut |v| match resolve(v) {
                    NumericArg::Number(n) => {
                        if min_ref.is_none_or(|m| n < m) {
                            *min_ref = Some(n);
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

    Value::number(min.unwrap_or(0.0))
}

#[cfg(test)]
mod tests {
    use xl_value::{ErrorKind, Value};

    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    use super::eval;

    #[test]
    fn range_counts_logicals_but_ignores_text_unlike_min() {
        // OXP-127: text is ignored (not 0). {2, "x", TRUE, FALSE, 5} is seen
        // as {2, 1, 0, 5} (text skipped) -> min is 0, from FALSE -> 0.
        let result = eval_direct(
            eval,
            vec![Range(vec![
                num(2.0),
                txt("x"),
                Value::Bool(true),
                Value::Bool(false),
                num(5.0),
            ])],
        );
        assert_eq!(result, num(0.0));
    }

    #[test]
    fn scalar_numbers_behave_like_min() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(4.0)), Scalar(num(2.0))]),
            num(2.0)
        );
    }

    #[test]
    fn range_true_counts_as_one() {
        let result = eval_direct(eval, vec![Range(vec![Value::Bool(true), num(5.0)])]);
        assert_eq!(result, num(1.0));
    }

    #[test]
    fn range_false_counts_as_zero() {
        let result = eval_direct(eval, vec![Range(vec![Value::Bool(false), num(5.0)])]);
        assert_eq!(result, num(0.0));
    }

    #[test]
    fn range_text_is_ignored_not_counted() {
        // OXP-127 RESOLVED by RUN-2026-07-11-oracle01: text in a range is
        // ignored (skipped), NOT counted as 0. {5, "hello"} -> {5} -> min 5.
        let result = eval_direct(eval, vec![Range(vec![num(5.0), txt("hello")])]);
        assert_eq!(result, num(5.0));
    }

    #[test]
    fn array_constant_drops_logicals_oxp188() {
        // OXP-188 (RUN-2026-07-13): a logical inside an ARRAY CONSTANT is
        // DROPPED, not counted 1/0 — Excel pins MINA({5,TRUE}) = 5. Contrast
        // `range_true_counts_as_one` above, where the same TRUE counts as 1 in
        // a range and would win the min. Mirrors SUM's OXP-006 and MAXA's.
        assert_eq!(
            eval_direct(eval, vec![Array(vec![num(5.0), Value::Bool(true)])]),
            num(5.0)
        );
        // Regression guard: a RANGE with the same contents still counts TRUE=1.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(5.0), Value::Bool(true)])]),
            num(1.0)
        );
    }

    #[test]
    fn oracle_mina_range_text_ignored_is_five() {
        // Pins RUN-2026-07-11-oracle01: MINA({5,"hello"}) = 5 (text ignored).
        let result = eval_direct(eval, vec![Range(vec![num(5.0), txt("hello")])]);
        assert_eq!(result, num(5.0));
    }

    #[test]
    fn range_blank_is_ignored_not_counted() {
        // Only a genuinely blank cell is skipped; here the min is the real
        // number, not 0 from the blank.
        let result = eval_direct(eval, vec![Range(vec![Value::Blank, num(3.0)])]);
        assert_eq!(result, num(3.0));
    }

    #[test]
    fn error_in_range_propagates() {
        let result = eval_direct(
            eval,
            vec![Range(vec![num(1.0), Value::Error(ErrorKind::Div0)])],
        );
        assert_eq!(result, Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn error_scalar_propagates() {
        let result = eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Na))]);
        assert_eq!(result, Value::Error(ErrorKind::Na));
    }

    #[test]
    fn no_values_returns_zero() {
        let result = eval_direct(eval, vec![Range(vec![Value::Blank, Value::Blank])]);
        assert_eq!(result, num(0.0));
    }

    #[test]
    fn scalar_true_is_one_scalar_false_is_zero() {
        assert_eq!(eval_direct(eval, vec![Scalar(Value::Bool(true))]), num(1.0));
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Bool(false))]),
            num(0.0)
        );
    }

    #[test]
    fn scalar_numeric_text_coerces_to_number() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("3"))]), num(3.0));
    }

    #[test]
    fn scalar_non_numeric_text_is_value_error() {
        let result = eval_direct(eval, vec![Scalar(txt("x"))]);
        assert_eq!(result, Value::Error(ErrorKind::Value));
    }

    #[test]
    fn scalar_blank_bare_reference_is_skipped() {
        // OXP-131 RESOLVED (RUN-2026-07-11-oracle01): a scalar blank from a
        // bare empty-cell reference is skipped, so a lone blank aggregates to
        // 0 — the oracle observed MINA(A1) = 0 (A1 empty).
        let result = eval_direct(eval, vec![Scalar(Value::Blank)]);
        assert_eq!(result, num(0.0));
    }

    #[test]
    fn oracle_mina_blank_reference_is_zero() {
        // Pins RUN-2026-07-11-oracle01: MINA(A1) = 0 and MINA(A1:A1) = 0 with
        // A1 empty — the scalar single-cell reference matches the 1×1 range.
        assert_eq!(eval_direct(eval, vec![Scalar(Value::Blank)]), num(0.0));
        assert_eq!(eval_direct(eval, vec![Range(vec![Value::Blank])]), num(0.0));
    }

    #[test]
    fn omitted_slot_blank_still_deferred() {
        // The omitted-slot sub-case (MINA(,5)) was NOT probed by OXP-131, so it
        // stays #UNSUPPORTED! rather than assume it behaves like a bare ref.
        let result = eval_direct(eval, vec![Omitted, Scalar(num(5.0))]);
        assert_eq!(result, Value::Error(ErrorKind::Unsupported));
    }

    // RFC 0010: a single-cell *reference* to text is IGNORED (OXP-127 range
    // rule), not coerced, so `MINA(text_ref, 5)` = 5 instead of `#VALUE!`.
    #[test]
    fn rfc0010_text_reference_is_ignored() {
        assert_eq!(
            eval_direct(eval, vec![CellRef(txt("hello")), Scalar(num(5.0))]),
            num(5.0)
        );
    }

    // RFC 0010: a scalar *literal* still coerces under `CoercionMode::Scalar`
    // (OXP-130), so non-numeric text typed directly is `#VALUE!` — the sharp
    // contrast with the ignored reference above.
    #[test]
    fn rfc0010_scalar_literal_still_coerces() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("hello")), Scalar(num(5.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn min_vs_mina_diverge_on_a_mixed_range() {
        // MIN over the same range would see only {2, 5} -> 2. MINA counts the
        // logical (TRUE -> 1) but ignores the text (OXP-127), so it sees
        // {2, 1, 5} -> 1. This locks in MINA's side of that divergence: the
        // difference is the counted logical, not the text.
        let result = eval_direct(
            eval,
            vec![Range(vec![num(2.0), txt("x"), Value::Bool(true), num(5.0)])],
        );
        assert_eq!(result, num(1.0));
    }
}

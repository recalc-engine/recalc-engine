//! `MAXA` — the largest value across all arguments, counting logical values
//! (and, for a **range/array**, counting them differently than direct
//! arguments) unlike `MAX`, which ignores them entirely inside ranges.
//!
//! # Provenance
//! Behavior contract: `docs/specs/MAXA.md`, clean-room from the Microsoft
//! Learn MAXA function page
//! (`https://support.microsoft.com/en-us/office/maxa-function-814bda1e-3840-4bff-9365-2f59ac2ee62d`,
//! verified 2026-07-11), cross-checked against MIN/MAX's parallel page
//! (`https://support.microsoft.com/en-us/office/max-function-e0012414-9ac8-4b34-9a47-73e662c08098`)
//! and MINA's page (`https://support.microsoft.com/en-us/office/mina-function-245a6f46-7ca5-4dc7-ab49-805341bc31d3`)
//! — MAXA's own page explicitly defers its examples to MINA's ("MAXA is
//! similar to MINA"), and both share identical Remarks wording. See
//! `docs/specs/MAXA.md` for the full verbatim citations and the reasoning
//! below.
//!
//! # Semantics implemented (spec bullets in parentheses)
//!
//! ## Range / array arguments
//! MAX/MIN's page reads: *"Empty cells, logical values, or text in the
//! array or reference are ignored."* MAXA/MINA's page reads: *"Empty cells
//! **and text values** in the array or reference are ignored"* — the
//! phrase "logical values" was deliberately dropped from the otherwise
//! near-identical sentence (word-for-word diff against the MAX page,
//! MAXA.md §Coercion). Combined with MAXA's own worked example (a range
//! `{0, 0.2, 0.5, 0.4, TRUE}` whose `MAXA` result is `1`, "[b]ecause a TRUE
//! value evaluates to 1, it is the largest") this pins:
//! - Numbers count as themselves.
//! - `TRUE`/`FALSE` count as `1`/`0` (the one thing MAXA adds over `MAX`
//!   for ranges — confirmed by the worked example).
//! - **Text is ignored (skipped), NOT counted as `0`.** **OXP-127 RESOLVED
//!   by RUN-2026-07-11-oracle01: range text is ignored (not 0)** — the oracle
//!   observed `MAXA({-5,"hello"})` = `-5` (and `MINA({5,"hello"})` = `5`), so
//!   text drawn from a range/array is treated exactly as `MAX`/`MIN` treat
//!   it. The page's self-contradiction (the "text values ... are ignored"
//!   clause vs. the "arguments that contain text ... evaluate as 0" bullet)
//!   is resolved in favor of "ignored". An earlier revision guessed
//!   count-as-0 (a wrong harmonization with `MINA`/AVERAGEA); the measurement
//!   overturns it. A genuinely blank cell is likewise ignored. The sole
//!   MAXA-vs-MAX range difference is that logicals are counted.
//! - Any error cell propagates.
//!
//! ### Array constants vs ranges — the logical exception
//! The "logicals count as 1/0" rule above holds for a **range or reference**
//! (and a single-cell reference). A top-level **array constant** `{...}`
//! instead **drops** logicals, exactly as `MAX`/`SUM` do — **OXP-188 (RUN-
//! 2026-07-13)** pinned `MAXA({-5,FALSE})` = `-5`, `MAXA({0.5,TRUE})` = `0.5`,
//! `MINA({5,TRUE})` = `5` (mirroring SUM's array-constant rule OXP-006). See
//! [`array_constant_value`]; the branch is selected by the argument's
//! [`ArgShape`].
//!
//! ## Direct (scalar) arguments
//! - Numbers count as themselves.
//! - `TRUE`/`FALSE` count as `1`/`0` (MAXA.md §Coercion, unambiguous: both
//!   "[l]ogical values ... typed directly ... are counted" and "[a]rguments
//!   that contain TRUE evaluate as 1 ... FALSE evaluate as 0" agree).
//! - Any error propagates.
//! - **A direct text argument coerces via `to_number` (`OXP-130` RESOLVED).**
//!   **OXP-130 RESOLVED by RUN-2026-07-11-oracle01**: the oracle observed
//!   `MINA(2,"5")` = `2` (numeric text `"5"` parses to its value `5`, which
//!   loses the min to `2`) and `MAXA(2,"abc")` = `#VALUE!` (non-numeric text
//!   errors). So a direct/scalar text argument parses numeric text to its
//!   number and raises `#VALUE!` on text that cannot be translated — the
//!   same [`CoercionMode::Scalar`] rule `MINA`/`MIN` already use, NOT `0` and
//!   NOT `#UNSUPPORTED!`. `MAXA` and `MINA` now share identical scalar-text
//!   handling. An earlier revision deferred this to `#UNSUPPORTED!`; the
//!   measurement resolves it.
//! - **A direct `Blank` argument — `OXP-131` RESOLVED for a bare empty-cell
//!   reference; an omitted slot stays deferred.** **RESOLVED by
//!   RUN-2026-07-11-oracle01:** a scalar blank from a *bare reference to an
//!   empty cell* is **skipped** (contributes nothing), exactly as a range
//!   blank — the oracle observed `MAXA(A1)` = `MAXA(A1:A1)` = `0` (A1 empty),
//!   so the single-cell reference matches the single-cell range (which already
//!   skips the blank) instead of guessing count-as-`0`. An **omitted slot**
//!   (`MAXA(,5)`) also yields [`Value::Blank`] but has no cell-reference twin
//!   and was **not** probed, so it stays `#UNSUPPORTED!` (mirroring `MAX`'s own
//!   still-open `OXP-087`).
//!
//! ## Empty aggregate
//! With no values found anywhere, MAXA returns `0`, never an error (MAXA.md
//! §Semantics; MAXA's page states this directly: "If the arguments contain
//! no values, MAXA returns 0").
//!
//! # Observability of the range-text rule
//! The range example `{-2, "x", TRUE, FALSE}` evaluates to `1` under either
//! reading (`TRUE` -> `1` is the maximum regardless of whether the text cell
//! is skipped or contributes `0`), so it does not distinguish them. A range
//! holding *only* a negative number and text — no logical — does: the oracle
//! probe `MAXA({-5,"hello"})` returned `-5` (text ignored), overturning the
//! earlier count-as-0 guess. See the `range_text_is_ignored_not_zero` test
//! and `docs/specs/MAXA.md` for the full citation trail.

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{ArgShape, CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Evaluate a `MAXA(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut max: Option<f64> = None;

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
                    // range blank — `MAXA(A1)` = `MAXA(A1:A1)` = 0. An *omitted*
                    // slot (`MAXA(,5)`) has no cell-reference twin and was not
                    // probed, so it stays deferred. See the module docs.
                    if shape == EffShape::Omitted {
                        return Value::Error(ErrorKind::Unsupported);
                    }
                    continue;
                }
                // Scalar coercion is identical to MINA's (and MIN's): numbers
                // pass through, TRUE/FALSE -> 1/0, numeric text -> its value,
                // non-numeric text -> #VALUE! (OXP-130 RESOLVED by
                // RUN-2026-07-11-oracle01 — MAXA(2,"abc") = #VALUE!).
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
                // OXP-188 (RUN-2026-07-13): a logical inside a top-level ARRAY
                // CONSTANT `{...}` is DROPPED, not counted 1/0 — `MAXA({-5,FALSE})`
                // = -5, `MAXA({0.5,TRUE})` = 0.5 (mirrors SUM's array-constant
                // rule OXP-006). The MAXA-vs-MAX "count logicals" difference
                // applies only to a RANGE/reference; a single-cell reference also
                // rides this Aggregate arm but must count logicals like a range,
                // so branch on the underlying shape, not on `eff_shape`.
                let is_array_constant = matches!(args.shape(i), ArgShape::Array);
                let mut err: Option<ErrorKind> = None;
                let max_ref = &mut max;
                let resolve = if is_array_constant {
                    array_constant_value
                } else {
                    range_value
                };
                args.for_each_cell(i, &mut |v| match resolve(v) {
                    RangeOutcome::Number(n) => {
                        if max_ref.is_none_or(|m| n > m) {
                            *max_ref = Some(n);
                        }
                        ControlFlow::Continue(())
                    }
                    RangeOutcome::Skip => ControlFlow::Continue(()),
                    RangeOutcome::Error(k) => {
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

/// Outcome of resolving a value pulled from a **range/array** `MAXA`
/// argument.
enum RangeOutcome {
    /// Contribute this number to the running maximum.
    Number(f64),
    /// Skip this cell entirely (not counted): blank or text.
    Skip,
    /// Propagate this error out of the whole call.
    Error(ErrorKind),
}

/// Resolves a value drawn from a range/array `MAXA` argument per the module
/// docs' §Range rules: numbers pass through, `TRUE`/`FALSE` -> `1`/`0`
/// (unlike `MAX`, which ignores logicals in ranges), **text is ignored**
/// (skipped, exactly as `MAX`; see OXP-127), a genuinely blank cell is
/// ignored, errors propagate.
fn range_value(v: &Value) -> RangeOutcome {
    match v {
        Value::Number(n) => RangeOutcome::Number(*n),
        Value::Bool(b) => RangeOutcome::Number(if *b { 1.0 } else { 0.0 }),
        // OXP-127 RESOLVED by RUN-2026-07-11-oracle01: text in a range/array
        // is ignored (skipped), NOT counted as 0. The oracle observed
        // `MAXA({-5,"hello"})` = -5 (not 0). So text is treated exactly as
        // `MAX` treats it; the sole MAXA-vs-MAX range difference is that
        // logicals are counted. An earlier revision guessed text->0 (a wrong
        // harmonization with the "evaluate as 0" bullet); the measurement
        // overturns it.
        Value::Text(_) => RangeOutcome::Skip,
        Value::Blank => RangeOutcome::Skip,
        Value::Error(k) => RangeOutcome::Error(*k),
        // An unresolved Ref must be pre-resolved by the caller.
        Value::Array(_) | Value::Ref(_) => RangeOutcome::Error(ErrorKind::Unsupported),
        // BC-6 (RFC-0012): a lambda is refused, not counted or skipped.
        Value::Lambda(_) => RangeOutcome::Error(ErrorKind::Unsupported),
    }
}

/// Resolve a value drawn from a top-level **array constant** `{...}` `MAXA`
/// argument. Unlike a range/reference, an array constant **drops** logicals
/// (and text and blanks) — only numbers contribute; errors propagate. Pinned
/// by **OXP-188 (RUN-2026-07-13)**: `MAXA({-5,FALSE})` = `-5`,
/// `MAXA({0.5,TRUE})` = `0.5`, `MINA({5,TRUE})` = `5` — the logical is dropped,
/// not counted `1`/`0`, mirroring `SUM`'s array-constant rule (OXP-006). The
/// MAXA-vs-`MAX` "count logicals" difference applies to the range/reference
/// form only; here `{...}` behaves exactly like `MAX`.
fn array_constant_value(v: &Value) -> RangeOutcome {
    match v {
        Value::Number(n) => RangeOutcome::Number(*n),
        // OXP-188: a logical inside an array constant is dropped, not 1/0.
        Value::Bool(_) | Value::Text(_) | Value::Blank => RangeOutcome::Skip,
        Value::Error(k) => RangeOutcome::Error(*k),
        Value::Array(_) | Value::Ref(_) => RangeOutcome::Error(ErrorKind::Unsupported),
        // BC-6 (RFC-0012): a lambda in an array constant is refused.
        Value::Lambda(_) => RangeOutcome::Error(ErrorKind::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn range_true_is_the_max_task_example() {
        // Range{-2, "x", TRUE, FALSE} -> 1. Robust under either reading of
        // range-text (ignored vs. counted as 0): TRUE -> 1 is the max
        // either way, since -2 < 1 and 0 <= 1.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(-2.0),
                    txt("x"),
                    Value::Bool(true),
                    Value::Bool(false),
                ])]
            ),
            num(1.0)
        );
    }

    #[test]
    fn scalar_numbers_like_max() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(4.0)), Scalar(num(2.0))]),
            num(4.0)
        );
    }

    #[test]
    fn scalar_true_counts_as_one() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Bool(true)), Scalar(num(0.5))]),
            num(1.0)
        );
    }

    #[test]
    fn range_false_counts_as_zero_and_beats_negative() {
        // Where MAX would see only the number -5 -> -5, MAXA counts FALSE
        // as 0, which is larger: the MAX-vs-MAXA divergence case.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(-5.0), Value::Bool(false)])]),
            num(0.0)
        );
    }

    #[test]
    fn array_constant_drops_logicals_oxp188() {
        // OXP-188 (RUN-2026-07-13): a logical inside an ARRAY CONSTANT is
        // DROPPED (not counted 1/0), unlike a range/reference. Excel pins
        // MAXA({-5,FALSE}) = -5 and MAXA({0.5,TRUE}) = 0.5. Contrast
        // `range_false_counts_as_zero_and_beats_negative` above, where the same
        // FALSE counts as 0 in a range. Mirrors SUM's array-constant rule
        // (OXP-006).
        assert_eq!(
            eval_direct(eval, vec![Array(vec![num(-5.0), Value::Bool(false)])]),
            num(-5.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Array(vec![num(0.5), Value::Bool(true)])]),
            num(0.5)
        );
        // Regression guard: a RANGE with the same contents still counts the
        // logical (the fix must not disturb the range/reference rule).
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(-5.0), Value::Bool(false)])]),
            num(0.0)
        );
    }

    #[test]
    fn range_text_is_ignored_not_zero() {
        // OXP-127 RESOLVED by RUN-2026-07-11-oracle01: text in a range is
        // ignored (skipped), NOT counted as 0. Here {-5, "hello"} → {-5} →
        // max is -5, exactly as plain MAX would give.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(-5.0), txt("hello")])]),
            num(-5.0)
        );
    }

    #[test]
    fn oracle_maxa_range_text_ignored_is_negative_five() {
        // Pins RUN-2026-07-11-oracle01: MAXA({-5,"hello"}) = -5 (text ignored).
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(-5.0), txt("hello")])]),
            num(-5.0)
        );
    }

    #[test]
    fn range_blank_is_ignored() {
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(3.0), Value::Blank])]),
            num(3.0)
        );
    }

    #[test]
    fn range_error_cell_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), Value::Error(ErrorKind::Div0)])]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn scalar_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(2.0)), Scalar(Value::Error(ErrorKind::Na))]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn scalar_non_numeric_text_is_value_error() {
        // OXP-130 RESOLVED by RUN-2026-07-11-oracle01: a direct non-numeric
        // text argument errors with #VALUE! (not #UNSUPPORTED!, not 0).
        // Oracle: MAXA(2,"abc") = #VALUE!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("x")), Scalar(num(2.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn oracle_maxa_scalar_non_numeric_text_is_value_error() {
        // Pins RUN-2026-07-11-oracle01: MAXA(2,"abc") = #VALUE!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.0)), Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn scalar_numeric_text_coerces_to_its_value() {
        // OXP-130 RESOLVED: numeric-looking direct text parses to its number
        // (SUM/MINA-style scalar coercion), NOT 0 and NOT #UNSUPPORTED!.
        // {"5", 2} -> {5, 2} -> max is 5.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("5")), Scalar(num(2.0))]),
            num(5.0)
        );
    }

    #[test]
    fn scalar_blank_bare_reference_is_skipped() {
        // OXP-131 RESOLVED (RUN-2026-07-11-oracle01): a scalar blank from a
        // bare empty-cell reference is skipped — a lone blank aggregates to 0.
        // Oracle: MAXA(A1) = 0 (A1 empty).
        assert_eq!(eval_direct(eval, vec![Scalar(Value::Blank)]), num(0.0));
    }

    #[test]
    fn oracle_maxa_blank_reference_is_zero() {
        // Pins RUN-2026-07-11-oracle01: MAXA(A1) = 0 and MAXA(A1:A1) = 0 with
        // A1 empty — the scalar single-cell reference matches the 1×1 range.
        assert_eq!(eval_direct(eval, vec![Scalar(Value::Blank)]), num(0.0));
        assert_eq!(eval_direct(eval, vec![Range(vec![Value::Blank])]), num(0.0));
    }

    #[test]
    fn omitted_slot_blank_still_deferred() {
        // The omitted-slot sub-case (MAXA(,2)) was NOT probed by OXP-131, so it
        // stays #UNSUPPORTED! rather than assume it behaves like a bare ref
        // (mirrors MAX's still-open OXP-087).
        assert_eq!(
            eval_direct(eval, vec![Omitted, Scalar(num(2.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn empty_aggregate_is_zero() {
        assert_eq!(eval_direct(eval, vec![Range(vec![])]), num(0.0));
    }

    #[test]
    fn no_args_is_zero() {
        assert_eq!(eval_direct(eval, vec![]), num(0.0));
    }

    // RFC 0010: a single-cell *reference* to text is IGNORED (OXP-127 range
    // rule), not coerced, so `MAXA(text_ref, -5)` = -5 instead of `#VALUE!`.
    #[test]
    fn rfc0010_text_reference_is_ignored() {
        assert_eq!(
            eval_direct(eval, vec![CellRef(txt("hello")), Scalar(num(-5.0))]),
            num(-5.0)
        );
    }

    // RFC 0010: a scalar *literal* still coerces under `CoercionMode::Scalar`
    // (OXP-130), so non-numeric text typed directly is `#VALUE!` — the sharp
    // contrast with the ignored reference above.
    #[test]
    fn rfc0010_scalar_literal_still_coerces() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("hello")), Scalar(num(-5.0))]),
            Value::Error(ErrorKind::Value)
        );
    }
}

//! `AND` — logical conjunction across all arguments.
//!
//! # Provenance
//! Behavior contract: `docs/specs/AND.md` (Microsoft Learn AND function
//! page, verified 2026-07-05). Scalar boolean coercion is entirely
//! `xl-value`'s [`to_bool`] — the frozen coercion contract, the same one
//! `IF` already uses for its test argument (`func_if.rs`).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Reduces to `TRUE` only if every *contributing* logical value is `TRUE`;
//!   `FALSE` as soon as one is `FALSE` (AND.md §1).
//! - **Scalar** arguments coerce through [`to_bool`]: numbers (`0`→`FALSE`,
//!   nonzero→`TRUE`) and `"TRUE"`/`"FALSE"` text (case-insensitive) coerce;
//!   other text is `#VALUE!` (AND.md §Coercion). An omitted argument slot
//!   evaluates to `Blank` per `CallArgs::eval_scalar`'s contract and is
//!   handled identically to a scalar `Blank` — see the oracle-deferred note
//!   below.
//! - **Range/array** arguments: a `Bool` cell contributes its value and a
//!   `Number` cell **participates** via [`to_bool`] (`0` → `FALSE`, nonzero →
//!   `TRUE`) — pinned by `OXP-208`/`OXP-174` (RUN-2026-07-16-oracle01) plus
//!   `OXP-184` (RUN 2026-07-12). Together they show AND/OR do **not** follow
//!   SUM's "ignore numbers in a reference" rule: a `0`-number beside a lone
//!   `TRUE` forces `AND(B1:B2)` (B1=0, B2=TRUE) to `FALSE` (OXP-208 H3), and a
//!   `0` in a 1×1 range anchor gives `OR(A2:A2)` = `FALSE` (OXP-208 H8),
//!   confirming the OXP-184 single-cell-number pin extends to N-cell ranges.
//!   `Text` (including `"TRUE"`/`"FALSE"` and numeric text) and `Blank` cells
//!   stay **non-participating** (ignored, not coerced, not erroring): a
//!   text/blank-only reference yields the empty-logical-set `#VALUE!`
//!   (`OXP-184`; `AND(A4:A5)` over {"abc", blank} → `#VALUE!`, OXP-208 H6).
//!   This range/array Number rule is deliberately DIFFERENT from the scalar
//!   rule below, and the Text-is-ignored rule is deliberately different from
//!   the scalar `"TRUE"`/`"FALSE"`-coerces rule.
//! - Any error, scalar or within a range, propagates immediately in
//!   left-to-right / first-cell-scanned order — the same short-circuit
//!   policy as `SUM`/`AVERAGE`/`MIN`/`MAX` (`OXP-082`, reused here as-is;
//!   AND.md's own "Oracle experiments needed" raises the identical
//!   multi-error-precedence question rather than a new one).
//! - If **zero** contributing values (no `Bool` and no `Number`) are found
//!   anywhere across all arguments (e.g. every range argument is entirely
//!   text/blank), the result is `#VALUE!` (AND.md §3/§Error behavior;
//!   `OXP-184`/`OXP-208` H6).
//!
//! # Scalar `Blank` argument — RESOLVED (`OXP-094`, RUN-2026-07-11-oracle01)
//! AND.md's own "Oracle experiments needed" section asked whether a
//! **scalar** `Blank` argument (a bare reference to an empty cell, or an
//! elided argument slot) *participates* in the reduction as `FALSE`, or is
//! *excluded* entirely — mirroring range-blank's "ignored" treatment. The
//! oracle pinned **excluded**: `=AND(A1,TRUE)` with `A1` empty = `TRUE`
//! (had the blank participated as `FALSE`, the result would be `FALSE`),
//! matching the range form `=AND(A1:A1,TRUE)` = `TRUE`. So a scalar `Blank`
//! is skipped exactly like a range-blank cell — it contributes nothing and
//! does not, on its own, satisfy the "at least one logical value" rule.
//! Every other scalar shape (number, bool, `"TRUE"`/`"FALSE"` text,
//! non-coercible text, error) is fully supported, as is every range/array
//! shape. (Shared with `OR` and `NOT` — see `docs/oracle-experiments.md`.)

use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value, to_bool};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Evaluate an `AND(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut found = false;
    let mut result = true;

    for i in 0..args.count() {
        match args.shape(i) {
            ArgShape::Omitted | ArgShape::Scalar => {
                let v = args.eval_scalar(i);
                // OXP-094 (RUN-2026-07-11-oracle01): a scalar Blank is excluded
                // from the reduction, exactly like a range-blank cell — it
                // contributes nothing and does not set `found`.
                if v.is_blank() {
                    continue;
                }
                match to_bool(&v) {
                    Ok(b) => {
                        found = true;
                        result &= b;
                    }
                    Err(k) => return Value::Error(k),
                }
            }
            ArgShape::Range | ArgShape::Array => {
                let mut err: Option<ErrorKind> = None;
                let found_ref = &mut found;
                let result_ref = &mut result;
                args.for_each_cell(i, &mut |v| match v {
                    Value::Bool(b) => {
                        *found_ref = true;
                        *result_ref &= *b;
                        ControlFlow::Continue(())
                    }
                    // OXP-208 (RUN-2026-07-16-oracle01) + OXP-184: a Number
                    // cell inside a range/array PARTICIPATES via `to_bool`
                    // (0 → FALSE, nonzero → TRUE) — NOT SUM's "ignore numbers
                    // in a reference" rule. `to_bool(&Number)` never errors, so
                    // this inlines its 0-vs-nonzero test.
                    Value::Number(n) => {
                        *found_ref = true;
                        *result_ref &= *n != 0.0;
                        ControlFlow::Continue(())
                    }
                    // Text (incl. "TRUE"/numeric text) and Blank are
                    // NON-participating (ignored): a text/blank-only reference
                    // → empty-logical-set #VALUE! (OXP-184).
                    Value::Text(_) | Value::Blank => ControlFlow::Continue(()),
                    Value::Error(k) => {
                        err = Some(*k);
                        ControlFlow::Break(())
                    }
                    // Not expected from a materialized range cell; treated
                    // the same as `coerce_number_arg`'s RangeAggregate arm
                    // (xl-value/src/coerce.rs) does for these shapes.
                    Value::Array(_) | Value::Ref(_) => {
                        err = Some(ErrorKind::Unsupported);
                        ControlFlow::Break(())
                    }
                    // BC-6 (RFC-0012): a lambda cell is refused, not silently
                    // skipped like a non-boolean number/text. Its own arm.
                    Value::Lambda(_) => {
                        err = Some(ErrorKind::Unsupported);
                        ControlFlow::Break(())
                    }
                });
                if let Some(k) = err {
                    return Value::Error(k);
                }
            }
        }
    }

    if !found {
        return Value::Error(ErrorKind::Value);
    }
    Value::Bool(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg, eval_direct, num, txt};

    #[test]
    fn oxp094_scalar_blank_is_excluded() {
        // RUN-2026-07-11-oracle01: =AND(A1,TRUE) with A1 empty → TRUE (the
        // blank is excluded, not coerced to FALSE), matching the range form.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(Value::Blank),
                    TestArg::Scalar(Value::Bool(true)),
                ]
            ),
            Value::Bool(true)
        );
        // =AND(A1:A1,TRUE) → TRUE (range form, for parity).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Range(vec![Value::Blank]),
                    TestArg::Scalar(Value::Bool(true)),
                ]
            ),
            Value::Bool(true)
        );
    }

    // OXP-208 (RUN-2026-07-16-oracle01) + OXP-184: a Number cell inside a
    // MULTI-cell range PARTICIPATES in AND (0 → FALSE, nonzero → TRUE); AND does
    // NOT ignore numbers-in-a-reference the way the SUM family does. Scaffold
    // A1=1,A2=0,A3=TRUE,A4="abc",A5=blank,B1=0,B2=TRUE.
    #[test]
    fn oxp208_number_in_multicell_range_participates() {
        use xl_value::ErrorKind;
        // H1 =AND(A1:A2) over {1,0} → FALSE (two numbers participate).
        assert_eq!(
            eval_direct(eval, vec![TestArg::Range(vec![num(1.0), num(0.0)])]),
            Value::Bool(false)
        );
        // H3 =AND(B1:B2) over {0-num, TRUE} → FALSE. THE KEY PROBE: the 0-number
        // participates (→ FALSE); had it been ignored, the lone TRUE → TRUE.
        assert_eq!(
            eval_direct(
                eval,
                vec![TestArg::Range(vec![num(0.0), Value::Bool(true)])]
            ),
            Value::Bool(false)
        );
        // H5 =AND(A1:A4) over {1,0,TRUE,"abc"} → FALSE (numbers participate,
        // text ignored).
        assert_eq!(
            eval_direct(
                eval,
                vec![TestArg::Range(vec![
                    num(1.0),
                    num(0.0),
                    Value::Bool(true),
                    txt("abc"),
                ])]
            ),
            Value::Bool(false)
        );
        // H7 =AND(A1:A3) over {1,0,TRUE} → FALSE (no text; numbers participate).
        assert_eq!(
            eval_direct(
                eval,
                vec![TestArg::Range(vec![num(1.0), num(0.0), Value::Bool(true)])]
            ),
            Value::Bool(false)
        );
        // H6 =AND(A4:A5) over {"abc", blank} → #VALUE!: text and blank are
        // non-participating, so the logical set is empty.
        assert_eq!(
            eval_direct(eval, vec![TestArg::Range(vec![txt("abc"), Value::Blank])]),
            Value::Error(ErrorKind::Value)
        );
    }

    // OXP-208 boundary regression: text ("TRUE"/numeric) inside a range is still
    // ignored (only Number/Bool participate), so a text-only range is #VALUE!.
    #[test]
    fn oxp208_text_in_range_still_ignored() {
        use xl_value::ErrorKind;
        assert_eq!(
            eval_direct(eval, vec![TestArg::Range(vec![txt("TRUE"), txt("1")])]),
            Value::Error(ErrorKind::Value)
        );
    }
}

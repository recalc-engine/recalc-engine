//! `OR` — logical disjunction across all arguments.
//!
//! # Provenance
//! Behavior contract: `docs/specs/OR.md`, which explicitly mirrors
//! `docs/specs/AND.md`'s coercion write-up with the logic inverted
//! (Microsoft Learn OR function page). Scalar boolean coercion is entirely
//! `xl-value`'s [`to_bool`] — the same frozen contract `AND`/`IF` use.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Reduces to `TRUE` as soon as one *contributing* logical value is
//!   `TRUE`; `FALSE` only if every contributing value is `FALSE` (OR.md §1,
//!   mirroring AND.md §1).
//! - **Scalar** arguments coerce through [`to_bool`], identical rules to
//!   `AND` (OR.md §Coercion, "identical structure to AND.md's Coercion
//!   section"). An omitted argument slot evaluates to `Blank` and is
//!   handled identically to a scalar `Blank` — see the oracle-deferred note
//!   below.
//! - **Range/array** arguments: a `Bool` cell contributes its value and a
//!   `Number` cell **participates** via [`to_bool`] (`0` → `FALSE`, nonzero →
//!   `TRUE`) — pinned by `OXP-208`/`OXP-174` (RUN-2026-07-16-oracle01) plus
//!   `OXP-184` (RUN 2026-07-12), same rule as `AND`. AND/OR do **not** follow
//!   SUM's "ignore numbers in a reference" rule: `OR({0,1})` = `TRUE`
//!   (OXP-174 — the `1` participates), and a `0` in a 1×1 range anchor gives
//!   `OR(A2:A2)` = `FALSE` (OXP-208 H8). `Text` (including `"TRUE"`/`"FALSE"`
//!   and numeric text) and `Blank` cells stay **non-participating** (ignored):
//!   a text/blank-only reference yields the empty-logical-set `#VALUE!`
//!   (`OXP-184`). This is deliberately DIFFERENT from the scalar rule above.
//! - Any error, scalar or within a range, propagates immediately in
//!   left-to-right / first-cell-scanned order — the same short-circuit
//!   policy as `SUM`/`AVERAGE`/`MIN`/`MAX`/`AND` (`OXP-082`, reused here
//!   as-is; OR.md's own "Oracle experiments needed" raises the identical
//!   multi-error-precedence question, independently, rather than a new
//!   one).
//! - If **zero** contributing values (no `Bool` and no `Number`) are found
//!   anywhere across all arguments, the result is `#VALUE!` (OR.md §Error
//!   behavior; `OXP-184`/`OXP-208`).
//!
//! # Scalar `Blank` argument — RESOLVED (`OXP-094`, RUN-2026-07-11-oracle01)
//! Same resolution as `AND` (see `func_and.rs`'s module docs): a scalar
//! `Blank` argument is **excluded** from the reduction, mirroring a
//! range-blank cell, rather than participating as `FALSE`. The oracle
//! probe `=OR(A1,FALSE)` with `A1` empty = `FALSE`, matching the range
//! form `=OR(A1:A1,FALSE)` = `FALSE`. Every other scalar shape and every
//! range/array shape is fully supported (`docs/oracle-experiments.md`).

use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value, to_bool};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Evaluate an `OR(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut found = false;
    let mut result = false;

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
                        result |= b;
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
                        *result_ref |= *b;
                        ControlFlow::Continue(())
                    }
                    // OXP-208 (RUN-2026-07-16-oracle01) + OXP-174 + OXP-184: a
                    // Number cell inside a range/array PARTICIPATES via
                    // `to_bool` (0 → FALSE, nonzero → TRUE) — NOT SUM's "ignore
                    // numbers in a reference" rule. `to_bool(&Number)` never
                    // errors, so this inlines its 0-vs-nonzero test.
                    Value::Number(n) => {
                        *found_ref = true;
                        *result_ref |= *n != 0.0;
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
        // RUN-2026-07-11-oracle01: =OR(A1,FALSE) with A1 empty → FALSE (blank
        // excluded), matching the range form =OR(A1:A1,FALSE) → FALSE.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Scalar(Value::Blank),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Bool(false)
        );
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    TestArg::Range(vec![Value::Blank]),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Bool(false)
        );
    }

    // OXP-208 (RUN-2026-07-16-oracle01) + OXP-184: a Number cell inside a
    // MULTI-cell range PARTICIPATES in OR (0 → FALSE, nonzero → TRUE). Scaffold
    // A1=1,A2=0,A3=TRUE,A4="abc",A5=blank,B1=0,B2=TRUE.
    #[test]
    fn oxp208_number_in_multicell_range_participates() {
        // H2 =OR(A1:A2) over {1,0} → TRUE (the 1 participates).
        assert_eq!(
            eval_direct(eval, vec![TestArg::Range(vec![num(1.0), num(0.0)])]),
            Value::Bool(true)
        );
        // H4 =OR(B1:B2) over {0-num, TRUE} → TRUE.
        assert_eq!(
            eval_direct(
                eval,
                vec![TestArg::Range(vec![num(0.0), Value::Bool(true)])]
            ),
            Value::Bool(true)
        );
        // H8 =OR(A2:A2) over {0} in a 1×1 range anchor → FALSE. THE KEY PROBE:
        // the lone 0-number participates (→ FALSE); had it been ignored, the
        // empty logical set would be #VALUE!.
        assert_eq!(
            eval_direct(eval, vec![TestArg::Range(vec![num(0.0)])]),
            Value::Bool(false)
        );
    }

    // OXP-174 (RUN-2026-07-16-oracle01): array-literal Number participation —
    // OR({0,1}) → TRUE (the array constant's `1` participates, exactly as a
    // range's number does). The `Array` shape shares OR's range/array arm.
    #[test]
    fn oxp174_array_literal_numbers_participate() {
        assert_eq!(
            eval_direct(eval, vec![TestArg::Array(vec![num(0.0), num(1.0)])]),
            Value::Bool(true)
        );
        // Companion: OR({0,0}) → FALSE (both participate, neither is truthy).
        assert_eq!(
            eval_direct(eval, vec![TestArg::Array(vec![num(0.0), num(0.0)])]),
            Value::Bool(false)
        );
    }

    // OXP-208 boundary regression: text ("TRUE"/numeric) inside a range is still
    // ignored (only Number/Bool participate), so a text-only range is #VALUE!.
    #[test]
    fn oxp208_text_in_range_still_ignored() {
        use xl_value::ErrorKind;
        assert_eq!(
            eval_direct(eval, vec![TestArg::Range(vec![txt("TRUE"), txt("0")])]),
            Value::Error(ErrorKind::Value)
        );
    }
}

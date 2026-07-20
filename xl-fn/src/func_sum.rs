//! `SUM` — add all numbers across the arguments.
//!
//! # Provenance
//! Behavior contract: `docs/specs/SUM.md` (which cites the Microsoft Learn SUM
//! function page). Coercion is deferred entirely to `xl-value`
//! ([`coerce_number_arg`] with the two [`CoercionMode`]s).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Direct **scalar** arguments coerce under [`CoercionMode::Scalar`]: numbers
//!   pass through, `TRUE`/`FALSE` → 1/0, numeric text → its number, blank → 0.
//!   This is the classic asymmetry `SUM(TRUE,"2")` = 3 (SUM.md §Coercion,
//!   hit-list). A scalar text that cannot parse as a number is `#VALUE!`.
//! - **Range / array** arguments aggregate under [`CoercionMode::RangeAggregate`]:
//!   only real numbers count; blank, boolean, and text cells (including
//!   numeric-looking text) are silently skipped (SUM.md §1, §Coercion). Array
//!   constants follow the same per-element rule (SUM.md §Coercion "nested
//!   array/array-constant arguments follow RangeAggregate rules per element").
//! - Omitted arguments contribute 0 and never error (SUM.md §3).
//! - With nothing numeric anywhere the result is 0, never an error (SUM.md §4).
//! - Any argument that evaluates to an error propagates it; the first error in
//!   left-to-right argument order (and, within a range, the first cell scanned)
//!   wins (SUM.md §Error behavior). The multi-error precedence order is now
//!   confirmed (`OXP-082`, RESOLVED `RUN-2026-07-11-oracle01`): observed
//!   `=SUM(1/0,NA())` = `#DIV/0!` and `=SUM(NA(),1/0)` = `#N/A`, i.e. the
//!   **leftmost argument's** error wins — exactly the pre-existing
//!   left-to-right policy, so no behavior change.
//! - Overflow (a non-finite running total) becomes `#NUM!` via
//!   [`Value::number`] (SUM.md §Error behavior / value invariant).
//!
//! # Consumed-array fold (M2 lane 6, OXP-201)
//! When SUM's `ScalarLiteral` argument evaluates to a **materialized multi-cell
//! `Value::Array`** — a consumed range produced by the RFC-0011 array-context
//! gate (`SUM(IF(range,…))`, `SUM(range*2)`) — SUM folds it element-by-element
//! under [`CoercionMode::RangeAggregate`], the identical per-element rule the
//! range (`Aggregate`) arm already applies. This is the OXP-201 residual idiom
//! `SUM(IF(SEQUENCE(5)>2,SEQUENCE(5),0))` = 12 (RUN 2026-07-14). **SUM is the
//! only OXP-201-pinned aggregator**; the other seam aggregators keep
//! `CoercionMode::Scalar` on a `Value::Array` and stay loud (`#UNSUPPORTED!`).
//! Spec: `docs/plans/2026-07-14-consumed-array-eval-spec.md` §2d.
//!
//! **The `""` else-branch reuse (documented extrapolation, spec §R1).** The
//! dominant corpus idiom is `SUM(IF(cond,val,""))` — a **text `""`** false
//! branch — whereas OXP-201 #10 pinned the **numeric `0`** form. The fold reuses
//! `coerce_number_arg(RangeAggregate)`, which **skips** `Text("")` exactly as it
//! skips text in a range — the OXP-006-pinned "SUM skips text/`""` in a range"
//! rule (`SUM({1,TRUE,"2"})=1`). Applying that frozen rule to a *computed* array
//! is an extrapolation from OXP-006 — now CONFIRMED by **OXP-205** (RUN
//! 2026-07-14, Excel 16.0 365 build 16.0.20131; `docs/oracle-experiments.md`):
//! the CSE-array corpus idiom `=SUM(IF(NOT(ISBLANK(range)),vals,""))` = the
//! condition-filtered sum, **byte-equal to the `0`-false-branch form** (`""` ≡
//! `0` under array-SUM), and `SUM(IF({T;F;T},{10;20;30},""))` = 40 confirms
//! array-SUM skips the `""` elements. (Distinct from *scalar* `SUM(IF(FALSE,
//! 5,""))` = `#VALUE!` — only an array *element* `""` is skipped, matching the
//! `RangeAggregate` fold reused here.) The FUSE dry-run gates landing
//! (unsupported→mismatch ≈ 0).

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Evaluate a `SUM(...)` call. See the module docs for the semantics and their
/// spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut acc = 0.0_f64;

    for i in 0..args.count() {
        // RFC 0010: a single-cell REFERENCE aggregates like a range (text/logical
        // ignored); only a scalar LITERAL coerces (`SUM("5")` = 5). `eff_shape`
        // makes that distinction that `shape()` alone cannot.
        match eff_shape(args, i) {
            EffShape::Omitted => {}
            EffShape::ScalarLiteral => {
                let v = args.eval_scalar_array_arg(i);
                match &v {
                    // Consumed-array fold (M2 lane 6, OXP-201 #10; SUM only in
                    // this landing): a materialized multi-cell `Value::Array`
                    // folds element-by-element under the frozen
                    // `RangeAggregate` rule — the SAME per-element rule the
                    // Aggregate arm below applies to a range (Text/""/Bool/Blank
                    // skipped, first error in row-major scan order propagates).
                    // The `as_scalar().is_none()` guard is load-bearing: a 1×1
                    // array must keep the `Scalar` path (which coerces
                    // text/logicals), since `RangeAggregate` *skips* text and
                    // would wrongly fold a 1×1 `Text("5")` to 0.
                    Value::Array(a) if a.as_scalar().is_none() => {
                        for el in a.iter() {
                            match coerce_number_arg(el, CoercionMode::RangeAggregate) {
                                NumericArg::Number(n) => acc += n,
                                NumericArg::Skip => {}
                                NumericArg::Error(k) => return Value::Error(k),
                            }
                        }
                    }
                    _ => match coerce_number_arg(&v, CoercionMode::Scalar) {
                        NumericArg::Number(n) => acc += n,
                        NumericArg::Skip => {}
                        NumericArg::Error(k) => return Value::Error(k),
                    },
                }
            }
            EffShape::Aggregate => {
                let mut err: Option<ErrorKind> = None;
                let acc_ref = &mut acc;
                args.for_each_cell(i, &mut |v| {
                    match coerce_number_arg(v, CoercionMode::RangeAggregate) {
                        NumericArg::Number(n) => {
                            *acc_ref += n;
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

    // Non-finite running total (overflow) → #NUM! per the value invariant.
    Value::number(acc)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // RFC 0010 canonical assertions (SUM is the worked example): a single-cell
    // *reference* is aggregated (text/logical ignored) while a scalar *literal*
    // still coerces. Also validates the `test_support::CellRef` mock against the
    // already-shipped reference implementation.
    #[test]
    fn rfc0010_reference_vs_literal() {
        // SUM(number_ref, text_ref) = the number (text ref ignored), not #VALUE!.
        assert_eq!(
            eval_direct(eval, vec![CellRef(num(5.0)), CellRef(txt("abc"))]),
            num(5.0)
        );
        // A lone text reference contributes nothing → 0.
        assert_eq!(eval_direct(eval, vec![CellRef(txt("abc"))]), num(0.0));
        // A scalar literal still coerces: SUM("5") = 5, SUM(TRUE) = 1.
        assert_eq!(eval_direct(eval, vec![Scalar(txt("5"))]), num(5.0));
        assert_eq!(eval_direct(eval, vec![Scalar(Value::Bool(true))]), num(1.0));
    }

    // OXP-082 (RUN-2026-07-11-oracle01): with several arguments each holding a
    // *different* error, the leftmost argument's error wins (left-to-right).
    // Observed `=SUM(1/0,NA())` = #DIV/0!.
    #[test]
    fn multi_error_leftmost_wins_div0_first() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Div0)),
                    Scalar(Value::Error(ErrorKind::Na)),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    // OXP-082 (RUN-2026-07-11-oracle01): observed `=SUM(NA(),1/0)` = #N/A —
    // the leftmost error still wins when the order is reversed.
    #[test]
    fn multi_error_leftmost_wins_na_first() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Na)),
                    Scalar(Value::Error(ErrorKind::Div0)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }
}

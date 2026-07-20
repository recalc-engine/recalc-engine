//! `ISBLANK` — TRUE iff `value` is `Blank`, and `Blank` only.
//!
//! # Provenance
//! Behavior contract: `docs/specs/ISBLANK.md` (Microsoft Learn combined
//! "IS functions" reference page).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `TRUE` iff `value` evaluates to [`Value::Blank`] — a genuinely empty
//!   cell, never written to (ISBLANK.md §1).
//! - `FALSE` for `Text("")` — a computed or literal empty string is a real
//!   (zero-length) text value, not the absence of a value: the canonical
//!   **`ISBLANK("")=FALSE` vs. `Blank=TRUE`** hit-list distinction
//!   (ISBLANK.md §2, and the explicit hit-list item this function exists
//!   to pin).
//! - `FALSE` for `0`, `FALSE`, or any other non-Blank value — ISBLANK does
//!   not treat "falsy" as blank (ISBLANK.md §3).
//! - `FALSE` for any *genuine Excel* error, **without propagating** the
//!   error (ISBLANK.md §4/§Error behavior; the "IS-functions report, do
//!   not propagate" hit-list rule shared by the whole family) — this
//!   includes a literal error constant passed directly as the argument,
//!   not only an error-valued cell reference.
//!
//! ## Recalc sentinels (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`) are propagated
//! A Recalc sentinel ([`xl_value::ErrorKind::is_recalc_sentinel`]) is not a
//! value ISBLANK can explicitly answer a predicate about — it is Recalc's own
//! declared admission that `value` was never actually computed, so whether
//! real Excel would have produced blank there is unknowable. Reporting
//! `FALSE` (the old behavior) would launder that gap into a confident,
//! possibly-wrong answer. Per Recalc Principle 2 ("never silently
//! wrong"), ISBLANK instead propagates the sentinel unchanged, so a
//! Recalc-caused gap surfaces as `xl-bench`'s explicit `EngineUnsupported`
//! rather than a silent `Mismatch`.
//!
//! # Coercion
//! `value`'s underlying kind is inspected directly (`Value::is_blank`),
//! not any coerced representation — ISBLANK.md's Coercion section is
//! explicit that this is categorically different from the
//! arithmetic/aggregating `Scalar`/`RangeAggregate` coercion modes.

use xl_value::{Array, ErrorKind, Value};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `ISBLANK(...)` call. See the module docs for the semantics
/// and their spec provenance.
///
/// # Consumed-array context (M2 lane 6, OXP-201)
/// Under an array-context aggregator argument ([`CallArgs::array_arg_ctx`]), a
/// multi-cell `Value::Array` argument (a materialized consumed range) is mapped
/// **element-wise**: each element's scalar rule (below, incl. recalc-sentinel
/// propagation) is applied to yield a `Bool`/sentinel array. Outside array
/// context the scalar path is byte-identical (a bare `Value::Array` never
/// reaches here on a supported path). Spec:
/// `docs/plans/2026-07-14-consumed-array-eval-spec.md` §2c.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let v = args.eval_scalar(0);
    if args.array_arg_ctx()
        && let Value::Array(a) = &v
        && a.as_scalar().is_none()
    {
        return map_isblank(a);
    }
    if let Value::Error(kind) = &v
        && kind.is_recalc_sentinel()
    {
        return v;
    }
    Value::Bool(v.is_blank())
}

/// Map ISBLANK's scalar rule over every element of a consumed array, preserving
/// per-element recalc-sentinel propagation.
fn map_isblank(a: &Array) -> Value {
    let data: Vec<Value> = a
        .iter()
        .map(|el| {
            if let Value::Error(kind) = el
                && kind.is_recalc_sentinel()
            {
                el.clone()
            } else {
                Value::Bool(el.is_blank())
            }
        })
        .collect();
    match Array::new(a.rows(), a.cols(), data) {
        Ok(arr) => Value::Array(arr),
        Err(_) => Value::Error(ErrorKind::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // ISBLANK.md §1: TRUE only for a genuinely blank cell.
    #[test]
    fn isblank_true_for_blank() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(Value::Blank)]),
            Value::Bool(true)
        );
    }

    // ISBLANK.md §2/§3/§4: FALSE for "", 0, FALSE, and every genuine Excel
    // error kind — including `#N/A`.
    #[test]
    fn isblank_false_for_non_blank_and_genuine_errors() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(txt(""))]),
            Value::Bool(false)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(num(0.0))]),
            Value::Bool(false)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(Value::Bool(false))]),
            Value::Bool(false)
        );
        for kind in [ErrorKind::Na, ErrorKind::Div0, ErrorKind::Value] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(Value::Error(kind))]),
                Value::Bool(false),
                "{kind:?} should report FALSE, not propagate"
            );
        }
    }

    // The fix: a Recalc sentinel argument is propagated unchanged, never
    // laundered into a guessed FALSE.
    #[test]
    fn isblank_propagates_recalc_sentinels() {
        for kind in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(Value::Error(kind))]),
                Value::Error(kind),
                "{kind:?} should propagate unchanged, not report FALSE"
            );
        }
    }
}

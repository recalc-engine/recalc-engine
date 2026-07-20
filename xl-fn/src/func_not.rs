//! `NOT` — boolean negation of a single argument.
//!
//! # Provenance
//! Behavior contract: `docs/specs/NOT.md` (Microsoft Learn NOT function
//! page). Coercion is entirely `xl-value`'s [`to_bool`], the same frozen
//! contract `AND`/`OR`/`IF` use.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Returns `TRUE` if `logical` is falsy, `FALSE` if truthy — plain
//!   negation (NOT.md §1).
//! - The single argument coerces through [`to_bool`]: numbers (`0`→
//!   `FALSE`, nonzero→`TRUE`) and `"TRUE"`/`"FALSE"` text
//!   (case-insensitive) coerce; other text is `#VALUE!` (NOT.md
//!   §Coercion). An error propagates as NOT's result (NOT.md §Error
//!   behavior).
//! - A multi-cell range/array argument in this scalar-only position
//!   follows `xl-value`'s general scalar rule (1×1 lifts to its element,
//!   larger → `#UNSUPPORTED!`), enforced by `CallArgs::eval_scalar` itself
//!   — NOT.md §2 notes any richer array/spill-context behavior is out of
//!   Tier-0 scope here.
//!
//! # Scalar `Blank` argument — RESOLVED (`OXP-094`, RUN-2026-07-11-oracle01)
//! `NOT` has a single argument, so — unlike `AND`/`OR`'s "exclude from the
//! aggregate" resolution — a `Blank` here has nothing else to reduce
//! against and simply coerces through [`to_bool`] (`Blank` → `FALSE`),
//! giving `=NOT(A1)` = `TRUE` with `A1` empty (and the range form
//! `=NOT(A1:A1)` = `TRUE` likewise). This is the plain `to_bool` reading;
//! no special-casing is needed (`docs/oracle-experiments.md`, `OXP-094`,
//! shared with `AND`/`OR`). Every other scalar shape is fully supported.

use xl_value::{Array, ErrorKind, Value, to_bool};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `NOT(...)` call. See the module docs for the semantics and
/// their spec provenance.
///
/// # Consumed-array context (M2 lane 6, OXP-201)
/// Under an array-context aggregator argument ([`CallArgs::array_arg_ctx`]), a
/// multi-cell `Value::Array` argument (a materialized consumed range) is negated
/// **element-wise** via [`to_bool`]; an element that fails to coerce becomes a
/// `Value::Error` in place (propagated by the downstream reduction, matching
/// leftmost-error semantics only at the reduce step). Outside array context the
/// scalar path is byte-identical. Spec:
/// `docs/plans/2026-07-14-consumed-array-eval-spec.md` §2c.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let v = args.eval_scalar(0);
    if args.array_arg_ctx()
        && let Value::Array(a) = &v
        && a.as_scalar().is_none()
    {
        let data: Vec<Value> = a
            .iter()
            .map(|el| match to_bool(el) {
                Ok(b) => Value::Bool(!b),
                Err(k) => Value::Error(k),
            })
            .collect();
        return match Array::new(a.rows(), a.cols(), data) {
            Ok(arr) => Value::Array(arr),
            Err(_) => Value::Error(ErrorKind::Unsupported),
        };
    }
    // OXP-094 (RUN-2026-07-11-oracle01): a Blank argument coerces through
    // `to_bool` (Blank → FALSE), so NOT(blank) = TRUE — no special-casing.
    match to_bool(&v) {
        Ok(b) => Value::Bool(!b),
        Err(k) => Value::Error(k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg, eval_direct};

    #[test]
    fn oxp094_blank_coerces_to_false() {
        // RUN-2026-07-11-oracle01: =NOT(A1) with A1 empty → TRUE (Blank →
        // FALSE, negated). The observed range form =NOT(A1:A1) is identical:
        // a 1×1 range lifts to its Blank element via `eval_scalar` in the
        // engine, reaching this same path.
        assert_eq!(
            eval_direct(eval, vec![TestArg::Scalar(Value::Blank)]),
            Value::Bool(true)
        );
    }
}

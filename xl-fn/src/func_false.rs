//! `FALSE` — the logical constant `FALSE`, in its **function-call** form.
//!
//! # Provenance
//! Behavior contract: `docs/specs/TRUE.md` (the MS Learn TRUE/FALSE functions
//! are documented together; FALSE is "Returns the logical value FALSE"). The
//! exact mirror of [`crate::func_true`]; shipped alongside it so that `FALSE()`
//! is not left refusing while `TRUE()` works (the two are indistinguishable in
//! risk — both are nullary constants).
//!
//! # Why the registry sees `FALSE` only in its `FALSE()` form
//! As with `TRUE`, a *bare* `FALSE` is folded to an
//! [`xl_ast::ExprKind::Bool`] literal by the parser and never reaches the
//! registry; only the explicit `FALSE()` call form is dispatched here.
//!
//! # Semantics implemented
//! - Takes zero arguments and returns [`Value::bool(false)`](Value::bool).
//!   Arity (`0..=0`) is enforced by the registry. Non-volatile, no coercion,
//!   no error path.

use xl_value::Value;

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `FALSE()` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, _args: &mut dyn CallArgs) -> Value {
    Value::bool(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::eval_direct;

    #[test]
    fn returns_logical_false() {
        assert_eq!(eval_direct(eval, vec![]), Value::bool(false));
    }
}

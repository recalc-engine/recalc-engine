//! `TRUE` — the logical constant `TRUE`, in its **function-call** form.
//!
//! # Provenance
//! Behavior contract: `docs/specs/TRUE.md`, which cites the Microsoft Learn
//! TRUE function page
//! (`https://support.microsoft.com/en-us/office/true-function-7652c6e3-8987-48d0-97cd-ef223246b3fb`).
//! The contract is one unambiguous sentence — "Returns the logical value TRUE"
//! — so this is implemented directly rather than queued as an oracle
//! experiment (the `PI`/`NA` precedent for a nullary constant).
//!
//! # Why the registry sees `TRUE` only in its `TRUE()` form
//! The lexer/parser folds a *bare* `TRUE` (no parentheses) into an
//! [`xl_ast::ExprKind::Bool`] literal directly (`xl-ast/src/parser.rs`), so it
//! never reaches the function registry. Only the explicit call form `TRUE()`
//! is dispatched here — which is exactly the ~20k declined cells attributed to
//! an "unimplemented TRUE function" in `docs/oracle-run-status.md` (they are
//! `TRUE()` calls, not bare literals). Both forms yield the identical value.
//!
//! # Semantics implemented
//! - Takes zero arguments and returns [`Value::bool(true)`](Value::bool)
//!   (TRUE.md §1). Arity (`0..=0`) is enforced by the registry, so `eval` does
//!   not inspect the argument list. Non-volatile, no coercion, no error path.

use xl_value::Value;

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `TRUE()` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, _args: &mut dyn CallArgs) -> Value {
    Value::bool(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::eval_direct;

    #[test]
    fn returns_logical_true() {
        assert_eq!(eval_direct(eval, vec![]), Value::bool(true));
    }
}

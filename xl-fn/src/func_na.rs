//! `NA` — returns the `#N/A` error value.
//!
//! # Provenance
//! Behavior contract: `docs/specs/NA.md`, which cites the Microsoft Learn
//! NA function page
//! (`https://support.microsoft.com/en-us/office/na-function-5469c2d1-a90c-4fb5-9bbc-64bd9bb6b47c`).
//! The source text is a single unambiguous sentence — "Returns the error
//! value #N/A" — so this is implemented directly rather than queued as an
//! oracle experiment.
//!
//! # Semantics implemented (spec bullet in parentheses)
//! - Takes zero arguments and always returns `Value::Error(ErrorKind::Na)`
//!   (the `#N/A` error value) (NA.md §1). Arity (`0..=0`) is enforced by the
//!   registry, so `eval` does not need to check argument count itself.

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `NA()` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, _args: &mut dyn CallArgs) -> Value {
    Value::Error(ErrorKind::Na)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::eval_direct;

    #[test]
    fn returns_na() {
        assert_eq!(eval_direct(eval, vec![]), Value::Error(ErrorKind::Na));
    }
}

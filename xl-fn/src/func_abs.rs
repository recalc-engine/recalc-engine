//! `ABS` — absolute value of a number.
//!
//! # Provenance
//! Behavior contract: `docs/specs/ABS.md` (which cites the Microsoft Learn ABS
//! function page). Coercion is deferred entirely to `xl-value`'s
//! [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Coerce the one argument via scalar numeric coercion (bool -> 1/0,
//!   numeric text -> number, blank -> 0) and return its magnitude (ABS.md
//!   §Coercion, §1).
//! - Non-numeric, non-coercible text -> `#VALUE!`; an error-valued argument
//!   propagates as-is, no special containment (ABS.md §Error behavior).

use xl_value::{Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `ABS(number)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    match to_number(&args.eval_scalar(0)) {
        Ok(n) => Value::number(n.abs()),
        Err(k) => Value::Error(k),
    }
}

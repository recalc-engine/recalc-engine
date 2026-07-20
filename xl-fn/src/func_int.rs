//! `INT` — rounds a number down to the nearest integer, toward negative
//! infinity (floor), not toward zero.
//!
//! # Provenance
//! Behavior contract: `docs/specs/INT.md` (Microsoft Learn INT function
//! page). Coercion via `xl-value`'s [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Coerce the one argument via scalar numeric coercion, then floor toward
//!   negative infinity: `INT(-8.9)` = `-9`, **not** truncation toward zero
//!   (`TRUNC(-8.9)` = `-8`) — the headline directional distinction (INT.md
//!   §1). For non-negative inputs floor and truncation coincide, so the
//!   distinction is only externally visible for negative inputs (INT.md
//!   §2).
//! - Non-numeric, non-coercible text -> `#VALUE!`; an error-valued argument
//!   propagates normally (INT.md §Error behavior).

use xl_value::{Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `INT(number)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    match to_number(&args.eval_scalar(0)) {
        Ok(n) => Value::number(n.floor()),
        Err(k) => Value::Error(k),
    }
}

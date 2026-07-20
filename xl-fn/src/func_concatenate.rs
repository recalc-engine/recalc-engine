//! `CONCATENATE` — joins the text representation of each argument, in
//! order, with no separator.
//!
//! # Provenance
//! Behavior contract: `docs/specs/CONCATENATE.md` (Microsoft Learn
//! CONCATENATE function page). Coercion deferred entirely to `xl-value`'s
//! [`to_text`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Each of 1..=255 arguments is coerced to text and appended in order,
//!   with no separator (CONCATENATE.md §1). Numbers use "General"
//!   number-to-text formatting via `to_text` (not a cell's display format);
//!   booleans -> `"TRUE"`/`"FALSE"`; blank -> `""`; text passes through
//!   unchanged (CONCATENATE.md §Coercion).
//! - Any argument evaluating to an error propagates as CONCATENATE's result
//!   (CONCATENATE.md §Error behavior).
//! - Each argument is evaluated in **scalar** context
//!   ([`CallArgs::eval_scalar`]), matching the documented legacy CONCATENATE
//!   behavior of taking a single value from a multi-cell range argument
//!   rather than flattening every cell into the joined output (CONCATENATE.md
//!   §2, the key difference from `CONCAT`). The exact cell such a range
//!   collapses to (top-left vs. true implicit-intersection row/column) is
//!   CONCATENATE.md's own flagged "Oracle experiments needed" item; per
//!   `CallArgs::eval_scalar`'s documented contract a multi-cell range/array in
//!   scalar context is `#UNSUPPORTED!` unless it is 1×1, so a genuine
//!   multi-cell range argument stays unguessed here rather than this module
//!   picking a cell itself.

use xl_value::{Value, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `CONCATENATE(text1, [text2], ...)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut out = String::new();
    for i in 0..args.count() {
        match to_text(&args.eval_scalar(i)) {
            Ok(t) => out.push_str(t.as_str()),
            Err(k) => return Value::Error(k),
        }
    }
    Value::text(&out)
}

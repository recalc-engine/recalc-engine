//! `MOD` — remainder after `number` is divided by `divisor`, taking the sign
//! of `divisor` (not `number`).
//!
//! # Provenance
//! Behavior contract: `docs/specs/MOD.md` (Microsoft Learn MOD function page,
//! verified 2026-07-05). Coercion via `xl-value`'s [`to_number`]. The `INT`
//! building block in MOD's documented formula is implemented directly here
//! via `f64::floor` (floor toward `-inf`) rather than calling the `INT`
//! function, matching MOD's own documented formula.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Documented formula `MOD(n, d) = n - d * INT(n/d)`, `INT` flooring
//!   toward `-inf` (MOD.md §2) — this is what makes the result **take the
//!   sign of `divisor`**, not the sign of `number`: `MOD(-3,2)` = `1` (not
//!   `-1`), `MOD(3,-2)` = `-1` (not `1`) (MOD.md §Hit-list).
//! - `divisor = 0` -> `#DIV/0!`, an explicit documented case, not merely
//!   fallout of a general division-by-zero rule (MOD.md §3).
//! - Non-numeric, non-coercible argument -> `#VALUE!`. `number` is coerced
//!   (and its error surfaced) before `divisor`, the same left-to-right
//!   argument-evaluation-order precedent `SUM` established (`OXP-082`,
//!   whose short-circuit policy `AVERAGE`/`MIN`/`MAX` already reuse rather
//!   than register a fresh id — MOD follows the same convention; MOD.md
//!   §"Oracle experiments needed" raises the identical open question about
//!   which argument's error wins).
//! - A non-finite computed result -> `#NUM!` via the [`Value::number`]
//!   invariant.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `MOD(number, divisor)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let number = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let divisor = match to_number(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    if divisor == 0.0 {
        return Value::Error(ErrorKind::Div0);
    }
    // MOD.md §2's documented formula: n - d * INT(n/d), INT flooring toward
    // -inf (not truncating toward zero) — the source of the
    // sign-follows-divisor behavior.
    let quotient = (number / divisor).floor();
    Value::number(number - divisor * quotient)
}

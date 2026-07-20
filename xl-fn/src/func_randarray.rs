//! `RANDARRAY` — refuses loudly in v0 (no seeded RNG wired into the engine).
//!
//! # Provenance
//! Behavior contract: Microsoft support "RANDARRAY function"
//! (<https://support.microsoft.com/en-us/office/randarray-function-21261e55-3bec-4885-86a6-8b0a47fd4d33>,
//! verified by WebFetch 2026-07-15). No `docs/specs/RANDARRAY.md` exists. The
//! `_xlfn.` authoring prefix is stripped by the parser
//! (`xl-ast::parser::parse_func_name`), so the registry name is bare
//! `RANDARRAY`.
//!
//! # Disposition — REFUSED (`#UNSUPPORTED!`), consistent with `RAND`
//! `RANDARRAY` fills every cell with a fresh random draw, i.e. it needs the same
//! entropy source as `RAND`. In v0 that source is deliberately absent:
//! [`EvalContext::rand`] returns `#UNSUPPORTED!` because determinism
//! (`implementation-plan.md` §5) requires the draw sequence be a function of a
//! workbook seed and the recalc order — a seeded RNG that has not yet been wired
//! into `xl-fn`. Inventing a non-deterministic RNG here is forbidden (Principle 2
//! and the sandbox's seedable-`RAND` requirement), so `RANDARRAY` refuses loudly
//! and consistently with `RAND`, rather than draw non-deterministically or
//! fabricate values.
//!
//! This module consumes the [`EvalContext::rand`] capability seam so the refusal
//! *tracks* `RAND` exactly: the moment a seeded RNG is injected (and `rand()`
//! stops returning `#UNSUPPORTED!`), this call site is where `RANDARRAY`'s
//! generation — argument validation (`rows`/`columns` counts, `min < max` →
//! `#VALUE!`, `whole_number`) and per-element draws — gets wired in. See
//! `docs/plans/2026-07-15-lane3a-probe-needed.md` (L3A-RAND).
//!
//! # Volatility
//! `RANDARRAY` is registered **non-volatile** in its [`crate::registry::FnSpec`]
//! because it does not (yet) recompute anything — it refuses deterministically.
//! Its graph-level volatility is already covered by `registry::VOLATILE_NAMES`
//! (which lists `RANDARRAY`), so `registry::is_volatile("RANDARRAY")` stays
//! `true`. When a real seeded RNG lands and this function actually draws, its
//! `FnSpec` flips to [`crate::registry::Volatility::Volatile`].

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `RANDARRAY(...)` call. Refuses with `#UNSUPPORTED!` in v0 — see the
/// module docs for why (no seeded RNG; guessing is forbidden).
pub(crate) fn eval(ctx: &EvalContext, _args: &mut dyn CallArgs) -> Value {
    // Consume the RNG seam so this refusal is identical to `RAND`'s and this is
    // the single site a future seeded RNG re-wires. In v0 `rand()` always errs.
    match ctx.rand() {
        Err(k) => Value::Error(k),
        // Unreachable in v0. If a seeded RNG is later injected without wiring the
        // full RANDARRAY generation here, still refuse loudly rather than emit a
        // single unshaped draw (never silently wrong).
        Ok(_) => Value::Error(ErrorKind::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::{ErrorKind, Value};

    // With no arguments, RANDARRAY refuses (#UNSUPPORTED!) — no RNG in v0.
    #[test]
    fn no_args_refused() {
        assert_eq!(
            eval_direct(eval, vec![]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // With the documented rows/columns/min/max/whole_number arguments it still
    // refuses (consistent with RAND) rather than draw or validate-then-guess.
    #[test]
    fn full_args_refused() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(2.0)),
                    Scalar(num(3.0)),
                    Scalar(num(1.0)),
                    Scalar(num(10.0)),
                    Scalar(Value::Bool(true)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

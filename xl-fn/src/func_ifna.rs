//! `IFNA` — the narrow error suppressor: return `value_if_na` iff `value` is
//! specifically `#N/A`, otherwise return `value` unchanged (every other error
//! kind passes straight through, unlike `IFERROR`).
//!
//! # Provenance
//! No `docs/specs/IFNA.md` exists in this pass. Per the task specification
//! this module is built by **mirroring `IFERROR`'s structure exactly**
//! ([`crate::func_iferror`]) — same laziness, same array/spill deferral
//! shape — narrowing only the error-kind test from "any `Value::Error`" to
//! "specifically `Value::Error(ErrorKind::Na)`". `IFERROR`'s own provenance
//! (`docs/specs/IFERROR.md`, citing the Microsoft support IFERROR page) is the
//! nearest pinned sibling; Microsoft's public IFNA reference documents the
//! *exact same* function shape with only the caught-error-kind narrowed to
//! `#N/A` (the family relationship `func_isna.rs` already relies on for its
//! own no-spec clean-room justification, mirrored here for the same reason).
//!
//! # Semantics implemented (mirrors IFERROR.md, spec bullets renamed)
//! - Evaluate `value` (argument 0) first — it is *both* the error test and,
//!   on success, the returned value. If it is **specifically**
//!   `Value::Error(ErrorKind::Na)` (`#N/A`), return `value_if_na` (argument 1)
//!   instead; otherwise return `value` unchanged, passing its value/type
//!   through with no coercion.
//! - **Every other error kind passes straight through unchanged** — this is
//!   IFNA's whole reason to exist over IFERROR: `#DIV/0!`, `#VALUE!`, `#REF!`,
//!   `#NAME?`, `#NUM!`, `#NULL!`, `#GETTING_DATA`, `#SPILL!`, `#CALC!`, and
//!   this crate's own sentinels `#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!` are
//!   **not** `#N/A`, so `IFNA` returns each of them as `value` itself — never
//!   suppressed, never replaced by `value_if_na`. This is the direct
//!   consequence of testing the specific `ErrorKind::Na` variant rather than
//!   `IFERROR`'s kind-agnostic `matches!(v, Value::Error(_))`.
//! - **Lazy w.r.t. `value_if_na`**, exactly like `IFERROR`'s laziness w.r.t.
//!   `value_if_error`: `value_if_na` is forced *only* when `value` is `#N/A`.
//!   A non-`#N/A` `value` (including one that is some *other* error) never
//!   triggers evaluation of the fallback branch.
//! - If `value_if_na` *itself* evaluates to an error, that error propagates as
//!   the final result — `IFNA` does not recursively suppress its own fallback
//!   branch (mirrors `IFERROR.md` §Error behavior).
//!
//! ## Array/spill operands are oracle-deferred (mirrors IFERROR exactly)
//! Identical reasoning and identical guard to `IFERROR`: neither this task's
//! specification nor any pinned spec documents element-wise `#N/A`-catching
//! over a genuine multi-cell `value` (Excel spills a per-element result
//! there). A 1×1 range/array lifts to its element and is handled as a scalar
//! (the engine classifies it [`ArgShape::Scalar`]); a wider
//! [`ArgShape::Range`]/[`ArgShape::Array`] operand — in *either* argument
//! position — is refused with [`ErrorKind::Unsupported`] rather than guessing
//! the spilled shape (Recalc Principle 2). See the `OXP (unassigned)`
//! notes at each guard, mirroring `func_iferror.rs`'s.

use xl_value::{ErrorKind, Value};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Is argument `index` a genuine multi-cell operand (a range or array literal
/// wider than 1×1)? Identical to `func_iferror::is_array_operand`.
fn is_array_operand(args: &mut dyn CallArgs, index: usize) -> bool {
    matches!(args.shape(index), ArgShape::Range | ArgShape::Array)
}

/// Evaluate an `IFNA(value, value_if_na)` call. See the module docs for the
/// semantics and their provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Both arguments are guaranteed present by the registry arity check
    // (min_args = max_args = 2).

    // Element-wise IFNA over a genuine multi-cell `value` is exactly as
    // undocumented as IFERROR's own array/spill case — defer loudly (mirrors
    // func_iferror::eval).
    //
    // OXP (unassigned): grid `IFNA(A1:A3/B1:B3, 0)` and `IFNA({1,#N/A,3}, 0)`
    // — confirm the spilled rectangle shape and per-element catching before
    // implementing (identical open question to IFERROR's own).
    if is_array_operand(args, 0) {
        return Value::Error(ErrorKind::Unsupported);
    }

    let value = args.eval_scalar(0);
    // The narrow test: ONLY #N/A is caught. Every other error kind (classic
    // Excel errors, dynamic-array-era errors, and this crate's own
    // #UNSUPPORTED!/#BLOCKED!/#RESOURCE! sentinels) passes through unchanged,
    // exactly like any non-error value.
    if !matches!(value, Value::Error(ErrorKind::Na)) {
        // `value_if_na` is never forced (laziness).
        return value;
    }

    // `value` is #N/A → the result is `value_if_na`. A multi-cell fallback is
    // the same undocumented spill shape as above (mirrors func_iferror::eval).
    //
    // OXP (unassigned): grid `IFNA(NA(), A1:A3)` — does a multi-cell
    // `value_if_na` spill, or lift/error? The documented case is a scalar
    // fallback.
    if is_array_operand(args, 1) {
        return Value::Error(ErrorKind::Unsupported);
    }
    args.eval_scalar(1)
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    fn err(kind: ErrorKind) -> Value {
        Value::Error(kind)
    }

    // The whole point of IFNA over IFERROR: ONLY #N/A is caught.
    #[test]
    fn ifna_catches_only_na() {
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Scalar(err(ErrorKind::Na)), Scalar(num(0.0))]
            ),
            num(0.0)
        );
    }

    // Every other classic Excel error kind passes straight through
    // UNCHANGED — not replaced by value_if_na.
    #[test]
    fn ifna_passes_other_classic_errors_through() {
        for kind in [
            ErrorKind::Null,
            ErrorKind::Div0,
            ErrorKind::Value,
            ErrorKind::Ref,
            ErrorKind::Name,
            ErrorKind::Num,
        ] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(err(kind)), Scalar(num(0.0))]),
                err(kind),
                "{kind:?} should pass through IFNA unchanged"
            );
        }
    }

    // Dynamic-array-era errors also pass through unchanged (not #N/A).
    #[test]
    fn ifna_passes_modern_errors_through() {
        for kind in [ErrorKind::GettingData, ErrorKind::Spill, ErrorKind::Calc] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(err(kind)), Scalar(txt("x"))]),
                err(kind),
                "{kind:?} should pass through IFNA unchanged"
            );
        }
    }

    // This crate's own sentinel errors are NOT #N/A, so they pass through
    // too — the direct consequence of testing the specific ErrorKind::Na
    // variant (contrast func_iferror, which catches these).
    #[test]
    fn ifna_passes_recalc_sentinels_through() {
        for kind in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(err(kind)), Scalar(num(1.0))]),
                err(kind),
                "{kind:?} should pass through IFNA unchanged, not be caught"
            );
        }
    }

    // A non-erroring `value` passes through unchanged, for every value kind.
    #[test]
    fn ifna_passes_non_error_value_through() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(num(42.0)), Scalar(num(0.0))]),
            num(42.0)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(txt("ok")), Scalar(num(0.0))]),
            txt("ok")
        );
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Scalar(Value::Bool(true)), Scalar(num(0.0))]
            ),
            Value::Bool(true)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(Value::Blank), Scalar(num(0.0))]),
            Value::Blank
        );
    }

    // Laziness: when `value` is not #N/A (including when it is some *other*
    // error), `value_if_na` is never evaluated. A `Poison` fallback panics if
    // forced.
    #[test]
    fn ifna_does_not_evaluate_fallback_when_value_not_na() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(num(7.0)), Poison]),
            num(7.0)
        );
        // Some *other* error still does not force the fallback.
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(err(ErrorKind::Div0)), Poison]),
            err(ErrorKind::Div0)
        );
    }

    // The fallback is returned on #N/A — and returned as-is, whatever its
    // (non-error) value/type.
    #[test]
    fn ifna_returns_fallback_on_na() {
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Scalar(err(ErrorKind::Na)), Scalar(txt("n/a"))]
            ),
            txt("n/a")
        );
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Scalar(err(ErrorKind::Na)), Scalar(Value::Blank)]
            ),
            Value::Blank
        );
    }

    // IFNA does not suppress *its own* fallback branch — an errored
    // value_if_na propagates as the final result.
    #[test]
    fn ifna_propagates_fallback_error() {
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Scalar(err(ErrorKind::Na)), Scalar(err(ErrorKind::Div0))]
            ),
            err(ErrorKind::Div0)
        );
    }

    // Array/spill deferral: a genuine multi-cell `value` is refused with
    // #UNSUPPORTED!, never silently coerced-then-"caught" (mirrors IFERROR).
    #[test]
    fn ifna_defers_multicell_value() {
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Range(vec![num(1.0), err(ErrorKind::Na)]), Scalar(num(0.0))]
            ),
            err(ErrorKind::Unsupported)
        );
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Array(vec![num(1.0), num(2.0)]), Scalar(num(0.0))]
            ),
            err(ErrorKind::Unsupported)
        );
    }

    // Array/spill deferral, fallback position: reached only because `value`
    // is #N/A, so this also confirms the multi-cell fallback guard fires
    // *after* the error test (laziness intact) rather than spilling a guess.
    #[test]
    fn ifna_defers_multicell_fallback_on_na() {
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Scalar(err(ErrorKind::Na)), Range(vec![num(1.0), num(2.0)])]
            ),
            err(ErrorKind::Unsupported)
        );
    }

    // A multi-cell fallback is not consulted when `value` is not #N/A —
    // laziness means the fallback guard never runs.
    #[test]
    fn ifna_ignores_multicell_fallback_when_value_ok() {
        assert_eq!(
            eval_direct(
                super::eval,
                vec![Scalar(num(5.0)), Range(vec![num(1.0), num(2.0)])]
            ),
            num(5.0)
        );
    }
}

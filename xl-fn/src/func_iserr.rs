//! `ISERR` — TRUE iff `value` is an error value of any kind **except**
//! `#N/A`, containing the error rather than letting it propagate.
//!
//! # Provenance
//! Behavior contract: `docs/specs/ISERR.md` (Microsoft Learn combined
//! "IS functions" reference page).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `TRUE` iff `value` evaluates to [`Value::Error`] with any
//!   [`ErrorKind`] **other than** [`ErrorKind::Na`] (ISERR.md §1/§2) — the
//!   middle predicate in the ISERROR/ISERR/ISNA family: broader than
//!   `ISNA` (`func_isna.rs`, `#N/A` only) but narrower than `ISERROR`
//!   (`func_iserror.rs`, every error kind including `#N/A`).
//! - `FALSE` for `#N/A` specifically (ISERR.md §2) — the one point of
//!   contrast with `ISERROR`.
//! - `FALSE` for every non-error value/kind: number, text, boolean, blank
//!   (ISERR.md §3).
//!
//! ## Recalc sentinels (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`) are propagated, not answered
//! Matching `func_iserror.rs`'s corrected decision (kept consistent within
//! the IS-family rather than re-litigated per function): these three
//! `ErrorKind` variants ([`xl_value::ErrorKind::is_recalc_sentinel`]) have
//! no Excel counterpart, but they are not "any error value except #N/A"
//! either — a Recalc sentinel is not a value Excel ever sees, it is
//! Recalc's own declared admission that it failed to compute `value` at
//! all. Whether real Excel would have produced some other error (`TRUE`)
//! or a perfectly fine value (`FALSE`) there is unknowable, so per
//! Recalc Principle 2 ("never silently wrong") ISERR does not guess
//! either boolean. It instead **propagates the sentinel unchanged**,
//! surfacing the gap as `xl-bench`'s explicit `EngineUnsupported` rather
//! than a silent `Mismatch`. Genuine Excel error kinds other than `#N/A`
//! are unaffected and still answer `TRUE`, per ISERR.md §1/§2.
//!
//! ## Error behavior — the containment guarantee
//! Like every IS-function, `args.eval_scalar(0)` fully evaluates the
//! argument (however deep the error originates within it), yielding a
//! [`Value::Error`] rather than a Rust `Err` — evaluation never "throws" in
//! this codebase, it returns an error *value*. ISERR simply inspects that
//! returned value's kind and converts it to a boolean, so the error is
//! contained here rather than bubbling further up the formula tree
//! (ISERR.md §Error behavior; see `func_iserror.rs`'s containment note).

use xl_value::{ErrorKind, Value};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `ISERR(...)` call. See the module docs for the semantics
/// and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let v = args.eval_scalar(0);
    if let Value::Error(kind) = &v
        && kind.is_recalc_sentinel()
    {
        return v;
    }
    Value::Bool(matches!(v, Value::Error(kind) if kind != ErrorKind::Na))
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // ISERR.md §1: TRUE for every documented Excel error kind except #N/A.
    #[test]
    fn iserr_true_for_error_kinds_other_than_na() {
        for kind in [
            ErrorKind::Null,
            ErrorKind::Div0,
            ErrorKind::Value,
            ErrorKind::Ref,
            ErrorKind::Name,
            ErrorKind::Num,
            ErrorKind::GettingData,
            ErrorKind::Spill,
            ErrorKind::Calc,
        ] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(Value::Error(kind))]),
                Value::Bool(true),
                "{kind:?} should be caught by ISERR"
            );
        }
    }

    // The fix: Recalc sentinels are propagated unchanged, never laundered
    // into a guessed TRUE (matching func_iserror.rs's corrected decision).
    #[test]
    fn iserr_propagates_recalc_sentinels() {
        for kind in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                eval_direct(super::eval, vec![Scalar(Value::Error(kind))]),
                Value::Error(kind),
                "{kind:?} should propagate unchanged, not report TRUE"
            );
        }
    }

    // ISERR.md §2: the one point of contrast with ISERROR — #N/A is FALSE.
    #[test]
    fn iserr_false_for_na() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(Value::Error(ErrorKind::Na))]),
            Value::Bool(false)
        );
    }

    // ISERR.md §3: FALSE for every non-error kind. This also proves
    // containment: ISERR never re-throws.
    #[test]
    fn iserr_false_for_non_error_values() {
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(num(42.0))]),
            Value::Bool(false)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(txt("x"))]),
            Value::Bool(false)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(Value::Bool(false))]),
            Value::Bool(false)
        );
        assert_eq!(
            eval_direct(super::eval, vec![Scalar(Value::Blank)]),
            Value::Bool(false)
        );
    }
}

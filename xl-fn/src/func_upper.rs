//! `UPPER` — converts the lowercase letters in `text` to uppercase.
//!
//! # Provenance
//! Behavior contract: `docs/specs/UPPER.md` (Microsoft "UPPER function" page,
//! verified 2026-07-05). Text coercion via `xl-value`'s [`to_text`] — the same
//! "General" numeric formatting used by `LEFT`/`CONCATENATE`.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `text` (arg 0) is coerced to text via [`to_text`]: numbers use "General"
//!   formatting, `TRUE`/`FALSE` for booleans (already uppercase — a no-op),
//!   `""` for blank, and text passes through (UPPER.md §Coercion). An
//!   error-valued `text` propagates (UPPER.md §Error behavior).
//! - Every ASCII lowercase letter `a`–`z` is mapped to `A`–`Z`; ASCII digits,
//!   punctuation, and symbols are left unchanged (UPPER.md §Semantics 1). This
//!   ASCII-only path is the sole **claimed-fidelity** behavior — it is
//!   unambiguous and locale-independent.
//!
//! # Non-ASCII letters — deferred (`OXP` unassigned)
//! Excel's casing for non-ASCII letters follows Windows locale rules, which
//! the Recalc design rules/`implementation-plan.md` §1 place **out of v1 scope**
//! ("no non-en-US locale semantics"). Rust's Unicode `char::to_uppercase` is
//! documented to diverge from that behavior in cases Excel does not necessarily
//! reproduce — e.g. German `ß` -> `SS` (a length-changing expansion), the
//! Turkish dotless-`ı`/dotted-`İ` pair, and ligature expansions — and Excel's
//! own output for these is not pinned by any oracle experiment. Per Principle 2
//! (never silently wrong: defer rather than guess), any `text` containing a
//! non-ASCII **alphabetic** character returns `#UNSUPPORTED!` instead of a
//! possibly-wrong casing. Non-ASCII **non-letters** (symbols, punctuation,
//! caseless scripts' marks) have no case mapping and pass through unchanged, so
//! they do not trigger the deferral.
//!
//! OXP (unassigned): probe UPPER over the Latin-1-supplement letter range
//! (e.g. `é`->`É`, `ñ`->`Ñ`), the `ß`/Turkish-`ı`/ligature edge cases, and a
//! caseless-script sample (CJK), on the pinned Excel build; promote confirmed
//! mappings out of the deferral. See UPPER.md §"Oracle experiments needed".

use xl_value::{ErrorKind, Value, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `UPPER(text)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let text = match to_text(&args.eval_scalar(0)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };

    // OXP (unassigned): non-ASCII letter casing is locale-sensitive in Excel
    // and out of v1 scope — defer rather than emit a possibly-wrong casing.
    // Non-ASCII non-letters have no case mapping, so they do not trigger this.
    if text
        .as_str()
        .chars()
        .any(|c| !c.is_ascii() && c.is_alphabetic())
    {
        return Value::Error(ErrorKind::Unsupported);
    }

    // ASCII-only fidelity path: `char::to_ascii_uppercase` maps a–z -> A–Z and
    // leaves every other character (digits, punctuation, and the non-ASCII
    // non-letters permitted above) untouched.
    let result: String = text
        .as_str()
        .chars()
        .map(|c| c.to_ascii_uppercase())
        .collect();
    Value::text(&result)
}

#[cfg(test)]
mod tests {
    use xl_value::{ErrorKind, Value};

    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    fn upper(args: Vec<crate::test_support::TestArg>) -> Value {
        eval_direct(super::eval, args)
    }

    #[test]
    fn ascii_lowercase_becomes_uppercase() {
        assert_eq!(upper(vec![Scalar(txt("hello"))]), txt("HELLO"));
    }

    #[test]
    fn mixed_case_becomes_uppercase() {
        assert_eq!(upper(vec![Scalar(txt("Hello World"))]), txt("HELLO WORLD"));
    }

    #[test]
    fn already_uppercase_is_unchanged() {
        assert_eq!(upper(vec![Scalar(txt("HELLO"))]), txt("HELLO"));
    }

    #[test]
    fn digits_and_punctuation_unchanged() {
        assert_eq!(
            upper(vec![Scalar(txt("a1-b2_c3! (x)"))]),
            txt("A1-B2_C3! (X)")
        );
    }

    #[test]
    fn number_argument_uses_general_text_coercion() {
        // to_text renders the number with "General" formatting; there are no
        // letters to uppercase, so the digits pass through unchanged.
        assert_eq!(upper(vec![Scalar(num(12345.0))]), txt("12345"));
    }

    #[test]
    fn boolean_true_coerces_to_already_uppercase_text() {
        assert_eq!(upper(vec![Scalar(Value::Bool(true))]), txt("TRUE"));
    }

    #[test]
    fn boolean_false_coerces_to_already_uppercase_text() {
        assert_eq!(upper(vec![Scalar(Value::Bool(false))]), txt("FALSE"));
    }

    #[test]
    fn blank_coerces_to_empty_string() {
        assert_eq!(upper(vec![Scalar(Value::Blank)]), txt(""));
    }

    #[test]
    fn empty_string_is_empty_string() {
        assert_eq!(upper(vec![Scalar(txt(""))]), txt(""));
    }

    #[test]
    fn error_in_text_argument_propagates() {
        assert_eq!(
            upper(vec![Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn non_ascii_letter_is_deferred() {
        // OXP (unassigned): 'é' is a non-ASCII letter whose Excel uppercasing
        // is locale-sensitive and unpinned — defer rather than guess 'É'.
        assert_eq!(
            upper(vec![Scalar(txt("café"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn sharp_s_expansion_case_is_deferred() {
        // 'ß' is the canonical divergence: Rust maps it to "SS", but Excel's
        // behavior is unpinned — deferral avoids a possibly-wrong length change.
        assert_eq!(
            upper(vec![Scalar(txt("straße"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn non_ascii_non_letter_passes_through() {
        // '€' and '—' (em dash) are non-ASCII but caseless: no case mapping,
        // so the ASCII letters are still uppercased and the symbols survive.
        assert_eq!(upper(vec![Scalar(txt("a€b—c"))]), txt("A€B—C"));
    }
}

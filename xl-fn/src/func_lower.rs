//! `LOWER` — converts all uppercase letters in `text` to lowercase;
//! non-alphabetic characters are left unchanged.
//!
//! # Provenance
//! Behavior contract: `docs/specs/LOWER.md` (Microsoft "LOWER function"
//! support page, verified 2026-07-05). Direct mirror of `UPPER`
//! (`docs/specs/UPPER.md`) — same coercion structure, same en-US-only casing
//! scope. Text coercion via `xl-value`'s [`to_text`] (the same "General"
//! numeric formatting `CONCATENATE`/`LEFT` use).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `text` (arg 0) is coerced to text via [`to_text`]: numbers use "General"
//!   formatting, `"TRUE"`/`"FALSE"` for booleans, `""` for blank, text passes
//!   through unchanged (LOWER.md §Coercion). An error-valued `text` propagates
//!   (LOWER.md §Error behavior).
//! - Uppercase ASCII `A`–`Z` map to `a`–`z`; every other character in the
//!   fidelity path (ASCII digits, punctuation, symbols, whitespace) is left
//!   unchanged (LOWER.md §Semantics).
//!
//! # Casing basis — ASCII-only fidelity claim (non-ASCII deferred)
//! Per v1 scope (the project's scope rules §1: "no non-en-US
//! locale semantics") Recalc targets en-US casing only. ASCII `A`–`Z` → `a`–`z`
//! is unambiguous and locale-independent, so it is the claimed-fidelity path.
//!
//! For **non-ASCII letters** Excel's lowercasing is locale-sensitive and not
//! pinned by the spec (LOWER.md §"Oracle experiments needed"): the Greek
//! final-sigma rule, the Turkish dotless-i (`İ`→`i`), and even the plain
//! Latin-1-supplement mapping (`É`→`é`) are all observed via the oracle, never
//! assumed. Rust's `char::to_lowercase` implements Unicode default casing,
//! which may diverge from Excel's Windows en-US behavior for these. Per the
//! never-guess rule, any `text` containing a non-ASCII character whose case
//! could matter returns `#UNSUPPORTED!` rather than a possibly-wrong result.
//! Non-ASCII characters that carry no case (emoji, CJK, punctuation, symbols)
//! are unaffected either way and pass through unchanged. This mirrors the
//! sibling `UPPER` implementation exactly.

use xl_value::{ErrorKind, Value, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// A character whose lowercasing Excel has *not* pinned for en-US: any
/// non-ASCII character that is a letter, or that otherwise changes under
/// Unicode simple lowercasing (catching cased non-letter symbols too). ASCII is
/// always the pinned-fidelity path, so it is never ambiguous.
fn is_unpinned_case(c: char) -> bool {
    !c.is_ascii() && (c.is_alphabetic() || c.to_lowercase().next() != Some(c))
}

/// Evaluate a `LOWER(text)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let text = match to_text(&args.eval_scalar(0)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };

    // OXP (unassigned): probe LOWER over the Latin-1 supplement (É→é, Ñ→ñ, ẞ)
    // and the known locale traps (Greek final sigma, Turkish İ/I) against the
    // Excel farm; until the en-US casing basis is pinned, defer rather than emit
    // a possibly-wrong result. Consistent with UPPER's sibling deferral.
    if text.as_str().chars().any(is_unpinned_case) {
        return Value::Error(ErrorKind::Unsupported);
    }

    // Only ASCII (pinned) letters can change here; `to_ascii_lowercase` leaves
    // every non-ASCII (case-free) character untouched — matching "non-alphabetic
    // characters are left unchanged".
    let result: String = text
        .as_str()
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .collect();
    Value::text(&result)
}

#[cfg(test)]
mod tests {
    use xl_value::{ErrorKind, Value};

    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    fn lower(args: Vec<crate::test_support::TestArg>) -> Value {
        eval_direct(super::eval, args)
    }

    #[test]
    fn ascii_uppercase_to_lowercase() {
        assert_eq!(lower(vec![Scalar(txt("HELLO"))]), txt("hello"));
    }

    #[test]
    fn mixed_case_lowercased() {
        assert_eq!(lower(vec![Scalar(txt("Hello World"))]), txt("hello world"));
    }

    #[test]
    fn already_lowercase_unchanged() {
        assert_eq!(lower(vec![Scalar(txt("hello"))]), txt("hello"));
    }

    #[test]
    fn digits_and_punctuation_unchanged() {
        assert_eq!(lower(vec![Scalar(txt("A1B2-C3!?"))]), txt("a1b2-c3!?"));
    }

    #[test]
    fn number_argument_uses_general_text_coercion() {
        // Coerced to "12345" (General), no letters to change.
        assert_eq!(lower(vec![Scalar(num(12345.0))]), txt("12345"));
    }

    #[test]
    fn bool_argument_coerces_then_lowercases() {
        // to_text(TRUE) = "TRUE" -> lowercased "true".
        assert_eq!(lower(vec![Scalar(Value::Bool(true))]), txt("true"));
        assert_eq!(lower(vec![Scalar(Value::Bool(false))]), txt("false"));
    }

    #[test]
    fn empty_string_is_empty() {
        assert_eq!(lower(vec![Scalar(txt(""))]), txt(""));
    }

    #[test]
    fn blank_argument_is_empty_string() {
        // to_text(Blank) = "" (LOWER.md §Coercion).
        assert_eq!(lower(vec![Scalar(Value::Blank)]), txt(""));
    }

    #[test]
    fn omitted_argument_is_empty_string() {
        // An absent arg reads as Blank via the mock, same as explicit blank.
        assert_eq!(lower(vec![Omitted]), txt(""));
    }

    #[test]
    fn error_in_text_argument_propagates() {
        assert_eq!(
            lower(vec![Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn non_ascii_letter_is_deferred() {
        // É (Latin-1 supplement) — casing basis unpinned for en-US, so defer
        // rather than guess É->é (LOWER.md §"Oracle experiments needed").
        assert_eq!(
            lower(vec![Scalar(txt("CAFÉ"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn non_ascii_lowercase_letter_also_deferred() {
        // Even an already-lowercase accented letter defers: we cannot verify the
        // round-trip / casing basis, so no fidelity claim is made for it.
        assert_eq!(
            lower(vec![Scalar(txt("café"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn non_ascii_caseless_passes_through() {
        // Emoji / punctuation carry no case: mixed with ASCII, only the ASCII
        // letters lowercase and the non-ASCII char is untouched (fidelity path).
        assert_eq!(lower(vec![Scalar(txt("HI😀"))]), txt("hi😀"));
    }
}

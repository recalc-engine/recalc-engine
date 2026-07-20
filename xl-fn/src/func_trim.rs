//! `TRIM` — remove leading/trailing regular spaces and collapse each run of
//! internal regular spaces to a single space.
//!
//! # Provenance
//! Behavior contract: `docs/specs/TRIM.md` (Microsoft "TRIM function" support
//! page, verified 2026-07-05). Text coercion via `xl-value`'s [`to_text`] (same
//! "General" numeric formatting as `CONCATENATE`/`LEN`).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `text` (arg 0) is coerced to text via [`to_text`]: numbers use "General"
//!   formatting, `TRUE`/`FALSE` for booleans, `""` for blank, text passes
//!   through unchanged (TRIM.md §Coercion). An error-valued `text` propagates
//!   (TRIM.md §Error behavior).
//! - Leading and trailing regular space characters are removed, and every run
//!   of multiple internal regular spaces is collapsed to a single space
//!   (TRIM.md §Semantics 1–2).
//!
//! # ASCII-space-only rule — the load-bearing detail (TRIM.md §Semantics 3)
//! TRIM operates on the **regular space character only** — ASCII `0x20`
//! (`U+0020`). It is documented as designed for text imported from other
//! applications with irregular spacing, and is explicitly `0x20`-specific: it
//! does **not** strip tabs (`U+0009`), the non-breaking space (`U+00A0`, the
//! `&nbsp;` well known to survive TRIM), or the various Unicode space
//! separators (`U+2000`–`U+200A`, `U+3000`, …). This is why the implementation
//! must NOT use Rust's `str::trim`/`split_whitespace` (which treat every
//! Unicode-whitespace code point): those would over-trim and silently diverge
//! from Excel. The rule is realized by splitting on the `' '` character only,
//! dropping empty pieces (which absorbs leading/trailing spaces and internal
//! runs), and re-joining with a single `' '` — every non-`0x20` character,
//! including any embedded whitespace-like character, is carried through
//! verbatim within its piece.
//!
//! An all-spaces input therefore yields `""` (no non-empty pieces), and an
//! input with no leading/trailing/multiple-internal `0x20` is returned
//! unchanged.
//!
//! # Not deferred
//! The other-whitespace edges the spec's "Oracle experiments needed" section
//! lists (NBSP, tab, Unicode separators) are **not ambiguous for this
//! implementation**: the documented semantics (§Semantics 3) already state
//! TRIM is `0x20`-specific and leaves them untouched, and treating only `0x20`
//! honors that directly (those characters simply never match the split
//! delimiter). No behavior here rests on an unobserved guess, so nothing is
//! routed to `#UNSUPPORTED!`.

use xl_value::{Value, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `TRIM(text)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let text = match to_text(&args.eval_scalar(0)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };

    // Split on the regular space (0x20) ONLY — never Unicode-whitespace-aware
    // `trim`/`split_whitespace`. Dropping empty pieces absorbs leading/trailing
    // 0x20 and collapses internal 0x20 runs; joining with a single ' ' restores
    // exactly one separator between words. Tabs, NBSP (0xA0), and Unicode space
    // separators are not the delimiter, so they ride through inside each piece.
    let result = text
        .as_str()
        .split(' ')
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<&str>>()
        .join(" ");

    Value::text(&result)
}

#[cfg(test)]
mod tests {
    use xl_value::{ErrorKind, Value};

    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    fn trim(args: Vec<crate::test_support::TestArg>) -> Value {
        eval_direct(super::eval, args)
    }

    #[test]
    fn leading_and_trailing_spaces_removed() {
        assert_eq!(trim(vec![Scalar(txt("   hello   "))]), txt("hello"));
    }

    #[test]
    fn internal_run_collapses_to_single_space() {
        assert_eq!(trim(vec![Scalar(txt("a     b"))]), txt("a b"));
    }

    #[test]
    fn combined_leading_trailing_and_internal() {
        assert_eq!(
            trim(vec![Scalar(txt("   the   quick  brown   "))]),
            txt("the quick brown")
        );
    }

    #[test]
    fn single_internal_space_unchanged() {
        assert_eq!(trim(vec![Scalar(txt("a b c"))]), txt("a b c"));
    }

    #[test]
    fn no_trimmable_spaces_is_identity() {
        assert_eq!(trim(vec![Scalar(txt("hello"))]), txt("hello"));
    }

    #[test]
    fn all_spaces_becomes_empty() {
        assert_eq!(trim(vec![Scalar(txt("     "))]), txt(""));
    }

    #[test]
    fn empty_string_stays_empty() {
        assert_eq!(trim(vec![Scalar(txt(""))]), txt(""));
    }

    #[test]
    fn tab_is_not_trimmed() {
        // U+0009 is NOT 0x20 — it must survive, both as a leading/trailing
        // character and embedded within a word. Only the regular spaces go.
        assert_eq!(trim(vec![Scalar(txt("\ta\tb\t"))]), txt("\ta\tb\t"));
        assert_eq!(trim(vec![Scalar(txt("  \t  "))]), txt("\t"));
    }

    #[test]
    fn nbsp_is_not_trimmed() {
        // U+00A0 (&nbsp;) is famously left untouched by Excel's TRIM: it is not
        // the regular space, so it neither is removed nor collapses runs.
        assert_eq!(
            trim(vec![Scalar(txt("\u{a0}a\u{a0}\u{a0}b\u{a0}"))]),
            txt("\u{a0}a\u{a0}\u{a0}b\u{a0}")
        );
    }

    #[test]
    fn unicode_space_separators_are_not_trimmed() {
        // U+2003 (EM SPACE) and U+3000 (IDEOGRAPHIC SPACE) are Unicode
        // whitespace but not 0x20 — a naive `split_whitespace` would eat them;
        // TRIM must not.
        assert_eq!(
            trim(vec![Scalar(txt("\u{2003}x\u{3000}y\u{2003}"))]),
            txt("\u{2003}x\u{3000}y\u{2003}")
        );
    }

    #[test]
    fn nbsp_between_regular_spaces_survives_while_spaces_collapse() {
        // "a  \u{a0}  b" -> the 0x20 runs on each side of the NBSP collapse, but
        // the NBSP itself is a non-empty piece and remains.
        assert_eq!(trim(vec![Scalar(txt("a  \u{a0}  b"))]), txt("a \u{a0} b"));
    }

    #[test]
    fn number_argument_uses_general_text_coercion() {
        // Coerced to "12345" (General), which has no spaces to trim.
        assert_eq!(trim(vec![Scalar(num(12345.0))]), txt("12345"));
    }

    #[test]
    fn boolean_argument_uses_general_text_coercion() {
        assert_eq!(trim(vec![Scalar(Value::Bool(true))]), txt("TRUE"));
    }

    #[test]
    fn blank_argument_coerces_to_empty_string() {
        assert_eq!(trim(vec![Omitted]), txt(""));
    }

    #[test]
    fn error_in_text_argument_propagates() {
        assert_eq!(
            trim(vec![Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Error(ErrorKind::Div0)
        );
    }
}

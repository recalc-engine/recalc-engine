//! `EXACT` — compares two text strings and returns `TRUE` iff they are
//! **exactly** equal (case-sensitive), `FALSE` otherwise.
//!
//! # Provenance
//! Behavior contract: `docs/specs/EXACT.md` (Microsoft Learn "EXACT function"
//! page). Text coercion deferred entirely to `xl-value`'s [`to_text`] — the
//! same "General" numeric formatting as `CONCATENATE`/`FIND`.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `text1` (arg 0) and `text2` (arg 1) are each coerced to text via scalar
//!   text coercion [`to_text`]: numbers use "General" formatting, booleans ->
//!   `"TRUE"`/`"FALSE"`, blank -> `""`, text passes through unchanged
//!   (EXACT.md §Coercion).
//! - Returns `TRUE` iff the two coerced strings are **byte-for-byte** equal,
//!   including case — the comparison is **case-sensitive** (EXACT.md §1,
//!   §Semantics 2), exactly as `FIND` is case-sensitive. Any difference,
//!   including leading/trailing whitespace or a letter-case mismatch, yields
//!   `FALSE`.
//! - An error-valued argument propagates as `EXACT`'s result; `text1` is
//!   evaluated first, so its error wins over a later `text2` error (EXACT.md
//!   §Error behavior). No non-error input can itself produce an error — every
//!   scalar coerces to some text — so `EXACT` only ever returns a `Bool` or a
//!   propagated argument error.
//!
//! This function has no ambiguous edges to defer: MS Learn states the
//! case-sensitive-equality contract directly and coercion follows the frozen
//! `to_text` rules, so no oracle experiment is queued (EXACT.md §Oracle
//! experiments needed = none).

use xl_value::{Value, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate an `EXACT(text1, text2)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let text1 = match to_text(&args.eval_scalar(0)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };
    let text2 = match to_text(&args.eval_scalar(1)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };
    // Byte-exact, case-sensitive comparison (like FIND). `Text::as_str` yields
    // the interned string; equality is a plain `&str` compare.
    Value::bool(text1.as_str() == text2.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use xl_value::ErrorKind;

    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    #[test]
    fn identical_text_is_true() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("Word")), Scalar(txt("Word"))]),
            Value::bool(true)
        );
    }

    #[test]
    fn case_difference_is_false() {
        // EXACT is case-sensitive: "Word" != "word".
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("Word")), Scalar(txt("word"))]),
            Value::bool(false)
        );
    }

    #[test]
    fn different_text_is_false() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("alpha")), Scalar(txt("beta"))]),
            Value::bool(false)
        );
    }

    #[test]
    fn trailing_whitespace_matters() {
        // Byte-exact: a trailing space is a difference.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("a ")), Scalar(txt("a"))]),
            Value::bool(false)
        );
    }

    #[test]
    fn number_coerces_to_general_text() {
        // 12345 -> "12345" (General), equals the literal text "12345".
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(12345.0)), Scalar(txt("12345"))]),
            Value::bool(true)
        );
    }

    #[test]
    fn two_numbers_compare_by_general_text() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.5)), Scalar(num(1.5))]),
            Value::bool(true)
        );
    }

    #[test]
    fn bool_coerces_to_uppercase_word() {
        // TRUE -> "TRUE" equals the literal text "TRUE".
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::bool(true)), Scalar(txt("TRUE"))]),
            Value::bool(true)
        );
    }

    #[test]
    fn blank_coerces_to_empty_string() {
        // Blank -> "" equals the literal empty text.
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Blank), Scalar(txt(""))]),
            Value::bool(true)
        );
    }

    #[test]
    fn error_in_text1_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Ref)), Scalar(txt("a"))]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    #[test]
    fn error_in_text2_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("a")), Scalar(Value::Error(ErrorKind::Div0))]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn text1_error_wins_over_text2_error() {
        // text1 is evaluated first, so its error is the result.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Ref)),
                    Scalar(Value::Error(ErrorKind::Div0))
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }
}

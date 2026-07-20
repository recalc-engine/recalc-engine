//! `PROPER` — capitalizes the first letter of each word in `text` and
//! lowercases every other letter; non-letter characters pass through unchanged
//! and delimit words.
//!
//! # Provenance
//! Microsoft Learn PROPER function page
//! (`https://support.microsoft.com/en-us/office/proper-function-52a5a283-e8b2-49be-8506-b2887b889f94`).
//! Text coercion via `xl-value`'s [`to_text`] (the same "General" numeric
//! formatting `LOWER`/`UPPER`/`CONCATENATE` use). Direct casing sibling of
//! `LOWER`/`UPPER` — same en-US-only ASCII casing scope.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `text` (arg 0) coerced via [`to_text`]: numbers use "General" formatting,
//!   `"TRUE"`/`"FALSE"` for booleans, `""` for blank, text passes through. An
//!   error-valued `text` propagates.
//! - A letter is **uppercased** when it is the first character or the preceding
//!   character is **not a letter**; otherwise it is **lowercased**. Non-letter
//!   characters (digits, spaces, punctuation) pass through unchanged and act as
//!   word delimiters — so `PROPER("2-cent's worth")` = `"2-Cent'S Worth"` (the
//!   documented Excel quirk: the apostrophe and the digit are separators, so
//!   the following letters capitalize), `PROPER("hello WORLD")` =
//!   `"Hello World"`.
//!
//! # Casing basis — ASCII-only fidelity claim (non-ASCII deferred)
//! Per v1 scope (the project's scope rules §1: "no non-en-US
//! locale semantics") only ASCII `A`–`Z`/`a`–`z` casing is claimed. For a
//! **non-ASCII** letter, both the case mapping (locale-sensitive, unpinned —
//! `docs/specs/LOWER.md` §"Oracle experiments needed") **and** whether Excel
//! treats it as a word character for PROPER's boundary logic are unconfirmed,
//! so any `text` containing a case-bearing or alphabetic non-ASCII character
//! returns `#UNSUPPORTED!` rather than a possibly-wrong result — mirroring the
//! `LOWER`/`UPPER` deferral (unassigned OXP). Caseless, non-alphabetic
//! non-ASCII characters (emoji, most symbols/punctuation) carry no case and no
//! ambiguous word-membership, so they pass through as plain delimiters.

use xl_value::{ErrorKind, Value, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// A character whose PROPER handling is unpinned for en-US: any non-ASCII
/// character that is a letter (word-membership + case mapping both unpinned) or
/// otherwise changes under Unicode simple lowercasing. ASCII is always the
/// pinned path. Mirrors `func_lower`/`func_upper`.
fn is_unpinned(c: char) -> bool {
    !c.is_ascii() && (c.is_alphabetic() || c.to_lowercase().next() != Some(c))
}

/// Evaluate a `PROPER(text)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let text = match to_text(&args.eval_scalar(0)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };

    // Defer if any non-ASCII letter / case-bearing character is present: both
    // its case mapping and its word-membership are unpinned for en-US.
    if text.as_str().chars().any(is_unpinned) {
        return Value::Error(ErrorKind::Unsupported);
    }

    let mut result = String::with_capacity(text.as_str().len());
    let mut prev_is_letter = false;
    for c in text.as_str().chars() {
        // After the screen above, the only letters reaching here are ASCII.
        let is_letter = c.is_ascii_alphabetic();
        if is_letter {
            if prev_is_letter {
                result.push(c.to_ascii_lowercase());
            } else {
                result.push(c.to_ascii_uppercase());
            }
        } else {
            result.push(c);
        }
        prev_is_letter = is_letter;
    }
    Value::text(&result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    #[test]
    fn simple_words() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("hello WORLD"))]),
            txt("Hello World")
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("this is a TITLE"))]),
            txt("This Is A Title")
        );
    }

    #[test]
    fn documented_separator_quirks() {
        // Microsoft Learn examples: digit + apostrophe are non-letter
        // separators, so the following letters capitalize.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("2-cent's worth"))]),
            txt("2-Cent'S Worth")
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("76BudGet"))]),
            txt("76Budget")
        );
    }

    #[test]
    fn already_proper_and_empty() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("Hello"))]), txt("Hello"));
        assert_eq!(eval_direct(eval, vec![Scalar(txt(""))]), txt(""));
    }

    #[test]
    fn number_and_bool_coercion() {
        // to_text(12345) = "12345" (no letters); to_text(TRUE) = "TRUE" -> "True".
        assert_eq!(eval_direct(eval, vec![Scalar(num(12345.0))]), txt("12345"));
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Bool(true))]),
            txt("True")
        );
    }

    #[test]
    fn blank_and_omitted_are_empty() {
        assert_eq!(eval_direct(eval, vec![Scalar(Value::Blank)]), txt(""));
        assert_eq!(eval_direct(eval, vec![Omitted]), txt(""));
    }

    #[test]
    fn caseless_non_ascii_is_a_delimiter() {
        // An emoji carries no case and is non-alphabetic: it passes through and
        // separates words, so the letter after it capitalizes.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("ab😀cd"))]),
            txt("Ab😀Cd")
        );
    }

    #[test]
    fn non_ascii_letter_is_deferred() {
        // Case-bearing (café) and alphabetic-but-caseless (CJK) non-ASCII both
        // defer — LOWER/UPPER precedent + unpinned word-membership.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("café"))]),
            Value::Error(ErrorKind::Unsupported)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("中文 text"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn error_argument_propagates() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Error(ErrorKind::Div0)
        );
    }
}

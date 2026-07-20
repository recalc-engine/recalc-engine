//! `RIGHT` — the rightmost `num_chars` characters of `text`.
//!
//! # Provenance
//! Behavior contract: `docs/specs/RIGHT.md` (Microsoft Learn "RIGHT, RIGHTB
//! functions" page — RIGHT and its byte-oriented, locale-dependent sibling
//! `RIGHTB` share one page; only RIGHT is in scope here). `RIGHT` is `LEFT`'s
//! mirror-image sibling (`func_left.rs`): same signature, same argument
//! coercions, same documented domain rules, same oracle-deferred questions —
//! only the end of the string it slices from differs. Text coercion via
//! `xl-value`'s [`to_text`] (same "General" numeric formatting as
//! `CONCATENATE`); `num_chars` coercion via [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `text` (arg 0) is coerced to text via [`to_text`]: numbers use
//!   "General" formatting, `TRUE`/`FALSE` for booleans, `""` for blank, text
//!   passes through unchanged (RIGHT.md §Coercion). An error-valued `text`
//!   propagates (RIGHT.md §Error behavior).
//! - `num_chars` (arg 1) is **optional**; MS Learn: "If num_chars is
//!   omitted, it is assumed to be 1" — `args.count() < 2` uses `1` without
//!   evaluating a second argument at all (RIGHT.md §Signature).
//! - When supplied, `num_chars` is scalar-numeric-coerced via [`to_number`];
//!   an error-valued `num_chars` propagates.
//! - MS Learn: "Num_chars must be greater than or equal to zero" — a
//!   negative `num_chars` is documented as invalid, mapped to `#VALUE!`
//!   (the same negative-argument-invalid → `#VALUE!` convention `LEFT.md`
//!   and `MID.md` already establish for their own count-like arguments).
//!   This check is sign-only and independent of the truncation question
//!   below: a negative value is out of the documented domain regardless of
//!   its fractional part.
//! - MS Learn: "If num_chars is greater than the length of text, RIGHT
//!   returns all of text" — no error, no padding. `num_chars = 0` returns
//!   `""`, the mechanical zero-length case of "the rightmost `num_chars`
//!   characters" (not a separately-documented rule, and not an ambiguity:
//!   every reading of "rightmost N characters" agrees N=0 is empty).
//!
//! # Non-integer `num_chars` — `OXP-107` RESOLVED
//! MS Learn's RIGHT/RIGHTB page does not address a fractional `num_chars`
//! at all (identical to LEFT's page). `OXP-107` (RESOLVED by
//! `RUN-2026-07-11-oracle01`; shared with `LEFT`, same open question, same
//! id) settled the direction the same open-question family — `DATE`'s
//! `OXP-091`, `EOMONTH`'s `OXP-092`, `VLOOKUP`'s `OXP-089`, `WEEKDAY`'s
//! `OXP-097`, `ROUND`'s `OXP-098` — poses: a non-integer `num_chars` is
//! **truncated toward zero** (`2.9` -> `2`) before use, not `#UNSUPPORTED!`
//! (`LEFT("abcdef",2.9)` = `"ab"` was the run's probe; RIGHT is LEFT's
//! mirror-image sibling and applies the same rule to its own slice). The
//! negative-`num_chars` -> `#VALUE!` check is unchanged and independent of
//! this (it is sign-only, applied to the raw value).
//!
//! # Character-counting basis: **Unicode scalars** (`OXP-161`, RESOLVED)
//! `num_chars` counts Rust `char`s (Unicode **scalar values**) via
//! `str::chars()`. `OXP-161` (`RUN-2026-07-16-oracle01`) settled the astral
//! case and showed Excel is **self-inconsistent**: `RIGHT`/`LEFT` count
//! **scalars** while `LEN`/`MID`/`FIND` count UTF-16 code units. The probe:
//! `RIGHT("X😀",1)` = `"😀"` (the **whole** emoji, one scalar, *not* a lone
//! surrogate half) — consistent with the RIGHT/RIGHTB page's note that "RIGHT
//! now supports Unicode surrogates via the Compatibility Version, Version 2."
//! A scalar slice never splits a surrogate pair, so counting from the end in
//! `char`s reproduces Excel exactly and always yields valid UTF-8; `RIGHT`
//! therefore **no longer defers** on non-BMP input (the earlier
//! `#UNSUPPORTED!` guard is removed).

use xl_value::{ErrorKind, Value, to_number, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `RIGHT(text, [num_chars])` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let text = match to_text(&args.eval_scalar(0)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };

    let num_chars: usize = if args.count() < 2 {
        1
    } else {
        let raw = match to_number(&args.eval_scalar(1)) {
            Ok(n) => n,
            Err(k) => return Value::Error(k),
        };
        // MS Learn: "Num_chars must be greater than or equal to zero" — sign
        // check is independent of the truncation-direction question below.
        if raw < 0.0 {
            return Value::Error(ErrorKind::Value);
        }
        // OXP-107 RESOLVED (RUN-2026-07-11-oracle01): a non-integer num_chars
        // truncates toward zero (2.9 -> 2) before use — see module docs.
        // `raw` is finite and non-negative here (xl-value invariant), so
        // `trunc()` truncates toward zero and the `as usize` cast saturates
        // harmlessly for an absurdly large num_chars (`.chars().rev().take(..)`
        // then just yields the whole string, matching "returns all of text").
        raw.trunc() as usize
    };

    // OXP-161 (RUN-2026-07-16-oracle01): RIGHT counts Unicode **scalars**, not
    // UTF-16 code units — RIGHT("X😀",1)="😀" (the whole emoji). A scalar slice
    // never splits a surrogate pair, so counting `char`s from the end matches
    // Excel exactly and always yields valid UTF-8; RIGHT no longer defers on
    // non-BMP input.

    // Rightmost `num_chars` characters: collect from the end, then restore
    // left-to-right order (mirror of LEFT's `.chars().take(num_chars)`).
    let mut result: Vec<char> = text.as_str().chars().rev().take(num_chars).collect();
    result.reverse();
    let result: String = result.into_iter().collect();
    Value::text(&result)
}

#[cfg(test)]
mod tests {
    use xl_value::{ErrorKind, Value};

    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    fn right(args: Vec<crate::test_support::TestArg>) -> Value {
        eval_direct(super::eval, args)
    }

    #[test]
    fn default_num_chars_is_one() {
        assert_eq!(right(vec![Scalar(txt("Hello"))]), txt("o"));
    }

    #[test]
    fn explicit_num_chars() {
        assert_eq!(
            right(vec![Scalar(txt("Hello")), Scalar(num(3.0))]),
            txt("llo")
        );
    }

    #[test]
    fn num_chars_past_length_returns_whole_string() {
        assert_eq!(right(vec![Scalar(txt("Hi")), Scalar(num(10.0))]), txt("Hi"));
    }

    #[test]
    fn num_chars_zero_is_empty_string() {
        assert_eq!(right(vec![Scalar(txt("Hi")), Scalar(num(0.0))]), txt(""));
    }

    #[test]
    fn negative_num_chars_is_value_error() {
        assert_eq!(
            right(vec![Scalar(txt("Hi")), Scalar(num(-1.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn number_argument_uses_general_text_coercion() {
        assert_eq!(
            right(vec![Scalar(num(12345.0)), Scalar(num(2.0))]),
            txt("45")
        );
    }

    #[test]
    fn fractional_num_chars_truncates_toward_zero() {
        // OXP-107 RESOLVED (RUN-2026-07-11-oracle01): analogous to the run's
        // LEFT("abcdef",2.9)="ab" probe, a non-integer num_chars truncates
        // toward zero (2.9 -> 2); RIGHT("abcdef",2.9) is the rightmost 2.
        assert_eq!(
            right(vec![Scalar(txt("abcdef")), Scalar(num(2.9))]),
            txt("ef")
        );
    }

    #[test]
    fn error_in_text_argument_propagates() {
        assert_eq!(
            right(vec![
                Scalar(Value::Error(ErrorKind::Div0)),
                Scalar(num(1.0))
            ]),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn error_in_num_chars_argument_propagates() {
        assert_eq!(
            right(vec![Scalar(txt("Hi")), Scalar(Value::Error(ErrorKind::Na))]),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn absent_second_arg_defaults_to_one_char() {
        // Only one argument supplied at all (args.count() < 2) — the
        // default-1 path, never touching a (nonexistent) second slot.
        assert_eq!(right(vec![Scalar(txt("Hello"))]), txt("o"));
    }

    #[test]
    fn explicitly_omitted_second_arg_coerces_blank_to_zero() {
        // A *present but elided* second argument (args.count() == 2) is
        // distinct from an *absent* one: it goes through normal Blank
        // coercion (to_number(Blank) = 0), not the default-1 shortcut.
        assert_eq!(right(vec![Scalar(txt("Hello")), Omitted]), txt(""));
    }

    #[test]
    fn astral_right_counts_scalars_never_splits_a_pair() {
        // OXP-161 (RUN-2026-07-16-oracle01): RIGHT counts Unicode scalars.
        // RIGHT("X😀",1)="😀" (H6) — the WHOLE emoji, not a lone surrogate half.
        // This is the measured per-function inconsistency: LEFT/RIGHT scalar,
        // LEN/MID/FIND UTF-16-unit.
        assert_eq!(right(vec![Scalar(txt("X😀")), Scalar(num(1.0))]), txt("😀"));
        // Two scalars from the end of "X😀" → the whole string.
        assert_eq!(
            right(vec![Scalar(txt("X😀")), Scalar(num(2.0))]),
            txt("X😀")
        );
        // Three emoji, take 2 scalars → the last two whole emoji.
        assert_eq!(
            right(vec![Scalar(txt("😀😀😀")), Scalar(num(2.0))]),
            txt("😀😀")
        );
    }
}

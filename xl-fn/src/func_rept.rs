//! `REPT` — repeats `text` a given number of times.
//!
//! # Provenance
//! Behavior contract: `docs/specs/REPT.md` (Microsoft Learn "REPT function"
//! page). Text coercion via `xl-value`'s [`to_text`] (same "General" numeric
//! formatting as `CONCATENATE`); `number_times` coercion via [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `text` (arg 0) is coerced to text via [`to_text`]: numbers use "General"
//!   formatting, booleans -> `"TRUE"`/`"FALSE"`, blank -> `""`, text passes
//!   through unchanged (REPT.md §Coercion). An error-valued `text` propagates
//!   (REPT.md §Error behavior).
//! - `number_times` (arg 1) is scalar-numeric-coerced via [`to_number`]; an
//!   error-valued `number_times` propagates. It is evaluated *after* `text`,
//!   so a `text` error wins.
//! - MS Learn: "If number_times is 0 (zero), REPT returns empty text" — and,
//!   mechanically, an empty `text` yields empty text for any count. Both
//!   collapse to `""` (REPT.md §Semantics 2).
//! - A **negative** `number_times` is out of the documented domain
//!   ("A positive number specifying the number of times to repeat text") and
//!   maps to `#VALUE!` — the same sign-check-first convention `LEFT`/`MID` use
//!   for their count arguments (REPT.md §Error behavior). The check is on the
//!   raw value and independent of the truncation below.
//! - A **non-integer** `number_times` is **truncated toward zero** (`2.9` ->
//!   `2`) before use. MS Learn does not address a fractional count; this is the
//!   truncate-toward-zero convention resolved for the numeric-argument family
//!   by `RUN-2026-07-11-oracle01` (`OXP-107`) and consistent with the wider
//!   `OXP-098` family (`ROUND` `OXP-098`, `DATE` `OXP-091`, `EOMONTH` `OXP-092`,
//!   `VLOOKUP` `OXP-089`, `WEEKDAY` `OXP-097`, `LEFT`/`RIGHT`/`MID`/`FIND`
//!   `OXP-107`) (REPT.md §Coercion).
//! - MS Learn: "The result of the REPT function ... cannot be longer than
//!   32,767 characters, or REPT returns #VALUE!". The cap is checked on the
//!   **projected** result length (`char_len * number_times`) **before** any
//!   allocation, so an astronomically large `number_times` can never trigger an
//!   unbounded allocation — it errors out first. A result of exactly 32,767
//!   characters is allowed; 32,768 or longer is `#VALUE!` (REPT.md §Error
//!   behavior).
//!
//! # Character basis
//! The 32,767 cap and the repeat both count Rust `char`s (Unicode scalar
//! values). Repetition concatenates *whole* copies of `text` and never slices a
//! surrogate pair, so — unlike `LEFT`/`MID`/`FIND` — there is no astral-plane
//! boundary hazard here and `OXP-108` does not apply; the count agrees with
//! Excel's UTF-16 basis for all BMP text (all ASCII / typical corpus text).

use xl_value::{ErrorKind, Value, to_number, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Excel's maximum cell/text length; a longer `REPT` result is `#VALUE!`.
const MAX_LEN: f64 = 32_767.0;

/// Evaluate a `REPT(text, number_times)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let text = match to_text(&args.eval_scalar(0)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };
    let raw = match to_number(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };

    // A negative count is a documented domain violation -> #VALUE! (sign check
    // on the raw value, independent of the truncation direction below).
    if raw < 0.0 {
        return Value::Error(ErrorKind::Value);
    }
    // OXP-107 / OXP-098 family: a non-integer count truncates toward zero
    // (2.9 -> 2). `raw` is finite and non-negative here (xl-value invariant),
    // so `trunc()` truncates toward zero.
    let count = raw.trunc();

    let s = text.as_str();
    let char_len = s.chars().count();

    // Enforce the 32,767-char cap on the *projected* length BEFORE building the
    // string, so a huge count cannot drive an unbounded allocation. Both
    // factors are finite and non-negative; the product is finite (or +inf for
    // an absurd count, which is > MAX_LEN and errors) — never NaN, never a
    // panic.
    if char_len as f64 * count > MAX_LEN {
        return Value::Error(ErrorKind::Value);
    }

    // Past the cap check `char_len * count <= 32767`. If either factor is zero
    // the result is empty — handle it before casting `count`, so a huge count
    // paired with empty `text` never reaches the (bounded) `as usize` cast.
    if char_len == 0 || count == 0.0 {
        return Value::text("");
    }

    // char_len >= 1 here, so count <= 32767: the cast is exact and small.
    Value::text(&s.repeat(count as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    #[test]
    fn basic_repeat() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("ab")), Scalar(num(3.0))]),
            txt("ababab")
        );
    }

    #[test]
    fn zero_times_is_empty() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("ab")), Scalar(num(0.0))]),
            txt("")
        );
    }

    #[test]
    fn negative_times_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("ab")), Scalar(num(-1.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn result_over_cap_is_value_error() {
        // 2 chars * 20000 = 40000 > 32767.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("ab")), Scalar(num(20_000.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn result_at_cap_boundary_is_allowed() {
        // 1 char * 32767 = 32767, exactly the cap -> allowed.
        let expected = "a".repeat(32_767);
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("a")), Scalar(num(32_767.0))]),
            txt(&expected)
        );
    }

    #[test]
    fn result_one_over_cap_is_value_error() {
        // 1 char * 32768 = 32768 > 32767.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("a")), Scalar(num(32_768.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn non_integer_count_truncates_toward_zero() {
        // OXP-107 / OXP-098 family: 2.9 -> 2.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("ab")), Scalar(num(2.9))]),
            txt("abab")
        );
    }

    #[test]
    fn empty_text_returns_empty() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("")), Scalar(num(5.0))]),
            txt("")
        );
    }

    #[test]
    fn empty_text_with_huge_count_does_not_allocate() {
        // char_len == 0 short-circuits to "" before casting the (enormous)
        // count to usize — guards against unbounded allocation / bad cast.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("")), Scalar(num(1e300))]),
            txt("")
        );
    }

    #[test]
    fn number_text_uses_general_coercion() {
        // 12 -> "12" (General), repeated twice.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(12.0)), Scalar(num(2.0))]),
            txt("1212")
        );
    }

    #[test]
    fn error_in_text_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Ref)), Scalar(num(2.0))]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    #[test]
    fn error_in_count_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("a")), Scalar(Value::Error(ErrorKind::Na))]
            ),
            Value::Error(ErrorKind::Na)
        );
    }
}

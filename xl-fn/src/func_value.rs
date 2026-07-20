//! `VALUE` — converts a text string that represents a number to a number.
//!
//! # Provenance
//! Behavior contract: `docs/specs/VALUE.md` (cites the Microsoft Learn
//! `VALUE` function page). Numeric-text parsing is deferred **entirely** to
//! `xl-value`'s [`to_number`] — `VALUE` never re-implements number parsing,
//! so its result always agrees with the engine's canonical scalar
//! text->number coercion. `to_number`'s own docs already cite the Microsoft
//! Learn `VALUE` page as one provenance source for the `Text("")` ->
//! `#VALUE!` rule (see `xl-value/src/coerce.rs` module docs), so that case
//! needs no new citation here.
//!
//! # Semantics implemented (spec bullets in parentheses; see VALUE.md)
//! - **Core** (VALUE.md §Supported): a `Number`/`Bool`/`Blank` argument, or a
//!   `Text` argument holding a plain/signed/decimal/scientific numeric
//!   string, an en-US-grouped thousands string (`"1,000"`), a trailing `%`
//!   percentage (`"50%"`), or a `$`-currency string (`"$1,000"`) -> that
//!   number, via `to_number` unchanged (VALUE.md §1).
//! - **Date/time text** (VALUE.md §1) — RESOLVED (OXP-160,
//!   RUN-2026-07-11-oracle01): `to_number` now coerces the Microsoft-Learn
//!   `VALUE` examples `="16:48:00"` and `="1/1/2020"` to their serials, so
//!   `VALUE` returns them directly (`VALUE("1/1/2020")` = 43831,
//!   `VALUE("16:48:00")` = 0.7 — the FULL serial incl. time fraction, matching
//!   bare `"text"+0` coercion). A recognized-but-invalid date is `#VALUE!`; a
//!   no-year form (`"1/1"`) is `#UNSUPPORTED!` — all inherited verbatim from
//!   the frozen `to_number` contract.
//! - Non-convertible text (e.g. `"abc"`, `""`, `"N/A"`) -> `#VALUE!`
//!   (VALUE.md §Error behavior).
//! - An error-valued argument propagates as-is (VALUE.md §Error behavior).
//!
//! # A thin delegate (no local heuristics)
//! `VALUE` is exactly `to_number` lifted into a function: whatever `to_number`
//! returns — a number, `#VALUE!`, or an `#UNSUPPORTED!` deferral (overflow,
//! percent-whitespace, an unprobed `$` placement, an out-of-range/no-year
//! date) — is `VALUE`'s result. The earlier `looks_like_deferred_format`
//! stopgap (which pre-empted a `#VALUE!` for `$`/`:`/`/`-shaped text while the
//! currency/date/time experiments OXP-010/OXP-001 were unresolved) is **gone**:
//! those experiments are resolved (OXP-010/OXP-160), so `to_number` is now the
//! single authority and `VALUE` agrees with bare coercion by construction.

use xl_value::{Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `VALUE(text)` call. Pure delegation to the frozen `to_number`
/// coercion contract; see the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    match to_number(&args.eval_scalar(0)) {
        Ok(n) => Value::number(n),
        Err(k) => Value::Error(k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::ErrorKind;

    #[test]
    fn plain_integer_text() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("123"))]), num(123.0));
    }

    #[test]
    fn decimal_text() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("1.5"))]), num(1.5));
    }

    #[test]
    fn signed_text() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("-42"))]), num(-42.0));
    }

    #[test]
    fn thousands_grouped_text() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("1,000"))]), num(1000.0));
    }

    #[test]
    fn percent_text() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("50%"))]), num(0.5));
    }

    #[test]
    fn number_argument_passes_through() {
        assert_eq!(eval_direct(eval, vec![Scalar(num(7.0))]), num(7.0));
    }

    #[test]
    fn blank_argument_is_zero() {
        assert_eq!(eval_direct(eval, vec![Scalar(Value::Blank)]), num(0.0));
    }

    #[test]
    fn bool_argument_coerces() {
        assert_eq!(eval_direct(eval, vec![Scalar(Value::Bool(true))]), num(1.0));
    }

    #[test]
    fn non_numeric_text_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn empty_text_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt(""))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn error_argument_propagates() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Error(ErrorKind::Div0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Na))]),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn currency_prefix_text_parses() {
        // OXP-010 (RUN-2026-07-11-oracle01): MS Learn's own VALUE example
        // (`=VALUE("$1,000")` -> 1000) — a leading `$` currency prefix
        // coerces via `to_number`.
        assert_eq!(eval_direct(eval, vec![Scalar(txt("$1,000"))]), num(1000.0));
    }

    #[test]
    fn time_text_now_coerces() {
        // OXP-160 (RUN-2026-07-11-oracle01): MS Learn's own VALUE example
        // `=VALUE("16:48:00")` now returns the serial time fraction 0.7 (was a
        // deferred #UNSUPPORTED! while OXP-001 date/time was unresolved). Matches
        // bare `"16:48:00"+0` = 0.7.
        assert_eq!(eval_direct(eval, vec![Scalar(txt("16:48:00"))]), num(0.7));
    }

    #[test]
    fn date_text_now_coerces() {
        // OXP-160 (RUN-2026-07-11-oracle01): `=VALUE("1/1/2020")` now returns
        // the serial 43831 (was deferred #UNSUPPORTED! under the old OXP-001
        // hold). Matches bare `"1/1/2020"+0` = 43831.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("1/1/2020"))]),
            num(43831.0)
        );
    }

    #[test]
    fn invalid_date_text_is_value_error() {
        // A recognized-but-invalid date is #VALUE! straight from `to_number`
        // (OXP-160), with no local heuristic in between.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("2/29/2021"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn percent_with_ascii_space_parses() {
        // OXP-013 (RUN-2026-07-11-oracle01): ASCII space(s) between the number
        // and a trailing `%` are accepted by `to_number` (`"50 %"` -> 0.5), and
        // VALUE agrees.
        assert_eq!(eval_direct(eval, vec![Scalar(txt("50 %"))]), num(0.5));
    }
}

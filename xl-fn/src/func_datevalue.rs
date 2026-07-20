//! `DATEVALUE` — convert a date/time **text** string to its serial number,
//! returning the **date part only** (OXP-160).
//!
//! # Provenance
//! Behavior contract: `docs/specs/DATEVALUE.md`. The date/time grammar is the
//! frozen `xl-value` contract [`xl_value::parse_datetime_text`] (the *same*
//! parser bare `"text"+0` coercion uses — the design rule that coercion tables live in
//! `xl-value`, never reimplemented per-function). The serial construction is
//! `xl-fn`'s own [`crate::datecore::date_to_serial`], so DATEVALUE honors the
//! workbook 1900/1904 date system.
//!
//! # Why the split (no crate cycle)
//! `xl-value` (the parser's home) must not depend on `xl-fn` (where `datecore`
//! lives) — the dependency runs one way, `xl-fn` → `xl-value`. So the parser
//! returns date **components** `(year, month, day)` plus a time fraction, and
//! each caller does its own serial math: bare `to_number` a self-contained
//! 1900-system calc, DATEVALUE the date-system-aware `datecore`. Both produce
//! identical serials for the pinned dates in the default 1900 system.
//!
//! # Semantics (OXP-160, RUN-2026-07-11-oracle01)
//! - **Date part only** — unlike bare coercion (which keeps the time fraction),
//!   DATEVALUE floors to the integer serial: `DATEVALUE("1/1/2020 16:48")` =
//!   43831 where `="1/1/2020 16:48"+0` = 43831.7. Since the parser already
//!   returns the date and time as separate components, "floor" is simply
//!   *dropping* the time fraction — no rounding needed.
//! - **A pure time has date part 0**: `DATEVALUE("16:48")` = 0 (bare coercion
//!   gives the fraction 0.7; DATEVALUE takes only its date part).
//! - **Invalid dates** (`"2/29/2021"`, `"13/1/2020"`) → `#VALUE!`.
//! - **No-year** forms (`"1/1"`, clock-dependent) → `#UNSUPPORTED!`, consistent
//!   with `to_number` — DATEVALUE has a clock (via `ctx`) but the no-year
//!   reading is deferred rather than guessed (never silently wrong).
//! - **Non-date text** (`"abc"`, `"5"`) → `#VALUE!` (not a date string).
//! - An **error** argument propagates; a non-text argument is unprobed →
//!   `#UNSUPPORTED!` (DATEVALUE takes a date *string*; other shapes were not
//!   measured, so they defer rather than guess).

use xl_value::{DateTimeText, ErrorKind, Value, parse_datetime_text};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::datecore::date_to_serial;

/// Evaluate a `DATEVALUE(date_text)` call. See the module docs.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let arg = args.eval_scalar(0);
    let text = match &arg {
        Value::Text(t) => t.as_str(),
        // Error propagates as-is.
        Value::Error(k) => return Value::Error(*k),
        // BC-6 (RFC-0012): a lambda argument is refused explicitly.
        Value::Lambda(_) => return Value::Error(ErrorKind::Unsupported),
        // DATEVALUE expects a date *string*; other argument shapes were not
        // probed → defer rather than guess a coercion.
        _ => return Value::Error(ErrorKind::Unsupported),
    };

    match parse_datetime_text(text) {
        DateTimeText::Parsed { date, .. } => match date {
            // Date part only: build the serial via `datecore` (honoring the
            // workbook date system); the time fraction is discarded.
            Some((y, m, d)) => {
                match date_to_serial(i64::from(y), i64::from(m), i64::from(d), ctx.date_system()) {
                    Ok(serial) => Value::number(serial as f64),
                    // A valid calendar date outside the representable serial
                    // range is unprobed for date-text → defer (never guessed).
                    Err(_) => Value::Error(ErrorKind::Unsupported),
                }
            }
            // A pure time (no date part) has date part 0.
            None => Value::number(0.0),
        },
        // Recognized shape but an invalid calendar date → #VALUE!.
        DateTimeText::Invalid => Value::Error(ErrorKind::Value),
        // No-year / unrecognized-but-shaped → defer, consistent with to_number.
        DateTimeText::Unsupported => Value::Error(ErrorKind::Unsupported),
        // Not a date string at all → #VALUE!.
        DateTimeText::NotDateTime => Value::Error(ErrorKind::Value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datecore::DateSystem;
    use crate::test_support::{TestArg::*, TestArgs, eval_direct, num, txt};

    /// Invoke `eval` under an explicit date system (for the 1904 cross-check).
    fn eval_in(system: DateSystem, args: Vec<crate::test_support::TestArg>) -> Value {
        let ctx = EvalContext::with_date_system(system);
        let mut ta = TestArgs::new(args);
        eval(&ctx, &mut ta as &mut dyn CallArgs)
    }

    // ---- OXP-160: date part only (RUN-2026-07-11-oracle01) ------------------

    #[test]
    fn oxp160_date_text_spellings_are_43831() {
        // Every accepted spelling of 2020-01-01 → serial 43831 (date part).
        for s in [
            "1/1/2020",
            "01/01/2020",
            "2020-01-01",
            "1-Jan-2020",
            "1-Jan-20",
            "Jan 1, 2020",
            "January 1, 2020",
            "Jan 2020",
        ] {
            assert_eq!(
                eval_direct(eval, vec![Scalar(txt(s))]),
                num(43831.0),
                "{s:?}"
            );
        }
    }

    #[test]
    fn oxp160_two_digit_year_pivot() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("1/1/29"))]), num(47119.0));
        assert_eq!(eval_direct(eval, vec![Scalar(txt("1/1/30"))]), num(10959.0));
    }

    #[test]
    fn oxp160_leap_valid_and_invalid() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("2/29/2020"))]),
            num(43890.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("2/29/2021"))]),
            Value::Error(ErrorKind::Value)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("13/1/2020"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // ---- OXP-160: DATEVALUE vs bare coercion — the TIME distinction ---------

    #[test]
    fn oxp160_datevalue_takes_date_part_only() {
        // The headline distinction: bare `"1/1/2020 16:48"+0` = 43831.7 but
        // DATEVALUE floors to the date part 43831 (the time fraction is dropped).
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("1/1/2020 16:48"))]),
            num(43831.0)
        );
        // A pure time has date part 0 (bare `"16:48"+0` = 0.7).
        assert_eq!(eval_direct(eval, vec![Scalar(txt("16:48"))]), num(0.0));
        assert_eq!(eval_direct(eval, vec![Scalar(txt("16:48:00"))]), num(0.0));
    }

    // ---- OXP-160: no-year defers (consistent with to_number) ----------------

    #[test]
    fn oxp160_no_year_form_defers() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("1/1"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // ---- non-date text and error propagation --------------------------------

    #[test]
    fn non_date_text_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
        // A pure numeric string is not a date string → #VALUE!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("43831"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn error_argument_propagates() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Na))]),
            Value::Error(ErrorKind::Na)
        );
    }

    // ---- 1904 date system: DATEVALUE honors the workbook flag ---------------

    #[test]
    fn respects_1904_date_system() {
        // 2020-01-01 is serial 43831 in the 1900 system but 43831 − 1462 =
        // 42369 in the 1904 system (the epochs differ by 1462 days). DATEVALUE
        // builds the serial via `datecore`, so it tracks the workbook flag.
        assert_eq!(
            eval_in(DateSystem::Excel1904, vec![Scalar(txt("1/1/2020"))]),
            num(42369.0)
        );
    }
}

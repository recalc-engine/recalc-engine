//! `HOUR` — the hour-of-day (0-23) of a time serial.
//!
//! # Provenance
//! Behavior contract: `docs/specs/HOUR.md`, which cites the Microsoft Learn
//! HOUR function page
//! (`https://support.microsoft.com/en-us/office/hour-function-a3afa879-86cb-4339-b1b5-2dd2d7310ac7`,
//! verified 2026-07-11). Numeric coercion is `xl-value`'s frozen
//! [`xl_value::to_number`] contract (the design rule that coercion tables live in
//! `xl-value`, never reimplemented per-function).
//!
//! # Semantics implemented
//! - `HOUR(serial_number)` — one required argument (`HOUR_SPEC` in
//!   `xl-fn/src/registry.rs`, already wired: `min_args: 1, max_args: Some(1)`).
//! - A serial number is a whole-day-plus-fraction `f64`: the **integer**
//!   part is the date, the **fractional** part is the time of day
//!   (`0.0` = midnight, `0.5` = noon — HOUR.md's cited remark: "Time values
//!   are a portion of a date value and represented by a decimal number").
//!   `HOUR` reads *only* the fractional part, so unlike `YEAR`/`MONTH`/`DAY`
//!   (which need the integer part to resolve a real calendar date, and so
//!   route through [`crate::date_common::coerce_serial`]'s floor-to-a-whole-
//!   day), this function coerces via plain [`to_number`] and never floors —
//!   flooring first would discard the very fraction being extracted.
//! - `hour = floor(round(fract(serial) * 86400) / 3600) mod 24`. Excel stores
//!   times to one-second (1/86400-day) precision and **rounds a computed
//!   serial to the nearest second before reading its time components**
//!   (resolved by the oracle — see the sub-second rounding note below), so the
//!   fractional day is converted to a whole number of seconds first, then to
//!   an hour. For an exact fraction this agrees with HOUR.md's cited example
//!   (`HOUR(0.75)` = `floor(round(0.75 * 86400) / 3600)` = `floor(64800/3600)`
//!   = `18`) — not an assumed formula.
//! - A serial's **integer** (date) part is irrelevant to the result: e.g.
//!   `HOUR(100.5)` and `HOUR(0.5)` both give `12`, matching HOUR.md's own
//!   "date with no time" / "date/time" examples, where only the fractional
//!   part ever participates.
//! - **Negative serial numbers** → `#NUM!`. Resolved by the oracle
//!   (RUN-2026-07-11-oracle01, `OXP-132`): `HOUR(-0.5)`, `HOUR(-1.25)` and
//!   `HOUR(-0.25)` all yield `#NUM!` — Excel rejects a negative time serial
//!   rather than reading a sign-adjusted fractional part.
//! - **Sub-second serial rounding**: resolved by the oracle
//!   (RUN-2026-07-11-oracle01, `OXP-133`). Excel rounds the serial to the
//!   nearest second before extracting the hour: `HOUR(0.5-0.0000001)` = `12`
//!   (43199.99 s → 43200 s = 12:00:00, not the naive `floor(fract*24)` = 11),
//!   `HOUR(13/24)` = `13` (the exact 46800 s, not `12` from `13/24*24`'s
//!   floating-point undershoot), and `HOUR(1-0.0000001)` = `0` (86399.99 s →
//!   86400 s, i.e. the following midnight, so the hour wraps `24 mod 24 = 0`).
//!   This is why the result takes `round(fract * 86400)` seconds and then
//!   `mod 24` on the hour.
//! - **Time-of-day text arguments** (e.g. `"6:45 PM"`): resolved by the oracle
//!   (RUN-2026-07-11-oracle01, `OXP-134`). Excel parses documented time
//!   strings to a serial: `HOUR("6:45 PM")` = `18`, `HOUR("18:45")` = `18`,
//!   `HOUR("6:45")` = `6`. A `Text` argument that [`to_number`] cannot read as
//!   *numeric* text is therefore run through [`parse_time_of_day`], which
//!   accepts the observation-anchored `H:MM[:SS]` grammar (24-hour without a
//!   meridiem; 12-hour with a trailing `AM`/`PM`) and yields a fractional day
//!   fed through the same second-rounding extraction. Text outside that
//!   grammar stays `#UNSUPPORTED!` (never silently `#VALUE!`): the parser only
//!   covers what the run pinned, and genuine garbage is indistinguishable from
//!   an unrecognized-but-valid format without a broader oracle sweep. A
//!   non-text coercion error (an already-`Error` value, an unsupported
//!   aggregate shape) propagates unchanged — ordinary error propagation.
//! - `#NUM!` for a non-finite serial (defensive: `xl-value`'s `Number`
//!   variant is not expected to ever hold `NaN`/`±inf`, but the check keeps
//!   the fraction math well-defined regardless).

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `HOUR(serial_number)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let arg = args.eval_scalar(0);
    let serial = match to_number(&arg) {
        Ok(n) => n,
        Err(k) => match &arg {
            // OXP-134 (RUN-2026-07-11-oracle01): a non-numeric text argument
            // may be a documented Excel time string. Parse the grammar the
            // run pinned; anything else stays #UNSUPPORTED! (not #VALUE!).
            Value::Text(t) => match parse_time_of_day(t.as_str()) {
                Some(fraction_of_day) => fraction_of_day,
                None => return Value::Error(ErrorKind::Unsupported),
            },
            // BC-6 (RFC-0012): refusal is deferred to `to_number(Lambda)`'s
            // `#UNSUPPORTED!` (`k`); make it explicit here too.
            Value::Lambda(_) => return Value::Error(k),
            _ => return Value::Error(k),
        },
    };

    if !serial.is_finite() {
        return Value::Error(ErrorKind::Num);
    }
    if serial < 0.0 {
        // OXP-132 (RUN-2026-07-11-oracle01): negative time serials → #NUM!.
        return Value::Error(ErrorKind::Num);
    }

    Value::number(hour_of_fraction(serial.fract()))
}

/// Extract the whole hour `0..=23` from a fraction-of-day in `[0, 1)`, applying
/// Excel's nearest-second pre-rounding (OXP-133): the fraction is snapped to a
/// whole number of seconds, then reduced to an hour. A fraction rounding up to
/// a full day (86400 s) wraps to hour `0` via the final `mod 24`.
fn hour_of_fraction(fraction_of_day: f64) -> f64 {
    let seconds = (fraction_of_day * 86_400.0).round();
    ((seconds / 3600.0).floor()) % 24.0
}

/// A trailing 12-hour-clock meridiem parsed off a time string.
enum Meridiem {
    Am,
    Pm,
}

/// Split an optional case-insensitive trailing `AM`/`PM` off a time string,
/// returning the remaining body (right-trimmed) and the meridiem if present.
fn split_meridiem(s: &str) -> (&str, Option<Meridiem>) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    if n >= 2 && s.is_char_boundary(n - 2) {
        let tail = &bytes[n - 2..];
        let meridiem = if tail.eq_ignore_ascii_case(b"pm") {
            Some(Meridiem::Pm)
        } else if tail.eq_ignore_ascii_case(b"am") {
            Some(Meridiem::Am)
        } else {
            None
        };
        if meridiem.is_some() {
            return (s[..n - 2].trim_end(), meridiem);
        }
    }
    (s, None)
}

/// Parse a documented Excel time-of-day text string into a fraction-of-day in
/// `[0, 1)`, returning `None` for any shape outside the grammar the oracle run
/// pinned (RUN-2026-07-11-oracle01, `OXP-134`: `"6:45 PM"` → 18:45,
/// `"18:45"` → 18:45, `"6:45"` → 06:45).
///
/// Accepted: `H:MM` or `H:MM:SS`, optionally suffixed with a space-separated
/// `AM`/`PM`. Without a meridiem the hour is 24-hour (`0..=23`); with one it is
/// a 12-hour clock (`1..=12`, `12 AM` = 00, `12 PM` = 12 per the universal
/// convention — only the `"6:45 PM"` → +12 mechanic is directly
/// observation-anchored). Anything else returns `None` so the caller can defer
/// rather than guess.
fn parse_time_of_day(text: &str) -> Option<f64> {
    let (body, meridiem) = split_meridiem(text.trim());
    let mut parts = body.split(':');
    let hour: u32 = parts.next()?.trim().parse().ok()?;
    let minute: u32 = parts.next()?.trim().parse().ok()?;
    let second: u32 = match parts.next() {
        Some(sec) => sec.trim().parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() || minute >= 60 || second >= 60 {
        return None;
    }
    let hour = match meridiem {
        None if hour <= 23 => hour,
        Some(Meridiem::Am) if hour == 12 => 0,
        Some(Meridiem::Am) if (1..=11).contains(&hour) => hour,
        Some(Meridiem::Pm) if hour == 12 => 12,
        Some(Meridiem::Pm) if (1..=11).contains(&hour) => hour + 12,
        _ => return None,
    };
    let total_seconds = hour * 3600 + minute * 60 + second;
    Some(f64::from(total_seconds) / 86_400.0)
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::{ErrorKind, Value};

    use super::eval;

    #[test]
    fn noon_is_12() {
        // HOUR(0.5) — 0.5 is noon (HOUR.md's cited remark: 12:00 PM = 0.5).
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.5))]), num(12.0));
    }

    #[test]
    fn midnight_is_0() {
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.0))]), num(0.0));
    }

    #[test]
    fn quarter_day_is_6() {
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.25))]), num(6.0));
    }

    #[test]
    fn ms_learn_cited_example_0_75_is_18() {
        // HOUR.md / MS Learn: HOUR(0.75) -> 18 ("75% of 24 hours").
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.75))]), num(18.0));
    }

    #[test]
    fn datetime_serial_uses_only_the_fraction() {
        // A full datetime serial (integer date part + 0.5 fraction) must give
        // the same hour as the bare fraction: only fract(serial) matters.
        assert_eq!(eval_direct(eval, vec![Scalar(num(100.5))]), num(12.0));
        assert_eq!(eval_direct(eval, vec![Scalar(num(44_927.25))]), num(6.0));
    }

    #[test]
    fn error_propagates() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Error(ErrorKind::Div0)
        );
    }

    // --- OXP-132 (RUN-2026-07-11-oracle01): negative serials → #NUM! ---

    #[test]
    fn oxp132_negative_serials_are_num_error() {
        // HOUR(-0.5), HOUR(-1.25), HOUR(-0.25) all observed as #NUM!.
        for s in [-0.5, -1.25, -0.25] {
            assert_eq!(
                eval_direct(eval, vec![Scalar(num(s))]),
                Value::Error(ErrorKind::Num),
                "HOUR({s})"
            );
        }
    }

    // --- OXP-133 (RUN-2026-07-11-oracle01): nearest-second pre-rounding ---

    #[test]
    fn oxp133_subsecond_rounds_to_nearest_second() {
        // Observed: HOUR(0.5)=12, HOUR(0.5-1e-7)=12, HOUR(13/24)=13,
        // HOUR(1-1e-7)=0 (wraps at the following midnight).
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.5))]), num(12.0));
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.5 - 0.000_000_1))]),
            num(12.0)
        );
        assert_eq!(eval_direct(eval, vec![Scalar(num(13.0 / 24.0))]), num(13.0));
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0 - 0.000_000_1))]),
            num(0.0)
        );
    }

    // --- OXP-134 (RUN-2026-07-11-oracle01): time-of-day text strings ---

    #[test]
    fn oxp134_parses_documented_time_strings() {
        // Observed: HOUR("6:45 PM")=18, HOUR("18:45")=18, HOUR("6:45")=6.
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::text("6:45 PM"))]),
            num(18.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::text("18:45"))]),
            num(18.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::text("6:45"))]),
            num(6.0)
        );
    }

    #[test]
    fn garbage_text_is_unsupported_not_guessed() {
        // Text outside the oracle-pinned grammar is indistinguishable from an
        // unparsed-but-valid format, so it defers rather than guessing #VALUE!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::text("not a time"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn numeric_text_still_coerces() {
        // "0.75" is numeric text (xl-value coercion), not a time string.
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::text("0.75"))]),
            num(18.0)
        );
    }
}

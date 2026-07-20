//! `xl-value` — the Recalc value model and coercion tables.
//!
//! Defines the runtime value type — [`Value`] with variants `Number(f64)`,
//! `Text` (interned), `Bool`, `Error`, `Blank`, `Array` (2-D, spillable),
//! `Ref` — and **every** coercion between them: number/text/bool coercion,
//! comparison, and the aggregate-vs-scalar coercion asymmetry.
//!
//! **This crate's public types are the frozen contract between `xl-io`,
//! `xl-ast`, `xl-graph`, `xl-fn`, and `xl-engine`** (`implementation-plan.md`
//! §2). The contract freezes first and is human-reviewed; cross-crate changes
//! to it require an RFC in `/rfcs`. All coercion logic lives here so no other
//! crate reimplements (and diverges on) Excel's rules.
//!
//! # Module-header provenance
//! Behavior is reconstructed **clean-room** from public sources only —
//! Microsoft Learn function/operator documentation and ECMA-376 (ISO/IEC
//! 29500). GPL spreadsheet source (LibreOffice/Gnumeric) is forbidden reading
//! (`implementation-plan.md` §9). Each rule cites its source in the module
//! where it lives (`error`, `coerce`). Rules that can only be pinned down
//! by running Excel are **not guessed**: their code path returns
//! [`ErrorKind::Unsupported`] and is queued in `docs/oracle-experiments.md`
//! (§0 "Never silently wrong").
//!
//! # Invariants (frozen)
//! - **No NaN/Inf in a cell.** [`Value::Number`] holds only finite `f64`;
//!   construct via [`Value::number`], which maps non-finite results to
//!   [`ErrorKind::Num`]. See [`Value`] for the full rationale.
//! - **`""` ≠ `Blank`.** `Text("")` and [`Value::Blank`] are different
//!   values with different coercions (blank → `0` numerically; `""` →
//!   `#VALUE!`). See the `coerce` module.
//! - **Case-insensitive comparison.** `=`/`<`/`>` fold case ([`compare`]);
//!   [`Text`]'s own `Eq`/`Ord` stay case-sensitive.
//! - **Ops never panic.** No coercion or comparison panics on any `Value`
//!   (property-tested).

#![forbid(unsafe_code)]

mod coerce;
mod error;
mod text;
mod value;

pub use coerce::{
    CoercionMode, DateTimeText, NumericArg, coerce_number_arg, compare, compare_fuzzy,
    parse_datetime_text, text_eq_ci, to_bool, to_number, to_text, total_order, values_equal,
};
pub use error::ErrorKind;
pub use text::Text;
pub use value::{Array, Lambda, LambdaObj, RectRange, Ref, ShapeError, SheetId, Value};

// ===========================================================================
// Table-driven unit tests for the documented coercion cases.
//
// These encode the rules spelled out in the module docs. They are a
// human-readable specification snapshot; once the Excel oracle grids exist
// (`implementation-plan.md` §3) those grids become the authority and these
// tests are expected to be *superseded* by (or cross-checked against) them.
// Anything oracle-deferred is asserted to return #UNSUPPORTED!, not a guess.
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use core::cmp::Ordering;

    fn num(x: f64) -> Value {
        Value::number(x)
    }
    fn txt(s: &str) -> Value {
        Value::text(s)
    }

    // ---- Number coercion (scalar) ----------------------------------------
    #[test]
    fn to_number_documented_cases() {
        assert_eq!(to_number(&Value::Blank), Ok(0.0));
        assert_eq!(to_number(&Value::Bool(true)), Ok(1.0));
        assert_eq!(to_number(&Value::Bool(false)), Ok(0.0));
        assert_eq!(to_number(&num(1.5)), Ok(1.5));

        // Text parsing.
        assert_eq!(to_number(&txt("1")), Ok(1.0));
        assert_eq!(to_number(&txt(" 1 ")), Ok(1.0));
        assert_eq!(to_number(&txt("1.5")), Ok(1.5));
        assert_eq!(to_number(&txt("1e2")), Ok(100.0));
        assert_eq!(to_number(&txt("-3")), Ok(-3.0));
        assert_eq!(to_number(&txt("50%")), Ok(0.5));
        assert_eq!(to_number(&txt("1,000")), Ok(1000.0));
        assert_eq!(to_number(&txt("12,345,678")), Ok(12_345_678.0));
        assert_eq!(to_number(&txt("1,000.5")), Ok(1000.5));

        // "" vs Blank: empty string is NOT a number.
        assert_eq!(to_number(&txt("")), Err(ErrorKind::Value));
        assert_eq!(to_number(&txt("abc")), Err(ErrorKind::Value));

        // Error propagates.
        assert_eq!(
            to_number(&Value::Error(ErrorKind::Div0)),
            Err(ErrorKind::Div0)
        );
    }

    // ---- Oracle-pinned text→number coercion (RUN-2026-07-11-oracle01) ----

    // OXP-002 (RUN-2026-07-11-oracle01): an overflowing numeric string is
    // #VALUE! — whether it overflows f64 ("1e999") or is a finite f64 above
    // Excel's entry limit 9.99999999999999E+307 ("1e308").
    #[test]
    fn oxp_002_numeric_overflow_is_value_error() {
        assert_eq!(to_number(&txt("1e999")), Err(ErrorKind::Value));
        assert_eq!(to_number(&txt("1e308")), Err(ErrorKind::Value));
        assert_eq!(to_number(&txt("-1e999")), Err(ErrorKind::Value));
        // Just under the entry limit still parses.
        assert_eq!(
            to_number(&txt("9.99999999999999e307")),
            Ok(9.999_999_999_999_99e307)
        );
        assert_eq!(to_number(&txt("1e307")), Ok(1e307));
    }

    // OXP-010 (RUN-2026-07-11-oracle01): a leading '$' currency prefix (with
    // a sign allowed *after* the '$') coerces; an unknown suffix is #VALUE!;
    // unprobed '$' placements defer to #UNSUPPORTED!.
    #[test]
    fn oxp_010_currency_prefix() {
        assert_eq!(to_number(&txt("$1,000")), Ok(1000.0));
        assert_eq!(to_number(&txt("$-5")), Ok(-5.0));
        // Unknown trailing suffix → #VALUE! (not a currency shape at all).
        assert_eq!(to_number(&txt("1,000 kr")), Err(ErrorKind::Value));
        // Unprobed '$' placements → #UNSUPPORTED! (never guessed).
        assert_eq!(to_number(&txt("-$5")), Err(ErrorKind::Unsupported));
        assert_eq!(to_number(&txt("$ 5")), Err(ErrorKind::Unsupported));
        assert_eq!(to_number(&txt("5$")), Err(ErrorKind::Unsupported));
        // OXP-162 (RUN-2026-07-11-oracle01): the currency compositions are now
        // pinned. Currency + exponent is ACCEPTED (`$1e2` → 100); currency +
        // percent is REJECTED as #VALUE! (`$50%`, `$5%`).
        assert_eq!(to_number(&txt("$1e2")), Ok(100.0));
        assert_eq!(to_number(&txt("$50%")), Err(ErrorKind::Value));
        assert_eq!(to_number(&txt("$5%")), Err(ErrorKind::Value));
    }

    // OXP-011/162 (RUN-2026-07-11-oracle01): a whole-string parenthesized
    // unsigned number negates; a `%` inside is now pinned (accepted, negated);
    // an exponent inside and signed/currency shapes still defer.
    #[test]
    fn oxp_011_parenthesized_negative() {
        assert_eq!(to_number(&txt("(1)")), Ok(-1.0));
        assert_eq!(to_number(&txt("(1,000)")), Ok(-1000.0));
        // OXP-162: a percent composed inside the parens is ACCEPTED and negated.
        assert_eq!(to_number(&txt("(50%)")), Ok(-0.5));
        // Signed / currency inside parens is unprobed → #UNSUPPORTED!.
        assert_eq!(to_number(&txt("(-1)")), Err(ErrorKind::Unsupported));
        assert_eq!(to_number(&txt("($1)")), Err(ErrorKind::Unsupported));
        // An exponent composed inside the parens is still unprobed →
        // #UNSUPPORTED! (not the plain parser's -100000).
        assert_eq!(to_number(&txt("(1e5)")), Err(ErrorKind::Unsupported));
    }

    // OXP-012 (RUN-2026-07-11-oracle01): a post-comma group needs ≥3 digits.
    // The fix accepts "1,0000" (previously wrongly rejected); the pinned
    // malformed groupings stay #VALUE!; exotic multi-comma shapes defer.
    #[test]
    fn oxp_012_thousands_grouping_leniency() {
        // Standard groupings still parse.
        assert_eq!(to_number(&txt("1,000")), Ok(1000.0));
        assert_eq!(to_number(&txt("12,345,678")), Ok(12_345_678.0));
        assert_eq!(to_number(&txt("1,000.5")), Ok(1000.5));
        // The fix: single-comma group of ≥3 digits is accepted.
        assert_eq!(to_number(&txt("1,0000")), Ok(10000.0));
        // Pinned rejects (a post-comma group < 3 digits) → #VALUE!.
        assert_eq!(to_number(&txt("1,00")), Err(ErrorKind::Value));
        assert_eq!(to_number(&txt("1,00,000")), Err(ErrorKind::Value));
        assert_eq!(to_number(&txt("12,34")), Err(ErrorKind::Value));
        // Exotic unprobed multi-comma (wide group + multiple commas) defers.
        assert_eq!(to_number(&txt("1,0000,000")), Err(ErrorKind::Unsupported));
    }

    // OXP-013 (RUN-2026-07-11-oracle01): ASCII space(s) between a number and a
    // trailing '%' are accepted; the adjacent form still works; an NBSP/tab
    // gap remains unprobed → #UNSUPPORTED!.
    #[test]
    fn oxp_013_percent_whitespace() {
        assert_eq!(to_number(&txt("50%")), Ok(0.5));
        assert_eq!(to_number(&txt("50 %")), Ok(0.5));
        assert_eq!(to_number(&txt("50  %")), Ok(0.5));
        // Non-ASCII-space gaps stay oracle-unknown.
        assert_eq!(to_number(&txt("50\u{00A0}%")), Err(ErrorKind::Unsupported));
        assert_eq!(to_number(&txt("50\t%")), Err(ErrorKind::Unsupported));
    }

    // OXP-160 (RUN-2026-07-11-oracle01): date/time-shaped text coerces to its
    // Excel serial — the FULL serial including any time fraction. All the
    // accepted date spellings of 2020-01-01 → 43831.
    #[test]
    fn oxp_160_date_text_spellings_all_43831() {
        for s in [
            "1/1/2020",
            "01/01/2020",
            "2020-01-01",  // ISO
            "1-Jan-2020",  // D-Mon-YYYY
            "1-Jan-20",    // D-Mon-YY (20 → 2020)
            "Jan 1, 2020", // Mon D, YYYY
            "January 1, 2020",
            "Jan 2020", // Mon YYYY → day 1
        ] {
            assert_eq!(to_number(&txt(s)), Ok(43831.0), "date {s:?}");
        }
        // A month-year with a different month resolves to its 1st.
        assert_eq!(to_number(&txt("December 2020")), Ok(44166.0));
    }

    // OXP-160: the 2-digit-year pivot (00-29 → 2000-2029, 30-99 → 1930-1999).
    #[test]
    fn oxp_160_two_digit_year_pivot() {
        assert_eq!(to_number(&txt("1/1/29")), Ok(47119.0)); // 2029-01-01
        assert_eq!(to_number(&txt("1/1/30")), Ok(10959.0)); // 1930-01-01
    }

    // OXP-160: leap-day validity — Feb 29 valid in a leap year, #VALUE! in a
    // common year; a month > 12 and a dotted (non-en-US) date are #VALUE!.
    #[test]
    fn oxp_160_leap_and_rejects() {
        assert_eq!(to_number(&txt("2/29/2020")), Ok(43890.0)); // 2020 is leap
        assert_eq!(to_number(&txt("2/29/2021")), Err(ErrorKind::Value)); // not leap
        assert_eq!(to_number(&txt("13/1/2020")), Err(ErrorKind::Value)); // month 13
        assert_eq!(to_number(&txt("1.1.2020")), Err(ErrorKind::Value)); // dotted
    }

    // OXP-160: time text → the fractional day (bare coercion keeps the time
    // fraction); a date+time keeps both parts.
    #[test]
    fn oxp_160_time_and_datetime() {
        assert_eq!(to_number(&txt("16:48")), Ok(0.7)); // 16:48 = 0.7 of a day
        assert_eq!(to_number(&txt("16:48:00")), Ok(0.7));
        assert_eq!(to_number(&txt("12:00")), Ok(0.5));
        assert_eq!(to_number(&txt("1/1/2020 16:48")), Ok(43831.7));
    }

    // OXP-160: a no-year form is clock-dependent; `to_number` is context-free
    // (no injectable clock), so it defers rather than guessing a year.
    #[test]
    fn oxp_160_no_year_form_defers() {
        assert_eq!(to_number(&txt("1/1")), Err(ErrorKind::Unsupported));
        // A shaped-but-unrecognized time (meridiem) also defers, never #VALUE!.
        assert_eq!(to_number(&txt("6:45 PM")), Err(ErrorKind::Unsupported));
    }

    // OXP-160: text that only *contains* a month-prefix word (whole-word match)
    // is not a date shape and stays plain #VALUE!; a plain number is never
    // mistaken for a date.
    #[test]
    fn oxp_160_non_date_shapes_unchanged() {
        assert_eq!(to_number(&txt("maybe1")), Err(ErrorKind::Value));
        assert_eq!(to_number(&txt("-3")), Ok(-3.0));
        assert_eq!(to_number(&txt("1e2")), Ok(100.0));
    }

    // the frozen-contract review (B1-B4): the parser is gated to the *probed*
    // forms; unprobed date shapes DEFER (#UNSUPPORTED!) rather than emit a
    // guessed serial or a definitive-wrong #VALUE!. Never guess (Principle 2).
    #[test]
    fn gates_unprobed_date_shapes_defer() {
        // B1: month-first `Mon D` (1-2-digit trailing number) is the no-year
        // form (clock-dependent), not a pivoted year → defer.
        assert_eq!(to_number(&txt("Jan 1")), Err(ErrorKind::Unsupported));
        assert_eq!(to_number(&txt("May 5")), Err(ErrorKind::Unsupported));
        assert_eq!(to_number(&txt("Jan 1, 20")), Err(ErrorKind::Unsupported));
        // B2: a 1-2-digit hyphen leader is US M-D-Y-hyphen (unprobed), not ISO.
        assert_eq!(to_number(&txt("12-25-2020")), Err(ErrorKind::Unsupported));
        assert_eq!(to_number(&txt("1-2-2020")), Err(ErrorKind::Unsupported));
        // B3: a >=3-digit first slash component is Y/M/D (unprobed) → defer
        // (distinct from the pinned 2-digit-month `"13/1/2020"` → #VALUE!).
        assert_eq!(to_number(&txt("2020/1/1")), Err(ErrorKind::Unsupported));
        assert_eq!(to_number(&txt("13/1/2020")), Err(ErrorKind::Value));
        // B4: Excel's phantom 1900-02-29 is accepted via serial math but its
        // text reading is unprobed → defer (not the proleptic #VALUE!).
        assert_eq!(to_number(&txt("2/29/1900")), Err(ErrorKind::Unsupported));
        // The pinned forms still parse (the 2-digit `D-Mon-YY` pivot is kept).
        assert_eq!(to_number(&txt("1-Jan-20")), Ok(43831.0));
        assert_eq!(to_number(&txt("2020-01-01")), Ok(43831.0));
        assert_eq!(to_number(&txt("Jan 2020")), Ok(43831.0));
    }

    // OXP-162 (RUN-2026-07-11-oracle01): the now-pinned numeric-string
    // compositions. Paren+percent negates; currency+exponent parses;
    // currency+percent rejects; a single wide comma group parses.
    #[test]
    fn oxp_162_numeric_string_compositions() {
        assert_eq!(to_number(&txt("(50%)")), Ok(-0.5));
        assert_eq!(to_number(&txt("$1e2")), Ok(100.0));
        assert_eq!(to_number(&txt("12,3456")), Ok(123_456.0));
        assert_eq!(to_number(&txt("$50%")), Err(ErrorKind::Value));
    }

    #[test]
    fn to_number_array_and_ref() {
        let one_by_one = Value::Array(Array::from_scalar(num(7.0)));
        assert_eq!(to_number(&one_by_one), Ok(7.0));

        let multi = Value::Array(Array::new(1, 2, vec![num(1.0), num(2.0)]).unwrap());
        assert_eq!(to_number(&multi), Err(ErrorKind::Unsupported));

        let r = Value::Ref(Ref::new(SheetId(0), RectRange::new(0, 0, 0, 0)));
        assert_eq!(to_number(&r), Err(ErrorKind::Unsupported));
    }

    // ---- Aggregate vs scalar coercion asymmetry --------------------------
    #[test]
    fn coercion_mode_asymmetry() {
        // SUM(TRUE,"2") → 3: direct scalar args coerce.
        assert_eq!(
            coerce_number_arg(&Value::Bool(true), CoercionMode::Scalar),
            NumericArg::Number(1.0)
        );
        assert_eq!(
            coerce_number_arg(&txt("2"), CoercionMode::Scalar),
            NumericArg::Number(2.0)
        );

        // In a range, text/logicals/blanks are ignored; errors propagate.
        assert_eq!(
            coerce_number_arg(&Value::Bool(true), CoercionMode::RangeAggregate),
            NumericArg::Skip
        );
        assert_eq!(
            coerce_number_arg(&txt("2"), CoercionMode::RangeAggregate),
            NumericArg::Skip
        );
        assert_eq!(
            coerce_number_arg(&Value::Blank, CoercionMode::RangeAggregate),
            NumericArg::Skip
        );
        assert_eq!(
            coerce_number_arg(&num(4.0), CoercionMode::RangeAggregate),
            NumericArg::Number(4.0)
        );
        assert_eq!(
            coerce_number_arg(&Value::Error(ErrorKind::Na), CoercionMode::RangeAggregate),
            NumericArg::Error(ErrorKind::Na)
        );
    }

    // ---- Text coercion ---------------------------------------------------
    #[test]
    fn to_text_documented_cases() {
        assert_eq!(to_text(&Value::Bool(true)).unwrap(), "TRUE");
        assert_eq!(to_text(&Value::Bool(false)).unwrap(), "FALSE");
        assert_eq!(to_text(&Value::Blank).unwrap(), "");
        assert_eq!(to_text(&num(0.0)).unwrap(), "0");
        assert_eq!(to_text(&num(-0.0)).unwrap(), "0");
        assert_eq!(to_text(&num(1.5)).unwrap(), "1.5");
        assert_eq!(to_text(&num(100.0)).unwrap(), "100");
        assert_eq!(to_text(&num(-3.0)).unwrap(), "-3");
        assert_eq!(to_text(&txt("hi")).unwrap(), "hi");
        assert_eq!(to_text(&Value::Error(ErrorKind::Ref)), Err(ErrorKind::Ref));

        // General switches to scientific outside base-10 exponent [-18, 19]
        // (OXP-020). 1e-5 (exponent -5) is now a plain decimal, not "1E-05".
        assert_eq!(to_text(&num(1e20)).unwrap(), "1E+20");
        assert_eq!(to_text(&num(1e-5)).unwrap(), "0.00001");

        // Extremes: 15-sig-digit rounding overflows to infinity internally
        // near f64::MAX; the formatter must fall back to the unrounded value
        // and never emit "inf"/"nan" for finite input.
        assert_eq!(to_text(&num(f64::MAX)).unwrap(), "1.7976931348623157E+308");
        assert_eq!(
            to_text(&num(-f64::MAX)).unwrap(),
            "-1.7976931348623157E+308"
        );
        for x in [f64::MAX, -f64::MAX, f64::MIN_POSITIVE, -f64::MIN_POSITIVE] {
            let t = to_text(&num(x)).unwrap();
            let lower = t.as_str().to_ascii_lowercase();
            assert!(
                !lower.contains("inf") && !lower.contains("nan"),
                "finite {x} formatted as {t:?}"
            );
        }
    }

    // OXP-020 (RUN-2026-07-11-oracle01): General uses a plain decimal for a
    // base-10 exponent in [-18, 19] and scientific outside. Exact crossovers:
    // 10^19 plain / 10^20 scientific (large), 10^-18 plain / 10^-19 scientific
    // (small).
    #[test]
    fn oxp_020_general_scientific_threshold() {
        // Large-side crossover.
        assert_eq!(to_text(&num(1e19)).unwrap(), "10000000000000000000");
        assert_eq!(to_text(&num(1e20)).unwrap(), "1E+20");
        assert_eq!(to_text(&num(1e21)).unwrap(), "1E+21");
        // Small-side crossover.
        assert_eq!(to_text(&num(1e-18)).unwrap(), "0.000000000000000001");
        assert_eq!(to_text(&num(1e-19)).unwrap(), "1E-19");
        // Interior sample that flipped scientific→plain under the old
        // (assumed ≥1e15 / <1e-4) thresholds.
        assert_eq!(to_text(&num(1e16)).unwrap(), "10000000000000000");
        assert_eq!(to_text(&num(1e-4)).unwrap(), "0.0001");
    }

    // OXP-162 (RUN-2026-07-11-oracle01): the General plain/scientific split is a
    // significant-digit *window* (10^19 .. 10^-18), not a pure magnitude band.
    // `9.9*10^19` prints plain (two sig digits, both inside the window) but
    // `1.5*10^-18` goes scientific (its 2nd digit sits at 10^-19, outside the
    // window) even though its magnitude exceeds the plain point 10^-18.
    #[test]
    fn oxp_162_general_significant_digit_window() {
        assert_eq!(
            to_text(&num(9.9 * 10f64.powi(19))).unwrap(),
            "99000000000000000000"
        );
        assert_eq!(to_text(&num(1.5 * 10f64.powi(-18))).unwrap(), "1.5E-18");
        // The plain-side crossover the window preserves: 10^-18 stays plain.
        assert_eq!(to_text(&num(1e-18)).unwrap(), "0.000000000000000001");
    }

    // OXP-021 (RUN-2026-07-11-oracle01, PROVENANCE): General rounds to 15
    // significant digits; the scientific mantissa carries those digits. This
    // confirms existing behavior — no logic changed.
    #[test]
    fn oxp_021_general_15_significant_digits() {
        assert_eq!(
            to_text(&num(1.234_567_890_123_45e20)).unwrap(),
            "1.23456789012345E+20"
        );
        assert_eq!(to_text(&num(1.0 / 3.0)).unwrap(), "0.333333333333333");
        assert_eq!(
            to_text(&num(123_456_789_012_345.0)).unwrap(),
            "123456789012345"
        );
    }

    // ---- Bool coercion ---------------------------------------------------
    #[test]
    fn to_bool_documented_cases() {
        assert_eq!(to_bool(&num(0.0)), Ok(false));
        assert_eq!(to_bool(&num(1.0)), Ok(true));
        assert_eq!(to_bool(&num(-2.0)), Ok(true));
        assert_eq!(to_bool(&Value::Blank), Ok(false));
        assert_eq!(to_bool(&txt("true")), Ok(true));
        assert_eq!(to_bool(&txt("FALSE")), Ok(false));
        assert_eq!(to_bool(&txt("TrUe")), Ok(true));
        // Numeric text is NOT a bool.
        assert_eq!(to_bool(&txt("1")), Err(ErrorKind::Value));
        assert_eq!(to_bool(&txt("yes")), Err(ErrorKind::Value));
        assert_eq!(to_bool(&Value::Error(ErrorKind::Num)), Err(ErrorKind::Num));
    }

    // ---- Comparison ------------------------------------------------------
    #[test]
    fn compare_documented_cases() {
        // Case-insensitive text equality.
        assert_eq!(compare(&txt("abc"), &txt("ABC")), Ok(Ordering::Equal));
        assert_eq!(compare(&txt("a"), &txt("b")), Ok(Ordering::Less));

        // Numeric.
        assert_eq!(compare(&num(1.0), &num(2.0)), Ok(Ordering::Less));
        assert_eq!(compare(&num(2.0), &num(2.0)), Ok(Ordering::Equal));

        // Cross-type: Number < Text < Bool.
        assert_eq!(compare(&num(9e9), &txt("a")), Ok(Ordering::Less));
        assert_eq!(compare(&txt("z"), &Value::Bool(false)), Ok(Ordering::Less));
        assert_eq!(
            compare(&Value::Bool(false), &Value::Bool(true)),
            Ok(Ordering::Less)
        );
        assert_eq!(compare(&num(1.0), &Value::Bool(false)), Ok(Ordering::Less));

        // Blank morphs: 0 vs numbers, "" vs text.
        assert_eq!(compare(&Value::Blank, &num(0.0)), Ok(Ordering::Equal));
        assert_eq!(compare(&Value::Blank, &num(1.0)), Ok(Ordering::Less));
        assert_eq!(compare(&Value::Blank, &txt("")), Ok(Ordering::Equal));
        assert_eq!(compare(&Value::Blank, &txt("a")), Ok(Ordering::Less));
        assert_eq!(compare(&Value::Blank, &Value::Blank), Ok(Ordering::Equal));

        // Error propagates leftmost.
        assert_eq!(
            compare(&Value::Error(ErrorKind::Na), &num(1.0)),
            Err(ErrorKind::Na)
        );
        assert_eq!(
            compare(&num(1.0), &Value::Error(ErrorKind::Div0)),
            Err(ErrorKind::Div0)
        );

        assert!(text_eq_ci("HeLLo", "hello"));
        assert!(!text_eq_ci("hello", "hell"));
    }

    // OXP-030 (RUN-2026-07-11-oracle01): a Blank compared with a Bool morphs
    // to Bool(false). `A1=TRUE` → FALSE, `A1=FALSE` → TRUE, `A1<TRUE` → TRUE,
    // `A1>FALSE` → FALSE (A1 empty). Previously #UNSUPPORTED!.
    #[test]
    fn oxp_030_blank_vs_bool_morphs_to_false() {
        // `values_equal` is in scope via `use super::*`.
        // A1=TRUE → FALSE
        assert_eq!(values_equal(&Value::Blank, &Value::Bool(true)), Ok(false));
        // A1=FALSE → TRUE
        assert_eq!(values_equal(&Value::Blank, &Value::Bool(false)), Ok(true));
        // A1<TRUE → TRUE  (false < true)
        assert_eq!(
            compare(&Value::Blank, &Value::Bool(true)),
            Ok(Ordering::Less)
        );
        // A1>FALSE → FALSE  (false == false, not greater)
        assert_eq!(
            compare(&Value::Blank, &Value::Bool(false)),
            Ok(Ordering::Equal)
        );
        // Symmetric orientation (Bool on the left).
        assert_eq!(
            compare(&Value::Bool(true), &Value::Blank),
            Ok(Ordering::Greater)
        );
    }

    // OXP-031 (RUN-2026-07-11-oracle01, HELD): any comparison involving
    // non-ASCII text defers to #UNSUPPORTED! — code-point order is provably
    // wrong (Excel ranks "ä" < "z", "ß" = "ss"). ASCII comparisons are
    // unaffected. Collation is deliberately NOT implemented.
    #[test]
    fn oxp_031_non_ascii_text_compare_is_deferred() {
        assert_eq!(compare(&txt("ä"), &txt("z")), Err(ErrorKind::Unsupported));
        assert_eq!(compare(&txt("ß"), &txt("ss")), Err(ErrorKind::Unsupported));
        assert_eq!(compare(&txt("z"), &txt("é")), Err(ErrorKind::Unsupported));
        // Non-ASCII vs a non-text operand also defers.
        assert_eq!(compare(&txt("ä"), &num(1.0)), Err(ErrorKind::Unsupported));
        // A pure-ASCII comparison is unaffected.
        assert_eq!(compare(&txt("abc"), &txt("ABD")), Ok(Ordering::Less));
        // An error operand still propagates ahead of the non-ASCII guard.
        assert_eq!(
            compare(&Value::Error(ErrorKind::Na), &txt("ä")),
            Err(ErrorKind::Na)
        );
    }

    // ---- Error display strings -------------------------------------------
    #[test]
    fn error_display_strings_exact() {
        assert_eq!(ErrorKind::Null.to_string(), "#NULL!");
        assert_eq!(ErrorKind::Div0.to_string(), "#DIV/0!");
        assert_eq!(ErrorKind::Value.to_string(), "#VALUE!");
        assert_eq!(ErrorKind::Ref.to_string(), "#REF!");
        assert_eq!(ErrorKind::Name.to_string(), "#NAME?");
        assert_eq!(ErrorKind::Num.to_string(), "#NUM!");
        assert_eq!(ErrorKind::Na.to_string(), "#N/A");
        assert_eq!(ErrorKind::GettingData.to_string(), "#GETTING_DATA");
        assert_eq!(ErrorKind::Spill.to_string(), "#SPILL!");
        assert_eq!(ErrorKind::Calc.to_string(), "#CALC!");
        assert_eq!(ErrorKind::Unsupported.to_string(), "#UNSUPPORTED!");
        assert_eq!(ErrorKind::Blocked.to_string(), "#BLOCKED!");
        assert_eq!(ErrorKind::Resource.to_string(), "#RESOURCE!");

        assert!(ErrorKind::Unsupported.is_recalc_sentinel());
        assert!(!ErrorKind::Div0.is_recalc_sentinel());
    }

    // ---- Value invariants ------------------------------------------------
    #[test]
    fn number_constructor_enforces_finiteness() {
        assert_eq!(Value::number(f64::NAN), Value::Error(ErrorKind::Num));
        assert_eq!(Value::number(f64::INFINITY), Value::Error(ErrorKind::Num));
        assert_eq!(
            Value::number(f64::NEG_INFINITY),
            Value::Error(ErrorKind::Num)
        );
        assert!(matches!(Value::number(3.0), Value::Number(_)));
        assert!(Value::number(f64::NAN).upholds_number_invariant());
    }

    // ---- Interner --------------------------------------------------------
    #[test]
    fn interner_dedups_equal_strings() {
        let a = Text::new("shared-string-example");
        let b = Text::new("shared-string-example");
        assert!(a.ptr_eq(&b));
        assert_eq!(a, b);

        let c = Text::new("different");
        assert!(!a.ptr_eq(&c));
        assert_ne!(a, c);
    }

    // ---- Array shape -----------------------------------------------------
    #[test]
    fn array_shape_enforced() {
        assert!(Array::new(2, 2, vec![num(1.0), num(2.0), num(3.0), num(4.0)]).is_ok());
        assert_eq!(
            Array::new(2, 2, vec![num(1.0)]),
            Err(ShapeError::LengthMismatch {
                rows: 2,
                cols: 2,
                len: 1
            })
        );
        assert_eq!(Array::new(0, 3, vec![]), Err(ShapeError::Empty));

        let a = Array::new(1, 3, vec![num(1.0), num(2.0), num(3.0)]).unwrap();
        assert_eq!(a.get(0, 2), Some(&num(3.0)));
        assert_eq!(a.get(0, 3), None);
        assert_eq!(a.get(1, 0), None);
        assert!(a.as_scalar().is_none());
        assert_eq!(Array::from_scalar(num(5.0)).as_scalar(), Some(&num(5.0)));
    }

    // ---- Value::Lambda is born refusing (RFC-0012 §2, BC-2/BC-3/BC-6) -----

    fn a_ref() -> Value {
        Value::Ref(Ref::new(SheetId(0), RectRange::new(0, 0, 0, 0)))
    }

    #[test]
    fn lambda_coercions_refuse_unsupported() {
        let lam = Value::test_lambda();
        assert_eq!(to_number(&lam), Err(ErrorKind::Unsupported));
        assert_eq!(to_text(&lam), Err(ErrorKind::Unsupported));
        assert_eq!(to_bool(&lam), Err(ErrorKind::Unsupported));
        assert_eq!(
            coerce_number_arg(&lam, CoercionMode::Scalar),
            NumericArg::Error(ErrorKind::Unsupported)
        );
        assert_eq!(
            coerce_number_arg(&lam, CoercionMode::RangeAggregate),
            NumericArg::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn lambda_comparison_refuses_rider_ii() {
        // Rider (ii): `=`/`<>`/ordering on lambdas must keep REFUSING until an
        // OXP pins Excel's behavior — never define equality from memory.
        let lam = Value::test_lambda();
        assert_eq!(compare(&lam, &num(1.0)), Err(ErrorKind::Unsupported));
        assert_eq!(compare(&num(1.0), &lam), Err(ErrorKind::Unsupported));
        assert_eq!(compare(&lam, &txt("x")), Err(ErrorKind::Unsupported));
        assert_eq!(compare_fuzzy(&lam, &lam), Err(ErrorKind::Unsupported));
        assert_eq!(values_equal(&lam, &num(0.0)), Err(ErrorKind::Unsupported));
    }

    #[test]
    fn lambda_total_order_is_bucketed_not_equal() {
        // Rider (i): a lambda sorts into the highest bucket, so it orders
        // AFTER every other variant deterministically (never pointer-derived,
        // never `Equal` to a non-lambda). Two lambdas never reach `total_order`
        // (consumers refuse upstream; that arm is a guarded-unreachable), so we
        // deliberately do not compare two lambdas here.
        let lam = Value::test_lambda();
        for other in [
            Value::Blank,
            num(1.0),
            txt("x"),
            Value::Bool(true),
            Value::Error(ErrorKind::Value),
            Value::Array(Array::from_scalar(num(1.0))),
            a_ref(),
        ] {
            assert_eq!(total_order(&lam, &other), Ordering::Greater);
            assert_eq!(total_order(&other, &lam), Ordering::Less);
        }
    }

    #[test]
    fn lambda_partial_eq_is_pointer_identity() {
        // `PartialEq` is `Arc::ptr_eq` — a totality crutch, NOT `=` semantics.
        let lam = Value::test_lambda();
        assert_eq!(lam, lam.clone()); // same Arc → equal
        assert_ne!(lam, Value::test_lambda()); // distinct Arc → unequal
    }

    #[test]
    fn lambda_upholds_number_invariant() {
        // A lambda holds no f64, so the NaN/Inf invariant is vacuously upheld.
        assert!(Value::test_lambda().upholds_number_invariant());
    }

    #[test]
    fn witness_every_value_variant() {
        // BC-6 witness (RFC-0012 §4, Part E item 4): one representative of
        // EVERY `Value` variant. The inner `match` has NO `_` arm, so adding a
        // variant fails to compile here — a forcing function to extend the
        // born-refusing audit. If this array's length no longer equals the
        // arm count, the next variant tripped it.
        let variants = [
            num(1.0),
            txt("x"),
            Value::Bool(true),
            Value::Error(ErrorKind::Value),
            Value::Blank,
            Value::Array(Array::from_scalar(num(1.0))),
            a_ref(),
            Value::test_lambda(),
        ];
        for v in &variants {
            match v {
                Value::Number(_)
                | Value::Text(_)
                | Value::Bool(_)
                | Value::Error(_)
                | Value::Blank
                | Value::Array(_)
                | Value::Ref(_)
                | Value::Lambda(_) => {}
            }
        }
        assert_eq!(variants.len(), 8, "one representative per Value variant");
    }
}

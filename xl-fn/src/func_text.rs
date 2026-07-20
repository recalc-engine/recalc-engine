//! `TEXT` — formats a numeric `value` as text per a number-format code.
//!
//! # Provenance
//! Behavior contract: `docs/specs/TEXT.md`, citing the Microsoft Learn
//! `TEXT function` page and the `Number format codes` page (both linked
//! there). Numeric coercion of `value` is deferred to `xl-value`'s
//! [`to_number`]; this module never re-implements number parsing.
//!
//! # Scope — deliberately SMALL, pinned subsets (TEXT.md §Supported)
//! Excel's number-format mini-language is huge (date/time tokens, scientific,
//! fractions, `@` text, repeated/escaped characters, `[color]`/`[condition]`
//! brackets, multi-section `;` formats, currency symbols, `?`/`#`-in-fraction
//! alignment placeholders). Per the "never silently wrong" principle, this
//! implementation supports only small, fully-pinnable grammars and returns
//! `#UNSUPPORTED!` for **every** format code outside them — a distinguishable
//! defer beats a plausible-but-wrong string. Two independent slices are
//! implemented, tried in order — the numeric digit/percent subset first, then
//! the **date/time-token slice** (OXP-022/OXP-164, see the module section
//! [`parse_datetime_format`] / [`render_datetime`] below); a `format_text`
//! matching neither defers.
//!
//! Supported numeric `format_text` grammar (a single section — no `;`), built
//! only from `0`, `#`, `,`, `.`, `%`:
//! - **Integer placeholders**: a run of `0`/`#`, with `,` acting purely as a
//!   thousands-grouping flag *only* when it sits **between** two digit
//!   placeholders (`#,##0`). The run must contain at least one `0`.
//! - **Optional fraction**: a single `.` followed by one-or-more `0` (only
//!   `0` — a `#` or `?` in the fraction is deferred). The count of `0`s is the
//!   decimal-place count.
//! - **Optional trailing `%`**: each trailing `%` scales the value by 100 and
//!   is echoed literally (`0.0%`: `0.125` -> `"12.5%"`).
//!
//! Formatting rules (TEXT.md §Supported):
//! - Round **half away from zero** to the decimal-place count (mirrors
//!   `ROUND`/`f64::round`; the binary-vs-decimal half-way ambiguity of
//!   `OXP-095`/`OXP-021` is inherited but not separately re-litigated here —
//!   the subset's pinned examples are exact).
//! - Pad the integer part with leading zeros to the count of `0`s.
//! - Group thousands with `,` iff the format's integer part carries a
//!   between-digits comma.
//! - A negative value gets a leading `-` (unless it rounds to zero).
//!
//! # Resolved by RUN-2026-07-11-oracle01
//! - **OXP-023** — a `Text` `value` under a numeric format now **coerces**:
//!   the string is read as an en-US number via `to_number` and then formatted
//!   (`TEXT("1234.5","0.00")` -> `"1234.50"`, `TEXT("1,234","#,##0")` ->
//!   `"1,234"`, `TEXT("  5 ","0")` -> `"5"`, `TEXT("1e3","0.0")` ->
//!   `"1000.0"`). Non-numeric text takes `to_number`'s `#VALUE!`.
//! - **OXP-024** — a non-`Text` `format_text` is resolved by type: a `Number`
//!   coerces to its General text form and is used as the format code
//!   (`TEXT(5,0)` -> `"5"`); a `Bool` is `#VALUE!` (`TEXT(5,TRUE)`); a `Blank`
//!   yields `""` (`TEXT(5,A1)` with `A1` empty). `Array`/`Ref` stay deferred.
//!
//! # Resolved by RUN-2026-07-11-oracle01 (OXP-164) — the date/time-token slice
//! - **OXP-022 (date/time subset) — RESOLVED**. A tech-lead review approved
//!   building exactly the date/time-token slice that covers the bulk of real
//!   corpus TEXT usage; RUN-2026-07-11-oracle01 pinned it as OXP-164 (128
//!   probes crossing every in-scope token with the phantom-leap edge serials).
//!   A `format_text` that is entirely in-scope date/time tokens + pinned
//!   separators renders exactly (see [`parse_datetime_format`]). IN tokens:
//!   `yyyy`/`yy` (year), `mmmm`/`mmm`/`mm`/`m` (month), `dddd`/`ddd`/`dd`/`d`
//!   (weekday name / day-of-month), `hh`/`h` (24-h, or 12-h beside an
//!   `AM/PM`), `ss`/`s`, `mm`/`m` **as minute** when adjacent to `h` (before)
//!   or `s` (after), and `AM/PM` / `A/P` (12-hour). Literal separators inside
//!   a date/time format: `/`, `-`, `:`, space, and **comma-as-literal**. Codes
//!   are case-insensitive (`M/D/YYYY`, `DD-MMM-YY`).
//!
//! # Deferred to `#UNSUPPORTED!` — not guessed (TEXT.md §Deferred)
//! - **OXP-022 (everything but the date/time slice)** — any `format_text`
//!   outside both grammars: `@` text, `#`/`?` in the fraction, `E`/scientific,
//!   fractions (`/` as fraction, `?/?`, `#/#`), multi-section `;` formats,
//!   `[color]`/`[condition]`/`[$-409]`/elapsed `[h]` brackets, currency
//!   symbols (`$` etc.), escaped/quoted literals (`\`, `"`), `*` fill, `_`
//!   skip, `General`, a `#`-only integer with no `0`, or a scaling/trailing
//!   comma. A date/time format is also deferred if it contains **any** token
//!   outside the pinned in-scope set (defer-on-first-unpinned), so an unprobed
//!   construct never yields a guessed string. Detected by parsing and
//!   deferring on the first out-of-grammar construct.
//!
//! An error-valued `value` or `format_text` argument propagates unchanged.

use xl_value::{ErrorKind, Value, to_number, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::datecore::{DateError, DateSystem, serial_to_ymd};

/// Evaluate a `TEXT(value, format_text)` call. See the module docs.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let value = args.eval_scalar(0);
    // An error `value` propagates (checked before touching `format_text`).
    if let Value::Error(k) = value {
        return Value::Error(k);
    }

    // Resolve `format_text` to a format-code string.
    //
    // OXP-024 (RUN-2026-07-11-oracle01) pins the non-text cases:
    // - `Number` → its General text representation, used as the format code
    //   (`TEXT(5,0)` -> `"5"`: the number `0` coerces to the code `"0"`).
    // - `Bool`   → `#VALUE!` (`TEXT(5,TRUE)` -> `#VALUE!`).
    // - `Blank`  → an empty format, which yields `""` (`TEXT(5,A1)` with `A1`
    //   empty -> `""`).
    // `Array`/`Ref` remain oracle-unknown -> defer.
    let fmt_text = match args.eval_scalar(1) {
        Value::Text(t) => t,
        // Error `format_text` propagates.
        Value::Error(k) => return Value::Error(k),
        // OXP-024: a numeric `format_text` coerces to its General text form.
        Value::Number(n) => match to_text(&Value::Number(n)) {
            Ok(t) => t,
            Err(k) => return Value::Error(k),
        },
        // OXP-024: a logical `format_text` is `#VALUE!`.
        Value::Bool(_) => return Value::Error(ErrorKind::Value),
        // OXP-024: a blank `format_text` yields the empty string.
        Value::Blank => return Value::text(""),
        // BC-6 (RFC-0012): a lambda `format_text` is refused explicitly (it was
        // already `#UNSUPPORTED!` via the wildcard; now it is a decision).
        Value::Lambda(_) => return Value::Error(ErrorKind::Unsupported),
        _ => return Value::Error(ErrorKind::Unsupported),
    };

    // OXP-023 (RUN-2026-07-11-oracle01): a text `value` coerces to a number
    // via the standard en-US text→number rules (`to_number`) and then formats
    // (`TEXT("1234.5","0.00")` -> `"1234.50"`, `TEXT("1,234","#,##0")` ->
    // `"1,234"`, `TEXT("  5 ","0")` -> `"5"`, `TEXT("1e3","0.0")` ->
    // `"1000.0"`). Non-numeric text takes `to_number`'s `#VALUE!` (or the
    // `#UNSUPPORTED!` it already flags for overflow / oracle-unknown sub-cases).
    // The coercion is shared by both slices; it is only performed once a format
    // has matched, so an out-of-subset `format_text` still defers regardless of
    // the value.

    // Slice 1 — the numeric digit/percent subset (unchanged). Any out-of-grammar
    // construct (incl. any date/time letter) makes this return `None`.
    if let Some(fmt) = parse_numeric_format(fmt_text.as_str()) {
        let n = match to_number(&value) {
            Ok(n) => n,
            Err(k) => return Value::Error(k),
        };
        return match format_number(n, &fmt) {
            Some(s) => Value::text(&s),
            // Non-finite (overflow/NaN) after scaling — defer rather than emit
            // a bogus string.
            None => Value::Error(ErrorKind::Unsupported),
        };
    }

    // Slice 2 — the date/time-token slice (OXP-022/OXP-164). Tried only after
    // the numeric subset declines; the two grammars are disjoint (date/time
    // formats always carry a `y`/`m`/`d`/`h`/`s`/`AM/PM` letter token, which the
    // numeric parser rejects, and numeric formats carry only `0`/`#`/`,`/`.`/`%`,
    // which the date tokenizer rejects). Any token outside the pinned in-scope
    // set makes `parse_datetime_format` return `None` (defer-on-first-unpinned).
    if let Some(tokens) = parse_datetime_format(fmt_text.as_str()) {
        let n = match to_number(&value) {
            Ok(n) => n,
            Err(k) => return Value::Error(k),
        };
        return match render_datetime(n, &tokens, ctx.date_system()) {
            Some(s) => Value::text(&s),
            // An out-of-range / non-finite serial (unprobed) defers rather than
            // guessing a string.
            None => Value::Error(ErrorKind::Unsupported),
        };
    }

    Value::Error(ErrorKind::Unsupported)
}

/// A parsed member of the supported numeric-format subset.
struct NumFormat {
    /// Minimum integer digits (count of `0` placeholders in the integer part).
    min_int_digits: usize,
    /// Decimal places (count of `0` placeholders after the `.`).
    decimals: usize,
    /// Group the integer part in thousands (a between-digits `,` was present).
    grouping: bool,
    /// Number of trailing `%` signs (each scales the value by 100).
    percent_pow: u32,
}

/// Parses `fmt` against the supported subset grammar, or `None` (defer) for
/// anything outside it. See the module docs for the exact grammar.
fn parse_numeric_format(fmt: &str) -> Option<NumFormat> {
    let mut chars: Vec<char> = fmt.chars().collect();
    if chars.is_empty() {
        return None;
    }

    // Strip a trailing run of `%` (each scales by 100). A `%` anywhere else
    // survives into the allowed-char check below and forces a defer.
    let mut percent_pow = 0u32;
    while matches!(chars.last(), Some('%')) {
        chars.pop();
        percent_pow += 1;
    }
    if chars.is_empty() {
        return None;
    }

    // Only `0 # , .` may remain — this single check defers date/time codes,
    // `@`, `$`, `E`, `?`, `/`, `;`, `[`/`]`, `\`, `"`, whitespace, an explicit
    // negative section, etc.
    if chars.iter().any(|&c| !matches!(c, '0' | '#' | ',' | '.')) {
        return None;
    }

    // At most one decimal point.
    let dot_positions: Vec<usize> = chars
        .iter()
        .enumerate()
        .filter(|&(_, &c)| c == '.')
        .map(|(i, _)| i)
        .collect();
    if dot_positions.len() > 1 {
        return None;
    }

    let (int_chars, frac_chars): (&[char], &[char]) = match dot_positions.first() {
        Some(&pos) => (&chars[..pos], &chars[pos + 1..]),
        None => (&chars[..], &[]),
    };

    // Fraction (if a `.` was present): one-or-more `0`, nothing else. A `#` or
    // `?` in the fraction is deferred (OXP-022).
    let has_dot = !dot_positions.is_empty();
    if has_dot && (frac_chars.is_empty() || frac_chars.iter().any(|&c| c != '0')) {
        return None;
    }
    let decimals = frac_chars.len();

    // Integer part: `0`/`#`/`,`, with at least one `0`.
    if !int_chars.contains(&'0') {
        return None;
    }
    let mut min_int_digits = 0usize;
    let mut grouping = false;
    for (i, &c) in int_chars.iter().enumerate() {
        match c {
            '0' => min_int_digits += 1,
            '#' => {}
            ',' => {
                // A grouping comma sits between two digit placeholders. A
                // leading comma (no digit before) or a trailing/scaling comma
                // (no digit after) is deferred (OXP-022).
                let before = int_chars[..i].iter().any(|&d| d == '0' || d == '#');
                let after = int_chars[i + 1..].iter().any(|&d| d == '0' || d == '#');
                if before && after {
                    grouping = true;
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }

    Some(NumFormat {
        min_int_digits,
        decimals,
        grouping,
        percent_pow,
    })
}

/// Renders `value` per `fmt`, or `None` if the (scaled) value is non-finite.
fn format_number(mut value: f64, fmt: &NumFormat) -> Option<String> {
    for _ in 0..fmt.percent_pow {
        value *= 100.0;
    }
    if !value.is_finite() {
        return None;
    }

    let negative = value < 0.0;
    let abs = value.abs();

    let scale = 10f64.powi(i32::try_from(fmt.decimals).ok()?);
    if !scale.is_finite() {
        return None;
    }
    // Round half away from zero (`f64::round`'s documented behavior).
    let scaled = (abs * scale).round();
    if !scaled.is_finite() {
        return None;
    }

    // All integer digits (integer part concatenated with the fraction digits),
    // in fixed notation — Rust's `{:.0}` never uses scientific form.
    let mut digits = format!("{scaled:.0}");
    // Pad on the left so there are at least `min_int_digits` integer digits
    // (leading-zero padding) plus the full fraction width.
    let min_len = fmt.min_int_digits + fmt.decimals;
    while digits.len() < min_len {
        digits.insert(0, '0');
    }
    let split = digits.len() - fmt.decimals;
    let int_part = &digits[..split];
    let frac_part = &digits[split..];

    let int_rendered = if fmt.grouping {
        group_thousands(int_part)
    } else {
        int_part.to_string()
    };

    let mut out = String::new();
    if negative && scaled != 0.0 {
        out.push('-');
    }
    out.push_str(&int_rendered);
    if fmt.decimals > 0 {
        out.push('.');
        out.push_str(frac_part);
    }
    for _ in 0..fmt.percent_pow {
        out.push('%');
    }
    Some(out)
}

/// Inserts `,` thousands separators into an all-ASCII-digit integer string.
fn group_thousands(digits: &str) -> String {
    let n = digits.len();
    let mut out = String::with_capacity(n + n / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

// =========================================================================
// Date/time-token format slice — OXP-022 / OXP-164 (RUN-2026-07-11-oracle01)
// =========================================================================
//
// A tech-lead review approved building exactly the date/time-token subset that
// covers the bulk of real corpus TEXT usage; RUN-2026-07-11-oracle01 pinned it
// as OXP-164 (128 probes crossing every in-scope token with the phantom-leap
// edge serials 0/1/59/60/61/43831/2958465 and the time fractions
// 0.5/0.7/0.99999/43831.7/60.5). Every observed string is matched byte-for-byte
// by the tests below.
//
// Design: the format string is tokenized (`parse_datetime_format`); if **any**
// character is outside the pinned in-scope set the whole format defers to
// `#UNSUPPORTED!` (defer-on-first-unpinned, mirroring the numeric path), so an
// unprobed construct never yields a guessed string. An in-scope format is then
// rendered (`render_datetime`) by decomposing the serial via `datecore`
// (`serial_to_ymd`, the 1900 phantom-leap seam at serial 60) and the same
// `round(fraction * 86400)` whole-seconds time extraction the `HOUR`/`MINUTE`/
// `SECOND` functions use (RUN-2026-07-11-oracle01, OXP-133).

/// English month names, indexed by `month - 1` (`mmmm`).
const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
/// English 3-letter month abbreviations, indexed by `month - 1` (`mmm`).
const MONTH_ABBR: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
/// English weekday names, indexed **Sunday = 0 .. Saturday = 6** (`dddd`).
const WEEKDAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
/// English 3-letter weekday abbreviations, Sunday = 0 (`ddd`).
const WEEKDAY_ABBR: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// A token in a parsed in-scope date/time `format_text`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DateToken {
    /// `yyyy` — 4-digit year.
    Year4,
    /// `yy` — 2-digit (zero-padded) year.
    Year2,
    /// `mmmm` — full month name.
    MonthName,
    /// `mmm` — 3-letter month abbreviation.
    MonthAbbr,
    /// A run of one (`m`) or two (`mm`) `m`, whose month-vs-minute role is
    /// resolved by adjacency in [`resolve_month_minute`]. `pad` = the run was
    /// two chars (`mm`) and so zero-pads to two digits.
    MonthOrMinute { pad: bool },
    /// A resolved MONTH number: `pad` = two-digit (`mm`) vs bare (`m`).
    MonthNum { pad: bool },
    /// A resolved MINUTE number: `pad` = two-digit (`mm`) vs bare (`m`).
    Minute { pad: bool },
    /// `dddd` — full weekday name.
    WeekdayName,
    /// `ddd` — 3-letter weekday abbreviation.
    WeekdayAbbr,
    /// Day-of-month: `pad` = two-digit (`dd`) vs bare (`d`).
    Day { pad: bool },
    /// Hour: `pad` = two-digit (`hh`) vs bare (`h`). 24-hour unless an `AmPm`
    /// token is present anywhere in the format, in which case 12-hour.
    Hour { pad: bool },
    /// Second: `pad` = two-digit (`ss`) vs bare (`s`).
    Second { pad: bool },
    /// A 12-hour meridiem indicator (`AM/PM` or `A/P`). `am`/`pm` carry the
    /// exact literal text (case echoed from the format) to emit for each half.
    AmPm { am: String, pm: String },
    /// A literal separator: one of `/`, `-`, `:`, space, or comma-as-literal.
    Literal(char),
}

impl DateToken {
    /// Whether this token carries a date/time value (vs a literal separator).
    /// Used both by the minute/month adjacency scan and the "a date format must
    /// contain at least one value token" guard.
    fn is_value(&self) -> bool {
        !matches!(self, DateToken::Literal(_))
    }
}

/// Length of the run of `lc` (compared case-insensitively) starting at `i`.
fn run_len(chars: &[char], i: usize, lc: char) -> usize {
    let mut j = i;
    while j < chars.len() && chars[j].to_ascii_lowercase() == lc {
        j += 1;
    }
    j - i
}

/// Whether `chars[i..]` begins with `pat` (an already-lowercase ASCII pattern),
/// compared case-insensitively.
fn matches_ci(chars: &[char], i: usize, pat: &str) -> bool {
    let pat: Vec<char> = pat.chars().collect();
    if i + pat.len() > chars.len() {
        return false;
    }
    pat.iter()
        .enumerate()
        .all(|(k, &p)| chars[i + k].to_ascii_lowercase() == p)
}

/// Parse an in-scope date/time `format_text` into tokens, or `None` (defer) for
/// anything carrying an out-of-scope construct. Case-insensitive. Recognizes
/// runs of `y`/`m`/`d`/`h`/`s`, the `AM/PM` and `A/P` meridiem tokens, and the
/// literal separators `/ - : space ,`; the month-vs-minute role of each `m` run
/// is resolved afterwards by [`resolve_month_minute`]. A format with no
/// value-bearing token (all separators) also defers — nothing pinned it.
fn parse_datetime_format(fmt: &str) -> Option<Vec<DateToken>> {
    let chars: Vec<char> = fmt.chars().collect();
    if chars.is_empty() {
        return None;
    }

    let mut tokens: Vec<DateToken> = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c.to_ascii_lowercase() {
            'y' => {
                let len = run_len(&chars, i, 'y');
                tokens.push(match len {
                    2 => DateToken::Year2,
                    4 => DateToken::Year4,
                    // `y`, `yyy`, `yyyyy`… are unprobed — defer.
                    _ => return None,
                });
                i += len;
            }
            'm' => {
                let len = run_len(&chars, i, 'm');
                tokens.push(match len {
                    1 => DateToken::MonthOrMinute { pad: false },
                    2 => DateToken::MonthOrMinute { pad: true },
                    3 => DateToken::MonthAbbr,
                    4 => DateToken::MonthName,
                    // `mmmmm` (single-letter month) is unprobed — defer.
                    _ => return None,
                });
                i += len;
            }
            'd' => {
                let len = run_len(&chars, i, 'd');
                tokens.push(match len {
                    1 => DateToken::Day { pad: false },
                    2 => DateToken::Day { pad: true },
                    3 => DateToken::WeekdayAbbr,
                    4 => DateToken::WeekdayName,
                    _ => return None,
                });
                i += len;
            }
            'h' => {
                let len = run_len(&chars, i, 'h');
                tokens.push(match len {
                    1 => DateToken::Hour { pad: false },
                    2 => DateToken::Hour { pad: true },
                    // `hhh`… and elapsed `[h]` are unprobed — defer.
                    _ => return None,
                });
                i += len;
            }
            's' => {
                let len = run_len(&chars, i, 's');
                tokens.push(match len {
                    1 => DateToken::Second { pad: false },
                    2 => DateToken::Second { pad: true },
                    _ => return None,
                });
                i += len;
            }
            'a' => {
                // Meridiem indicator: `AM/PM` (5 chars) or `A/P` (3 chars),
                // echoing the exact case of the literal for each half.
                if matches_ci(&chars, i, "am/pm") {
                    let am: String = chars[i..i + 2].iter().collect();
                    let pm: String = chars[i + 3..i + 5].iter().collect();
                    tokens.push(DateToken::AmPm { am, pm });
                    i += 5;
                } else if matches_ci(&chars, i, "a/p") {
                    let am: String = chars[i..i + 1].iter().collect();
                    let pm: String = chars[i + 2..i + 3].iter().collect();
                    tokens.push(DateToken::AmPm { am, pm });
                    i += 3;
                } else {
                    // A lone `a`/`p` outside the meridiem tokens is unprobed.
                    return None;
                }
            }
            '/' | '-' | ':' | ' ' | ',' => {
                tokens.push(DateToken::Literal(c));
                i += 1;
            }
            // Everything else (`0 # . % [ ] $ E @ * _ ? " \ ; p` …) is either a
            // numeric-subset char (handled by slice 1) or unprobed — defer.
            _ => return None,
        }
    }

    // A pure run of separators was never pinned — defer rather than emit it.
    if !tokens.iter().any(DateToken::is_value) {
        return None;
    }
    resolve_month_minute(&mut tokens);
    Some(tokens)
}

/// Resolve each unresolved `m` run to MONTH or MINUTE by adjacency.
///
/// The crux of the date/time slice (OXP-164 sections C): `m`/`mm` is a MINUTE
/// when it is adjacent to an hour token *before* it or a second token *after*
/// it (skipping intervening literal separators), and a MONTH otherwise. Pinned:
/// `"h:mm"` renders `mm` as minute (`0.5` → `"12:00"`) while `"mm/dd"` renders
/// `mm` as month (`60` → `"02/29"`); `"m/d h:mm"` and `"mm/dd hh:mm"` carry both
/// roles in one string. The "second after" arm is per the reviewed scope (the
/// probes pinned the "hour before" and "month" arms directly).
fn resolve_month_minute(tokens: &mut [DateToken]) {
    let n = tokens.len();
    for idx in 0..n {
        let pad = match tokens[idx] {
            DateToken::MonthOrMinute { pad } => pad,
            _ => continue,
        };
        // Nearest value token before / after, skipping literal separators.
        let prev_is_hour = (0..idx)
            .rev()
            .map(|j| &tokens[j])
            .find(|t| t.is_value())
            .is_some_and(|t| matches!(t, DateToken::Hour { .. }));
        let next_is_second = ((idx + 1)..n)
            .map(|j| &tokens[j])
            .find(|t| t.is_value())
            .is_some_and(|t| matches!(t, DateToken::Second { .. }));
        tokens[idx] = if prev_is_hour || next_is_second {
            DateToken::Minute { pad }
        } else {
            DateToken::MonthNum { pad }
        };
    }
}

/// Render `serial` per a parsed, in-scope date/time token list, or `None`
/// (defer) if the serial's date is out of the representable range (unprobed).
///
/// The integer part is the date (decomposed via `datecore::serial_to_ymd`, with
/// the 1900-system serial 0 mapped to the pinned "January 0, 1900" triple
/// `(1900, 1, 0)`); the fractional part is the time, converted to whole seconds
/// as `round(fraction * 86400)` — the same second-rounding the time functions
/// use (OXP-133) — before extracting hour/minute/second. The weekday is derived
/// from the raw serial (`(serial - 1) mod 7`, Sunday-anchored), which is correct
/// across the phantom day because Excel's serials advance one weekday each,
/// regardless of the displayed calendar label (mirrors `WEEKDAY`).
fn render_datetime(serial: f64, tokens: &[DateToken], system: DateSystem) -> Option<String> {
    if !serial.is_finite() {
        return None;
    }

    let mut day_serial = serial.floor() as i64;
    let frac = serial - serial.floor(); // in [0, 1)
    let mut total_secs = (frac * 86_400.0).round() as i64;
    // A time that rounds up to a full day rolls into the next midnight (unprobed
    // but the calendar-correct extension; keeps the hour in 0..=23).
    if total_secs >= 86_400 {
        total_secs -= 86_400;
        day_serial += 1;
    }
    let hour24 = total_secs / 3600;
    let minute = (total_secs % 3600) / 60;
    let second = total_secs % 60;

    // Calendar parts. Serial 0 in the 1900 system is Excel's "January 0, 1900"
    // (pinned by OXP-090 / OXP-164 H110 as month 1, day 0, year 1900); every
    // other serial resolves through `serial_to_ymd`, and an out-of-range serial
    // defers.
    let (year, month, day) = match serial_to_ymd(day_serial, system) {
        Ok(ymd) => ymd,
        Err(DateError::JanuaryZero) => (1900, 1, 0),
        Err(DateError::OutOfRange) => return None,
    };
    // Sunday-anchored weekday index (0 = Sunday), from the raw serial.
    let weekday = (day_serial - 1).rem_euclid(7) as usize;

    let ampm_present = tokens.iter().any(|t| matches!(t, DateToken::AmPm { .. }));
    // Hour value actually displayed: 12-hour (1..=12) when a meridiem token is
    // present, else 24-hour (0..=23).
    let hour_display = if ampm_present {
        let h = hour24 % 12;
        if h == 0 { 12 } else { h }
    } else {
        hour24
    };

    let mut out = String::new();
    for token in tokens {
        match token {
            DateToken::Year4 => out.push_str(&format!("{year:04}")),
            DateToken::Year2 => out.push_str(&format!("{:02}", year.rem_euclid(100))),
            DateToken::MonthName => out.push_str(MONTH_NAMES[(month - 1) as usize]),
            DateToken::MonthAbbr => out.push_str(MONTH_ABBR[(month - 1) as usize]),
            DateToken::MonthNum { pad: true } => out.push_str(&format!("{month:02}")),
            DateToken::MonthNum { pad: false } => out.push_str(&format!("{month}")),
            DateToken::Minute { pad: true } => out.push_str(&format!("{minute:02}")),
            DateToken::Minute { pad: false } => out.push_str(&format!("{minute}")),
            DateToken::WeekdayName => out.push_str(WEEKDAY_NAMES[weekday]),
            DateToken::WeekdayAbbr => out.push_str(WEEKDAY_ABBR[weekday]),
            DateToken::Day { pad: true } => out.push_str(&format!("{day:02}")),
            DateToken::Day { pad: false } => out.push_str(&format!("{day}")),
            DateToken::Hour { pad: true } => out.push_str(&format!("{hour_display:02}")),
            DateToken::Hour { pad: false } => out.push_str(&format!("{hour_display}")),
            DateToken::Second { pad: true } => out.push_str(&format!("{second:02}")),
            DateToken::Second { pad: false } => out.push_str(&format!("{second}")),
            DateToken::AmPm { am, pm } => {
                out.push_str(if hour24 < 12 { am } else { pm });
            }
            DateToken::Literal(c) => out.push(*c),
            // Unreachable after `resolve_month_minute` (every `m` run is
            // resolved to MonthNum/Minute there), but kept total: render as
            // month, matching the default resolution.
            DateToken::MonthOrMinute { pad: true } => out.push_str(&format!("{month:02}")),
            DateToken::MonthOrMinute { pad: false } => out.push_str(&format!("{month}")),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    // --- Supported subset ---------------------------------------------------

    #[test]
    fn decimal_two_places_rounds_half_away() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1234.567)), Scalar(txt("0.00"))]),
            txt("1234.57")
        );
    }

    #[test]
    fn grouped_integer_rounds_to_zero_places() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1234.5)), Scalar(txt("#,##0"))]),
            txt("1,235")
        );
    }

    #[test]
    fn percent_one_place() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.125)), Scalar(txt("0.0%"))]),
            txt("12.5%")
        );
    }

    #[test]
    fn negative_gets_leading_minus() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-5.0)), Scalar(txt("0.00"))]),
            txt("-5.00")
        );
    }

    #[test]
    fn plain_integer_format() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(42.0)), Scalar(txt("0"))]),
            txt("42")
        );
    }

    #[test]
    fn leading_zero_padding() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(5.0)), Scalar(txt("000"))]),
            txt("005")
        );
    }

    #[test]
    fn grouped_with_two_decimals() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1234.567)), Scalar(txt("#,##0.00"))]),
            txt("1,234.57")
        );
    }

    #[test]
    fn three_decimals() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.0)), Scalar(txt("0.000"))]),
            txt("2.000")
        );
    }

    #[test]
    fn value_rounding_up_to_zero_has_no_minus() {
        // -0.001 -> "0.00", not "-0.00".
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-0.001)), Scalar(txt("0.00"))]),
            txt("0.00")
        );
    }

    #[test]
    fn million_grouping() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1234567.0)), Scalar(txt("#,##0"))]),
            txt("1,234,567")
        );
    }

    #[test]
    fn blank_value_is_zero() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Blank), Scalar(txt("0.00"))]),
            txt("0.00")
        );
    }

    #[test]
    fn bool_value_coerces() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Bool(true)), Scalar(txt("0"))]),
            txt("1")
        );
    }

    // --- Deferred (#UNSUPPORTED!) ------------------------------------------

    #[test]
    fn currency_format_is_deferred() {
        // OXP-022: a `$` currency symbol is outside the subset.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1234.567)), Scalar(txt("$#,##0.00"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn at_text_format_is_deferred() {
        // OXP-022: the `@` text placeholder is outside the subset.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0)), Scalar(txt("@"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn hash_in_fraction_is_deferred() {
        // OXP-022: `#` in the fraction is outside the subset.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.5)), Scalar(txt("0.0#"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn question_placeholder_is_deferred() {
        // OXP-022: the `?` alignment placeholder is outside the subset.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.5)), Scalar(txt("0.??"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn scientific_format_is_deferred() {
        // OXP-022: scientific `E` notation is outside the subset.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(12345.0)), Scalar(txt("0.00E+00"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn multi_section_format_is_deferred() {
        // OXP-022: `;`-separated positive/negative sections are outside the
        // subset (an explicit negative section is deferred).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-5.0)), Scalar(txt("0.00;(0.00)"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn hash_only_integer_is_deferred() {
        // OXP-022: an integer part with no `0` (the empty-when-zero edge) is
        // deferred.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1234.0)), Scalar(txt("#,###"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn trailing_scaling_comma_is_deferred() {
        // OXP-022: a trailing comma scales by 1000 — deferred.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1234000.0)), Scalar(txt("0,"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn general_format_is_deferred() {
        // OXP-022: the "General" keyword is outside the subset.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.5)), Scalar(txt("General"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn empty_format_is_deferred() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.5)), Scalar(txt(""))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // --- OXP-023: text `value` coerces (RUN-2026-07-11-oracle01) -----------

    #[test]
    fn text_value_decimal_coerces_oxp023() {
        // RUN-2026-07-11-oracle01 OXP-023 H1: TEXT("1234.5","0.00") -> "1234.50".
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("1234.5")), Scalar(txt("0.00"))]),
            txt("1234.50")
        );
    }

    #[test]
    fn text_value_grouped_coerces_oxp023() {
        // RUN-2026-07-11-oracle01 OXP-023 H2: TEXT("1,234","#,##0") -> "1,234".
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("1,234")), Scalar(txt("#,##0"))]),
            txt("1,234")
        );
    }

    #[test]
    fn text_value_whitespace_coerces_oxp023() {
        // RUN-2026-07-11-oracle01 OXP-023 H3: TEXT("  5 ","0") -> "5".
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("  5 ")), Scalar(txt("0"))]),
            txt("5")
        );
    }

    #[test]
    fn text_value_scientific_coerces_oxp023() {
        // RUN-2026-07-11-oracle01 OXP-023 H4: TEXT("1e3","0.0") -> "1000.0".
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("1e3")), Scalar(txt("0.0"))]),
            txt("1000.0")
        );
    }

    #[test]
    fn non_numeric_text_value_is_value_error() {
        // Non-numeric text takes `to_number`'s #VALUE! on the OXP-023 path.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("abc")), Scalar(txt("0.00"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // --- OXP-024: non-text `format_text` (RUN-2026-07-11-oracle01) ----------

    #[test]
    fn number_format_code_coerces_oxp024() {
        // RUN-2026-07-11-oracle01 OXP-024 H1: TEXT(5,0) -> "5" (the number 0
        // coerces to the format code "0").
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(5.0)), Scalar(num(0.0))]),
            txt("5")
        );
    }

    #[test]
    fn bool_format_code_is_value_error_oxp024() {
        // RUN-2026-07-11-oracle01 OXP-024 H2: TEXT(5,TRUE) -> #VALUE!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(5.0)), Scalar(Value::Bool(true))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn blank_format_code_is_empty_oxp024() {
        // RUN-2026-07-11-oracle01 OXP-024 H3: TEXT(5,A1) with A1 empty -> "".
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(5.0)), Scalar(Value::Blank)]),
            txt("")
        );
    }

    // --- OXP-022: every probed format code stays #UNSUPPORTED! -------------

    #[test]
    fn oxp022_probed_formats_stay_unsupported() {
        // RUN-2026-07-11-oracle01 OXP-022: these probed codes lie outside BOTH
        // the digit/percent subset and the date/time-token slice (H1 date and
        // H2 time from the original OXP-022 probe are now covered by the slice —
        // see `oxp164_all_pinned_pairs` — and so are excluded here). Excel emits
        // a real string for each remaining code (noted), but faithfully
        // reproducing any one requires a dedicated format sub-engine (fraction,
        // scientific, multi-section, color/condition, currency, escape, scaling
        // comma, `#`-only integer) that would generalize to unprobed inputs — a
        // guessed formatting rule (forbidden). All stay `#UNSUPPORTED!`.
        let cases: &[(f64, &str)] = &[
            (1234.5, "@"),        // Excel: "1234.5"   @ text placeholder
            (0.25, "# ?/?"),      // Excel: " 1/4"     ?/? fraction
            (0.25, "# #/#"),      // Excel: "1/4"      #/# fraction
            (12345.0, "0.0E+00"), // Excel: "1.2E+04"  scientific
            (0.5, "# 1/2"),       // Excel: " 1/2"     fixed-denominator fraction
            (-5.0, "0;(0)"),      // Excel: "(5)"      multi-section `;`
            (1234.5, "[Red]0"),   // Excel: "1235"     `[color]` bracket
            (5.0, "[>3]0"),       // Excel: "5"        `[condition]` bracket
            (1234.5, "$0.00"),    // Excel: "$1234.50" currency literal
            (5.0, "\\x0"),        // Excel: "x5"       `\` escaped literal
            (1234567.0, "#,"),    // Excel: "1235"     trailing/scaling comma
            (5.0, "#"),           // Excel: "5"        `#`-only integer
        ];
        for &(v, f) in cases {
            assert_eq!(
                eval_direct(eval, vec![Scalar(num(v)), Scalar(txt(f))]),
                Value::Error(ErrorKind::Unsupported),
                "format {f:?} should stay #UNSUPPORTED!",
            );
        }
    }

    // --- OXP-022/OXP-164: date/time-token slice (RUN-2026-07-11-oracle01) ----

    /// Every one of the 128 pinned `OXP-164.845f8a37a0a2` pairs, matched
    /// byte-for-byte. RUN-2026-07-11-oracle01, 1900 date system (the sidecar's
    /// `date_system_1904 = false`). Rows 126/127 are the numeric-slice contrast
    /// probes (they exercise comma-as-GROUPING and confirm the dispatch still
    /// routes numeric formats to slice 1); every other row is the date/time
    /// slice. The `m` month-vs-minute ambiguity (H83-91, H128), AM/PM & A/P
    /// 12-hour conversion (H92-101, H128), and the phantom serial 60 (H4, H11,
    /// H18, H25, H32, H39, H46, H53, H60, H67, H88, H91, H103, H107, H111,
    /// H115, H119, H123) are all included.
    #[test]
    fn oxp164_all_pinned_pairs() {
        let pinned: &[(f64, &str, &str)] = &[
            (0.0, "yyyy", "1900"),
            (1.0, "yyyy", "1900"),
            (59.0, "yyyy", "1900"),
            (60.0, "yyyy", "1900"),
            (61.0, "yyyy", "1900"),
            (43831.0, "yyyy", "2020"),
            (2958465.0, "yyyy", "9999"),
            (0.0, "yy", "00"),
            (1.0, "yy", "00"),
            (59.0, "yy", "00"),
            (60.0, "yy", "00"),
            (61.0, "yy", "00"),
            (43831.0, "yy", "20"),
            (2958465.0, "yy", "99"),
            (0.0, "mmmm", "January"),
            (1.0, "mmmm", "January"),
            (59.0, "mmmm", "February"),
            (60.0, "mmmm", "February"),
            (61.0, "mmmm", "March"),
            (43831.0, "mmmm", "January"),
            (2958465.0, "mmmm", "December"),
            (0.0, "mmm", "Jan"),
            (1.0, "mmm", "Jan"),
            (59.0, "mmm", "Feb"),
            (60.0, "mmm", "Feb"),
            (61.0, "mmm", "Mar"),
            (43831.0, "mmm", "Jan"),
            (2958465.0, "mmm", "Dec"),
            (0.0, "mm", "01"),
            (1.0, "mm", "01"),
            (59.0, "mm", "02"),
            (60.0, "mm", "02"),
            (61.0, "mm", "03"),
            (43831.0, "mm", "01"),
            (2958465.0, "mm", "12"),
            (0.0, "m", "1"),
            (1.0, "m", "1"),
            (59.0, "m", "2"),
            (60.0, "m", "2"),
            (61.0, "m", "3"),
            (43831.0, "m", "1"),
            (2958465.0, "m", "12"),
            (0.0, "dddd", "Saturday"),
            (1.0, "dddd", "Sunday"),
            (59.0, "dddd", "Tuesday"),
            (60.0, "dddd", "Wednesday"),
            (61.0, "dddd", "Thursday"),
            (43831.0, "dddd", "Wednesday"),
            (2958465.0, "dddd", "Friday"),
            (0.0, "ddd", "Sat"),
            (1.0, "ddd", "Sun"),
            (59.0, "ddd", "Tue"),
            (60.0, "ddd", "Wed"),
            (61.0, "ddd", "Thu"),
            (43831.0, "ddd", "Wed"),
            (2958465.0, "ddd", "Fri"),
            (0.0, "dd", "00"),
            (1.0, "dd", "01"),
            (59.0, "dd", "28"),
            (60.0, "dd", "29"),
            (61.0, "dd", "01"),
            (43831.0, "dd", "01"),
            (2958465.0, "dd", "31"),
            (0.0, "d", "0"),
            (1.0, "d", "1"),
            (59.0, "d", "28"),
            (60.0, "d", "29"),
            (61.0, "d", "1"),
            (43831.0, "d", "1"),
            (2958465.0, "d", "31"),
            (0.5, "hh", "12"),
            (0.7, "hh", "16"),
            (0.99999, "hh", "23"),
            (0.5, "h", "12"),
            (0.7, "h", "16"),
            (0.99999, "h", "23"),
            (0.5, "ss", "00"),
            (0.7, "ss", "00"),
            (0.99999, "ss", "59"),
            (0.5, "s", "0"),
            (0.7, "s", "0"),
            (0.99999, "s", "59"),
            (0.5, "h:mm", "12:00"),
            (0.7, "h:mm", "16:48"),
            (0.99999, "h:mm", "23:59"),
            (43831.7, "h:mm", "16:48"),
            (0.0, "mm/dd", "01/00"),
            (60.0, "mm/dd", "02/29"),
            (43831.0, "mm/dd", "01/01"),
            (43831.7, "m/d h:mm", "1/1 16:48"),
            (60.5, "mm/dd hh:mm", "02/29 12:00"),
            (0.25, "AM/PM", "AM"),
            (0.5, "AM/PM", "PM"),
            (0.7, "AM/PM", "PM"),
            (0.25, "h:mm AM/PM", "6:00 AM"),
            (0.5, "h:mm AM/PM", "12:00 PM"),
            (0.7, "h:mm AM/PM", "4:48 PM"),
            (0.25, "A/P", "A"),
            (0.5, "A/P", "P"),
            (0.7, "A/P", "P"),
            (0.7, "h:mm A/P", "4:48 P"),
            (0.0, "mm/dd/yy", "01/00/00"),
            (60.0, "mm/dd/yy", "02/29/00"),
            (43831.0, "mm/dd/yy", "01/01/20"),
            (2958465.0, "mm/dd/yy", "12/31/99"),
            (0.0, "mmm-yy", "Jan-00"),
            (60.0, "mmm-yy", "Feb-00"),
            (43831.0, "mmm-yy", "Jan-20"),
            (2958465.0, "mmm-yy", "Dec-99"),
            (0.0, "mmmm d, yyyy", "January 0, 1900"),
            (60.0, "mmmm d, yyyy", "February 29, 1900"),
            (43831.0, "mmmm d, yyyy", "January 1, 2020"),
            (2958465.0, "mmmm d, yyyy", "December 31, 9999"),
            (0.0, "mm/dd/yyyy", "01/00/1900"),
            (60.0, "mm/dd/yyyy", "02/29/1900"),
            (43831.0, "mm/dd/yyyy", "01/01/2020"),
            (2958465.0, "mm/dd/yyyy", "12/31/9999"),
            (0.0, "dd-mmm", "00-Jan"),
            (60.0, "dd-mmm", "29-Feb"),
            (43831.0, "dd-mmm", "01-Jan"),
            (2958465.0, "dd-mmm", "31-Dec"),
            (0.0, "m/d/yyyy", "1/0/1900"),
            (60.0, "m/d/yyyy", "2/29/1900"),
            (43831.0, "m/d/yyyy", "1/1/2020"),
            (2958465.0, "m/d/yyyy", "12/31/9999"),
            (1234567.0, "#,##0", "1,234,567"),
            (1234.5, "#,##0.0", "1,234.5"),
            (43831.7, "m/d/yyyy h:mm AM/PM", "1/1/2020 4:48 PM"),
        ];
        assert_eq!(pinned.len(), 128, "the OXP-164 grid has 128 rows");
        for &(serial, code, want) in pinned {
            assert_eq!(
                eval_direct(eval, vec![Scalar(num(serial)), Scalar(txt(code))]),
                txt(want),
                "TEXT({serial}, {code:?}) should be {want:?}",
            );
        }
    }

    #[test]
    fn m_month_vs_minute_ambiguity_oxp164() {
        // The crux (OXP-164 section C): `mm` reads as MINUTE beside `h`, as
        // MONTH beside `d`, and both roles coexist in one string.
        // `mm` = MINUTE in "h:mm" (0.5 → noon → "12:00").
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.5)), Scalar(txt("h:mm"))]),
            txt("12:00")
        );
        // `mm` = MONTH in "mm/dd" (phantom serial 60 → "02/29").
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(60.0)), Scalar(txt("mm/dd"))]),
            txt("02/29")
        );
        // Both in one string: "m/d h:mm" @ 43831.7 → month 1, minute 48.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(43831.7)), Scalar(txt("m/d h:mm"))]),
            txt("1/1 16:48")
        );
        // Both again with 2-digit runs: "mm/dd hh:mm" @ 60.5 → month 02, min 00.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(60.5)), Scalar(txt("mm/dd hh:mm"))]),
            txt("02/29 12:00")
        );
    }

    #[test]
    fn ampm_twelve_hour_conversion_oxp164() {
        // OXP-164 section D: AM/PM & A/P drive the 12-hour hour value.
        // 06:00 → AM, hour 6.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.25)), Scalar(txt("h:mm AM/PM"))]),
            txt("6:00 AM")
        );
        // Noon → PM, hour 12.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.5)), Scalar(txt("h:mm AM/PM"))]),
            txt("12:00 PM")
        );
        // 16:48 → PM, hour 4 (16 mod 12).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.7)), Scalar(txt("h:mm A/P"))]),
            txt("4:48 P")
        );
    }

    #[test]
    fn phantom_serial_60_and_serial_0_oxp164() {
        // The 1900 leap-year bug: serial 60 is the fictitious 1900-02-29, a
        // Wednesday, rendering as day 29 of February.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(60.0)), Scalar(txt("mmmm d, yyyy"))]),
            txt("February 29, 1900")
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(60.0)), Scalar(txt("dddd"))]),
            txt("Wednesday")
        );
        // Serial 0 is Excel's "January 0, 1900" (a Saturday); day renders as 0.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0)), Scalar(txt("mmmm d, yyyy"))]),
            txt("January 0, 1900")
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0)), Scalar(txt("dddd"))]),
            txt("Saturday")
        );
    }

    #[test]
    fn seconds_rounding_near_midnight_oxp164() {
        // OXP-164: serial 0.99999 is 23:59:59.136 — the minute truncates to 59
        // (it does NOT round up to the next hour/day), and the second is 59.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.99999)), Scalar(txt("h:mm"))]),
            txt("23:59")
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.99999)), Scalar(txt("hh"))]),
            txt("23")
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.99999)), Scalar(txt("ss"))]),
            txt("59")
        );
    }

    #[test]
    fn case_insensitive_date_codes_oxp164() {
        // The reviewed scope notes the corpus carries upper-case codes like
        // `M/D/YYYY` and `DD-MMM-YY`; the tokenizer is case-insensitive.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(43831.0)), Scalar(txt("M/D/YYYY"))]),
            txt("1/1/2020")
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(43831.0)), Scalar(txt("DD-MMM-YY"))]),
            txt("01-Jan-20")
        );
    }

    #[test]
    fn out_of_scope_datetime_constructs_defer() {
        // A date/time format carrying ANY out-of-scope token defers (never a
        // guessed string): elapsed `[h]`, a `[$-409]` locale tag, an escaped
        // literal, a quoted literal, single-letter month `mmmmm`, a `;`
        // multi-section date, and `General` all stay #UNSUPPORTED!.
        let deferred: &[&str] = &[
            "[h]:mm",       // elapsed hours bracket
            "[$-409]mm/dd", // locale tag
            "yyyy\\-mm",    // backslash-escaped literal
            "yyyy\"Q\"",    // quoted literal
            "mmmmm",        // single-letter month
            "mm/dd;@",      // multi-section
            "General",      // General keyword
        ];
        for f in deferred {
            assert_eq!(
                eval_direct(eval, vec![Scalar(num(43831.0)), Scalar(txt(f))]),
                Value::Error(ErrorKind::Unsupported),
                "date format {f:?} should stay #UNSUPPORTED!",
            );
        }
    }

    // --- Error propagation --------------------------------------------------

    #[test]
    fn value_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Div0)), Scalar(txt("0.00"))]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn format_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(1.5)), Scalar(Value::Error(ErrorKind::Na))]
            ),
            Value::Error(ErrorKind::Na)
        );
    }
}

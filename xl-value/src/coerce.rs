//! **The coercion tables.** All type-coercion logic in Recalc lives here; no
//! other crate reimplements any of it — `xl-fn` etc. call these functions.
//!
//! ## Provenance
//! Every rule below cites Microsoft Learn function/operator docs, ECMA-376
//! (ISO/IEC 29500) §18.17 (formula semantics), or the coercion asymmetries in
//! `implementation-plan.md` §2's "Semantics hit-list". Where a rule is only
//! knowable by running Excel, the code path returns `#UNSUPPORTED!` and is
//! flagged `TODO(oracle-experiment)` with a matching probe queued in
//! `docs/oracle-experiments.md` — **guessing is forbidden**
//! (`implementation-plan.md` §0).
//!
//! Key sources:
//! - Arithmetic operators & text→number coercion: Microsoft Learn
//!   "Calculation operators and precedence in Excel".
//! - `""` vs blank, logical/text handling in aggregates vs scalars:
//!   hit-list, plus `SUM`/`N`/`VALUE` function docs on Microsoft Learn.
//! - Comparison (case-insensitive, cross-type ordering): hit-list
//!   "case-insensitive string equality"; ECMA-376 §18.17.
//! - Oracle run **RUN-2026-07-11-oracle01** pins the text→number leniency
//!   (currency `$`, parenthesized negatives, comma grouping, `%` whitespace,
//!   magnitude overflow — OXP-002/010/011/012/013), the General-format
//!   scientific threshold (OXP-020/021/162), and the `Blank`-vs-`Bool` compare
//!   (OXP-030). **Date/time coercion is now RESOLVED (OXP-160, resolving the
//!   long-held OXP-001):** a date/time-shaped string coerces to its Excel
//!   serial — the FULL serial including any time fraction — via
//!   [`parse_datetime_text`] (e.g. `="2020-01-01"+0` → 43831, `="16:48"+0` →
//!   0.7); a recognized-but-invalid date (`"2/29/2021"`) is `#VALUE!`, and a
//!   no-year form (`"1/1"`, clock-dependent) stays `#UNSUPPORTED!` because
//!   `to_number` is context-free (no injectable clock). The
//!   currency/paren/exponent **compositions** are pinned by OXP-162
//!   (`"$1e2"` → 100, `"(50%)"` → −0.5, `"$50%"` → `#VALUE!`). Only non-ASCII
//!   collation (OXP-031) remains **held**: its shapes route to
//!   `#UNSUPPORTED!` (an explicit deferral) pending a dedicated probe sweep.
//!
//! ## Date-text public surface (RFC-0007, ratified)
//! [`parse_datetime_text`] + [`DateTimeText`] are an **additive** public export
//! (see `rfcs/0007-datetime-text-parser.md`, ratified by the the contract review tech-lead
//! decision deferred by the user, 2026-07-11). They are the single grammar
//! source shared by [`to_number`] (bare coercion), `xl-fn`'s `DATEVALUE`, and
//! `xl-fn`'s criteria engine. The parser is deliberately **gated to the probed
//! forms** — unprobed date shapes (`Mon D` no-year, US `M-D-Y`-hyphen,
//! `Y/M/D` slash, the phantom `1900-02-29`) defer to `#UNSUPPORTED!` rather than
//! guess (the contract review B1–B4).
//!
//! ### Documented limitation: 1904 workbooks (review B6)
//! [`to_number`] is **context-free** — it has no workbook date-system flag — so
//! its date-text serials are computed for the **1900 system** (the Windows
//! default, and every OXP-160 pin). In a **1904** workbook, bare
//! `="1/1/2020"+0` should be `42369`, not the `43831` `to_number` returns; that
//! path cannot see the flag, so this is a known limitation, **not silently
//! papered over**. `DATEVALUE` **does** honor the flag (it has `ctx`), so it is
//! the correct path for 1904 workbooks. Queued: `OXP (unassigned)` — 1904
//! bare-coercion of date text.
//!
//! ## The two coercion contexts (an intentional Excel asymmetry)
//! Excel coerces operands differently depending on context — this is not a
//! bug we smooth over, it is behavior we must reproduce:
//!
//! - **[`CoercionMode::Scalar`]** — a *direct* operand of an operator or a
//!   direct scalar argument to a function. Text that looks numeric and
//!   logicals **do** coerce: `="1"+1` → `2`, `SUM(TRUE,"2")` → `3`.
//! - **[`CoercionMode::RangeAggregate`]** — a value pulled from a *range*
//!   feeding an aggregate. Text and logicals are **ignored**, blanks are
//!   skipped, only real numbers count, and errors still propagate: a range
//!   cell holding `TRUE` or `"2"` contributes nothing to `SUM`.
//!
//! [`to_number`]/[`to_text`]/[`to_bool`] implement the *scalar* rules.
//! [`coerce_number_arg`] selects between the two modes for aggregation.

use core::cmp::Ordering;

use crate::error::ErrorKind;
use crate::text::Text;
use crate::value::{Array, Value};

// ===========================================================================
// Number coercion (scalar / operator context)
// ===========================================================================

/// Coerces a value to a number in **scalar/operator context** (`to_number`).
///
/// Rules (Microsoft Learn "Calculation operators and precedence"; hit-list):
/// - `Number(n)` → `n`.
/// - `Bool(true)` → `1.0`, `Bool(false)` → `0.0`.
/// - `Blank` → `0.0` (an empty cell reads as zero in arithmetic). **Note:**
///   this differs from `Text("")`, which is *not* a number → `#VALUE!`.
/// - `Text` → parsed as an en-US number if possible (see the internal
///   `parse_number_text`); otherwise `#VALUE!`. Overflow — either an
///   f64-non-finite magnitude (`"1e999"`) or a finite f64 above Excel's
///   entry limit `9.99999999999999E+307` (`"1e308"`) — is `#VALUE!` per
///   OXP-002 (RUN-2026-07-11-oracle01).
/// - `Error(k)` → propagates `k`.
/// - `Array` → 1×1 arrays lift to their element; larger arrays are
///   `#UNSUPPORTED!` here (implicit intersection / spill is an `xl-ast`
///   concern and its scalar-context result is oracle-unknown —
///   `TODO(oracle-experiment)`).
/// - `Ref` → `#UNSUPPORTED!` (must be resolved to a value first).
///
/// # Errors
/// Returns the propagated/derived [`ErrorKind`] when the value cannot be a
/// finite number in this context.
pub fn to_number(value: &Value) -> Result<f64, ErrorKind> {
    match value {
        Value::Number(n) => Ok(*n),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Blank => Ok(0.0),
        Value::Error(k) => Err(*k),
        Value::Text(t) => match parse_number_text(t.as_str()) {
            ParsedNum::Num(n) => Ok(n),
            // OXP-002 (RUN-2026-07-11-oracle01): an overflowing numeric string
            // — non-finite (`"1e999"`) or finite-but-above Excel's entry
            // limit `9.99999999999999E+307` (`"1e308"`) — is `#VALUE!`.
            ParsedNum::Overflow => Err(ErrorKind::Value),
            ParsedNum::NotNumeric => Err(ErrorKind::Value),
            ParsedNum::NeedsOracle => Err(ErrorKind::Unsupported),
        },
        Value::Array(a) => match a.as_scalar() {
            Some(inner) => to_number(inner),
            // TODO(oracle-experiment): implicit intersection of a multi-cell
            // array/range in scalar arithmetic (probe OXP-004).
            None => Err(ErrorKind::Unsupported),
        },
        // TODO(oracle-experiment): Ref coercion requires resolving the range
        // against sheet data (not available in this crate) — see OXP-005.
        Value::Ref(_) => Err(ErrorKind::Unsupported),
        // BC-6 (RFC-0012): a lambda is not a number — refuse loudly. A lambda
        // should never reach a scalar context (it is `#CALC!` in a cell), but
        // if one does, born-refusing forbids a guess.
        Value::Lambda(_) => Err(ErrorKind::Unsupported),
    }
}

/// Outcome of number coercion for a single aggregate argument.
///
/// Distinguishes "use this number", "ignore this operand" (the value is
/// present but does not participate — e.g. text in a range for `SUM`), and
/// "propagate this error".
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NumericArg {
    /// Contribute this number to the aggregate.
    Number(f64),
    /// Skip this operand entirely (not counted; e.g. text/logical/blank in a
    /// range).
    Skip,
    /// Propagate this error out of the whole aggregate.
    Error(ErrorKind),
}

/// Which coercion context applies. See the module docs for the asymmetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoercionMode {
    /// Direct operand/argument: text and logicals coerce.
    Scalar,
    /// Value drawn from a range feeding an aggregate: text and logicals are
    /// ignored, blanks skipped, errors propagate.
    RangeAggregate,
}

/// Coerces one aggregate argument to a number under the given mode.
///
/// This is the single entry point `xl-fn` aggregates (`SUM`, `AVERAGE`, …)
/// call per operand. It encodes the Excel asymmetry:
/// - `SUM(TRUE, "2")` → `3` uses [`CoercionMode::Scalar`] on each *direct*
///   argument (`TRUE`→1, `"2"`→2).
/// - `SUM(A1:A3)` where a cell holds `TRUE`/`"2"` uses
///   [`CoercionMode::RangeAggregate`], which yields [`NumericArg::Skip`] for
///   those, so they contribute nothing.
///
/// Provenance: hit-list "coercion asymmetries (`="1"+1`→2, but SUM over a
/// range ignores text/logicals while `SUM(TRUE,"2")`→3)"; Microsoft Learn
/// `SUM` remarks ("logical values and text … in a reference are ignored").
#[must_use]
pub fn coerce_number_arg(value: &Value, mode: CoercionMode) -> NumericArg {
    match mode {
        CoercionMode::Scalar => match to_number(value) {
            Ok(n) => NumericArg::Number(n),
            Err(k) => NumericArg::Error(k),
        },
        CoercionMode::RangeAggregate => match value {
            Value::Number(n) => NumericArg::Number(*n),
            // Ignored in range/aggregate context:
            Value::Blank | Value::Text(_) | Value::Bool(_) => NumericArg::Skip,
            Value::Error(k) => NumericArg::Error(*k),
            // TODO(oracle-experiment): logicals/text inside an *array
            // constant* argument (e.g. SUM({1,TRUE,"2"})) may differ from
            // both a plain range and direct args (probe OXP-006). Refs in a
            // range context must be pre-resolved by the caller.
            Value::Array(_) | Value::Ref(_) => NumericArg::Error(ErrorKind::Unsupported),
            // BC-6 (RFC-0012): a lambda has no place in a numeric aggregate —
            // its own refusing arm (never folded into the Array/Ref group).
            Value::Lambda(_) => NumericArg::Error(ErrorKind::Unsupported),
        },
    }
}

/// Result of parsing a string as an en-US number.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ParsedNum {
    /// A finite parsed value within Excel's entry range.
    Num(f64),
    /// Syntactically a number but out of range — non-finite as an f64, or a
    /// finite f64 whose magnitude exceeds Excel's entry limit
    /// `9.99999999999999E+307` (→ `#VALUE!` per OXP-002).
    Overflow,
    /// Not a number (→ `#VALUE!`).
    NotNumeric,
    /// Syntactically number-*like* but whose Excel reading is oracle-unknown
    /// (→ `#UNSUPPORTED!`): a **no-year** date form (`"1/1"`, clock-dependent —
    /// OXP-160) or a date/time-shaped-but-unrecognized string (`"6:45 PM"`
    /// meridiem), a date whose serial is outside the 1900 system's
    /// representable range, a non-ASCII-space gap before a trailing `%`
    /// (OXP-013), an unprobed `$` placement (OXP-010), or an exotic multi-comma
    /// grouping (OXP-012).
    NeedsOracle,
}

/// Excel's largest enterable magnitude: `9.99999999999999E+307`. A parsed
/// numeric string whose value exceeds this — even a finite f64 such as
/// `1e308` — is rejected as `#VALUE!` (OXP-002, RUN-2026-07-11-oracle01).
const EXCEL_MAX_MAGNITUDE: f64 = 9.999_999_999_999_99e307;

/// Parses `s` as an en-US number for text→number coercion.
///
/// Handles (per Microsoft Learn `VALUE`/operator docs and the pinned oracle
/// run RUN-2026-07-11-oracle01): leading/trailing whitespace, sign, decimal
/// point, scientific notation (`1e2`), a single trailing percent sign
/// (`50%` → `0.5`, incl. ASCII-space gaps `"50 %"` → `0.5`, OXP-013),
/// thousands-grouping commas (`1,000`; OXP-012 accepts any post-comma group
/// of ≥3 digits so `"1,0000"` → `10000`), a `$` currency prefix
/// (`"$1,000"` → `1000`, `"$-5"` → `-5`, OXP-010), and a whole-string
/// parenthesized negative (`"(1)"` → `-1`, OXP-011).
///
/// **Date/time-shaped text (OXP-160, RUN-2026-07-11-oracle01):** handed to
/// [`parse_datetime_text`] and, when it names a valid date/time, coerced to the
/// **full** 1900-system serial (date part + time fraction) via
/// [`ymd_to_serial_1900`] — `"2020-01-01"` → 43831, `"16:48"` → 0.7,
/// `"1/1/2020 16:48"` → 43831.7. A recognized-but-invalid date (`"2/29/2021"`,
/// `"13/1/2020"`) is `#VALUE!`; a no-year form (`"1/1"`, clock-dependent) or an
/// unrecognized-but-shaped form (`"6:45 PM"`) is `#UNSUPPORTED!`.
///
/// **Routed to `#UNSUPPORTED!` (oracle-unknown, never guessed):**
/// - Unprobed `$` placements (`"-$5"`, `"$ 5"`, a trailing/internal `$`).
/// - Unprobed paren shapes (`"(-1)"`, `"($1)"`) and an **exponent** inside
///   parens (`"(1e5)"` — still unprobed; only a `%` composition inside parens
///   is now pinned, OXP-162).
/// - Exotic multi-comma groupings and non-ASCII-space `%` gaps.
///
/// **Rejected as `#VALUE!`:** ordinary non-numeric text, the pinned
/// malformed groupings (`"1,00"`, `"12,34"`, `"1,00,000"`), an unknown suffix
/// (`"1,000 kr"`), out-of-range magnitudes (OXP-002), and the pinned
/// currency-with-percent composition (`"$50%"` — OXP-162, currency + `%` is
/// rejected even though currency + exponent `"$1e2"` → 100 is accepted).
fn parse_number_text(s: &str) -> ParsedNum {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return ParsedNum::NotNumeric;
    }

    // OXP-160 (RUN-2026-07-11-oracle01): Excel coerces date/time-shaped text to
    // its serial in arithmetic (`="2020-01-01"+0` → 43831, `="16:48"+0` → 0.7).
    // Parse the pinned grammar to the FULL serial (date part + time fraction);
    // invalid dates are `#VALUE!`, no-year/unrecognized shapes defer. No valid
    // en-US number is date/time-shaped, so intercepting here never shadows a
    // real numeric parse (a plain number never contains `:`/`/`, a month word,
    // or a digit-flanked `-`).
    match parse_datetime_text(trimmed) {
        DateTimeText::Parsed {
            date,
            time_fraction,
        } => {
            return match date {
                Some((y, m, d)) => match ymd_to_serial_1900(y, m, d) {
                    Some(serial) => ParsedNum::Num(serial + time_fraction),
                    // A valid calendar date outside the 1900 system's
                    // representable serial range (e.g. pre-1900) is unprobed for
                    // text coercion — defer rather than guess `#VALUE!`.
                    None => ParsedNum::NeedsOracle,
                },
                // A pure time has no date part: its serial is just the fraction.
                None => ParsedNum::Num(time_fraction),
            };
        }
        DateTimeText::Invalid => return ParsedNum::NotNumeric,
        DateTimeText::Unsupported => return ParsedNum::NeedsOracle,
        // Not date/time-shaped — fall through to plain numeric parsing.
        DateTimeText::NotDateTime => {}
    }

    // OXP-011/162 (RUN-2026-07-11-oracle01): a whole-string parenthesized
    // *unsigned* magnitude is a negative (`"(1)"` → -1, `"(1,000)"` → -1000).
    // OXP-162 now pins a `%` composed inside as ACCEPTED and negated
    // (`"(50%)"` → -0.5). Still oracle-unknown → `#UNSUPPORTED!`: a leading
    // sign, `$`, or nested paren inside (`"(-1)"`, `"($1)"`), and an **exponent**
    // inside (`"(1e5)"`, never probed). Accept digits, grouping commas, a single
    // decimal point, and a trailing `%` inside the parens.
    if trimmed.starts_with('(') && trimmed.ends_with(')') && trimmed.len() >= 2 {
        let inner = &trimmed[1..trimmed.len() - 1];
        if inner.starts_with(['+', '-', '$', '(']) || inner.contains(['e', 'E']) {
            return ParsedNum::NeedsOracle;
        }
        return match parse_plain(inner) {
            ParsedNum::Num(n) => ParsedNum::Num(-n),
            other => other,
        };
    }

    // OXP-010/162 (RUN-2026-07-11-oracle01): a `$` currency prefix immediately
    // before the (optionally signed) plain magnitude is accepted (`"$1,000"` →
    // 1000, `"$-5"` → -5). OXP-162 pins the compositions: currency + **exponent**
    // is ACCEPTED (`"$1e2"` → 100), but currency + **percent** is REJECTED as
    // `#VALUE!` (`"$50%"`, `"$5%"`). Still oracle-unknown → `#UNSUPPORTED!`: a
    // space after `$`, a second `$`, or a `$` anywhere but the very start.
    if let Some(rest) = trimmed.strip_prefix('$') {
        if rest.contains('$') {
            return ParsedNum::NeedsOracle;
        }
        return match rest.chars().next() {
            None => ParsedNum::NotNumeric,
            Some(c) if c.is_whitespace() => ParsedNum::NeedsOracle,
            // OXP-162: a `%` anywhere in the currency body is the rejected
            // composition (→ `#VALUE!`); the plain parser (which would scale a
            // trailing `%`) is bypassed. An exponent stays in and parses.
            _ if rest.contains('%') => ParsedNum::NotNumeric,
            _ => parse_plain(rest),
        };
    }
    if trimmed.contains('$') {
        // A `$` that is not the strict prefix (`"-$5"`, `"5$"`) — unprobed.
        return ParsedNum::NeedsOracle;
    }

    parse_plain(trimmed)
}

/// Parses a "plain" number body — one with no `$` prefix and no parenthesis
/// wrapper (those are peeled off by [`parse_number_text`]). Handles the
/// trailing `%`, thousands grouping, sign, decimal, exponent, and the
/// Excel-entry magnitude cap.
fn parse_plain(body: &str) -> ParsedNum {
    // A single trailing '%' scales by 1/100. OXP-013 (RUN-2026-07-11-oracle01):
    // ASCII space(s) between the number and the '%' are accepted
    // (`"50 %"` → 0.5). Strip ASCII spaces (0x20) *explicitly* — NOT via
    // `trim_end`, which also eats U+00A0/tab; an NBSP or tab gap stays
    // oracle-unknown → `#UNSUPPORTED!`.
    let (num_body, percent) = match body.strip_suffix('%') {
        Some(rest) => {
            let ascii_stripped = rest.trim_end_matches(' ');
            if ascii_stripped != ascii_stripped.trim_end() {
                return ParsedNum::NeedsOracle;
            }
            (ascii_stripped, true)
        }
        None => (body, false),
    };
    if num_body.is_empty() {
        return ParsedNum::NotNumeric;
    }

    let cleaned = match strip_grouping(num_body) {
        Grouping::Cleaned(c) => c,
        Grouping::Invalid => return ParsedNum::NotNumeric,
        Grouping::Unsupported => return ParsedNum::NeedsOracle,
    };

    // Reject Rust's `inf`/`nan`/`infinity` spellings — Excel does not read
    // those as numbers.
    let lower = cleaned.to_ascii_lowercase();
    if lower.contains("inf") || lower.contains("nan") {
        return ParsedNum::NotNumeric;
    }

    match cleaned.parse::<f64>() {
        Ok(v) if v.is_finite() => {
            let scaled = if percent { v / 100.0 } else { v };
            // OXP-002: a finite value above Excel's entry limit is overflow.
            if scaled.is_finite() && scaled.abs() <= EXCEL_MAX_MAGNITUDE {
                ParsedNum::Num(scaled)
            } else {
                ParsedNum::Overflow
            }
        }
        // Parsed but non-finite ⇒ magnitude overflow (e.g. "1e999").
        Ok(_) => ParsedNum::Overflow,
        Err(_) => ParsedNum::NotNumeric,
    }
}

/// Shape gate for the date/time grammar (OXP-160): does `s` (already trimmed)
/// look like a **date or time** literal rather than a plain number?
///
/// This is the first check in [`parse_datetime_text`]: a string that is *not*
/// date/time-shaped is `NotDateTime` (the caller falls through to plain numeric
/// parsing), while a shaped string is then parsed against the recognized
/// grammar. It detects the *shape only* — it never parses a date.
///
/// Matched shapes: a `:`, `-`, or `/` flanked by ASCII digits (`"12:00"`,
/// `"2020-01-01"`, `"1/1/2020"`), or an English month name (abbrev or full)
/// appearing as a whole word alongside a digit (`"Jan 2020"`). No valid en-US
/// numeric string matches (numbers never contain `:`/`/`, a month word, or a
/// digit-flanked `-` — a sign `-` is leading or follows `e`), so this never
/// intercepts a real number.
fn looks_like_datetime(s: &str) -> bool {
    let bytes = s.as_bytes();
    for i in 1..bytes.len() {
        let c = bytes[i];
        if (c == b':' || c == b'-' || c == b'/')
            && bytes[i - 1].is_ascii_digit()
            && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
        {
            return true;
        }
    }
    contains_month_name_with_digit(s)
}

/// Whether `s` contains an English month name (abbreviation or full name) as a
/// whole ASCII-alphabetic word, alongside at least one ASCII digit — the
/// `"Jan 2020"` date shape for [`looks_like_datetime`]. Whole-word matching
/// (maximal alphabetic runs) avoids false positives like `"maybe1"`.
fn contains_month_name_with_digit(s: &str) -> bool {
    const MONTHS: &[&str] = &[
        "jan",
        "january",
        "feb",
        "february",
        "mar",
        "march",
        "apr",
        "april",
        "may",
        "jun",
        "june",
        "jul",
        "july",
        "aug",
        "august",
        "sep",
        "september",
        "oct",
        "october",
        "nov",
        "november",
        "dec",
        "december",
    ];
    let lower = s.to_ascii_lowercase();
    if !lower.bytes().any(|b| b.is_ascii_digit()) {
        return false;
    }
    let bytes = lower.as_bytes();
    let mut start: Option<usize> = None;
    for i in 0..=bytes.len() {
        let is_alpha = i < bytes.len() && bytes[i].is_ascii_alphabetic();
        match (start, is_alpha) {
            (None, true) => start = Some(i),
            (Some(st), false) => {
                if MONTHS.contains(&&lower[st..i]) {
                    return true;
                }
                start = None;
            }
            _ => {}
        }
    }
    false
}

/// Outcome of validating/removing en-US thousands-grouping commas.
enum Grouping {
    /// The comma-free string, ready for `f64::parse`.
    Cleaned(String),
    /// Malformed for a number → `#VALUE!` (non-digit groups, a pinned
    /// too-short group like `"1,00"`).
    Invalid,
    /// A syntactically-plausible but unprobed grouping → `#UNSUPPORTED!`
    /// (an exotic multi-comma shape, or an unusual leading-group width).
    Unsupported,
}

/// Validates and removes en-US thousands-grouping commas from a numeric
/// string, returning the comma-free string ready for `f64::parse`.
///
/// Only the integer part of the significand may contain commas; the fractional
/// part and exponent must not (the exponent is passed through untouched for
/// `f64::parse` to validate).
///
/// Grouping rule (OXP-012, RUN-2026-07-11-oracle01): **every post-comma group
/// must have ≥ 3 digits** (`"1,000"`, `"12,345,678"`, and — the fix — the
/// single-comma `"1,0000"` → `10000`). A post-comma group of < 3 digits is a
/// hard `#VALUE!` (`"1,00"`, `"12,34"`, `"1,00,000"`). A wide (> 3-digit)
/// post-comma group is only probed for the single-comma case; a *multi*-comma
/// shape carrying one is exotic/unprobed → `#UNSUPPORTED!`, as is an unusual
/// leading-group width (> 3 digits).
fn strip_grouping(s: &str) -> Grouping {
    let mut out = String::with_capacity(s.len());

    // Optional leading sign.
    let rest = if let Some(sign) = s.strip_prefix(['+', '-']) {
        out.push(s.as_bytes()[0] as char);
        sign
    } else {
        s
    };
    if rest.is_empty() {
        return Grouping::Invalid;
    }

    // Split off exponent (commas are not allowed within it).
    let (mantissa, exponent) = match rest.find(['e', 'E']) {
        Some(p) => (&rest[..p], Some(&rest[p..])),
        None => (rest, None),
    };

    // Split mantissa into integer and fractional parts.
    let (int_part, frac_part) = match mantissa.find('.') {
        Some(p) => (&mantissa[..p], Some(&mantissa[p + 1..])),
        None => (mantissa, None),
    };

    if int_part.contains(',') {
        let groups: Vec<&str> = int_part.split(',').collect();
        // Need at least one comma → at least two groups.
        if groups.len() < 2 {
            return Grouping::Invalid;
        }
        // Every group must be a non-empty run of ASCII digits.
        for g in &groups {
            if g.is_empty() || !g.bytes().all(|b| b.is_ascii_digit()) {
                return Grouping::Invalid;
            }
        }
        // Any post-comma group shorter than 3 digits is a hard reject
        // (`"1,00"`, `"12,34"`, `"1,00,000"`).
        if groups[1..].iter().any(|g| g.len() < 3) {
            return Grouping::Invalid;
        }
        // An unusual leading-group width (> 3 digits) is unprobed.
        if groups[0].len() > 3 {
            return Grouping::Unsupported;
        }
        // A wide (> 3-digit) post-comma group is probed only for the
        // single-comma case (`"1,0000"`); with multiple commas it is exotic.
        let has_wide = groups[1..].iter().any(|g| g.len() > 3);
        if has_wide && groups.len() > 2 {
            return Grouping::Unsupported;
        }
        out.push_str(&groups.concat());
    } else {
        // Integer part may be empty (".5"); any non-digit is malformed.
        if !int_part.bytes().all(|b| b.is_ascii_digit()) {
            return Grouping::Invalid;
        }
        out.push_str(int_part);
    }

    if let Some(frac) = frac_part {
        if !frac.bytes().all(|b| b.is_ascii_digit()) {
            return Grouping::Invalid;
        }
        out.push('.');
        out.push_str(frac);
    }

    if let Some(exp) = exponent {
        out.push_str(exp);
    }

    Grouping::Cleaned(out)
}

// ===========================================================================
// Date/time text parsing (OXP-160 / OXP-001 resolved, RUN-2026-07-11-oracle01)
// ===========================================================================

/// The outcome of matching a string against Excel's en-US **date/time text
/// grammar** (OXP-160, RUN-2026-07-11-oracle01).
///
/// Shared by [`to_number`] (bare `"text"+0` coercion, which reads the **full**
/// serial *including* any time fraction) and `xl-fn`'s `DATEVALUE` (which takes
/// only the integer **date part**). The parser returns *components* rather than
/// a serial so each caller can apply its own serial math — [`to_number`] a
/// self-contained 1900-system calc ([`ymd_to_serial_1900`]), `DATEVALUE` the
/// workbook-date-system-aware `datecore` — without `xl-value` depending on
/// `xl-fn` (which would be a crate cycle: `xl-fn` → `xl-value` is the only
/// allowed direction).
///
/// # Accepted forms (all pinned by RUN-2026-07-11-oracle01)
/// - `M/D/YYYY`, `M/D/YY` (en-US month-first slashes); `YYYY-MM-DD` (ISO);
///   `D-Mon-YYYY`, `D-Mon-YY`; `Mon D, YYYY` / `Month D, YYYY`;
///   `Mon YYYY` / `Month YYYY` (→ day 1).
/// - Times `H:MM`, `H:MM:SS` (24-hour), optionally preceded by a date and a
///   space (`"1/1/2020 16:48"`).
/// - **2-digit-year pivot:** `00..=29` → `2000..=2029`, `30..=99` →
///   `1930..=1999` (`"1/1/29"` → 2029, `"1/1/30"` → 1930).
///
/// # Deferred / rejected
/// - **No-year** forms (`"1/1"`) are [`DateTimeText::Unsupported`]: they need
///   the current year (the injectable clock), which `to_number` does not have.
/// - Semantically invalid dates (month > 12, day out of range, non-leap
///   `2/29`) are [`DateTimeText::Invalid`] (→ `#VALUE!`).
/// - Shaped-but-unrecognized forms (a meridiem `"6:45 PM"`, dotted `"1.1.2020"`
///   is not even shaped) are [`DateTimeText::Unsupported`] — never guessed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DateTimeText {
    /// A recognized, calendar-valid date and/or time.
    Parsed {
        /// Resolved `(year, month, day)` if a date part was present (already
        /// 4-digit-year-resolved and calendar-validated); `None` for a pure
        /// time.
        date: Option<(i32, u32, u32)>,
        /// Fraction-of-day in `[0, 1)` for the time part; `0.0` if none.
        time_fraction: f64,
    },
    /// Matched a recognized form but semantically invalid → `#VALUE!`.
    Invalid,
    /// Date/time-*shaped* but its Excel reading is oracle-unknown → defer to
    /// `#UNSUPPORTED!` (a no-year form, an unrecognized shape).
    Unsupported,
    /// Not date/time-shaped at all — the caller should fall through to plain
    /// numeric parsing.
    NotDateTime,
}

/// Parse `s` against Excel's en-US date/time text grammar (OXP-160). See
/// [`DateTimeText`] for the accepted forms and the deferral policy.
///
/// This is the single source of truth for the grammar: [`to_number`] and
/// `DATEVALUE` both call it, and `xl-fn`'s criteria engine consults it to keep a
/// date-shaped criteria operand from being read as a bare numeric threshold
/// (its ordering semantics are a separate, still-unprobed experiment).
#[must_use]
pub fn parse_datetime_text(s: &str) -> DateTimeText {
    let s = s.trim();
    if s.is_empty() || !looks_like_datetime(s) {
        return DateTimeText::NotDateTime;
    }

    // A ':' unambiguously marks a time (no date form contains one). Split an
    // optional trailing `H:MM[:SS]` time from an optional leading date.
    let (date_str, time_str): (&str, Option<&str>) = if s.contains(':') {
        match s.rsplit_once(char::is_whitespace) {
            Some((left, right)) if right.contains(':') => (left.trim(), Some(right.trim())),
            // A ':' anywhere but a clean trailing token is an unrecognized shape
            // (e.g. `"6:45 PM"` — the meridiem grammar is unprobed here).
            Some(_) => return DateTimeText::Unsupported,
            None => ("", Some(s)),
        }
    } else {
        (s, None)
    };

    let time_fraction = match time_str {
        Some(t) => match parse_clock(t) {
            Some(f) => f,
            None => return DateTimeText::Unsupported,
        },
        None => 0.0,
    };

    let date = if date_str.is_empty() {
        None
    } else {
        match parse_date_part(date_str) {
            DatePart::Ymd(y, m, d) => Some((y, m, d)),
            DatePart::Invalid => return DateTimeText::Invalid,
            DatePart::Unsupported => return DateTimeText::Unsupported,
        }
    };

    DateTimeText::Parsed {
        date,
        time_fraction,
    }
}

/// Outcome of parsing the **date** portion of a date/time string.
enum DatePart {
    /// A calendar-valid `(year, month, day)`.
    Ymd(i32, u32, u32),
    /// Recognized shape but an invalid calendar date → `#VALUE!`.
    Invalid,
    /// Shaped but unrecognized / no-year → `#UNSUPPORTED!`.
    Unsupported,
}

/// Parse the (non-empty) date portion: month-name forms first (`"1-Jan-2020"`,
/// `"Jan 1, 2020"`, `"Jan 2020"`), then slash `M/D/Y`, then ISO `Y-M-D`.
fn parse_date_part(s: &str) -> DatePart {
    if s.bytes().any(|b| b.is_ascii_alphabetic()) {
        parse_month_name_date(s)
    } else if s.contains('/') {
        parse_slash_date(s)
    } else if s.contains('-') {
        parse_iso_date(s)
    } else {
        DatePart::Unsupported
    }
}

/// Parse `M/D/YYYY` / `M/D/YY`. Two components (`"1/1"`, no year) defer.
fn parse_slash_date(s: &str) -> DatePart {
    let parts: Vec<&str> = s.split('/').collect();
    match parts.as_slice() {
        [m, d, y] => {
            // OXP (unassigned, review B3): the en-US month component is 1-2
            // digits. A >=3-digit first component (`"2020/1/1"`, a Y/M/D form) is
            // a *different*, unprobed class → defer, don't guess/reject. Only the
            // pinned `"13/1/2020"` (2-digit month 13 → `#VALUE!`) reaches
            // `finish_ymd`.
            if m.len() >= 3 {
                return DatePart::Unsupported;
            }
            match (parse_u32(m), parse_u32(d), parse_year(y)) {
                (Some(month), Some(day), Some(year)) => finish_ymd(year, month, day),
                _ => DatePart::Unsupported,
            }
        }
        // `"1/1"` — no year, clock-dependent (OXP-160): defer.
        _ => DatePart::Unsupported,
    }
}

/// Parse ISO `YYYY-MM-DD` (the year is taken literally, not 2-digit-pivoted).
///
/// OXP (unassigned, review B2): only a **>=3-digit** leading component is a real
/// ISO year ([`parse_year_wide`]). A 1-2-digit leader (`"1-2-2020"`,
/// `"12-25-2020"` — a US M-D-Y-hyphen form) is a *different*, unprobed class →
/// defer (`#UNSUPPORTED!`) rather than emit a definitive wrong `#VALUE!`.
fn parse_iso_date(s: &str) -> DatePart {
    let parts: Vec<&str> = s.split('-').collect();
    match parts.as_slice() {
        [y, m, d] => match (parse_year_wide(y), parse_u32(m), parse_u32(d)) {
            (Some(year), Some(month), Some(day)) => finish_ymd(year, month, day),
            _ => DatePart::Unsupported,
        },
        _ => DatePart::Unsupported,
    }
}

/// Parse a month-name date: tokens split on `-`, `,`, and whitespace, with
/// exactly one English month word. `Mon YYYY` → day 1; `Mon D, YYYY` (month
/// first) and `D-Mon-YYYY` (month middle) carry an explicit day.
///
/// OXP (unassigned, review B1): the **month-first** forms (`Mon <n>`,
/// `Mon D, <n>`) require a **>=3-digit** year token ([`parse_year_wide`]). A
/// 1-2-digit trailing number is a *day* in the no-year `Mon D` form (`"Jan 1"`,
/// `"May 5"` — clock-dependent, same class as `"1/1"`), so it defers rather than
/// being mis-read as a pivoted year. Only 4-digit `"Jan 2020"` was probed. The
/// **`D-Mon-Y`** form keeps the 2-digit pivot — `"1-Jan-20"` → 2020 *is* pinned.
///
/// OXP (unassigned, review B5): the tokenizer accepts separator *arrangements*
/// beyond the strictly-probed ones (probed: `Mon D, YYYY` comma/space,
/// `Mon YYYY` space, `D-Mon-YYYY` hyphens). Variants like `"Jan-1-2020"`,
/// `"1 January 2020"`, `"1,Jan,2020"` are accepted by structural analogy —
/// queued for farm confirmation, not silently claimed correct.
fn parse_month_name_date(s: &str) -> DatePart {
    let tokens: Vec<&str> = s
        .split(|c: char| c == '-' || c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .collect();

    let mut month_idx: Option<usize> = None;
    let mut month = 0u32;
    for (i, t) in tokens.iter().enumerate() {
        if let Some(m) = month_from_name(t) {
            if month_idx.is_some() {
                return DatePart::Unsupported; // two month words — unrecognized
            }
            month_idx = Some(i);
            month = m;
        }
    }
    let mi = match month_idx {
        Some(i) => i,
        None => return DatePart::Unsupported,
    };

    match (tokens.len(), mi) {
        // `"Mon YYYY"` → the first day of the month (year token must be >=3
        // digits; a 1-2-digit trailing number is the no-year `Mon D` form).
        (2, 0) => match parse_year_wide(tokens[1]) {
            Some(year) => finish_ymd(year, month, 1),
            None => DatePart::Unsupported,
        },
        // `"Mon D, YYYY"` (year token must be >=3 digits).
        (3, 0) => match (parse_u32(tokens[1]), parse_year_wide(tokens[2])) {
            (Some(day), Some(year)) => finish_ymd(year, month, day),
            _ => DatePart::Unsupported,
        },
        // `"D-Mon-YYYY"` / `"D-Mon-YY"` — the pinned 2-digit pivot applies here.
        (3, 1) => match (parse_u32(tokens[0]), parse_year(tokens[2])) {
            (Some(day), Some(year)) => finish_ymd(year, month, day),
            _ => DatePart::Unsupported,
        },
        // Anything else (`"D Mon"` no-year, month last, …) is unrecognized.
        _ => DatePart::Unsupported,
    }
}

/// Validate a `(year, month, day)` and wrap it, or report [`DatePart::Invalid`]
/// (→ `#VALUE!`) for an impossible calendar date.
fn finish_ymd(year: i32, month: u32, day: u32) -> DatePart {
    // OXP (unassigned, review B4): Excel's phantom "1900-02-29" is accepted via
    // serial arithmetic (serial 60), but its *text*-coercion reading is unprobed
    // — Excel famously accepts it. Defer rather than reject it as `#VALUE!`
    // (proleptic 1900 is not a leap year, so `days_in_month` would say 28).
    if year == 1900 && month == 2 && day == 29 {
        return DatePart::Unsupported;
    }
    if (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month) {
        DatePart::Ymd(year, month, day)
    } else {
        DatePart::Invalid
    }
}

/// Days in `month` of `year` (proleptic Gregorian). The phantom Excel
/// "1900-02-29" is handled upstream in [`finish_ymd`] (it defers), so this pure
/// proleptic count never has to special-case it.
fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Proleptic-Gregorian leap-year test.
fn is_leap_year(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Map an English month name (abbreviation or full, case-insensitive) to
/// `1..=12`.
fn month_from_name(word: &str) -> Option<u32> {
    let w = word.to_ascii_lowercase();
    Some(match w.as_str() {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return None,
    })
}

/// Parse a 24-hour `H:MM[:SS]` clock string into a fraction-of-day in `[0, 1)`.
/// `None` for any shape outside that grammar (a meridiem, a 4th `:` group, or
/// an out-of-range minute/second/hour) so the caller can defer.
///
/// OXP (unassigned, review B5): the probed times are `"16:48"` (H:MM) and
/// `"16:48:00"` (H:MM:SS, standalone), and the probed *datetime* is
/// `"1/1/2020 16:48"` (no seconds). A datetime carrying seconds
/// (`"1/1/2020 16:48:30"`) is accepted by structural analogy — queued for farm
/// confirmation.
fn parse_clock(s: &str) -> Option<f64> {
    let mut parts = s.split(':');
    let hour = parse_u32(parts.next()?)?;
    let minute = parse_u32(parts.next()?)?;
    let second = match parts.next() {
        Some(sec) => parse_u32(sec)?,
        None => 0,
    };
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some(f64::from(hour * 3600 + minute * 60 + second) / 86_400.0)
}

/// Resolve a year token with the OXP-160 2-digit pivot: a 1-or-2-digit value
/// `00..=29` → `2000..=2029`, `30..=99` → `1930..=1999`; a longer token is
/// taken literally. Used by the pinned pivot forms (slash `M/D/YY`, `D-Mon-YY`).
fn parse_year(s: &str) -> Option<i32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: i32 = s.parse().ok()?;
    Some(if s.len() <= 2 {
        if value <= 29 {
            2000 + value
        } else {
            1900 + value
        }
    } else {
        value
    })
}

/// A **wide** year token: only a `>=3`-digit token is a year (taken literally;
/// no 2-digit pivot). `None` for a 1-2-digit token. Used by the forms where a
/// 1-2-digit trailing number is *not* a year — ISO leader (review B2) and the
/// month-first `Mon [D,] YYYY` forms (review B1, where it would be a no-year day).
fn parse_year_wide(s: &str) -> Option<i32> {
    if s.len() < 3 {
        return None;
    }
    parse_year(s)
}

/// Parse an all-ASCII-digit token as a `u32` (rejecting signs/spaces that
/// `str::parse` would otherwise tolerate for `+`).
fn parse_u32(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

// --- Self-contained 1900-system serial math (for `to_number` only) ----------
//
// `datecore` (with the workbook date-system flag) lives in `xl-fn`, which
// `xl-value` must not depend on. `to_number` is context-free, so it computes
// the default **1900-system** serial here. This mirrors `datecore` exactly
// (same public-domain Howard Hinnant `days_from_civil`, same phantom-day seam);
// `DATEVALUE` cross-checks the two by producing identical serials for the
// pinned dates in the 1900 system.

/// Days from 1970-01-01 to the proleptic-Gregorian date `(y, m, d)` (Howard
/// Hinnant, public domain — a widely-published calendar identity, not read from
/// any GPL source). `m` in `1..=12`, `d` in `1..=31`.
const fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Proleptic ordinal of 1899-12-31 — the day before 1900 serial 1.
const ANCHOR_1900: i64 = days_from_civil(1899, 12, 31);
/// Largest 1900-system serial (9999-12-31, incl. the phantom-day shift).
const MAX_SERIAL_1900: i64 = 2_958_465;

/// Convert a **valid** `(year, month, day)` to its 1900-system serial as an
/// `f64`, applying the Lotus-1-2-3 leap-year bug (serial 60 = the phantom
/// "1900-02-29"; every date from 1900-03-01 on is shifted `+1`). `None` if the
/// serial is outside the representable range `[1, 2_958_465]`.
fn ymd_to_serial_1900(year: i32, month: u32, day: u32) -> Option<f64> {
    let base = days_from_civil(i64::from(year), i64::from(month), i64::from(day)) - ANCHOR_1900;
    let serial = if base >= 60 { base + 1 } else { base };
    if (1..=MAX_SERIAL_1900).contains(&serial) {
        #[allow(clippy::cast_precision_loss)]
        Some(serial as f64)
    } else {
        None
    }
}

// ===========================================================================
// Text coercion
// ===========================================================================

/// Coerces a value to text (`to_text`).
///
/// Rules (Microsoft Learn "Calculation operators"; `TEXT`/`T` docs):
/// - `Text(t)` → `t`.
/// - `Number(n)` → General-format string (see the internal `general_format`).
/// - `Bool(true)` → `"TRUE"`, `Bool(false)` → `"FALSE"`.
/// - `Blank` → `""`.
/// - `Error(k)` → propagates `k`.
/// - `Array` 1×1 lift; larger → `#UNSUPPORTED!`
///   (`TODO(oracle-experiment)` OXP-004). `Ref` → `#UNSUPPORTED!`.
///
/// # Performance caveat (interner hot path)
/// Every number formatted here is routed through the **global string
/// interner** ([`Text::new`]). Computed-number strings are mostly *unique*,
/// so unlike workbook shared strings they gain nothing from deduplication
/// while still (a) growing the never-emptied global pool one entry per
/// distinct result and (b) taking the pool's mutex — a serialization point
/// for future parallel recalc. This is accepted for the v0 freeze because
/// the fix (a non-interning `Text` constructor or a scoped arena) is an
/// internal change that does not alter this signature; see the "Known
/// limitation" note in the `text` module for the swap plan.
///
/// # Errors
/// Propagates an [`ErrorKind`] for error inputs and unresolved refs/arrays.
pub fn to_text(value: &Value) -> Result<Text, ErrorKind> {
    match value {
        Value::Text(t) => Ok(t.clone()),
        Value::Number(n) => Ok(Text::new(&general_format(*n))),
        Value::Bool(b) => Ok(Text::new(if *b { "TRUE" } else { "FALSE" })),
        Value::Blank => Ok(Text::new("")),
        Value::Error(k) => Err(*k),
        Value::Array(a) => match a.as_scalar() {
            Some(inner) => to_text(inner),
            None => Err(ErrorKind::Unsupported),
        },
        Value::Ref(_) => Err(ErrorKind::Unsupported),
        // BC-6 (RFC-0012): a lambda has no text form — refuse loudly.
        Value::Lambda(_) => Err(ErrorKind::Unsupported),
    }
}

/// Formats a finite `f64` as Excel's **General** format for string coercion
/// (the `&` operator / `TEXT(x,"General")`).
///
/// Excel General rounds to **15 significant digits** (OXP-021,
/// RUN-2026-07-11-oracle01: `""&(1/3)` → `"0.333333333333333"`) and prints a
/// plain decimal iff **every significant digit** of the (15-sig-rounded) value
/// falls within the fixed base-10 place-value window `10^19 .. 10^-18`
/// (inclusive), switching to scientific notation otherwise. Equivalently, with
/// `msd`/`lsd` the exponents of the most/least significant digit: plain iff
/// `msd ≤ 19` **and** `lsd ≥ -18`. See [`fits_plain_window`].
///
/// This reproduces every pinned point (RUN-2026-07-11-oracle01):
/// - Large side (OXP-020/162): `""&10^19` → `"10000000000000000000"` (plain),
///   `""&10^20` → `"1E+20"`; `""&9.9*10^19` → `"99000000000000000000"` (plain,
///   two sig digits at `10^19`/`10^18`).
/// - Small side (OXP-020/162): `""&10^-18` → plain (one digit at `10^-18`),
///   `""&10^-19` → `"1E-19"`; `""&1.5*10^-18` → `"1.5E-18"` — the extra digit
///   sits at `10^-19`, *outside* the window, so it goes scientific even though
///   its magnitude (`1.5e-18`) is above `10^-18`. A pure magnitude band cannot
///   distinguish `10^-18` (plain) from `1.5*10^-18` (scientific); the
///   significant-digit window does.
///
/// The round-trip guarantee still holds: [`to_number`] reads the result back
/// to within 15-significant-digit tolerance (property-tested); both the plain
/// and scientific spellings round-trip.
fn general_format(x: f64) -> String {
    // Covers +0.0 and -0.0.
    if x == 0.0 {
        return "0".to_string();
    }

    // Rounding via format-and-reparse can overflow to infinity when `x` is
    // within half a decimal ULP of `f64::MAX` (the 15-significant-digit
    // decimal rounds *up* past the largest finite double). The output must
    // never be non-finite for finite input, so fall back to the unrounded
    // value in that case.
    let rounded = round_to_sig(x, 15);
    let rounded = if rounded.is_finite() { rounded } else { x };
    if rounded == 0.0 {
        return "0".to_string();
    }

    if fits_plain_window(rounded) {
        // Rust's `Display` for f64 is the shortest string that round-trips to
        // `rounded`, always in fixed (non-scientific) notation.
        format!("{rounded}")
    } else {
        scientific_format(rounded)
    }
}

/// Whether every significant digit of the finite, non-zero `rounded` value fits
/// the General plain-decimal place-value window `10^19 .. 10^-18` (OXP-020/162,
/// RUN-2026-07-11-oracle01). See [`general_format`] for the pinned crossovers.
///
/// Works off the shortest round-tripping scientific spelling (`{:e}`): its
/// exponent is the most-significant-digit place, and its mantissa digit count
/// gives the least-significant-digit place.
///
/// OXP (unassigned, review B5): the exact crossovers are pinned at
/// `10^19`/`9.9*10^19`/`10^20` (large) and `10^-18`/`1.5*10^-18`/`10^-19`
/// (small); intermediate multi-digit values in the flip region (e.g.
/// `"9.9*10^-18"`, `"1.5*10^19"`) follow this window rule by construction but
/// were not each individually probed — queued for a confirming sweep.
fn fits_plain_window(rounded: f64) -> bool {
    let sci = format!("{:e}", rounded.abs());
    let Some((mantissa, exp)) = sci.split_once('e') else {
        // Defensive: a finite value always has an exponent in `{:e}`.
        return true;
    };
    let msd_exp: i32 = exp.parse().unwrap_or(0);
    let sig_digits = mantissa.bytes().filter(u8::is_ascii_digit).count() as i32;
    let lsd_exp = msd_exp - (sig_digits - 1);
    msd_exp <= 19 && lsd_exp >= -18
}

/// Rounds `x` to `sig` significant decimal digits (via a scientific
/// round-trip). `sig` is expected to be ≥ 1.
///
/// **May return infinity for a finite `x` near `f64::MAX`** (the decimal
/// rounds up past the largest finite double); the caller is responsible for
/// checking finiteness of the result.
fn round_to_sig(x: f64, sig: u32) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let digits = sig.saturating_sub(1) as usize;
    let s = format!("{x:.digits$e}");
    s.parse::<f64>().unwrap_or(x)
}

/// Formats `x` in Excel-style scientific notation: uppercase `E`, an explicit
/// exponent sign, and at least two exponent digits (`1.23E+20`, `1E-05`).
fn scientific_format(x: f64) -> String {
    // Rust's `{:e}` gives a shortest-round-tripping mantissa and a signless,
    // zero-padless exponent, e.g. "1.23e20" / "1e-5" / "-4e-7".
    let raw = format!("{x:e}");
    match raw.split_once('e') {
        Some((mantissa, exp)) => {
            let exp_val: i32 = exp.parse().unwrap_or(0);
            let sign = if exp_val < 0 { '-' } else { '+' };
            format!("{mantissa}E{sign}{:02}", exp_val.abs())
        }
        // Reachable only for non-finite `x` (`{:e}` on inf/NaN yields
        // "inf"/"NaN" with no exponent). Callers pass finite values only —
        // `general_format` falls back to the unrounded input if 15-digit
        // rounding overflows — so this stays a defensive no-panic arm.
        None => raw,
    }
}

// ===========================================================================
// Bool coercion
// ===========================================================================

/// Coerces a value to a boolean (`to_bool`).
///
/// Rules (Microsoft Learn logical-function docs; hit-list):
/// - `Bool(b)` → `b`.
/// - `Number(0)` → `false`, any other finite number → `true`.
/// - `Text` → case-insensitive `"TRUE"`/`"FALSE"` only; **any other text**
///   (including numeric text like `"1"`) → `#VALUE!`.
/// - `Blank` → `false`.
/// - `Error(k)` → propagates `k`.
/// - `Array` 1×1 lift; larger → `#UNSUPPORTED!`. `Ref` → `#UNSUPPORTED!`.
///
/// # Errors
/// Returns `#VALUE!` for non-boolean text and propagates error inputs.
pub fn to_bool(value: &Value) -> Result<bool, ErrorKind> {
    match value {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => Ok(*n != 0.0),
        Value::Blank => Ok(false),
        Value::Error(k) => Err(*k),
        Value::Text(t) => {
            if t.as_str().eq_ignore_ascii_case("TRUE") {
                Ok(true)
            } else if t.as_str().eq_ignore_ascii_case("FALSE") {
                Ok(false)
            } else {
                Err(ErrorKind::Value)
            }
        }
        Value::Array(a) => match a.as_scalar() {
            Some(inner) => to_bool(inner),
            None => Err(ErrorKind::Unsupported),
        },
        Value::Ref(_) => Err(ErrorKind::Unsupported),
        // BC-6 (RFC-0012): a lambda is not a logical — refuse loudly.
        Value::Lambda(_) => Err(ErrorKind::Unsupported),
    }
}

// ===========================================================================
// Comparison semantics
// ===========================================================================

/// Excel comparison of two scalar values for the `=`, `<>`, `<`, `<=`, `>`,
/// `>=` operators.
///
/// Rules (hit-list "case-insensitive string equality"; ECMA-376 §18.17;
/// Microsoft Learn operator docs):
/// - **Errors propagate**, leftmost first: if `a` is an error, its kind is
///   returned; else if `b` is an error, its kind.
/// - **Text is compared case-insensitively** (`"a" = "A"` → equal). Ordering
///   folds case; see the v0 collation caveat below.
/// - **Cross-type ordering** for `<`/`>`: any number `<` any text `<` `FALSE`
///   `<` `TRUE`. So `Number < Text < Bool`.
/// - **`Blank` morphs by the other operand:** `0` versus a number, `""`
///   versus text, `Bool(false)` versus a bool (OXP-030,
///   RUN-2026-07-11-oracle01: `A1=TRUE` → FALSE, `A1=FALSE` → TRUE,
///   `A1<TRUE` → TRUE, `A1>FALSE` → FALSE), and equal to another `Blank`.
///
/// ### Deliberately deferred
/// - Any comparison where either operand is **non-ASCII text** returns
///   `#UNSUPPORTED!` (OXP-031, RUN-2026-07-11-oracle01, HELD). Excel's
///   locale collation is provably *not* code-point order (it ranks `"ä"` <
///   `"z"` and treats `"ß"` = `"ss"`); until a collation probe sweep pins the
///   ordering, a non-ASCII comparison defers rather than returning a wrong
///   verdict. ASCII case-insensitive equality/ordering is correct and stays.
/// - `Array`/`Ref` operands must be resolved first; here they return
///   `#UNSUPPORTED!`.
///
/// # Errors
/// Returns the propagated/derived [`ErrorKind`] when a comparison cannot be
/// performed.
pub fn compare(a: &Value, b: &Value) -> Result<Ordering, ErrorKind> {
    compare_with(a, b, cmp_f64)
}

/// Excel-faithful (**fuzzy**) comparison — RFC 0009. Identical to [`compare`]
/// except that two `Number`s (and the `Blank`↔`Number` morph) compare under
/// Excel's ~15-significant-figure rule ([`cmp_f64_fuzzy`]) instead of exact
/// IEEE. This is what the comparison **operators** use (`ops.rs::compare_op`);
/// no other caller is switched (RFC 0009 §4 — lookups/criteria/min-max stay on
/// [`compare`] until each is separately probed). `total_order` stays bit-exact.
///
/// # Errors
/// Same as [`compare`] (error propagation, non-ASCII text deferral).
pub fn compare_fuzzy(a: &Value, b: &Value) -> Result<Ordering, ErrorKind> {
    compare_with(a, b, cmp_f64_fuzzy)
}

/// The shared comparison body, parameterized by the `Number`×`Number`
/// comparator so exact ([`compare`]) and Excel-fuzzy ([`compare_fuzzy`]) share
/// one implementation of all the type/coercion arms.
fn compare_with(
    a: &Value,
    b: &Value,
    cmp_num: impl Fn(f64, f64) -> Ordering,
) -> Result<Ordering, ErrorKind> {
    // Error propagation, leftmost first.
    if let Value::Error(k) = a {
        return Err(*k);
    }
    if let Value::Error(k) = b {
        return Err(*k);
    }

    // OXP-031 (RUN-2026-07-11-oracle01, HELD): non-ASCII text collation is
    // locale-sensitive and our code-point fold is provably wrong — defer any
    // comparison involving non-ASCII text rather than return a wrong verdict.
    if has_non_ascii_text(a) || has_non_ascii_text(b) {
        return Err(ErrorKind::Unsupported);
    }

    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(cmp_num(*x, *y)),
        (Value::Bool(x), Value::Bool(y)) => Ok(x.cmp(y)),
        (Value::Text(x), Value::Text(y)) => Ok(cmp_text_ci(x.as_str(), y.as_str())),
        (Value::Blank, Value::Blank) => Ok(Ordering::Equal),

        // Blank morphs to 0 vs numbers, "" vs text.
        (Value::Blank, Value::Number(y)) => Ok(cmp_num(0.0, *y)),
        (Value::Number(x), Value::Blank) => Ok(cmp_num(*x, 0.0)),
        (Value::Blank, Value::Text(y)) => Ok(cmp_text_ci("", y.as_str())),
        (Value::Text(x), Value::Blank) => Ok(cmp_text_ci(x.as_str(), "")),

        // OXP-030 (RUN-2026-07-11-oracle01): a Blank compared with a Bool
        // morphs to Bool(false).
        (Value::Blank, Value::Bool(y)) => Ok(false.cmp(y)),
        (Value::Bool(x), Value::Blank) => Ok(x.cmp(&false)),

        // Rider (ii) — RFC-0012 BC-6: `=`/`<>`/ordering on a lambda is
        // **unpinned** (no oracle experiment). Refuse **explicitly** here
        // rather than lean on the `scalar_type_rank` → `None` → `Err` accident
        // below, and do **not** define lambda equality from memory — Excel's
        // behavior comparing two LAMBDAs is unknown. Keep refusing until an OXP
        // pins it. (Placed above the cross-type fallback so it wins.)
        (Value::Lambda(_), _) | (_, Value::Lambda(_)) => Err(ErrorKind::Unsupported),

        // Remaining scalar cross-type pairs: order by type rank
        // (Number < Text < Bool). Array/Ref are not comparable here. Named
        // bindings (not a bare `_`) so the BC-6 guard holds and a new `Value`
        // variant forces a decision.
        (lhs, rhs) => match (scalar_type_rank(lhs), scalar_type_rank(rhs)) {
            (Some(ra), Some(rb)) => Ok(ra.cmp(&rb)),
            (None, _) | (_, None) => Err(ErrorKind::Unsupported),
        },
    }
}

/// Convenience: `true` iff `a` and `b` compare **exactly** equal (via
/// [`compare`], case-insensitive text, `Blank` morphs). Used by exact-match
/// lookups and other callers that need bit-exact numeric equality. NB: the `=`
/// **operator** does NOT route through this — it uses [`compare_fuzzy`] (Excel's
/// ~15-sig-fig fuzz, RFC 0009). Errors propagate.
///
/// # Errors
/// Propagates the [`ErrorKind`] from [`compare`].
pub fn values_equal(a: &Value, b: &Value) -> Result<bool, ErrorKind> {
    compare(a, b).map(|o| o == Ordering::Equal)
}

/// Case-insensitive text equality (Excel's `=` on two strings). Pure ASCII/
/// Unicode-lowercase fold; the ordering caveat of [`compare`] applies.
#[must_use]
pub fn text_eq_ci(a: &str, b: &str) -> bool {
    cmp_text_ci(a, b) == Ordering::Equal
}

/// Whether `v` is a `Text` value containing at least one non-ASCII character.
/// Used to defer locale-sensitive collation in [`compare`] (OXP-031, held).
fn has_non_ascii_text(v: &Value) -> bool {
    matches!(v, Value::Text(t) if !t.as_str().is_ascii())
}

/// Cross-type rank for the scalar comparison order `Number < Text < Bool`.
/// `None` for anything not directly comparable (`Blank` is handled by
/// morphing before this is consulted; `Array`/`Ref`/`Error` are excluded).
fn scalar_type_rank(v: &Value) -> Option<u8> {
    match v {
        Value::Number(_) => Some(0),
        Value::Text(_) => Some(1),
        Value::Bool(_) => Some(2),
        // Not directly comparable by cross-type rank: `Blank` morphs before
        // this is consulted, `Error` propagates earlier, and `Array`/`Ref`/
        // `Lambda` are refused by the caller. Enumerated (no bare `_`) so the
        // next `Value` variant breaks the build here — BC-6 (RFC-0012).
        Value::Blank | Value::Error(_) | Value::Array(_) | Value::Ref(_) | Value::Lambda(_) => None,
    }
}

/// Compares two finite `f64`s. `NaN` cannot occur (value invariant); a
/// defensive `Equal` is returned if it somehow does, to never panic.
fn cmp_f64(x: f64, y: f64) -> Ordering {
    x.partial_cmp(&y).unwrap_or(Ordering::Equal)
}

/// Excel's ~15-significant-figure **fuzzy** comparison of two `f64`s (RFC 0009,
/// pinned by OXP-179/180/181/182). Excel treats two numbers as equal when they
/// agree to ~15 significant decimal figures (so `0.1+0.2 = 0.3` is TRUE); the
/// ordering operators respect it (a fuzz-equal pair is neither less nor
/// greater). Used only by the comparison **operators** via [`compare_fuzzy`].
///
/// **Algorithm A** (the OXP-182 grid bit-confirmed it and ruled out a
/// delta-threshold via the nonzero-vs-0 rows): round both operands to 15
/// significant figures, then compare exactly. Guarded so the (allocating)
/// [`round_to_sig`] runs only for genuine near-ties:
/// 1. exact-equal fast path (covers `±0` and the common case);
/// 2. non-finite → exact (`±inf` order exactly; NaN can't occur — defensive);
/// 3. band guard: operands farther apart than `1e-13 · max(|x|,|y|)` (a 10×
///    margin above the worst-case merge distance, `~1e-14 · scale` within a
///    decade) get the exact order, which equals the rounded order — no rounding;
/// 4. near-band: round both to 15 sig figs and compare, falling back to exact if
///    rounding pushed a finite operand to non-finite (near-`f64::MAX`, RFC 0009
///    §7.5, unpinned).
fn cmp_f64_fuzzy(x: f64, y: f64) -> Ordering {
    if x == y {
        return Ordering::Equal;
    }
    if !x.is_finite() || !y.is_finite() {
        return cmp_f64(x, y);
    }
    let diff = (x - y).abs();
    let scale = x.abs().max(y.abs());
    if diff > 1e-13 * scale {
        return cmp_f64(x, y);
    }
    let rx = round_to_sig(x, 15);
    let ry = round_to_sig(y, 15);
    if !rx.is_finite() || !ry.is_finite() {
        return cmp_f64(x, y);
    }
    cmp_f64(rx, ry)
}

/// Case-insensitive comparison of two strings by lowercased code points.
fn cmp_text_ci(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

// ===========================================================================
// Total order (for sort-later; NOT yet Excel SORT semantics)
// ===========================================================================

/// A **deterministic total order** over *all* [`Value`]s, for internal use
/// where a stable, panic-free ordering is required (dedup, canonicalizing,
/// and eventually `SORT`/`UNIQUE`).
///
/// Unlike [`compare`], this:
/// - is defined for every value (including `Error`, `Array`, `Ref`),
/// - never returns an error,
/// - is a genuine total order (reflexive, antisymmetric, transitive, total) —
///   in particular text ties broken case-insensitively are further broken by
///   exact code points, so distinct values never compare `Equal`.
///
/// It is a *mechanical* order. Cross-type buckets are `Blank < Number < Text
/// < Bool < Error < Array < Ref < Lambda`. **This is not Excel's `SORT`
/// ordering** (which places blanks last, has its own error handling, and locale
/// collation) — that mapping is `TODO(oracle-experiment)` OXP-040 and will
/// wrap this helper rather than replace it.
///
/// **Lambda carve-out (RFC-0012 BC-6):** a lambda cross-orders deterministically
/// via its bucket, but two *distinct* lambdas must never reach the same-bucket
/// arm — SORT/UNIQUE-class consumers refuse lambdas upstream, so that arm is
/// `unreachable!` (identical `Arc`s excepted, which are genuinely `Equal`). It
/// panics loudly in every build rather than fabricate a non-run-stable order.
#[must_use]
pub fn total_order(a: &Value, b: &Value) -> Ordering {
    fn bucket(v: &Value) -> u8 {
        match v {
            Value::Blank => 0,
            Value::Number(_) => 1,
            Value::Text(_) => 2,
            Value::Bool(_) => 3,
            Value::Error(_) => 4,
            Value::Array(_) => 5,
            Value::Ref(_) => 6,
            // RFC-0012 BC-6 (rider i): lambdas get their own highest bucket, so
            // cross-bucket ordering against every other type is deterministic
            // and stable — never derived from the `Arc` address. Two lambdas
            // (same bucket) are handled in the pair match below.
            Value::Lambda(_) => 7,
        }
    }

    let (ba, bb) = (bucket(a), bucket(b));
    if ba != bb {
        return ba.cmp(&bb);
    }

    match (a, b) {
        (Value::Blank, Value::Blank) => Ordering::Equal,
        // `total_cmp` gives a total order over all f64 bit patterns; values
        // are finite so this coincides with numeric order (–0 < +0, which is
        // fine for a mechanical total order but note it differs from Excel
        // equality — that lives in `compare`).
        (Value::Number(x), Value::Number(y)) => x.total_cmp(y),
        (Value::Text(x), Value::Text(y)) => {
            cmp_text_ci(x.as_str(), y.as_str()).then_with(|| x.as_str().cmp(y.as_str()))
        }
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Error(x), Value::Error(y)) => (*x as u8).cmp(&(*y as u8)),
        (Value::Array(x), Value::Array(y)) => total_order_array(x, y),
        (Value::Ref(x), Value::Ref(y)) => x.sheet.cmp(&y.sheet).then_with(|| x.range.cmp(&y.range)),
        // RFC-0012 BC-6 (rider i): two lambdas share bucket 7, but distinct
        // lambdas MUST NOT compare `Equal` (that would collapse them in
        // SORT/UNIQUE dedup — a Principle-2 silent-wrong) and MUST NOT be
        // ordered by their `Arc` pointer address (not run-stable →
        // non-deterministic SORT). SORT/UNIQUE-class consumers refuse lambdas
        // *before* consulting `total_order`, so this arm is unreachable by
        // construction for *distinct* lambdas. Identical `Arc`s (`x == y` is
        // `Arc::ptr_eq`) are genuinely equal and safe to return `Equal`;
        // anything else is a consumer bug we fail LOUDLY on — in release too
        // (a bare `debug_assert!(false); Equal` would silently mis-dedup in
        // release). TODO(OXP-040): true SORT order for lambdas unpinned.
        (Value::Lambda(x), Value::Lambda(y)) => {
            if x == y {
                Ordering::Equal
            } else {
                unreachable!(
                    "total_order reached two distinct lambdas: SORT/UNIQUE-class \
                     consumers must refuse lambdas upstream (TODO(OXP-040))"
                )
            }
        }
        // Unreachable: the `ba != bb` early return above guarantees equal
        // buckets, and every same-bucket pair is enumerated. Reaching here
        // means a new `Value` variant was added without a self-pair arm — the
        // witness test (`witness_every_value_variant`) trips on it. Fail loudly
        // (release included) rather than fabricate an order. Named bindings, not
        // a bare `_`, so the BC-6 guard holds.
        (other_a, other_b) => unreachable!(
            "total_order: unhandled same-bucket Value pair {other_a:?} vs \
             {other_b:?} — a new variant needs an explicit self-pair arm"
        ),
    }
}

/// Total order over arrays: by shape, then elementwise in row-major order.
fn total_order_array(x: &Array, y: &Array) -> Ordering {
    x.rows()
        .cmp(&y.rows())
        .then_with(|| x.cols().cmp(&y.cols()))
        .then_with(|| {
            for (ex, ey) in x.iter().zip(y.iter()) {
                let o = total_order(ex, ey);
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        })
}

#[cfg(test)]
mod fuzzy_compare_tests {
    use super::*;

    /// Reference: the fuzzy rule with NO fast-path / band-guard — always round
    /// both to 15 sig figs and compare. `cmp_f64_fuzzy` must equal this exactly.
    fn unguarded(x: f64, y: f64) -> Ordering {
        if !x.is_finite() || !y.is_finite() {
            return cmp_f64(x, y);
        }
        let rx = round_to_sig(x, 15);
        let ry = round_to_sig(y, 15);
        if !rx.is_finite() || !ry.is_finite() {
            return cmp_f64(x, y);
        }
        cmp_f64(rx, ry)
    }

    // RFC 0009 regression grid — every row is a farm-pinned OXP-180/181/182
    // observation (Excel build 16.0). `cmp_f64_fuzzy` must reproduce all of them.
    #[test]
    fn cmp_f64_fuzzy_reproduces_oxp_grid() {
        let eq = |x: f64, y: f64| cmp_f64_fuzzy(x, y) == Ordering::Equal;
        // Boundary sweep (mag 1): 4e-15 equal, 5e-15 not (OXP-180/181).
        assert!(eq(1.0 + 4e-15, 1.0));
        assert!(!eq(1.0 + 5e-15, 1.0));
        // Multi-exponent (OXP-182).
        assert!(!eq(8.0 + 3e-14, 8.0));
        assert!(!eq(8.0 + 4e-14, 8.0));
        assert!(eq(1024.0 + 3e-12, 1024.0));
        assert!(eq(0.125 + 4e-16, 0.125));
        // Nonzero-vs-0: algorithm A — no nonzero ever fuzz-equals 0 (rules out B).
        assert!(!eq(1e-15, 0.0));
        assert!(!eq(1e-19, 0.0));
        assert!(!eq(1e-300, 0.0));
        assert!(eq(1.0 - 1.0, 0.0));
        // Negatives / cross-zero.
        assert!(eq(-1.0 - 4e-15, -1.0));
        assert!(!eq(1e-15, -1e-15));
        // Large magnitude.
        assert!(eq(1e15 + 1.0, 1e15));
        assert!(!eq(1e15 + 10.0, 1e15));
        // Canonical.
        assert!(eq(0.1 + 0.2, 0.3));
        assert!(eq(16.0 / 9.0, 1.777_777_777_777_78));
        // Remaining pinned OXP-180/181/182 rows (completeness — every pin encoded).
        assert!(eq(1.0 + 2e-15, 1.0)); // OXP-180
        assert!(!eq(1.0 + 1e-14, 1.0)); // OXP-180
        assert!(eq(1.0 / 3.0, 0.333_333_333_333_333)); // OXP-180
        assert!(eq(1e10 + 1e-5, 1e10)); // OXP-180 (mag 1e10, rel 1e-15)
        assert!(!eq(1e10 + 1e-3, 1e10)); // OXP-180 (rel 1e-13)
        assert!(eq(1.0 + 4.5e-15, 1.0)); // OXP-181 (upper edge, still equal)
        assert!(!eq(2.0 + 7e-15, 2.0)); // OXP-181 (mag 2)
        assert!(eq(100.0 + 5e-13, 100.0)); // OXP-181 (mag 100)
        assert!(eq(1e19 + 1e3, 1e19)); // OXP-182 (mag 1e19)
        // Ordering respects the fuzz: a fuzz-equal pair is neither < nor >.
        assert_eq!(cmp_f64_fuzzy(1.0 + 4e-15, 1.0), Ordering::Equal);
        assert_eq!(cmp_f64_fuzzy(1.0, 1.0 + 4e-15), Ordering::Equal);
        // ...but a clearly-different pair still orders exactly.
        assert_eq!(cmp_f64_fuzzy(1.0, 2.0), Ordering::Less);
        assert_eq!(cmp_f64_fuzzy(2.0, 1.0), Ordering::Greater);
    }

    // RFC 0009 §7.4: the fast-path + band-guard must NEVER change the result
    // versus always rounding-then-comparing.
    #[test]
    fn cmp_f64_fuzzy_guarded_equals_unguarded() {
        let bases = [
            0.0, 1.0, -1.0, 0.125, 8.0, 1024.0, 1e6, 1e15, 1e-9, -3.5, 0.3,
        ];
        let deltas = [
            0.0, 1e-16, 1e-15, 4e-15, 5e-15, 1e-14, 1e-13, 1e-12, 1e-6, 1.0, -2e-15, -1e-13,
        ];
        for &b in &bases {
            for &d in &deltas {
                let x = b + d;
                assert_eq!(cmp_f64_fuzzy(x, b), unguarded(x, b), "x={x} b={b}");
                assert_eq!(cmp_f64_fuzzy(b, x), unguarded(b, x), "b={b} x={x}");
            }
        }
    }

    // The exact `compare` path (used by lookups/criteria/min-max) is UNCHANGED —
    // it must still report `0.1+0.2 != 0.3`. Only the operators fuzz.
    #[test]
    fn exact_compare_is_untouched() {
        let a = Value::Number(0.1 + 0.2);
        let b = Value::Number(0.3);
        assert_ne!(compare(&a, &b), Ok(Ordering::Equal));
        assert_eq!(compare_fuzzy(&a, &b), Ok(Ordering::Equal));
    }
}

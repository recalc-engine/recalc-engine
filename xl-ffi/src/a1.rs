//! A tiny, self-contained parser for a single **A1 cell address** (e.g.
//! `"B2"`), used by the Python binding's `Workbook.cell(sheet, a1)`
//! convenience.
//!
//! # What it accepts
//! One cell reference of the form `[$]COLUMN[$]ROW`:
//! * `COLUMN` — one or more ASCII letters (case-insensitive), read as Excel's
//!   **bijective base-26** column label (`A`→1, `Z`→26, `AA`→27, …).
//! * `ROW` — one or more ASCII digits, a 1-based row number.
//! * A single optional `$` absolute-reference marker may precede the column
//!   and/or the row (`$B$2`, `B$2`, `$B2`, `B2` all parse identically).
//!
//! Both components are returned **0-based** as `(row, col)` — the coordinate
//! convention [`xl_engine::Engine::value`] and [`xl_value::RectRange`] use, so
//! `"A1"` → `(0, 0)` and `"B2"` → `(1, 1)`.
//!
//! # What it rejects (loudly — never a silent guess)
//! Anything else is an [`A1Error`]: an empty string, a missing column or row
//! part, a `0` row, trailing junk, embedded whitespace, a sheet qualifier
//! (`"Sheet1!A1"` — pass the sheet separately to `value`/`cell`), a range
//! (`"A1:B2"`), or a column/row so large it overflows `u32`. This upholds the
//! project's "never silently wrong" principle at the binding boundary
//! (the Recalc design rules §0).
//!
//! # Provenance
//! Column-label arithmetic is the standard ECMA-376 A1 addressing scheme
//! (bijective base-26); this is a clean-room implementation with no external
//! dependency.

use core::fmt;

/// The reason an A1 address string was rejected by [`parse_a1`].
///
/// Carries the offending input plus a short human-readable reason; the Python
/// binding surfaces `to_string()` as the message of a `ValueError`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct A1Error {
    /// The full input string that failed to parse.
    pub input: String,
    /// A short explanation of what was wrong.
    pub reason: &'static str,
}

impl A1Error {
    fn new(input: &str, reason: &'static str) -> A1Error {
        A1Error {
            input: input.to_string(),
            reason,
        }
    }
}

impl fmt::Display for A1Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid A1 cell address {:?}: {}",
            self.input, self.reason
        )
    }
}

impl std::error::Error for A1Error {}

/// Parses a single A1 cell address into **0-based** `(row, col)` coordinates.
///
/// See the [module docs](self) for the exact accepted grammar and the full
/// list of rejected shapes. `"B2"` → `Ok((1, 1))`; `"A1"` → `Ok((0, 0))`;
/// `"$C$10"` → `Ok((9, 2))`.
///
/// # Errors
/// Returns [`A1Error`] for any malformed address (empty, missing column or
/// row, `0` row, trailing characters, whitespace, sheet qualifier, range, or
/// `u32` overflow).
pub fn parse_a1(addr: &str) -> Result<(u32, u32), A1Error> {
    let bytes = addr.as_bytes();
    let mut i = 0;

    // Optional absolute-column marker.
    if bytes.first() == Some(&b'$') {
        i += 1;
    }

    // Column letters → 1-based bijective base-26.
    let col_start = i;
    let mut col: u32 = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        let digit = u32::from(bytes[i].to_ascii_uppercase() - b'A') + 1; // 1..=26
        col = col
            .checked_mul(26)
            .and_then(|c| c.checked_add(digit))
            .ok_or_else(|| A1Error::new(addr, "column label is too large"))?;
        i += 1;
    }
    if i == col_start {
        return Err(A1Error::new(addr, "expected one or more column letters"));
    }

    // Optional absolute-row marker.
    if i < bytes.len() && bytes[i] == b'$' {
        i += 1;
    }

    // Row digits → 1-based row number.
    let row_start = i;
    let mut row: u32 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        let digit = u32::from(bytes[i] - b'0');
        row = row
            .checked_mul(10)
            .and_then(|r| r.checked_add(digit))
            .ok_or_else(|| A1Error::new(addr, "row number is too large"))?;
        i += 1;
    }
    if i == row_start {
        return Err(A1Error::new(addr, "expected a row number after the column"));
    }

    // Nothing may follow the row (rejects "A1:B2", "A1 ", "Sheet1!A1", ...).
    if i != bytes.len() {
        return Err(A1Error::new(addr, "unexpected trailing characters"));
    }
    if row == 0 {
        return Err(A1Error::new(addr, "row number must be 1 or greater"));
    }

    // Both parts are 1-based here; convert to the engine's 0-based coordinates.
    Ok((row - 1, col - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_addresses() {
        assert_eq!(parse_a1("A1"), Ok((0, 0)));
        assert_eq!(parse_a1("B2"), Ok((1, 1)));
        assert_eq!(parse_a1("Z1"), Ok((0, 25)));
        assert_eq!(parse_a1("A100"), Ok((99, 0)));
    }

    #[test]
    fn multi_letter_columns_are_bijective_base26() {
        assert_eq!(parse_a1("AA1"), Ok((0, 26))); // 26*1 + 1 = 27 → 0-based 26
        assert_eq!(parse_a1("AZ1"), Ok((0, 51)));
        assert_eq!(parse_a1("BA1"), Ok((0, 52)));
        // Excel's last column, XFD (16384), is 0-based 16383.
        assert_eq!(parse_a1("XFD1"), Ok((0, 16383)));
    }

    #[test]
    fn case_insensitive_columns() {
        assert_eq!(parse_a1("a1"), Ok((0, 0)));
        assert_eq!(parse_a1("aa1"), Ok((0, 26)));
        assert_eq!(parse_a1("aA1"), parse_a1("AA1"));
    }

    #[test]
    fn absolute_markers_are_accepted() {
        let plain = parse_a1("C10").unwrap();
        assert_eq!(parse_a1("$C$10"), Ok(plain));
        assert_eq!(parse_a1("$C10"), Ok(plain));
        assert_eq!(parse_a1("C$10"), Ok(plain));
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",          // empty
            "$",         // marker only
            "$$A1",      // doubled marker
            "1",         // no column
            "1A",        // digits before letters
            "A",         // no row
            "A0",        // zero row (A1 rows are 1-based)
            "AB",        // no row after multi-letter column
            "A1B",       // trailing letter
            "A1:B2",     // a range, not a cell
            "A 1",       // embedded space
            " A1",       // leading space
            "A1 ",       // trailing space
            "Sheet1!A1", // sheet-qualified (pass the sheet separately)
            "A-1",       // negative-looking
            "A1.5",      // fractional row
            "€1",        // non-ASCII column
        ] {
            assert!(parse_a1(bad).is_err(), "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn row_zero_is_distinct_from_missing_row() {
        // "A0" reaches the row parser (digit present) but is rejected for the
        // 1-based invariant, with a different reason than a missing row.
        let zero = parse_a1("A0").unwrap_err();
        assert_eq!(zero.reason, "row number must be 1 or greater");
        let missing = parse_a1("A").unwrap_err();
        assert_eq!(missing.reason, "expected a row number after the column");
    }

    #[test]
    fn column_overflow_is_rejected_not_wrapped() {
        // A 40-letter column label vastly overflows u32 and must error rather
        // than silently wrap.
        let huge = "A".repeat(40) + "1";
        assert!(parse_a1(&huge).is_err());
    }

    #[test]
    fn row_overflow_is_rejected_not_wrapped() {
        let huge = format!("A{}", "9".repeat(20));
        assert!(parse_a1(&huge).is_err());
    }
}

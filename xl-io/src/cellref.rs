//! A1 cell-reference parsing for `<c r="...">` attributes.
//!
//! ## Provenance
//! Column-letter encoding (`A`=1, ..., `Z`=26, `AA`=27, ...) and the `A1`
//! grammar are ECMA-376 §18.17.2 / the general A1 reference notation
//! documented at Microsoft Learn "Overview of formulas". This module parses
//! only the bare, unqualified `COLROW` form found in `<c r="...">` (no sheet
//! name, no `$`) — anything richer than that is `xl-ast`'s grammar, not this
//! crate's.
//!
//! This crate never guesses on a malformed reference: anything that doesn't
//! match `[A-Z]+[0-9]+` exactly is a [`crate::IoError::Structure`].

use crate::error::IoError;

/// Parses an OOXML cell reference like `"A1"` or `"XFD1048576"` into
/// **0-based** `(row, col)`, matching [`xl_value::RectRange`]'s convention.
pub(crate) fn parse_a1(part: &str, r: &str) -> Result<(u32, u32), IoError> {
    let split = r.find(|c: char| c.is_ascii_digit());
    let (col_str, row_str) = match split {
        Some(i) if i > 0 => (&r[..i], &r[i..]),
        _ => return Err(bad_ref(part, r)),
    };
    let col = parse_col(col_str).ok_or_else(|| bad_ref(part, r))?;
    let row: u32 = row_str.parse().map_err(|_| bad_ref(part, r))?;
    if row == 0 || row > 1_048_576 {
        return Err(bad_ref(part, r));
    }
    Ok((row - 1, col))
}

/// Parses a bare column-letter string (`"A"`, `"XFD"`, ...) into a 0-based
/// column index. Excel's grid is capped at column `XFD` (16384); this
/// rejects anything larger rather than silently wrapping, which matters for
/// hostile input (an absurdly long letter run must not be treated as a valid
/// huge index).
fn parse_col(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 3 || !s.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let mut n: u32 = 0;
    for b in s.bytes() {
        n = n.checked_mul(26)?.checked_add(u32::from(b - b'A') + 1)?;
    }
    if n == 0 || n > 16_384 {
        return None;
    }
    Some(n - 1)
}

fn bad_ref(part: &str, r: &str) -> IoError {
    IoError::structure(part, format!("`{r}` is not a valid cell reference"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_refs() {
        assert_eq!(parse_a1("p", "A1").unwrap(), (0, 0));
        assert_eq!(parse_a1("p", "B1").unwrap(), (0, 1));
        assert_eq!(parse_a1("p", "A2").unwrap(), (1, 0));
        assert_eq!(parse_a1("p", "Z1").unwrap(), (0, 25));
        assert_eq!(parse_a1("p", "AA1").unwrap(), (0, 26));
        assert_eq!(parse_a1("p", "XFD1048576").unwrap(), (1_048_575, 16_383));
    }

    #[test]
    fn rejects_malformed_refs() {
        assert!(parse_a1("p", "").is_err());
        assert!(parse_a1("p", "1A").is_err());
        assert!(parse_a1("p", "A0").is_err());
        assert!(parse_a1("p", "A").is_err());
        assert!(parse_a1("p", "1").is_err());
        assert!(parse_a1("p", "XFE1").is_err());
        assert!(parse_a1("p", "a1").is_err());
        assert!(parse_a1("p", "AAAA1").is_err());
        assert!(parse_a1("p", "A1048577").is_err());
        assert!(parse_a1("p", "A1048576").is_ok());
    }
}

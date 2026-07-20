//! A1-style address rendering.
//!
//! `xl-io` stores cells at **0-based** `(row, col)` coordinates
//! (`xl_io::Sheet::cells`'s documented convention); this harness reports
//! addresses the way a human (and `docs/sidecar-format.md`'s `cell` column)
//! expects to read them — 1-based row numbers, base-26 column letters
//! (`A`, `B`, ..., `Z`, `AA`, ...). This is a report-formatting concern only;
//! `xl-io`'s own `cellref` module (which does the inverse: A1 text →
//! 0-based coordinates) is private to that crate, so this is a small,
//! independent re-implementation of the encode direction, not a reuse of
//! private API.

/// Renders a 0-based column index as its base-26 spreadsheet letters
/// (`0` → `"A"`, `25` → `"Z"`, `26` → `"AA"`).
#[must_use]
pub fn col_letters(col0: u32) -> String {
    // Classic "bijective base 26" conversion: unlike plain base-26, there is
    // no digit for zero, so each step consumes one letter's worth of value
    // (`n -= 1`) before taking the remainder.
    let mut n = col0 + 1; // 1-based for the bijective-base-26 algorithm.
    let mut letters = Vec::new();
    while n > 0 {
        n -= 1;
        let digit = (n % 26) as u8;
        letters.push(b'A' + digit);
        n /= 26;
    }
    letters.reverse();
    String::from_utf8(letters).expect("ASCII letters are always valid UTF-8")
}

/// Renders a 0-based `(row, col)` pair as an A1-style reference, e.g.
/// `(0, 0)` → `"A1"`, `(6, 1)` → `"B7"`.
#[must_use]
pub fn a1_ref(row0: u32, col0: u32) -> String {
    format!("{}{}", col_letters(col0), row0 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_letters() {
        assert_eq!(col_letters(0), "A");
        assert_eq!(col_letters(1), "B");
        assert_eq!(col_letters(25), "Z");
    }

    #[test]
    fn double_letters() {
        assert_eq!(col_letters(26), "AA");
        assert_eq!(col_letters(27), "AB");
        assert_eq!(col_letters(51), "AZ");
        assert_eq!(col_letters(52), "BA");
    }

    #[test]
    fn refs() {
        assert_eq!(a1_ref(0, 0), "A1");
        assert_eq!(a1_ref(6, 1), "B7");
        assert_eq!(a1_ref(0, 25), "Z1");
        assert_eq!(a1_ref(0, 26), "AA1");
    }
}

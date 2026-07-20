//! `FIND` — locates the first case-sensitive occurrence of `find_text`
//! within `within_text`, starting at a given 1-based character position,
//! and returns that occurrence's 1-based character start position.
//!
//! # Provenance
//! Behavior contract: `docs/specs/FIND.md` (Microsoft Learn "FIND, FINDB
//! functions" page). Coercion via `xl-value`'s [`to_text`]/[`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `find_text` (arg0) and `within_text` (arg1) are each coerced via
//!   scalar text coercion [`to_text`]; an error-valued argument propagates
//!   (FIND.md §Error behavior).
//! - `FIND` is **case-sensitive** and does **not** support wildcard
//!   characters — the headline distinction from `SEARCH`, which is out of
//!   scope here (FIND.md §Semantics 6).
//! - `start_num` (arg2, optional) is coerced via [`to_number`] when supplied;
//!   omitted defaults to `1` (FIND.md §Semantics 4, §Coercion). `args.count()`
//!   (not `Value::Blank`) distinguishes an omitted third argument from an
//!   explicit one. A **non-integer** `start_num` is **truncated toward zero**
//!   (`4.9` -> `4`) before use — **OXP-107 RESOLVED by
//!   `RUN-2026-07-11-oracle01`**: `FIND("c","abcabc",4.9)` = `6`, consistent
//!   with the LEFT/RIGHT/MID text family.
//! - `start_num < 1` -> `#VALUE!`; for a **non-empty** `find_text`, a
//!   `start_num` greater than the character length of `within_text` -> `#VALUE!`
//!   (a needle cannot start past the end) (FIND.md §Error behavior 2-3).
//! - Search starts at `start_num` and finds the **first** case-sensitive,
//!   literal (non-wildcard) occurrence of `find_text`; found -> its 1-based
//!   start position as `Value::number`; not found -> `#VALUE!` (FIND.md
//!   §Semantics 1, §Error behavior 1).
//! - Empty `find_text` (`""`) matches trivially at `start_num` and returns
//!   that position (FIND.md §Semantics 7). This includes the one-past-the-end
//!   position (`start_num == len + 1`) — **OXP-109 RESOLVED by
//!   `RUN-2026-07-11-oracle01`**: `FIND("","abc",4)` = `4`, `FIND("","abc",1)`
//!   = `1`, and `FIND("","")` = `1` (empty `within_text`, `start_num`
//!   defaulting to `1` = `len + 1`). An empty needle matches at `len + 1`, not
//!   `#VALUE!`. Further past the end (`start_num > len + 1`) is beyond the
//!   probed boundary and stays the conservative `#VALUE!`.
//! - **OXP-108/OXP-161 — character basis: UTF-16 code units (RESOLVED).**
//!   Positions and lengths are counted in **UTF-16 code units**. `OXP-161`
//!   (`RUN-2026-07-16-oracle01`) observed `FIND("😀","A😀B")` = `2` — the emoji
//!   U+1F600 occupies units 2-3, so the match position is `2` on a **code-unit**
//!   basis (a scalar-value basis would give the same `2` here, but `LEN("😀")`=`2`
//!   / `MID("A😀B",2,1)`=a lone surrogate half in the same run decisively pin the
//!   basis as code units for the LEN/MID/FIND family — Excel is self-inconsistent,
//!   `LEFT`/`RIGHT` count scalars). FIND therefore encodes both operands to UTF-16
//!   (`str::encode_utf16`) and searches over code units. This is **identical** to
//!   the old scalar-`char` search for all ASCII/BMP text (a BMP scalar is exactly
//!   one code unit), and correct for astral text — it **no longer defers**
//!   (`#UNSUPPORTED!`) on non-BMP input (FIND.md §Character basis).

use xl_value::{ErrorKind, Value, to_number, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `FIND(find_text, within_text, [start_num])` call. See the
/// module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let find_text = match to_text(&args.eval_scalar(0)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };
    let within_text = match to_text(&args.eval_scalar(1)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };
    let start_num = if args.count() > 2 {
        match to_number(&args.eval_scalar(2)) {
            Ok(n) => n,
            Err(k) => return Value::Error(k),
        }
    } else {
        1.0
    };
    // OXP-107 RESOLVED (RUN-2026-07-11-oracle01): a non-integer `start_num`
    // truncates toward zero (4.9 -> 4) before use — see module docs. The
    // truncation is applied at the `as usize` cast below; the `< 1` domain
    // check stays on the raw value (sign/range only).

    // OXP-161 (RUN-2026-07-16-oracle01): FIND counts **UTF-16 code units**, not
    // Unicode scalars — `FIND("😀","A😀B")` = `2`. Encode both operands to UTF-16
    // and search over code units. Identical to the old scalar-`char` search for
    // all BMP/ASCII text (a BMP scalar is exactly one code unit); correct for
    // astral text, so FIND no longer defers on non-BMP input.
    let within_units: Vec<u16> = within_text.as_str().encode_utf16().collect();
    let find_units: Vec<u16> = find_text.as_str().encode_utf16().collect();

    let len = within_units.len();
    if start_num < 1.0 {
        return Value::Error(ErrorKind::Value);
    }
    let start = start_num.trunc() as usize; // >= 1; truncated toward zero (OXP-107).

    if find_units.is_empty() {
        // Empty `find_text` matches at `start_num` within the string, and —
        // OXP-109 RESOLVED (RUN-2026-07-11-oracle01) — also at the one-past-the-
        // end position (`start == len + 1`): FIND("","abc",4)=4, FIND("","")=1.
        // Beyond that boundary (`start > len + 1`) is unobserved → #VALUE!.
        return if start <= len + 1 {
            Value::number(start as f64)
        } else {
            Value::Error(ErrorKind::Value)
        };
    }

    // A non-empty needle cannot match starting past the last code unit.
    if start > len {
        return Value::Error(ErrorKind::Value);
    }
    let flen = find_units.len();
    let remaining = len - (start - 1);
    if flen > remaining {
        return Value::Error(ErrorKind::Value);
    }
    for i in (start - 1)..=(len - flen) {
        if within_units[i..i + flen] == find_units[..] {
            return Value::number((i + 1) as f64);
        }
    }
    Value::Error(ErrorKind::Value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    #[test]
    fn finds_simple_occurrence() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("b")), Scalar(txt("abc"))]),
            num(2.0)
        );
    }

    #[test]
    fn case_sensitive_not_found() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("B")), Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn start_num_skips_earlier_occurrence() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("c")), Scalar(txt("abcabc")), Scalar(num(4.0))]
            ),
            num(6.0)
        );
    }

    #[test]
    fn not_found() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("z")), Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn start_num_below_one_is_value_error() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("a")), Scalar(txt("abc")), Scalar(num(0.0))]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn start_num_beyond_length_is_value_error() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("a")), Scalar(txt("abc")), Scalar(num(5.0))]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn empty_find_text_matches_start_num() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("")), Scalar(txt("abc"))]),
            num(1.0)
        );
    }

    #[test]
    fn numeric_arguments_are_coerced_to_text() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.0)), Scalar(num(123.0))]),
            num(2.0)
        );
    }

    #[test]
    fn error_in_find_text_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Ref)), Scalar(txt("abc"))]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    #[test]
    fn error_in_within_text_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("a")), Scalar(Value::Error(ErrorKind::Div0))]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn error_in_start_num_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("a")),
                    Scalar(txt("abc")),
                    Scalar(Value::Error(ErrorKind::Num))
                ]
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn non_integer_start_num_truncates_toward_zero() {
        // OXP-107 RESOLVED (RUN-2026-07-11-oracle01): FIND("c","abcabc",4.9)=6
        // — a non-integer start_num truncates toward zero (4.9 -> 4).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("c")), Scalar(txt("abcabc")), Scalar(num(4.9))]
            ),
            num(6.0)
        );
    }

    #[test]
    fn astral_positions_count_utf16_code_units() {
        // OXP-161 (RUN-2026-07-16-oracle01): FIND counts UTF-16 code units.
        // FIND("😀","A😀B") = 2 — the emoji U+1F600 starts at code unit 2 (A is
        // unit 1). This was the decisive probe H5.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("😀")), Scalar(txt("A😀B"))]),
            num(2.0)
        );
        // A needle *after* the astral char sees its 2-unit width: in "A😀B" the
        // "B" is at code unit 4 (A=1, 😀=2-3, B=4), not 3.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("B")), Scalar(txt("A😀B"))]),
            num(4.0)
        );
        // A plain BMP needle inside astral text still resolves on the unit basis.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("a")), Scalar(txt("a😀b"))]),
            num(1.0)
        );
        // An astral needle matches its own 2-unit run.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("😀")), Scalar(txt("😀"))]),
            num(1.0)
        );
    }

    #[test]
    fn empty_find_text_matches_one_past_end() {
        // OXP-109 RESOLVED (RUN-2026-07-11-oracle01): an empty find_text matches
        // at the one-past-the-end position (start_num == len + 1), returning it.
        // FIND("","abc",4) = 4.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("")), Scalar(txt("abc")), Scalar(num(4.0))]
            ),
            num(4.0)
        );
        // FIND("","abc",1) = 1 (within bounds).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("")), Scalar(txt("abc")), Scalar(num(1.0))]
            ),
            num(1.0)
        );
        // FIND("","") = 1: empty within_text, start_num defaults to 1 = len + 1.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("")), Scalar(txt(""))]),
            num(1.0)
        );
        // Beyond the probed boundary (start_num > len + 1) stays #VALUE!.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("")), Scalar(txt("abc")), Scalar(num(5.0))]
            ),
            Value::Error(ErrorKind::Value)
        );
    }
}

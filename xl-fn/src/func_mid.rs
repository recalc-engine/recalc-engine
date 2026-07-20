//! `MID` — returns `num_chars` characters from `text`, starting at the
//! 1-based position `start_num`.
//!
//! # Provenance
//! Behavior contract: `docs/specs/MID.md` (Microsoft Learn "MID, MIDB
//! functions" page,
//! <https://support.microsoft.com/en-us/office/mid-midb-functions-d5f9e25c-d7d6-472e-b568-4ecb12433028>).
//! Text coercion deferred to `xl-value`'s [`to_text`]; numeric coercion for
//! `start_num`/`num_chars` deferred to [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `text` (arg 0) is coerced via scalar text coercion ([`to_text`]);
//!   an error argument propagates as MID's result (MID.md §Coercion,
//!   §Error behavior).
//! - `start_num` (arg 1) is coerced via [`to_number`] (error propagates).
//!   `start_num < 1` -> `#VALUE!` (MID.md §4).
//! - `num_chars` (arg 2) is coerced via [`to_number`] (error propagates).
//!   `num_chars < 0` -> `#VALUE!` (MID.md §4).
//! - The result is the `num_chars` characters of `text` beginning at the
//!   1-based position `start_num`. If `start_num` exceeds the length of
//!   `text`, the result is `""` (MID.md §2). If `start_num + num_chars`
//!   runs past the end of `text`, the result is clamped to however many
//!   characters remain, with no padding and no error (MID.md §3).
//!   `num_chars == 0` -> `""`.
//! - Positions and lengths are counted in **UTF-16 code units** (`OXP-161`;
//!   see below), so `text` is sliced over its `str::encode_utf16()` units.
//!   For every Basic-Multilingual-Plane character this is identical to a
//!   Unicode-scalar (`str::chars()`) count — a BMP scalar is exactly one code
//!   unit — and for astral text it matches Excel's measured basis.
//!
//! # Non-integer `start_num`/`num_chars` — `OXP-107` RESOLVED
//! `OXP-107` (RESOLVED by `RUN-2026-07-11-oracle01`) settled the truncation
//! direction the same open-question family — `DATE`'s `OXP-091`, `ROUND`'s
//! `OXP-098`, `EOMONTH`'s `OXP-092`, `VLOOKUP`'s `OXP-089`, `WEEKDAY`'s
//! `OXP-097` — poses: `MID("abcdef",2.9,2.1)` = `"bc"`, i.e. a non-integer
//! `start_num` or `num_chars` is **truncated toward zero** (`2.9` -> `2`,
//! `2.1` -> `2`) before use, not `#UNSUPPORTED!`. The `start_num < 1` and
//! `num_chars < 0` domain checks are unchanged and applied to the raw value
//! (sign/range only).
//!
//! # Non-BMP `text`: UTF-16 code units, incl. a lone surrogate half (`OXP-161`)
//! `OXP-161` (`RUN-2026-07-16-oracle01`) settled the basis decisively:
//! `MID("A😀B",2,1)` returned a **lone high-surrogate half** (U+D83D), and
//! `LEN("😀")`=`2` / `FIND("😀","A😀B")`=`2` in the same run confirm MID indexes
//! by **UTF-16 code unit** (and will split a surrogate pair). MID therefore
//! slices `text.encode_utf16()` and decodes the requested unit range:
//! - When the slice boundaries fall on surrogate-pair boundaries (or all-BMP),
//!   it decodes to a valid string and is returned verbatim — e.g.
//!   `MID("A😀B",2,2)` = `"😀"`. This is the direct mechanical consequence of
//!   the measured unit basis, not a fresh guess.
//! - When a boundary **splits** a pair, the faithful Excel result is a lone
//!   surrogate half (e.g. `MID("A😀B",2,1)` = U+D83D). A Rust `str` / the
//!   interned [`xl_value::Text`] (`Arc<str>`) **cannot represent** an unpaired
//!   surrogate, so lossless output of that half is deferred to **RFC-0015**
//!   (a WTF-8 `Text` extension — a frozen-contract change awaiting human
//!   ratification); until then this exact split case refuses loudly with
//!   `#UNSUPPORTED!` rather than emit a lossy U+FFFD replacement. This is a
//!   narrow residual: only a slice that splits a pair defers (the old code
//!   deferred on *any* astral character).

use xl_value::{ErrorKind, Value, to_number, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `MID(text, start_num, num_chars)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let text = match to_text(&args.eval_scalar(0)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };

    let start_raw = match to_number(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    // OXP-107 RESOLVED (RUN-2026-07-11-oracle01): a non-integer start_num
    // truncates toward zero (2.9 -> 2). The `< 1` domain check is unchanged
    // and applied to the raw value (sign/range only).
    if start_raw < 1.0 {
        return Value::Error(ErrorKind::Value);
    }

    let num_chars_raw = match to_number(&args.eval_scalar(2)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    // OXP-107 RESOLVED (RUN-2026-07-11-oracle01): a non-integer num_chars
    // truncates toward zero (2.1 -> 2). The `< 0` domain check is unchanged.
    if num_chars_raw < 0.0 {
        return Value::Error(ErrorKind::Value);
    }

    // OXP-161: index by UTF-16 code unit. Truncate toward zero (OXP-107), then
    // slice the code units. Float-to-int casts saturate in Rust (no UB), so
    // pathologically large start_num/num_chars values just clamp against the
    // unit length below.
    let units: Vec<u16> = text.as_str().encode_utf16().collect();
    let start_idx = (start_raw.trunc() as usize).saturating_sub(1);
    if start_idx >= units.len() {
        return Value::text("");
    }
    let end_idx = start_idx
        .saturating_add(num_chars_raw.trunc() as usize)
        .min(units.len());
    let slice = &units[start_idx..end_idx];

    // A slice on surrogate-pair boundaries decodes to a valid string and is
    // returned verbatim (MID("A😀B",2,2)="😀"). A slice that SPLITS a pair is a
    // lone surrogate half (MID("A😀B",2,1)=U+D83D per OXP-161) that `Arc<str>`
    // cannot hold — deferred to RFC-0015 (WTF-8 `Text`); refuse loudly meanwhile
    // rather than emit a lossy U+FFFD.
    match String::from_utf16(slice) {
        Ok(out) => Value::text(&out),
        Err(_) => Value::Error(ErrorKind::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    use super::eval as mid_eval;

    fn call(args: Vec<crate::test_support::TestArg>) -> Value {
        eval_direct(mid_eval, args)
    }

    #[test]
    fn basic_substring() {
        assert_eq!(
            call(vec![
                Scalar(txt("abcdef")),
                Scalar(num(2.0)),
                Scalar(num(3.0))
            ]),
            txt("bcd")
        );
    }

    #[test]
    fn num_chars_past_end_clamps() {
        assert_eq!(
            call(vec![
                Scalar(txt("abc")),
                Scalar(num(2.0)),
                Scalar(num(10.0))
            ]),
            txt("bc")
        );
    }

    #[test]
    fn start_num_past_end_is_empty() {
        assert_eq!(
            call(vec![Scalar(txt("abc")), Scalar(num(5.0)), Scalar(num(2.0))]),
            txt("")
        );
    }

    #[test]
    fn zero_num_chars_is_empty() {
        assert_eq!(
            call(vec![Scalar(txt("abc")), Scalar(num(1.0)), Scalar(num(0.0))]),
            txt("")
        );
    }

    #[test]
    fn start_num_below_one_is_value_error() {
        assert_eq!(
            call(vec![Scalar(txt("abc")), Scalar(num(0.0)), Scalar(num(2.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn negative_num_chars_is_value_error() {
        assert_eq!(
            call(vec![
                Scalar(txt("abc")),
                Scalar(num(1.0)),
                Scalar(num(-1.0))
            ]),
            Value::Error(ErrorKind::Value)
        );
    }

    /// OXP-107 RESOLVED (RUN-2026-07-11-oracle01): a non-integer start_num or
    /// num_chars truncates toward zero before use. The run's probe was
    /// `MID("abcdef",2.9,2.1)` = "bc" (2.9 -> 2, 2.1 -> 2).
    #[test]
    fn non_integer_args_truncate_toward_zero() {
        assert_eq!(
            call(vec![
                Scalar(txt("abcdef")),
                Scalar(num(2.9)),
                Scalar(num(2.1))
            ]),
            txt("bc")
        );
        // Fractional start_num only.
        assert_eq!(
            call(vec![
                Scalar(txt("abcdef")),
                Scalar(num(2.9)),
                Scalar(num(2.0))
            ]),
            txt("bc")
        );
        // Fractional num_chars only.
        assert_eq!(
            call(vec![
                Scalar(txt("abcdef")),
                Scalar(num(2.0)),
                Scalar(num(2.9))
            ]),
            txt("bc")
        );
    }

    #[test]
    fn numeric_text_arg_is_coerced() {
        assert_eq!(
            call(vec![
                Scalar(num(12345.0)),
                Scalar(num(2.0)),
                Scalar(num(2.0))
            ]),
            txt("23")
        );
    }

    #[test]
    fn error_in_text_arg_propagates() {
        assert_eq!(
            call(vec![
                Scalar(Value::Error(ErrorKind::Div0)),
                Scalar(num(1.0)),
                Scalar(num(2.0)),
            ]),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn astral_slice_on_pair_boundary_counts_utf16_code_units() {
        // OXP-161 (RUN-2026-07-16-oracle01): MID indexes UTF-16 code units. In
        // "A😀B" the units are [A, D83D, DE00, B]; MID(_,2,2) takes units 2-3 =
        // the whole pair = "😀" (the direct consequence of the measured unit
        // basis — the same indexing that split the pair at H4).
        assert_eq!(
            call(vec![
                Scalar(txt("A\u{1F600}B")),
                Scalar(num(2.0)),
                Scalar(num(2.0))
            ]),
            txt("\u{1F600}")
        );
        // Take the astral pair plus the trailing BMP char: units 2-4 = "😀B".
        assert_eq!(
            call(vec![
                Scalar(txt("A\u{1F600}B")),
                Scalar(num(2.0)),
                Scalar(num(3.0))
            ]),
            txt("\u{1F600}B")
        );
        // num_chars past the end still clamps, on the unit basis: units 2.. = "😀B".
        assert_eq!(
            call(vec![
                Scalar(txt("A\u{1F600}B")),
                Scalar(num(2.0)),
                Scalar(num(9.0))
            ]),
            txt("\u{1F600}B")
        );
        // start_num past the unit length is empty (len = 4 units here).
        assert_eq!(
            call(vec![
                Scalar(txt("A\u{1F600}B")),
                Scalar(num(5.0)),
                Scalar(num(1.0))
            ]),
            txt("")
        );
    }

    #[test]
    fn astral_slice_splitting_a_pair_defers_pending_wtf8() {
        // OXP-161 H4: MID("A😀B",2,1) is a LONE high-surrogate half (U+D83D).
        // `Arc<str>`/`Text` cannot hold an unpaired surrogate, so lossless output
        // is deferred to RFC-0015 (WTF-8 `Text`); refuse loudly meanwhile rather
        // than emit a lossy U+FFFD. Only the split case defers — the pair-aligned
        // cases above compute the correct substring.
        assert_eq!(
            call(vec![
                Scalar(txt("A\u{1F600}B")),
                Scalar(num(2.0)),
                Scalar(num(1.0))
            ]),
            Value::Error(ErrorKind::Unsupported)
        );
        // Splitting at the *end* of a slice (take up to the high surrogate only)
        // is equally a lone half: units 1-2 = [A, D83D].
        assert_eq!(
            call(vec![
                Scalar(txt("A\u{1F600}B")),
                Scalar(num(1.0)),
                Scalar(num(2.0))
            ]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

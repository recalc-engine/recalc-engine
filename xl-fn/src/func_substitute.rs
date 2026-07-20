//! `SUBSTITUTE` — replaces occurrences of `old_text` with `new_text` inside
//! `text`, either every occurrence or (with `instance_num`) only the Nth.
//!
//! # Provenance
//! Behavior contract: `docs/specs/SUBSTITUTE.md` (Microsoft Learn "SUBSTITUTE
//! function" page,
//! <https://support.microsoft.com/en-us/office/substitute-function-6434944e-a904-4336-a9b0-1e58df3bc332>).
//! Text coercion deferred to `xl-value`'s [`to_text`]; `instance_num` numeric
//! coercion to [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `text` (arg 0), `old_text` (arg 1), `new_text` (arg 2) are each coerced
//!   via scalar text coercion ([`to_text`]) — numbers use "General"
//!   formatting, booleans `"TRUE"`/`"FALSE"`, blank `""`, text passes through
//!   (SUBSTITUTE.md §Coercion). An error-valued argument propagates
//!   (SUBSTITUTE.md §Error behavior).
//! - Matching is **case-sensitive and literal** (no wildcards) — the same
//!   basis as its sibling `FIND` (SUBSTITUTE.md §Case sensitivity). MS Learn
//!   does not state this explicitly; it is the observed behavior, and the
//!   literal-substring reading is the only one consistent with `FIND`.
//! - Without `instance_num`: **every** occurrence of `old_text` is replaced,
//!   scanning **left-to-right, non-overlapping** (Rust `str::replace`'s
//!   contract, which matches Excel's) (SUBSTITUTE.md §2).
//! - With `instance_num` (arg 3, optional): **only the Nth** occurrence
//!   (1-based, counted left-to-right, non-overlapping) is replaced; the rest
//!   of `text` is left verbatim (SUBSTITUTE.md §3). If there are **fewer than
//!   N** occurrences, `text` is returned **unchanged** (no error)
//!   (SUBSTITUTE.md §3).
//! - `old_text == ""`: Excel performs **no** replacement and returns `text`
//!   unchanged (SUBSTITUTE.md §Empty old_text). This is a deliberate special
//!   case — a naive "replace the empty string" would splice `new_text` between
//!   every character; Excel does not.
//!
//! # `instance_num` domain + truncation
//! - `instance_num` is coerced via [`to_number`] (error propagates). A raw
//!   value `< 1` -> `#VALUE!` (SUBSTITUTE.md §Error behavior) — the sign/range
//!   check on the raw value, mirroring `FIND`'s `start_num < 1` rule.
//! - A **non-integer** `instance_num` is **truncated toward zero** (`2.9` ->
//!   `2`) before use, following the resolved `OXP-107` truncation direction
//!   for the text family (`FIND`/`LEFT`/`RIGHT`/`MID`). MS Learn does not show
//!   a fractional `instance_num`; the direction is taken from that established
//!   family convention rather than guessed.
//!
//! # Ordering of the domain check vs. empty `old_text`
//! Following `FIND` (which checks `start_num < 1` before its empty-`find_text`
//! case), the `instance_num < 1` -> `#VALUE!` check is applied **before** the
//! `old_text == ""` -> unchanged short-circuit, and the astral `#UNSUPPORTED!`
//! guard precedes both (as in `FIND`). The precise Excel precedence for the
//! collision of an empty `old_text` with an out-of-domain `instance_num` is
//! not oracle-confirmed; the ordering here is chosen for sibling consistency,
//! not observed.
//! // OXP (unassigned): =SUBSTITUTE("abc","",0) — empty old_text AND
//! // instance_num < 1; confirm #VALUE! vs. text-unchanged precedence.
//!
//! # Distinction from `REPLACE`
//! `SUBSTITUTE` is **content-based** (find matching `old_text` and swap it);
//! `REPLACE` is **position-based** (overwrite a fixed character range given by
//! `start_num`/`num_chars`). Use `SUBSTITUTE` when the text to change is known,
//! `REPLACE` when the position is known (SUBSTITUTE.md §Distinction from
//! REPLACE).
//!
//! # Oracle-deferred edge: non-BMP `text` (`OXP-108`, still deferred)
//! Consistent with `FIND`/`LEFT`/`RIGHT`/`MID`, when any text argument
//! contains a character outside the Basic Multilingual Plane
//! (`char as u32 > 0xFFFF`) the call returns `#UNSUPPORTED!` rather than a
//! possibly-wrong result. Excel's character basis for astral text is UTF-16
//! code units (`RUN-2026-07-11-oracle01` observed `LEN("𝛑")` = `2`), which the
//! scalar-value occurrence counting used here could diverge from around a
//! surrogate boundary; the probe carried no SUBSTITUTE-on-astral observation,
//! so the astral case stays deferred. Pure-BMP text is the fidelity path.

use xl_value::{ErrorKind, Value, to_number, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `SUBSTITUTE(text, old_text, new_text, [instance_num])` call. See
/// the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let text = match to_text(&args.eval_scalar(0)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };
    let old_text = match to_text(&args.eval_scalar(1)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };
    let new_text = match to_text(&args.eval_scalar(2)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };
    // Evaluate the optional `instance_num` (propagating an error value) before
    // any domain check, so an error argument surfaces regardless of the other
    // arguments — SUBSTITUTE is not a lazy function.
    let instance_raw = if args.count() > 3 {
        match to_number(&args.eval_scalar(3)) {
            Ok(n) => Some(n),
            Err(k) => return Value::Error(k),
        }
    } else {
        None
    };

    let text_s = text.as_str();
    let old_s = old_text.as_str();
    let new_s = new_text.as_str();

    // OXP-108: occurrence counting/splicing is done on Unicode scalar values;
    // Excel's abstract basis is UTF-16 code units. Identical for all BMP/ASCII
    // text; defer rather than risk a surrogate-boundary divergence — consistent
    // with FIND/LEFT/RIGHT/MID.
    if text_s
        .chars()
        .chain(old_s.chars())
        .chain(new_s.chars())
        .any(|c| (c as u32) > 0xFFFF)
    {
        return Value::Error(ErrorKind::Unsupported);
    }

    // `instance_num < 1` -> `#VALUE!` (raw sign/range check, before the empty-
    // old_text short-circuit — see module docs for the ordering rationale).
    let instance = match instance_raw {
        Some(raw) => {
            if raw < 1.0 {
                return Value::Error(ErrorKind::Value);
            }
            // OXP-107 family convention: truncate toward zero (2.9 -> 2). `raw`
            // is finite and >= 1 here; `as usize` saturates harmlessly for an
            // absurdly large value (`nth` then just exhausts the match iterator
            // -> unchanged).
            Some(raw.trunc() as usize)
        }
        None => None,
    };

    // Empty `old_text`: Excel replaces nothing and returns `text` unchanged.
    if old_s.is_empty() {
        return Value::text(text_s);
    }

    match instance {
        // Replace only the Nth occurrence; fewer than N -> `text` unchanged.
        Some(n) => match text_s.match_indices(old_s).nth(n - 1) {
            Some((pos, matched)) => {
                let mut out = String::with_capacity(text_s.len() - matched.len() + new_s.len());
                out.push_str(&text_s[..pos]);
                out.push_str(new_s);
                out.push_str(&text_s[pos + matched.len()..]);
                Value::text(&out)
            }
            None => Value::text(text_s),
        },
        // Replace every (non-overlapping, left-to-right) occurrence.
        None => Value::text(&text_s.replace(old_s, new_s)),
    }
}

#[cfg(test)]
mod tests {
    use super::eval as substitute_eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    fn call(args: Vec<crate::test_support::TestArg>) -> Value {
        eval_direct(substitute_eval, args)
    }

    #[test]
    fn replaces_all_occurrences_without_instance_num() {
        assert_eq!(
            call(vec![
                Scalar(txt("a-b-c")),
                Scalar(txt("-")),
                Scalar(txt("+"))
            ]),
            txt("a+b+c")
        );
    }

    #[test]
    fn replaces_only_nth_occurrence() {
        // The 3rd "1" (MS Learn's own example shape) is replaced.
        assert_eq!(
            call(vec![
                Scalar(txt("1-1-1")),
                Scalar(txt("1")),
                Scalar(txt("2")),
                Scalar(num(3.0))
            ]),
            txt("1-1-2")
        );
        // The 1st occurrence.
        assert_eq!(
            call(vec![
                Scalar(txt("1-1-1")),
                Scalar(txt("1")),
                Scalar(txt("2")),
                Scalar(num(1.0))
            ]),
            txt("2-1-1")
        );
    }

    #[test]
    fn instance_num_beyond_count_returns_text_unchanged() {
        assert_eq!(
            call(vec![
                Scalar(txt("a-b")),
                Scalar(txt("-")),
                Scalar(txt("+")),
                Scalar(num(2.0))
            ]),
            txt("a-b")
        );
    }

    #[test]
    fn empty_old_text_returns_text_unchanged() {
        // Neither replace-all nor Nth splices new_text when old_text is "".
        assert_eq!(
            call(vec![Scalar(txt("abc")), Scalar(txt("")), Scalar(txt("X"))]),
            txt("abc")
        );
        assert_eq!(
            call(vec![
                Scalar(txt("abc")),
                Scalar(txt("")),
                Scalar(txt("X")),
                Scalar(num(1.0))
            ]),
            txt("abc")
        );
    }

    #[test]
    fn matching_is_case_sensitive() {
        // Uppercase "A" does not match lowercase "a": no replacement.
        assert_eq!(
            call(vec![Scalar(txt("aAa")), Scalar(txt("A")), Scalar(txt("x"))]),
            txt("axa")
        );
        // Lowercase needle matches only the lowercase occurrences.
        assert_eq!(
            call(vec![Scalar(txt("aAa")), Scalar(txt("a")), Scalar(txt("x"))]),
            txt("xAx")
        );
    }

    #[test]
    fn non_overlapping_left_to_right() {
        // "aa" matched at 0, leaving a trailing "a" — Excel's non-overlapping
        // left-to-right scan (matches Rust str::replace).
        assert_eq!(
            call(vec![
                Scalar(txt("aaa")),
                Scalar(txt("aa")),
                Scalar(txt("b"))
            ]),
            txt("ba")
        );
    }

    #[test]
    fn numeric_arguments_are_coerced_to_text() {
        // text 12321, old "2", new "9" -> "19391".
        assert_eq!(
            call(vec![
                Scalar(num(12321.0)),
                Scalar(num(2.0)),
                Scalar(num(9.0))
            ]),
            txt("19391")
        );
    }

    #[test]
    fn non_integer_instance_num_truncates_toward_zero() {
        // 2.9 -> 2: the 2nd occurrence is replaced (OXP-107 family convention).
        assert_eq!(
            call(vec![
                Scalar(txt("1-1-1")),
                Scalar(txt("1")),
                Scalar(txt("2")),
                Scalar(num(2.9))
            ]),
            txt("1-2-1")
        );
    }

    #[test]
    fn instance_num_below_one_is_value_error() {
        assert_eq!(
            call(vec![
                Scalar(txt("1-1")),
                Scalar(txt("1")),
                Scalar(txt("2")),
                Scalar(num(0.0))
            ]),
            Value::Error(ErrorKind::Value)
        );
        assert_eq!(
            call(vec![
                Scalar(txt("1-1")),
                Scalar(txt("1")),
                Scalar(txt("2")),
                Scalar(num(-1.0))
            ]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn error_in_text_propagates() {
        assert_eq!(
            call(vec![
                Scalar(Value::Error(ErrorKind::Div0)),
                Scalar(txt("a")),
                Scalar(txt("b"))
            ]),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn error_in_old_text_propagates() {
        assert_eq!(
            call(vec![
                Scalar(txt("abc")),
                Scalar(Value::Error(ErrorKind::Ref)),
                Scalar(txt("b"))
            ]),
            Value::Error(ErrorKind::Ref)
        );
    }

    #[test]
    fn error_in_new_text_propagates() {
        assert_eq!(
            call(vec![
                Scalar(txt("abc")),
                Scalar(txt("a")),
                Scalar(Value::Error(ErrorKind::Na))
            ]),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn error_in_instance_num_propagates() {
        assert_eq!(
            call(vec![
                Scalar(txt("abc")),
                Scalar(txt("a")),
                Scalar(txt("b")),
                Scalar(Value::Error(ErrorKind::Num))
            ]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn omitted_new_text_removes_old_text() {
        // A present-but-elided new_text coerces Blank -> "", i.e. deletion.
        assert_eq!(
            call(vec![Scalar(txt("a-b-c")), Scalar(txt("-")), Omitted]),
            txt("abc")
        );
    }

    #[test]
    fn new_text_can_be_longer_than_old_text() {
        assert_eq!(
            call(vec![
                Scalar(txt("a.b")),
                Scalar(txt(".")),
                Scalar(txt(" and "))
            ]),
            txt("a and b")
        );
    }

    #[test]
    fn non_bmp_text_is_deferred_unsupported() {
        // OXP-108 KEPT DEFERRED: any astral char in a text argument -> defer,
        // consistent with FIND/LEFT/RIGHT/MID (Excel's UTF-16 code-unit basis
        // vs. this module's scalar-value counting is unobserved for SUBSTITUTE).
        assert_eq!(
            call(vec![
                Scalar(txt("a\u{1F600}b")),
                Scalar(txt("a")),
                Scalar(txt("x"))
            ]),
            Value::Error(ErrorKind::Unsupported)
        );
        // Astral in old_text also defers.
        assert_eq!(
            call(vec![
                Scalar(txt("abc")),
                Scalar(txt("\u{1F600}")),
                Scalar(txt("x"))
            ]),
            Value::Error(ErrorKind::Unsupported)
        );
        // Astral in new_text also defers (conservative: any astral input).
        assert_eq!(
            call(vec![
                Scalar(txt("abc")),
                Scalar(txt("a")),
                Scalar(txt("\u{1F600}"))
            ]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

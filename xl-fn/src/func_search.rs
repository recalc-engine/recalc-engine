//! `SEARCH` — locates the first **case-insensitive** occurrence of `find_text`
//! within `within_text`, from a 1-based start position, returning that
//! occurrence's 1-based position.
//!
//! # Provenance
//! Microsoft Learn "SEARCH, SEARCHB functions" page
//! (`https://support.microsoft.com/en-us/office/search-searchb-functions-9ab04538-0e55-4719-a72e-b6f54513b495`).
//! Coercion via `xl-value`'s [`to_text`]/[`to_number`]. SEARCH is the
//! **case-insensitive, wildcard-aware** sibling of `FIND`; it shares FIND's
//! position semantics (start handling, empty-needle boundary, UTF-16 code-unit
//! character basis — OXP-107/109/161) and differs only in case-folding and
//! wildcard support.
//!
//! # Scope — served: literal, case-insensitive, ASCII-cased substring search
//! - `find_text` (arg0) / `within_text` (arg1) coerced via [`to_text`]; an
//!   error-valued argument propagates.
//! - `start_num` (arg2, optional) defaults to `1`; coerced via [`to_number`],
//!   **truncated toward zero** (OXP-107 family), `< 1` → `#VALUE!`; for a
//!   non-empty `find_text` a `start_num` past the last code unit → `#VALUE!`.
//!   An empty `find_text` matches at `start_num` (incl. the one-past-end
//!   position `len + 1`) — FIND's OXP-109 resolution, shared by this sibling.
//! - Matching is **case-insensitive on ASCII** (`A`–`Z` fold to `a`–`z`);
//!   positions are counted in **UTF-16 code units** (OXP-161, matching FIND).
//!   Found → its 1-based position; not found → `#VALUE!`.
//!
//! # Wildcards — OXP-225 (RESOLVED, RUN-2026-07-20-oracle01)
//! SEARCH honors `*` (any run, including empty), `?` (any single UTF-16 code
//! unit) and `~` (escape). The pinned Excel 16.0 build was probed and decides:
//! - `*` spans a run: `SEARCH("a*c","abXXc") = 1`, `SEARCH("a*c","aXc") = 1`.
//! - `?` matches exactly one code unit: `SEARCH("b?d","abcd") = 2`,
//!   `SEARCH("?","abc") = 1`.
//! - `~` escapes the following metacharacter to a literal:
//!   `SEARCH("~*","a*b") = 2`, `SEARCH("~?","a?b") = 2` (and, by the same pinned
//!   escape mechanism, `~~` → a literal `~`).
//! - matching is **case-insensitive** across wildcards:
//!   `SEARCH("A*C","abcABC") = 1`.
//!
//! The returned value is the 1-based position where the match **begins** (the
//! wildcard run may extend to end-of-string; the match need not reach it). `?`
//! counts one UTF-16 code unit, the same basis FIND/SEARCH already use (OXP-161).
//!
//! **Still deferred (never guess):** a `~` that is *not* followed by a
//! metacharacter (`*`/`?`/`~`), or a trailing `~`, is an unpinned escape → the
//! needle refuses loudly (`#UNSUPPORTED!`) rather than guess whether Excel drops
//! the tilde, treats it literally, or errors. Provenance: OXP-225.
//!
//! # Other deferred edges (never guess)
//! - **Non-ASCII case folding → deferred** under the same reasoning as
//!   `LOWER`/`UPPER` (en-US non-ASCII casing is locale-sensitive and unpinned;
//!   `docs/specs/LOWER.md` §"Oracle experiments needed", unassigned OXP). If
//!   either operand contains a non-ASCII character whose case could matter,
//!   SEARCH refuses (`#UNSUPPORTED!`) rather than fold it wrongly. Caseless
//!   non-ASCII (emoji, CJK, punctuation) is unaffected and searched normally.

use xl_value::{ErrorKind, Value, to_number, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// A character whose case-folding Excel has not pinned for en-US: any non-ASCII
/// character that is a letter, or that otherwise changes under Unicode simple
/// lowercasing. ASCII is always the pinned path. Mirrors `func_lower`.
fn is_unpinned_case(c: char) -> bool {
    !c.is_ascii() && (c.is_alphabetic() || c.to_lowercase().next() != Some(c))
}

/// Fold a UTF-16 code unit for ASCII-case-insensitive comparison: `A`–`Z`
/// (65..=90) → `a`–`z`; every other unit unchanged. Non-ASCII units that carry
/// case have already been screened out by [`is_unpinned_case`].
fn ascii_fold_unit(u: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&u) {
        u + 32
    } else {
        u
    }
}

const STAR: u16 = b'*' as u16;
const QMARK: u16 = b'?' as u16;
const TILDE: u16 = b'~' as u16;

/// One token of a parsed SEARCH pattern (OXP-225).
enum Tok {
    /// `*` — matches any run of code units (including empty).
    Star,
    /// `?` — matches exactly one code unit.
    Any,
    /// A literal code unit (compared ASCII-case-folded).
    Lit(u16),
}

/// Parse a needle's UTF-16 units into wildcard tokens. `~` escapes the following
/// metacharacter (`*`/`?`/`~`) to a literal (OXP-225). Returns `None` — the
/// caller then refuses with `#UNSUPPORTED!` — for an **unpinned** escape: a `~`
/// followed by a non-metacharacter, or a trailing `~` (never guessed).
fn parse_pattern(units: &[u16]) -> Option<Vec<Tok>> {
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        match units[i] {
            TILDE => match units.get(i + 1) {
                Some(&n) if n == STAR || n == QMARK || n == TILDE => {
                    out.push(Tok::Lit(n));
                    i += 2;
                }
                _ => return None, // unpinned escape (~ before non-meta, or trailing ~)
            },
            STAR => {
                out.push(Tok::Star);
                i += 1;
            }
            QMARK => {
                out.push(Tok::Any);
                i += 1;
            }
            u => {
                out.push(Tok::Lit(u));
                i += 1;
            }
        }
    }
    Some(out)
}

/// Does `pat` match a substring of `text` that **begins** at index `start`? The
/// match need not reach the end of `text` (SEARCH finds a substring); it
/// succeeds as soon as the pattern is fully consumed. Standard two-pointer glob
/// with single-star backtracking; comparisons are ASCII-case-folded (OXP-225).
fn glob_prefix_match(pat: &[Tok], text: &[u16], start: usize) -> bool {
    let mut pi = 0usize;
    let mut ti = start;
    let mut star_pi: Option<usize> = None;
    let mut star_ti = start;
    loop {
        if pi == pat.len() {
            return true; // pattern fully consumed = substring match
        }
        match pat[pi] {
            Tok::Star => {
                star_pi = Some(pi);
                star_ti = ti;
                pi += 1;
            }
            Tok::Any => {
                if ti < text.len() {
                    pi += 1;
                    ti += 1;
                } else if let Some(sp) = star_pi {
                    star_ti += 1;
                    if star_ti > text.len() {
                        return false;
                    }
                    ti = star_ti;
                    pi = sp + 1;
                } else {
                    return false;
                }
            }
            Tok::Lit(u) => {
                if ti < text.len() && ascii_fold_unit(text[ti]) == ascii_fold_unit(u) {
                    pi += 1;
                    ti += 1;
                } else if let Some(sp) = star_pi {
                    star_ti += 1;
                    if star_ti > text.len() {
                        return false;
                    }
                    ti = star_ti;
                    pi = sp + 1;
                } else {
                    return false;
                }
            }
        }
    }
}

/// Evaluate a `SEARCH(find_text, within_text, [start_num])` call. See the
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

    // Non-ASCII case folding is unpinned for en-US (LOWER/UPPER precedent):
    // defer if either operand carries a case-bearing non-ASCII character.
    if find_text.as_str().chars().any(is_unpinned_case)
        || within_text.as_str().chars().any(is_unpinned_case)
    {
        return Value::Error(ErrorKind::Unsupported);
    }

    let start_num = if args.count() > 2 {
        match to_number(&args.eval_scalar(2)) {
            Ok(n) => n,
            Err(k) => return Value::Error(k),
        }
    } else {
        1.0
    };
    if start_num < 1.0 {
        return Value::Error(ErrorKind::Value);
    }
    let start = start_num.trunc() as usize; // >= 1; truncated toward zero (OXP-107).

    // UTF-16 code-unit basis (OXP-161), folded at comparison time.
    let within_units: Vec<u16> = within_text.as_str().encode_utf16().collect();
    let find_units: Vec<u16> = find_text.as_str().encode_utf16().collect();

    let len = within_units.len();
    if find_units.is_empty() {
        // Empty needle matches at start_num, incl. one-past-the-end (FIND's
        // OXP-109 resolution, shared by this sibling).
        return if start <= len + 1 {
            Value::number(start as f64)
        } else {
            Value::Error(ErrorKind::Value)
        };
    }

    // OXP-225 (RUN-2026-07-20-oracle01): parse `*`/`?`/`~`; an unpinned escape
    // (`~` before a non-metacharacter, or trailing `~`) refuses loudly.
    let pat = match parse_pattern(&find_units) {
        Some(p) => p,
        None => return Value::Error(ErrorKind::Unsupported),
    };

    // Leftmost match at or after `start`: return the 1-based position where the
    // (possibly wildcard) match begins.
    for i in (start - 1)..=len {
        if glob_prefix_match(&pat, &within_units, i) {
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
    fn case_insensitive_match() {
        // SEARCH is case-insensitive: "B" matches "b" at position 2.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("B")), Scalar(txt("abc"))]),
            num(2.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("cd")), Scalar(txt("ABCDEF"))]),
            num(3.0)
        );
    }

    #[test]
    fn start_num_skips_earlier_occurrence() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("C")), Scalar(txt("abcabc")), Scalar(num(4.0))]
            ),
            num(6.0)
        );
    }

    #[test]
    fn not_found_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("z")), Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn start_num_below_one_and_past_end() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("a")), Scalar(txt("abc")), Scalar(num(0.0))]
            ),
            Value::Error(ErrorKind::Value)
        );
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("a")), Scalar(txt("abc")), Scalar(num(5.0))]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn empty_find_text_matches_start() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("")), Scalar(txt("abc"))]),
            num(1.0)
        );
        // one-past-the-end (OXP-109 sibling).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("")), Scalar(txt("abc")), Scalar(num(4.0))]
            ),
            num(4.0)
        );
    }

    #[test]
    fn non_integer_start_num_truncates_toward_zero() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("c")), Scalar(txt("abcabc")), Scalar(num(4.9))]
            ),
            num(6.0)
        );
    }

    #[test]
    fn numeric_arguments_coerced_to_text() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(3.0)), Scalar(num(1234.0))]),
            num(3.0)
        );
    }

    #[test]
    fn wildcard_semantics_oxp225() {
        // OXP-225 (RUN-2026-07-20-oracle01): the pinned wildcard positions.
        let cases = [
            ("a*c", "abXXc", 1.0),  // * spans a run
            ("a*c", "aXc", 1.0),    // * spans a single unit
            ("b?d", "abcd", 2.0),   // ? = one unit
            ("~*", "a*b", 2.0),     // escaped literal *
            ("~?", "a?b", 2.0),     // escaped literal ?
            ("?", "abc", 1.0),      // bare ? at start
            ("A*C", "abcABC", 1.0), // case-fold across a wildcard
            ("~~", "a~b", 2.0),     // escaped literal ~ (same pinned mechanism)
        ];
        for (needle, hay, want) in cases {
            assert_eq!(
                eval_direct(eval, vec![Scalar(txt(needle)), Scalar(txt(hay))]),
                num(want),
                "SEARCH({needle:?},{hay:?})"
            );
        }
    }

    #[test]
    fn unpinned_tilde_escape_is_deferred_oxp225() {
        // A ~ before a non-metacharacter, or a trailing ~, is unpinned -> defer.
        for needle in ["a~b", "ab~", "~a"] {
            assert_eq!(
                eval_direct(eval, vec![Scalar(txt(needle)), Scalar(txt("abc"))]),
                Value::Error(ErrorKind::Unsupported),
                "needle {needle:?} should defer (unpinned escape)"
            );
        }
    }

    #[test]
    fn wildcard_not_found_is_value_error() {
        // A pattern with a required literal that is absent -> #VALUE!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("a*z")), Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn non_ascii_case_bearing_operand_deferred() {
        // A cased non-ASCII letter in either operand defers (LOWER/UPPER
        // precedent — en-US non-ASCII casing unpinned).
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("É")), Scalar(txt("café"))]),
            Value::Error(ErrorKind::Unsupported)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("e")), Scalar(txt("café"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn caseless_non_ascii_is_searched() {
        // Emoji carry no case: an ASCII needle after an astral char sees its
        // 2-unit width (UTF-16 basis, OXP-161).
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("B")), Scalar(txt("a😀B"))]),
            num(4.0)
        );
    }

    #[test]
    fn error_arguments_propagate() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Ref)), Scalar(txt("abc"))]
            ),
            Value::Error(ErrorKind::Ref)
        );
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
}

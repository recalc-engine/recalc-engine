//! `LEN` — the number of characters in `text`.
//!
//! # Provenance
//! Behavior contract: `docs/specs/LEN.md` (Microsoft Learn "LEN, LENB
//! functions" page — LEN and its byte-oriented, locale-dependent sibling
//! `LENB` share one page; only LEN is in scope here). Text coercion via
//! `xl-value`'s [`to_text`] (same "General" numeric formatting as
//! `CONCATENATE`/`LEFT`).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `text` (arg 0) is coerced to text via [`to_text`]: numbers use "General"
//!   formatting, `TRUE`/`FALSE` for booleans, `""` for blank (so `LEN(blank)` =
//!   `0`), text passes through unchanged (LEN.md §Coercion). An error-valued
//!   `text` propagates (LEN.md §Error behavior).
//! - Returns the number of characters (spaces included) in that text as a
//!   `Value::number` (LEN.md §Semantics 1). A numeric argument is counted over
//!   its General-format text representation, not the digit count of the
//!   underlying float (LEN.md §Semantics 3) — a consequence of coercing first.
//!
//! # Character basis — UTF-16 code units (`OXP-108`/`OXP-161`, RESOLVED)
//! Excel counts characters in **UTF-16 code units**, so a non-BMP (astral)
//! character — encoded as a surrogate pair — counts as **2**.
//! `RUN-2026-07-11-oracle01` (`OXP-108`) observed `LEN("𝛑")` = `2` (U+1D6D1),
//! and `RUN-2026-07-16-oracle01` (`OXP-161`) observed `LEN("😀")` = `2`
//! (U+1F600), together pinning the basis. LEN is implemented with
//! [`str::encode_utf16`]`().count()`, matching Excel exactly and, unlike the
//! slicing siblings, with **no** surrogate-boundary ambiguity: *counting* code
//! units is exact regardless of where boundaries fall. `OXP-161` also resolved
//! the slicing family on the same measured basis — `MID`/`FIND` count UTF-16
//! code units (like LEN), while `LEFT`/`RIGHT` count Unicode **scalars** (Excel
//! is self-inconsistent); all four now compute on that basis rather than
//! deferring (`MID`'s one residual — a slice that splits a pair into a lone
//! surrogate half — is tracked to RFC-0015). See `func_mid.rs`/`func_left.rs`.

use xl_value::{Value, to_text};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `LEN(text)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let text = match to_text(&args.eval_scalar(0)) {
        Ok(t) => t,
        Err(k) => return Value::Error(k),
    };
    // OXP-108/OXP-161: Excel counts UTF-16 code units, so an astral char counts
    // as 2 — LEN("𝛑")=2 (OXP-108), LEN("😀")=2 (OXP-161, H1). Counting code units
    // is exact even for non-BMP text (no surrogate-boundary ambiguity).
    let len = text.as_str().encode_utf16().count();
    Value::number(len as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    #[test]
    fn ascii_length_counts_all_characters() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("Hello"))]), num(5.0));
    }

    #[test]
    fn spaces_are_counted() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt("a b c"))]), num(5.0));
    }

    #[test]
    fn empty_string_is_zero() {
        assert_eq!(eval_direct(eval, vec![Scalar(txt(""))]), num(0.0));
    }

    #[test]
    fn blank_argument_is_zero() {
        // to_text(Blank) = "" → LEN(blank) = 0 (LEN.md §Coercion).
        assert_eq!(eval_direct(eval, vec![Omitted]), num(0.0));
    }

    #[test]
    fn number_argument_uses_general_text_coercion() {
        // LEN(100) counts "100" → 3 (LEN.md §Semantics 3).
        assert_eq!(eval_direct(eval, vec![Scalar(num(100.0))]), num(3.0));
    }

    #[test]
    fn boolean_argument_uses_literal_text() {
        assert_eq!(eval_direct(eval, vec![Scalar(Value::Bool(true))]), num(4.0));
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Bool(false))]),
            num(5.0)
        );
    }

    #[test]
    fn error_in_text_propagates() {
        use xl_value::ErrorKind;
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn astral_character_counts_as_two_utf16_code_units() {
        // OXP-161 (RUN-2026-07-16-oracle01, H1): LEN("😀")=2 — Excel counts UTF-16
        // code units, so a single astral char (surrogate pair) counts as 2.
        // OXP-108's LEN("𝛑")=2 (U+1D6D1) is the same basis.
        assert_eq!(eval_direct(eval, vec![Scalar(txt("😀"))]), num(2.0));
        assert_eq!(eval_direct(eval, vec![Scalar(txt("𝛑"))]), num(2.0));
        // Mixed BMP + astral: "a😀b" = 1 + 2 + 1 = 4 code units.
        assert_eq!(eval_direct(eval, vec![Scalar(txt("a😀b"))]), num(4.0));
    }
}

//! `HYPERLINK` — create a shortcut that jumps to a location; the **computed
//! value** is the text/value displayed in the cell.
//!
//! # Provenance
//! Behavior contract: `docs/specs/HYPERLINK.md`, which cites the Microsoft
//! Learn HYPERLINK function page
//! (`https://support.microsoft.com/en-us/office/hyperlink-function-333c7ce6-c5ae-4164-9c47-7de9b76f577f`).
//!
//! # What a headless engine computes
//! Recalc has no UI and never navigates — the *jump* is inert. What matters for
//! fidelity is the cell's **value**, which Excel caches as the displayed jump
//! text: the `friendly_name` when supplied, otherwise the `link_location`
//! itself (MS page: "If `friendly_name` is omitted, the cell displays the
//! `link_location` as the jump text"). The value's **type is preserved** —
//! `=HYPERLINK("u", 42)` displays the number `42`, not `"42"`.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Evaluate `link_location` (arg 0); an error propagates (HYPERLINK.md
//!   §Error behavior). Excel evaluates every argument, so a `link_location`
//!   error wins even when a `friendly_name` is present.
//! - If a non-omitted `friendly_name` (arg 1) is supplied, evaluate it; an
//!   error propagates, otherwise it **is** the result, type preserved
//!   (HYPERLINK.md §1). If `friendly_name` is omitted, the result is
//!   `link_location`, returned as-is (HYPERLINK.md §1).
//!
//! # Numeric `link_location` coercion (OXP-217, RUN-2026-07-16-oracle01)
//! When `link_location` is itself the displayed value (no `friendly_name`), the
//! pinned Excel 16.0 build **coerces a numeric link to its General text form** —
//! `=HYPERLINK(123)` → text `"123"` — in deliberate contrast to a
//! `friendly_name`, whose **type is preserved** (`=HYPERLINK("u", 42)` → the
//! number `42`, re-confirmed by the same probe). So a `Number` `link_location`
//! used as the result is passed through [`to_text`] (General); every other type
//! (text, bool) passes through unchanged. An empty-string `friendly_name`
//! (`=HYPERLINK("u", "")`) is returned as the text `""` (also pinned).
//!
//! # Oracle experiment still needed (OXP-217, blank operand)
//! One genuinely ambiguous sub-case remains **deferred loudly** rather than
//! guessed: when the selected operand evaluates to **`Blank`**
//! (`=HYPERLINK("u", A1)` with `A1` empty, or `=HYPERLINK(A1)` with `A1` empty).
//! Excel's general empty-cell-reference rule materializes `0` in a value
//! context, but whether HYPERLINK yields `0`, `""`, an empty cell, or falls back
//! to the other operand is not documented, and the OXP-217 run that pinned the
//! numeric-link coercion did **not** author the blank-operand cells (they need a
//! blank-reference scaffold). So a blank selected operand still returns
//! `#UNSUPPORTED!` pending a re-authored probe. The dominant corpus form — a
//! literal or populated-cell `friendly_name`, or a text `link_location` — is
//! fully served.

use xl_value::{Value, to_text};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Evaluate a `HYPERLINK(link_location, [friendly_name])` call. See the module
/// docs (including the OXP-217 blank-operand deferral).
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Excel evaluates link_location even when a friendly_name is present; its
    // error propagates first.
    let link = args.eval_scalar(0);
    if let Value::Error(k) = link {
        return Value::Error(k);
    }

    let has_friendly = args.count() >= 2 && args.shape(1) != ArgShape::Omitted;
    let result = if has_friendly {
        let friendly = args.eval_scalar(1);
        if let Value::Error(k) = friendly {
            return Value::Error(k);
        }
        // The friendly_name's type is preserved (OXP-217: numeric 42 → 42).
        friendly
    } else {
        // OXP-217: a numeric link_location used as the displayed value is
        // coerced to General text (HYPERLINK(123) → "123"), unlike a
        // friendly_name. Other types (text, bool, blank) pass through — a Blank
        // is caught by the deferral below.
        if let Value::Number(_) = link {
            match to_text(&link) {
                Ok(t) => Value::Text(t),
                Err(k) => return Value::Error(k),
            }
        } else {
            link
        }
    };

    // A blank selected operand (0 vs "" vs blank vs fallback) is unpinned —
    // defer loudly rather than guess. See module docs / OXP-217.
    if result.is_blank() {
        return Value::Error(xl_value::ErrorKind::Unsupported);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::ErrorKind;

    #[test]
    fn friendly_name_text_is_returned() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("http://example.com")), Scalar(txt("Example"))],
            ),
            txt("Example")
        );
    }

    #[test]
    fn friendly_name_number_preserves_type() {
        // OXP-217: =HYPERLINK("u", 42) displays the number 42, not "42" — a
        // friendly_name keeps its type.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("u")), Scalar(num(42.0))]),
            num(42.0)
        );
    }

    #[test]
    fn numeric_link_location_coerces_to_text() {
        // OXP-217 (RUN-2026-07-16-oracle01): =HYPERLINK(123) with no
        // friendly_name displays the *text* "123" — a numeric link used as the
        // displayed value is coerced to General text (unlike a friendly_name).
        assert_eq!(eval_direct(eval, vec![Scalar(num(123.0))]), txt("123"));
    }

    #[test]
    fn empty_string_friendly_is_returned_as_text() {
        // OXP-217: =HYPERLINK("u", "") → the text "" (an empty *string* is not
        // Blank, so it is not deferred).
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("u")), Scalar(txt(""))]),
            txt("")
        );
    }

    #[test]
    fn friendly_name_bool_preserves_type() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("u")), Scalar(Value::bool(true))]),
            Value::bool(true)
        );
    }

    #[test]
    fn omitted_friendly_returns_link_location() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("mailto:x@y.com"))]),
            txt("mailto:x@y.com")
        );
    }

    #[test]
    fn explicitly_omitted_friendly_returns_link() {
        // =HYPERLINK("u",) — a present-but-omitted second slot uses the link.
        assert_eq!(eval_direct(eval, vec![Scalar(txt("u")), Omitted]), txt("u"));
    }

    #[test]
    fn link_location_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Ref)), Scalar(txt("ok"))],
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    #[test]
    fn friendly_name_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("u")), Scalar(Value::Error(ErrorKind::Div0))],
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn blank_friendly_is_deferred_loudly() {
        // OXP-217: blank selected operand (0 vs "" vs blank) is unpinned.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("u")), Scalar(Value::Blank)]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn blank_lone_link_is_deferred_loudly() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Blank)]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

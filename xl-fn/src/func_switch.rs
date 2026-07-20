//! `SWITCH` — match an expression against a list of values and return the
//! result paired with the first match, else an optional default.
//!
//! # Provenance
//! Behavior contract: Microsoft support "SWITCH function"
//! (<https://support.microsoft.com/en-us/office/switch-function-47ab33c0-28ce-4530-8a45-d532ec4aa25e>,
//! verified by WebFetch 2026-07-15). No `docs/specs/SWITCH.md` exists in this
//! pass. Equality of `expression` against each `valueN` is `xl-value`'s frozen
//! [`values_equal`] contract (Excel's `=` operator), so SWITCH is exactly the
//! documented "syntactic sugar for a chain of `expression = valueN` tests".
//!
//! # Behavior contract (one line)
//! `SWITCH(expr, v1, r1, [v2, r2], …, [default])` returns the `rN` of the first
//! `vN` that equals `expr` (Excel `=`); no match → `default` if present, else
//! `#N/A`.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - `expression` (argument 0) "is the value (such as a number, date or some
//!   text) that will be compared against value1…value126." Evaluated once in
//!   scalar context.
//! - Each `valueN` is compared to `expression` in argument order via
//!   [`values_equal`] — Excel's `=`: numeric equality, `Number < Text < Bool`
//!   cross-type (so different types never match), and **leftmost-first error
//!   propagation**. "Returns the result corresponding to the first matching
//!   value." A match is only *computed* when it is exact under both plausible
//!   readings of the undocumented "matching value" (see the refusal note
//!   below); the ambiguous cases refuse rather than guess.
//! - The matched `resultN` is the only result forced
//!   ([`eval_scalar`](CallArgs::eval_scalar)) — lazy; non-selected results and
//!   the default are not evaluated unless reached.
//! - **Default**: "identified by having no corresponding resultN expression …
//!   Default must be the final argument." Detected as an odd number of
//!   arguments after `expression`. Returned when no `valueN` matches.
//! - "If there are no matching values, and no default argument is supplied, the
//!   SWITCH function returns the #N/A error."
//! - Up to 126 `value`/`result` pairs ("functions are limited to 254
//!   arguments").
//!
//! # Refused edges (loud, not guessed)
//! Because matching delegates to [`values_equal`]/[`compare`], an **error** in
//! `expression` or in a `valueN` propagates that error (leftmost-first) and a
//! **non-ASCII text** comparison returns `#UNSUPPORTED!` (OXP-031, held —
//! locale collation is unpinned). Beyond those, SWITCH's undocumented "matching
//! value" has two plausible readings — byte-exact match and Excel's `=` — that
//! agree everywhere except: (a) text equal only after case-folding
//! (`SWITCH("a","A",1)` → `1` under `=`, but the default under a case-sensitive
//! reading), and (b) a `Blank` `expression` morphing to `""`/`0`/`FALSE`. Since
//! exact-match ⊆ `=`-match, every non-match and every byte-exact / same-type
//! numeric·boolean match is safe under both readings and is computed; the two
//! ambiguous zones return `#UNSUPPORTED!` via [`match_is_exact`] rather than
//! guess. One farm run flips those refusals to computes — see
//! `docs/plans/2026-07-15-lane5-probe-needed.md` (L5-6).

use xl_value::{ErrorKind, Value, values_equal};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `SWITCH(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let count = args.count();
    // Registry enforces min_args = 3 (expression + at least one value/result
    // pair). Arguments after `expression`:
    let after_expr = count - 1;
    // An odd tail means a trailing default with no paired result.
    let has_default = !after_expr.is_multiple_of(2);
    let num_pairs = after_expr / 2;

    // Evaluate `expression` once in scalar context.
    let expr = args.eval_scalar(0);

    for k in 0..num_pairs {
        let value_index = 1 + 2 * k;
        let result_index = 2 + 2 * k;
        let candidate = args.eval_scalar(value_index);
        match values_equal(&expr, &candidate) {
            // First match wins — but only *compute* the result when the match
            // holds under BOTH plausible readings of SWITCH's undocumented
            // "matching value" (exact-match ⊆ Excel `=`-match). The ambiguous
            // zone — text equal only up to case, and a `Blank` morphing to
            // `""`/`0`/`FALSE` — is unpinned (L5-6), so refuse there loudly
            // rather than guess which reading Excel's SWITCH uses.
            Ok(true) => {
                return if match_is_exact(&expr, &candidate) {
                    args.eval_scalar(result_index)
                } else {
                    Value::Error(ErrorKind::Unsupported)
                };
            }
            Ok(false) => continue,
            // A comparison that cannot be evaluated (error in expression or a
            // value → propagate its kind; non-ASCII text → #UNSUPPORTED!,
            // OXP-031 held). Later values are not compared.
            Err(k) => return Value::Error(k),
        }
    }

    // No value matched.
    if has_default {
        // The trailing default is the final argument; force it now (lazy).
        args.eval_scalar(count - 1)
    } else {
        // "no matching values, and no default argument is supplied" → #N/A.
        Value::Error(ErrorKind::Na)
    }
}

/// Is a `values_equal` match safe to *compute* — i.e. does it hold under both
/// plausible readings of SWITCH's undocumented equality (byte-exact match and
/// Excel's `=`)? `Number`/`Number` and `Bool`/`Bool` equality is exact and
/// unambiguous. Two `Text`s must be **byte-identical**: a match that holds only
/// after case-folding diverges if Excel's SWITCH is case-sensitive (unpinned —
/// L5-6). A `Blank` matching a non-`Blank` via morphing (`""`/`0`/`FALSE`) is
/// likewise unpinned. Those two zones are `false` here and refuse loudly; a
/// `Blank`-vs-`Blank` match is same-type and exact.
fn match_is_exact(expr: &Value, candidate: &Value) -> bool {
    match (expr, candidate) {
        (Value::Text(a), Value::Text(b)) => a.as_str() == b.as_str(),
        (Value::Blank, Value::Blank) => true,
        (Value::Blank, _) | (_, Value::Blank) => false,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // First matching value's result is returned. SWITCH(2, 1,"a", 2,"b", 3,"c")
    // → "b".
    #[test]
    fn first_match_returns_its_result() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(2.0)),
                    Scalar(num(1.0)),
                    Scalar(txt("a")),
                    Scalar(num(2.0)),
                    Scalar(txt("b")),
                    Scalar(num(3.0)),
                    Scalar(txt("c")),
                ]
            ),
            txt("b")
        );
    }

    // No match, default supplied (odd tail) → default. SWITCH(9, 1,"a", 2,"b",
    // "none") → "none".
    #[test]
    fn no_match_returns_default() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(9.0)),
                    Scalar(num(1.0)),
                    Scalar(txt("a")),
                    Scalar(num(2.0)),
                    Scalar(txt("b")),
                    Scalar(txt("none")),
                ]
            ),
            txt("none")
        );
    }

    // No match, no default (even tail) → #N/A.
    #[test]
    fn no_match_no_default_is_na() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(9.0)),
                    Scalar(num(1.0)),
                    Scalar(txt("a")),
                    Scalar(num(2.0)),
                    Scalar(txt("b"))
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // Byte-identical text matches and computes (safe under both readings).
    // SWITCH("mon", "mon", 1, "tue", 2) → 1.
    #[test]
    fn text_match_byte_exact_computes() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("mon")),
                    Scalar(txt("mon")),
                    Scalar(num(1.0)),
                    Scalar(txt("tue")),
                    Scalar(num(2.0)),
                ]
            ),
            num(1.0)
        );
    }

    // A case-fold-ONLY text match (equal under `=` but not byte-identical) is
    // the unpinned zone (L5-6) → refuse loudly, do NOT guess `1`.
    // SWITCH("Mon", "mon", 1, "tue", 2) → #UNSUPPORTED!.
    #[test]
    fn text_match_case_fold_only_refuses() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("Mon")),
                    Scalar(txt("mon")),
                    Scalar(num(1.0)),
                    Scalar(txt("tue")),
                    Scalar(num(2.0)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // A `Blank` expression morphing to a non-`Blank` value (`""`/`0`/`FALSE`) is
    // also unpinned (L5-6) → refuse. SWITCH(<blank>, 0, "z", "def") →
    // #UNSUPPORTED! (blank-morphs-to-0 under `=`, but the reading is unpinned).
    #[test]
    fn blank_morph_match_refuses() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Blank),
                    Scalar(num(0.0)),
                    Scalar(txt("z")),
                    Scalar(txt("def")),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // Cross-type values never match: SWITCH(1, "1", "text-one", 1, "num-one")
    // → "num-one" (Number 1 does not equal Text "1").
    #[test]
    fn cross_type_does_not_match() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(1.0)),
                    Scalar(txt("1")),
                    Scalar(txt("text-one")),
                    Scalar(num(1.0)),
                    Scalar(txt("num-one")),
                ]
            ),
            txt("num-one")
        );
    }

    // Lazy: only the matched result is forced. SWITCH(1, 1,"a", 2,<poison>) →
    // "a" without forcing the second result.
    #[test]
    fn is_lazy_unmatched_results_not_evaluated() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(1.0)),
                    Scalar(num(1.0)),
                    Scalar(txt("a")),
                    Scalar(num(2.0)),
                    Poison
                ]
            ),
            txt("a")
        );
    }

    // Lazy: the default is not forced when a value matches. SWITCH(1, 1,"a",
    // <poison-default>) → "a".
    #[test]
    fn is_lazy_default_not_evaluated_on_match() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(1.0)), Scalar(num(1.0)), Scalar(txt("a")), Poison]
            ),
            txt("a")
        );
    }

    // An error in `expression` propagates (via values_equal/compare, leftmost
    // first). SWITCH(#DIV/0!, 1, "a", "def") → #DIV/0!.
    #[test]
    fn error_in_expression_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Div0)),
                    Scalar(num(1.0)),
                    Scalar(txt("a")),
                    Scalar(txt("def")),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    // Non-ASCII text comparison defers loudly via compare (OXP-031 held).
    // SWITCH("ä", "ä", 1) → #UNSUPPORTED!.
    #[test]
    fn non_ascii_comparison_defers() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("ä")), Scalar(txt("ä")), Scalar(num(1.0))]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // Boolean expression matches a boolean value. SWITCH(TRUE, FALSE,"f",
    // TRUE,"t") → "t".
    #[test]
    fn boolean_match() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(true)),
                    Scalar(Value::Bool(false)),
                    Scalar(txt("f")),
                    Scalar(Value::Bool(true)),
                    Scalar(txt("t")),
                ]
            ),
            txt("t")
        );
    }
}

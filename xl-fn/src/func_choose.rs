//! `CHOOSE` — lazily select one of N value arguments by a 1-based index.
//!
//! # Provenance
//! Behavior contract: `docs/specs/CHOOSE.md`, which cites the Microsoft Learn
//! CHOOSE function page
//! (<https://support.microsoft.com/en-us/office/choose-function-fc5c184f-cb62-4ec7-a46e-38653b98f5bc>,
//! verified by WebFetch 2026-07-08). Numeric coercion of `index_num` is
//! `xl-value`'s [`to_number`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `index_num` (argument 0) is evaluated in scalar context and coerced with
//!   [`to_number`]. An error in `index_num` propagates immediately
//!   (CHOOSE.md §Error behavior).
//! - **Fractional `index_num` is truncated to the lowest integer before use.**
//!   The MS Learn page states this verbatim ("If index_num is a fraction, it is
//!   truncated to the lowest integer before being used"), so this is
//!   *documented*, not guessed: `2.9` selects `value2`. Truncation is
//!   [`f64::floor`] ("lowest integer"). This is a deliberate divergence from the
//!   LEFT/MID/FIND non-integer family (OXP-107), whose direction the public docs
//!   do **not** pin — CHOOSE's *is* pinned, so it is implemented rather than
//!   deferred (CHOOSE.md §Non-integer index_num).
//! - The truncated index selects among `value1..valueN` (arguments
//!   `1..=count-1`). A 1-based index `k` maps to argument position `k` (arg 0 is
//!   `index_num`, arg 1 is `value1`, ...). `k < 1` or `k` greater than the number
//!   of value arguments → `#VALUE!` (CHOOSE.md §Range). A non-finite index
//!   (e.g. `NaN`) also lands in this `#VALUE!` bucket rather than re-reading
//!   arg 0.
//! - **Lazy evaluation** (CHOOSE.md §Laziness): only the *selected* value
//!   argument is forced via [`eval_scalar`](CallArgs::eval_scalar). The
//!   non-selected value arguments are never evaluated, so an error, a
//!   `#UNSUPPORTED!` construct, a division by zero, or a volatile read inside
//!   them cannot affect the result. The selected value passes through untouched
//!   (no type unification across the value list).

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `CHOOSE(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Argument 0 (index_num) is guaranteed present by the registry arity check
    // (min 2 args). Evaluate eagerly in scalar context and coerce to a number;
    // an error propagates.
    let raw = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };

    // MS Learn: "If index_num is a fraction, it is truncated to the lowest
    // integer before being used." Floor = lowest integer.
    let index = raw.floor();

    // Value arguments occupy positions 1..=count-1, so there are `count - 1` of
    // them, and the 1-based index maps straight onto the argument position.
    let value_count = args.count() - 1;

    // `index < 1` or `index > value_count` → #VALUE!. Written as the negation of
    // the valid range so a non-finite `index` (NaN) also falls through to
    // #VALUE! instead of being cast to an argument position.
    if !(index >= 1.0 && index <= value_count as f64) {
        return Value::Error(ErrorKind::Value);
    }

    // `index` is now a finite integer in `1..=value_count`, so the cast is exact
    // and in range. Force *only* the selected value argument — laziness
    // guaranteed; the other value arguments are never evaluated.
    let selected = index as usize;
    args.eval_scalar(selected)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn selects_second_value() {
        // CHOOSE(2, "a", "b", "c") = "b".
        let got = eval_direct(
            eval,
            vec![
                Scalar(num(2.0)),
                Scalar(txt("a")),
                Scalar(txt("b")),
                Scalar(txt("c")),
            ],
        );
        assert_eq!(got, txt("b"));
    }

    #[test]
    fn selects_first_and_last() {
        // CHOOSE(1, "a", "b", "c") = "a".
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(1.0)),
                    Scalar(txt("a")),
                    Scalar(txt("b")),
                    Scalar(txt("c")),
                ],
            ),
            txt("a")
        );
        // CHOOSE(3, "a", "b", "c") = "c" (last value arg).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(3.0)),
                    Scalar(txt("a")),
                    Scalar(txt("b")),
                    Scalar(txt("c")),
                ],
            ),
            txt("c")
        );
    }

    #[test]
    fn index_below_one_is_value_error() {
        // CHOOSE(0, "a", "b") → #VALUE!.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.0)), Scalar(txt("a")), Scalar(txt("b"))]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn index_above_n_is_value_error() {
        // CHOOSE(3, "a", "b") → #VALUE! (only two value args).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(3.0)), Scalar(txt("a")), Scalar(txt("b"))]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn fractional_index_truncates_to_lowest_integer() {
        // MS Learn: a fractional index_num is truncated to the lowest integer
        // before use, so 2.9 selects value2 = "b".
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(2.9)),
                    Scalar(txt("a")),
                    Scalar(txt("b")),
                    Scalar(txt("c")),
                ],
            ),
            txt("b")
        );
        // 1.0001 truncates to 1 → value1 = "a".
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(1.0001)), Scalar(txt("a")), Scalar(txt("b"))],
            ),
            txt("a")
        );
        // 0.9 truncates to 0 → below 1 → #VALUE!.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.9)), Scalar(txt("a")), Scalar(txt("b"))]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn is_lazy_unselected_values_not_evaluated() {
        // CHOOSE(1, "x", <poison>) = "x" without forcing the poison arg. If
        // CHOOSE eagerly evaluated value2, the Poison mock would panic.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0)), Scalar(txt("x")), Poison]),
            txt("x")
        );
        // Symmetrically, selecting the later value must not force the earlier
        // one: CHOOSE(2, <poison>, "y") = "y".
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.0)), Poison, Scalar(txt("y"))]),
            txt("y")
        );
    }

    #[test]
    fn error_in_index_num_propagates() {
        // CHOOSE(#DIV/0!, "a", "b") → #DIV/0! (the index error propagates and
        // no value arg is evaluated — the poison stays untouched).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Div0)),
                    Scalar(txt("a")),
                    Poison
                ],
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn non_numeric_index_is_value_error() {
        // A non-numeric text index_num coerces via to_number → #VALUE!.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("not a number")), Scalar(txt("a")), Poison]
            ),
            Value::Error(ErrorKind::Value)
        );
    }
}

//! `XOR` — logical exclusive-or (parity of TRUE inputs) across all arguments.
//!
//! # Provenance
//! Behavior contract: Microsoft support "XOR function"
//! (<https://support.microsoft.com/en-us/office/xor-function-1548d4c2-5e47-4f77-9a92-0533bba14f37>,
//! verified by WebFetch 2026-07-15). No `docs/specs/XOR.md` exists in this
//! pass; this module mirrors the `AND`/`OR` family ([`crate::func_and`],
//! [`crate::func_or`]) exactly for coercion and range handling, swapping the
//! conjunction/disjunction fold for a **parity** fold. Scalar boolean coercion
//! is entirely `xl-value`'s frozen [`to_bool`] contract — the same one
//! `AND`/`OR`/`IF` use.
//!
//! # Behavior contract (one line)
//! `XOR(l1, [l2], …)` = TRUE iff an **odd** number of contributing logical
//! values are TRUE; only real `Bool` cells in a range participate; `#VALUE!`
//! if no logical value is found anywhere.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - Parity fold: "The result of XOR is TRUE when the number of TRUE inputs is
//!   odd and FALSE when the number of TRUE inputs is even." Implemented by
//!   XOR-ing each contributing boolean into a running accumulator.
//! - **Scalar** arguments coerce through [`to_bool`] (numbers `0`→`FALSE`,
//!   nonzero→`TRUE`; `"TRUE"`/`"FALSE"` text case-insensitively; other text →
//!   `#VALUE!`) — the identical scalar rule `AND`/`OR` use. (The XOR page does
//!   not itself spell out scalar number→logical coercion, but it is the shared
//!   frozen `to_bool` contract every logical function is built on, not a
//!   per-function guess.)
//! - **Range/array** arguments: "If an array or reference argument contains
//!   text or empty cells, those values are ignored." `Text` and `Blank` cells
//!   inside a range are ignored. A **`Number` cell in a range/array
//!   participates** via its 0-vs-nonzero value (`0` → FALSE, nonzero → TRUE) —
//!   pinned by **OXP-213** (RUN-2026-07-16-oracle01, live-COM `live_probe.py`),
//!   the XOR-specific follow-up to OXP-208 (which had pinned this for
//!   `AND`/`OR` only). Excel's own live measurement, scaffold
//!   `A1=1,A2=0,A3=TRUE,B1=1,B2=TRUE`: `XOR(A1:A2)`={1,0} → TRUE (one TRUE, so
//!   the numbers ARE logical values — otherwise `#VALUE!`), `XOR(A1:A3)` →
//!   FALSE (two TRUEs), `XOR(B1:B2)`={1,TRUE} → FALSE (two TRUEs), and
//!   `XOR(FALSE,{1;0})` → TRUE (the array Number participates). No probe
//!   contradicts participation. This is now identical to `AND`/`OR`'s range
//!   rule (`func_and`/`func_or`); the pre-OXP-208 "ignore numbers" reading is
//!   retired. See `docs/plans/2026-07-15-lane5-probe-needed.md` (L5-7).
//! - Any error, scalar or within a range, propagates immediately in
//!   left-to-right / first-cell-scanned order — the same short-circuit policy
//!   as `AND`/`OR`/`SUM` (`OXP-082`, reused).
//! - "If the specified range contains no logical values, XOR returns the
//!   #VALUE! error value." If **zero** `Bool` values are found across all
//!   arguments, the result is `#VALUE!`.
//!
//! # Scalar `Blank` argument — REFUSED (loud), pending a probe
//! A **scalar** `Blank` argument (a bare reference to an empty cell, or an
//! elided argument slot) returns `#UNSUPPORTED!`. `AND`/`OR` pinned "a scalar
//! blank is excluded, mirroring a range-blank cell" via OXP-094, but that farm
//! run did **not** cover `XOR`; extrapolating its result to `XOR` would be a
//! guess (a hard design rule). Range/array blank cells are handled per the MS
//! page (ignored). See `docs/plans/2026-07-15-lane5-probe-needed.md` (L5-1).

use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value, to_bool};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Evaluate an `XOR(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut found = false;
    // Parity accumulator: flips on every contributing TRUE.
    let mut result = false;

    for i in 0..args.count() {
        match args.shape(i) {
            ArgShape::Omitted | ArgShape::Scalar => {
                let v = args.eval_scalar(i);
                // A scalar Blank is REFUSED loudly (L5-1): whether it counts as
                // FALSE or is excluded is unpinned for XOR (AND/OR's OXP-094
                // does not cover it).
                if v.is_blank() {
                    return Value::Error(ErrorKind::Unsupported);
                }
                match to_bool(&v) {
                    Ok(b) => {
                        found = true;
                        result ^= b;
                    }
                    Err(k) => return Value::Error(k),
                }
            }
            ArgShape::Range | ArgShape::Array => {
                let mut err: Option<ErrorKind> = None;
                let found_ref = &mut found;
                let result_ref = &mut result;
                args.for_each_cell(i, &mut |v| match v {
                    Value::Bool(b) => {
                        *found_ref = true;
                        *result_ref ^= *b;
                        ControlFlow::Continue(())
                    }
                    // OXP-213 (RUN-2026-07-16-oracle01, live-COM): a Number cell
                    // inside a range/array PARTICIPATES in the parity via its
                    // 0-vs-nonzero value (0 → FALSE, nonzero → TRUE), exactly as
                    // AND/OR do post-OXP-208 — NOT ignored. `to_bool(&Number)`
                    // never errors, so this inlines its 0-vs-nonzero test.
                    Value::Number(n) => {
                        *found_ref = true;
                        *result_ref ^= *n != 0.0;
                        ControlFlow::Continue(())
                    }
                    // MS page: text/empty cells in an array or reference are
                    // ignored (non-participating, never set `found`).
                    Value::Text(_) | Value::Blank => ControlFlow::Continue(()),
                    Value::Error(k) => {
                        err = Some(*k);
                        ControlFlow::Break(())
                    }
                    // Not expected from a materialized range cell; treated the
                    // same as AND/OR do for these shapes. A `Lambda` is
                    // engine-internal (RFC-0012 BC-6) and never a legitimate
                    // cell value — refuse loudly rather than guess a parity
                    // contribution (BC-11: no `_` catch-all).
                    Value::Array(_) | Value::Ref(_) | Value::Lambda(_) => {
                        err = Some(ErrorKind::Unsupported);
                        ControlFlow::Break(())
                    }
                });
                if let Some(k) = err {
                    return Value::Error(k);
                }
            }
        }
    }

    if !found {
        // "If the specified range contains no logical values, XOR returns the
        // #VALUE! error value."
        return Value::Error(ErrorKind::Value);
    }
    Value::Bool(result)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // Two TRUEs → even → FALSE.
    #[test]
    fn two_trues_is_false() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Bool(true)), Scalar(Value::Bool(true))]
            ),
            Value::Bool(false)
        );
    }

    // One TRUE among falses → odd → TRUE.
    #[test]
    fn single_true_is_true() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(false)),
                    Scalar(Value::Bool(true)),
                    Scalar(Value::Bool(false)),
                ]
            ),
            Value::Bool(true)
        );
    }

    // Three TRUEs → odd → TRUE.
    #[test]
    fn three_trues_is_true() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(true)),
                    Scalar(Value::Bool(true)),
                    Scalar(Value::Bool(true)),
                ]
            ),
            Value::Bool(true)
        );
    }

    // All FALSE → zero TRUEs → even → FALSE (but a logical value WAS found).
    #[test]
    fn all_false_is_false() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Bool(false)), Scalar(Value::Bool(false))]
            ),
            Value::Bool(false)
        );
    }

    // Scalar number coercion via to_bool: 0→FALSE, nonzero→TRUE. XOR(1, 0, 5):
    // TRUE ^ FALSE ^ TRUE = FALSE (two TRUEs).
    #[test]
    fn scalar_number_coercion() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(1.0)), Scalar(num(0.0)), Scalar(num(5.0))]
            ),
            Value::Bool(false)
        );
        // XOR(1, 0) → TRUE ^ FALSE = TRUE (one TRUE).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0)), Scalar(num(0.0))]),
            Value::Bool(true)
        );
    }

    // "TRUE"/"FALSE" text coerces case-insensitively via to_bool.
    #[test]
    fn text_true_false_coercion() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("true")), Scalar(txt("FALSE"))]),
            Value::Bool(true)
        );
    }

    // Non-coercible scalar text → #VALUE! (via to_bool).
    #[test]
    fn non_logical_text_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("banana")), Scalar(Value::Bool(true))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // OXP-213 (RUN-2026-07-16-oracle01, live-COM): Bool AND Number cells in a
    // range participate (0 → FALSE, nonzero → TRUE); Text/Blank are ignored.
    // Range [TRUE, 5, "x", TRUE, Blank] → TRUE, 5→TRUE, TRUE = three TRUEs →
    // odd → TRUE.
    #[test]
    fn range_bool_and_number_cells_participate() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    Value::Bool(true),
                    num(5.0),
                    txt("x"),
                    Value::Bool(true),
                    Value::Blank,
                ])]
            ),
            Value::Bool(true)
        );
        // [TRUE, 5, "x", Blank] → TRUE, 5→TRUE = two TRUEs → even → FALSE.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    Value::Bool(true),
                    num(5.0),
                    txt("x"),
                    Value::Blank
                ])]
            ),
            Value::Bool(false)
        );
    }

    // OXP-213 exact discriminating probes (Excel's own values), scaffold
    // A1=1,A2=0,A3=TRUE,B1=1,B2=TRUE. Each would differ if numbers were ignored.
    #[test]
    fn oxp213_numbers_in_range_participate() {
        // =XOR(A1:A2) {1,0} → TRUE (one TRUE). Ignored ⇒ #VALUE! (no logical).
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(1.0), num(0.0)])]),
            Value::Bool(true)
        );
        // =XOR(A1:A3) {1,0,TRUE} → FALSE (two TRUEs). Ignored ⇒ TRUE (lone TRUE).
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), num(0.0), Value::Bool(true)])]
            ),
            Value::Bool(false)
        );
        // =XOR(B1:B2) {1,TRUE} → FALSE (two TRUEs). Ignored ⇒ TRUE (lone TRUE).
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(1.0), Value::Bool(true)])]),
            Value::Bool(false)
        );
        // =XOR(FALSE,{1;0}) → TRUE (one TRUE). Ignored ⇒ FALSE (lone FALSE).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Bool(false)), Range(vec![num(1.0), num(0.0)])]
            ),
            Value::Bool(true)
        );
    }

    // A range with NO logical cells (text/blank only — numbers now count) →
    // #VALUE!. Matches OXP-213 D5 =XOR(A4:A5) over {"abc", blank} → #VALUE!.
    #[test]
    fn no_logical_values_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Range(vec![txt("abc"), Value::Blank])]),
            Value::Error(ErrorKind::Value)
        );
    }

    // An error in a scalar argument propagates.
    #[test]
    fn scalar_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(true)),
                    Scalar(Value::Error(ErrorKind::Div0))
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    // An error within a range propagates (first cell scanned).
    #[test]
    fn range_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    Value::Bool(true),
                    Value::Error(ErrorKind::Na),
                    Value::Bool(false),
                ])]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // A scalar Blank argument is REFUSED loudly (L5-1) — unpinned for XOR.
    #[test]
    fn scalar_blank_refused_loudly() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Blank), Scalar(Value::Bool(true))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // A range containing ONLY a blank cell contributes no logical value → the
    // blank is ignored (per the MS page), leaving zero logical values found →
    // #VALUE!. This is the range form and is NOT refused (contrast the scalar
    // blank above).
    #[test]
    fn range_blank_cell_ignored_not_refused() {
        assert_eq!(
            eval_direct(eval, vec![Range(vec![Value::Blank])]),
            Value::Error(ErrorKind::Value)
        );
    }
}

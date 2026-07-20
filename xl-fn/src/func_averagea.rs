//! `AVERAGEA` — arithmetic mean that, unlike `AVERAGE`, **counts** text and
//! logical values (text as `0`, `TRUE`/`FALSE` as `1`/`0`) toward both the sum
//! and the denominator.
//!
//! # Provenance
//! Behavior contract: `docs/specs/AVERAGEA.md`, clean-room from the Microsoft
//! Learn AVERAGEA function page
//! (`https://support.microsoft.com/en-us/office/averagea-function-f5f84098-d453-4f4c-bbba-3d2c66356091`,
//! verified 2026-07-11), cross-checked against `AVERAGE.md` (the sum/count
//! skeleton and the `#DIV/0!`-on-empty rule) and against `func_mina`/`func_maxa`
//! (the `*A` scalar coercion — OXP-130). The mean skeleton (scalar
//! [`CoercionMode::Scalar`] path, range streaming, `#DIV/0!` on an empty
//! aggregate) is `AVERAGE`'s; the range-cell rule is AVERAGEA's own.
//!
//! # The one thing AVERAGEA adds over AVERAGE (and why it is *not* MINA/MAXA)
//! The AVERAGEA page's Remarks are internally self-contradictory in exactly the
//! same way the MINA/MAXA pages are — one bullet says *"Array or reference
//! arguments that contain text evaluate as 0 (zero)"*, the next says *"Empty
//! cells and text values in the array or reference are ignored"*. For MINA/MAXA
//! the oracle (OXP-127, RUN-2026-07-11-oracle01) resolved that contradiction in
//! favor of **ignored** (`MINA({5,"hello"})` = `5`). **AVERAGEA is different**:
//! its own worked example *discriminates* the two readings and pins the
//! opposite. Data `A2:A6 = {10, 7, 9, 2, "Not available"}`, `=AVERAGEA(A2:A6)`
//! = **`5.6`**. That is `28 / 5` — the text cell `"Not available"` contributed
//! `0` **and** was counted (denominator `5`). Had range text been *ignored* the
//! result would be `28 / 4` = `7.0`. So for AVERAGEA, **non-numeric text drawn
//! from a range evaluates to `0` and IS counted** (the "text evaluate as 0"
//! bullet wins, pinned by the worked example, not by prose alone). Blindly
//! mirroring MINA/MAXA's range-text-skip here would reproduce `7.0` and
//! contradict the documented `5.6`.
//!
//! # Semantics implemented (spec bullets in parentheses)
//!
//! ## Range / array arguments
//! - Numbers count as themselves (AVERAGEA.md §Range).
//! - `TRUE`/`FALSE` count as `1`/`0` — the logical-counting rule AVERAGEA
//!   shares with the `*A` family and adds over `AVERAGE` (MS Learn "Arguments
//!   that contain TRUE evaluate as 1; arguments that contain FALSE evaluate as
//!   0"). **This holds for a range/reference.**
//!   - **Logical inside an ARRAY CONSTANT is DROPPED (OXP-193, RUN 2026-07-13).**
//!     A top-level array constant `{...}` **drops** logicals (`AVERAGEA({4,TRUE})`
//!     = `4`, `AVERAGEA({-5,FALSE})` = `-5` — not `2.5`/`-2.5`), exactly as
//!     `MAXA`/`MINA` do (OXP-188) and `SUM` does (OXP-006), while text still
//!     evaluates to `0` and is counted (`AVERAGEA({4,"x"})` = `2`) — the
//!     array-constant rule is *drop logicals, keep text→0*, a split distinct
//!     from `MAXA` (which drops text too). Implemented by branching on
//!     `ArgShape::Array` → [`averagea_array_constant_cell`]; a range/reference
//!     (incl. a single-cell ref) still counts logicals `1`/`0`.
//! - **Non-numeric text (including `""`) evaluates to `0` and IS counted** —
//!   pinned by the worked example (`5.6 = 28/5`, see above). This is the
//!   crucial AVERAGEA-vs-AVERAGE divergence: `AVERAGE` skips range text
//!   entirely; `AVERAGEA` folds it in as a counted `0`.
//! - A genuinely **blank** cell is ignored — neither summed nor counted (MS
//!   Learn "Empty cells ... in the array or reference are ignored"). Real range
//!   blanks are elided by [`CallArgs::for_each_cell`] before they reach the
//!   cell handler; the explicit `Blank` arm covers a blank inside an array
//!   constant.
//! - Any error cell propagates (first cell scanned wins — the `SUM`/`AVERAGE`
//!   short-circuit policy, `OXP-082`).
//! - **Numeric-looking text drawn from a range is DEFERRED (`#UNSUPPORTED!`).**
//!   The worked example exercises only *non-numeric* text; the "text
//!   representations of numbers ... are counted" clause is scoped to args you
//!   *type directly* (see the scalar path), not to range cells; so a range
//!   `"5"` has three live readings — `0`-and-counted (bullet 4), its value `5`,
//!   or ignored — and no oracle probe for AVERAGEA distinguishes them. Never
//!   guess (Recalc Principle 2). See `averagea_cell`.
//!
//! ## Direct (scalar) arguments — mirrors MINA/MAXA (OXP-130)
//! Coerced under [`CoercionMode::Scalar`], identical to `AVERAGE`/`MINA`/`MAXA`:
//! numbers pass through, `TRUE`/`FALSE` → `1`/`0`, **numeric** text → its value
//! (MS Learn "text representations of numbers that you type directly ... are
//! counted"), non-numeric text → `#VALUE!` (MS Learn "text that cannot be
//! translated into numbers cause errors"; OXP-130 pinned this for the `*A`
//! twins), errors propagate. Every non-error scalar counts toward the
//! denominator.
//!
//! ## Empty aggregate
//! With no countable value found anywhere, AVERAGEA returns `#DIV/0!` (a mean
//! of nothing), matching `AVERAGE.md` §4 — *not* `MINA`/`MAXA`'s "no values →
//! 0", because AVERAGEA divides.
//!
//! # Deferrals (never guess — Recalc Principle 2)
//! - **Scalar `Blank` argument** (a bare reference to an empty cell, or an
//!   omitted slot) → `#UNSUPPORTED!`. `AVERAGE`'s scalar blank was probed and
//!   *excluded* (OXP-083) and MINA/MAXA's was probed and *skipped* (OXP-131),
//!   but both are function-specific runs; AVERAGEA — which counts strictly more
//!   than AVERAGE — has no probe of its own, and AVERAGE-vs-AVERAGEA may
//!   diverge here. See the `// OXP (unassigned)` note in `eval`.
//! - **Numeric-looking text drawn from a range** → `#UNSUPPORTED!` (see above).

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{ArgShape, CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Evaluate an `AVERAGEA(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut sum = 0.0_f64;
    let mut count = 0u64;

    for i in 0..args.count() {
        // RFC 0010: a single-cell REFERENCE aggregates like a range — its text
        // evaluates to a counted 0 (AVERAGEA's range rule), numeric-looking text
        // is DEFERRED, and a blank is skipped — while only a scalar LITERAL /
        // omitted slot takes the scalar path below.
        match eff_shape(args, i) {
            EffShape::Omitted | EffShape::ScalarLiteral => {
                let v = args.eval_scalar_array_arg(i);
                if v.is_blank() {
                    // A scalar Blank (bare empty-cell reference or omitted slot)
                    // has no AVERAGEA-specific oracle probe. AVERAGE's OXP-083
                    // (excluded) and MINA/MAXA's OXP-131 (skipped) are
                    // per-function runs, and AVERAGEA counts strictly more than
                    // AVERAGE, so their answers do not transfer. Never guess.
                    //
                    // OXP (unassigned): =AVERAGEA(A1,5) and =AVERAGEA(,5) with A1
                    // empty; compare =AVERAGEA(A1:A1,5). Does a scalar blank
                    // exclude (like AVERAGE's OXP-083 → 5) or count as 0 (→ 2.5)?
                    return Value::Error(ErrorKind::Unsupported);
                }
                // Direct/scalar coercion is identical to AVERAGE/MINA/MAXA:
                // numbers pass, TRUE/FALSE → 1/0, numeric text → its value,
                // non-numeric text → #VALUE! (OXP-130). Every non-error scalar
                // is counted.
                match coerce_number_arg(&v, CoercionMode::Scalar) {
                    NumericArg::Number(n) => {
                        sum += n;
                        count += 1;
                    }
                    // CoercionMode::Scalar never yields Skip.
                    NumericArg::Skip => {}
                    NumericArg::Error(k) => return Value::Error(k),
                }
            }
            EffShape::Aggregate => {
                // OXP-193 (RUN-2026-07-13): a logical inside a top-level ARRAY
                // CONSTANT `{...}` is DROPPED (not counted 1/0), while text still
                // counts as 0 — so array constants use a resolver that differs
                // from the range/reference one only in the `Bool` arm. A
                // single-cell reference rides this Aggregate arm but is
                // `ArgShape::Scalar`, so it still counts logicals like a range.
                let resolve = if matches!(args.shape(i), ArgShape::Array) {
                    averagea_array_constant_cell
                } else {
                    averagea_cell
                };
                let mut err: Option<ErrorKind> = None;
                let sum_ref = &mut sum;
                let count_ref = &mut count;
                args.for_each_cell(i, &mut |v| match resolve(v) {
                    RangeCell::Count(n) => {
                        *sum_ref += n;
                        *count_ref += 1;
                        ControlFlow::Continue(())
                    }
                    RangeCell::Skip => ControlFlow::Continue(()),
                    RangeCell::Error(k) => {
                        err = Some(k);
                        ControlFlow::Break(())
                    }
                    RangeCell::Defer => {
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

    if count == 0 {
        // A mean of nothing — matches AVERAGE (AVERAGE.md §4), not MINA/MAXA's
        // "no values → 0" (AVERAGEA divides).
        return Value::Error(ErrorKind::Div0);
    }
    Value::number(sum / count as f64)
}

/// Outcome of resolving one **range/array** cell for `AVERAGEA`.
enum RangeCell {
    /// Add this number to the sum **and** increment the count. Numbers,
    /// logicals (`TRUE`→1/`FALSE`→0), and non-numeric text (→`0`) all land
    /// here — AVERAGEA counts each of them.
    Count(f64),
    /// Skip entirely — neither summed nor counted (a genuinely blank cell).
    Skip,
    /// Propagate this error out of the whole call.
    Error(ErrorKind),
    /// Defer to `#UNSUPPORTED!` — a shape/edge AVERAGEA's documentation and
    /// worked example do not pin (numeric-looking range text; a nested
    /// array/ref). Never guessed (Recalc Principle 2).
    Defer,
}

/// Resolve a value drawn from a **range/array** AVERAGEA argument per the
/// module docs' §Range rules.
///
/// The key AVERAGEA-vs-`AVERAGE`/`MINA`/`MAXA` divergence is here:
/// **non-numeric text is counted as `0`** (pinned by the `5.6 = 28/5` worked
/// example), whereas those functions skip range text. Numeric-looking range
/// text is the untested edge and is deferred.
fn averagea_cell(v: &Value) -> RangeCell {
    match v {
        Value::Number(n) => RangeCell::Count(*n),
        // TRUE → 1, FALSE → 0, both counted — the `*A` logical rule AVERAGEA
        // adds over AVERAGE for ranges.
        Value::Bool(b) => RangeCell::Count(if *b { 1.0 } else { 0.0 }),
        Value::Text(_) => match coerce_number_arg(v, CoercionMode::Scalar) {
            // Numeric-looking text from a *range* ("5", "20", …): three live
            // readings (0-and-counted / its value / ignored) and no AVERAGEA
            // probe to pick one. Defer.
            //
            // OXP (unassigned): =AVERAGEA(A1:A2) with A1=10 (number),
            // A2="20" (numeric text). 0-and-counted → 5, its value → 15,
            // ignored → 10.
            NumericArg::Number(_) => RangeCell::Defer,
            // Non-numeric text (including empty text "") → 0, and it IS
            // counted. Documented ("text evaluate as 0") and pinned by the
            // worked example (28/5 = 5.6, not 28/4 = 7.0).
            NumericArg::Error(_) => RangeCell::Count(0.0),
            // CoercionMode::Scalar never yields Skip; treat defensively as the
            // non-numeric-text outcome.
            NumericArg::Skip => RangeCell::Count(0.0),
        },
        // Genuinely blank cell → ignored (MS Learn "Empty cells ... are
        // ignored"). Real range blanks are elided by `for_each_cell` upstream;
        // this arm covers an explicit blank inside an array constant.
        Value::Blank => RangeCell::Skip,
        Value::Error(k) => RangeCell::Error(*k),
        // A nested array constant or an unresolved reference inside a
        // range/array argument is not a shape this function resolves itself
        // (same guard as MINA/MAXA).
        Value::Array(_) | Value::Ref(_) => RangeCell::Defer,
        // BC-6 (RFC-0012): a lambda is refused loudly (`#UNSUPPORTED!`), not
        // deferred as an unprobed edge and not counted as text→0.
        Value::Lambda(_) => RangeCell::Error(ErrorKind::Unsupported),
    }
}

/// Resolve one cell of a top-level **array constant** `{...}` for `AVERAGEA`.
/// Identical to [`averagea_cell`] **except a logical is dropped** (not counted
/// `1`/`0`): **OXP-193 (RUN-2026-07-13)** pinned `AVERAGEA({4,TRUE})` = `4` and
/// `AVERAGEA({-5,FALSE})` = `-5` (the logical is dropped), while text still
/// counts as `0` (`AVERAGEA({4,"x"})` = `2`). This mirrors the array-constant
/// logical-drop of `MAXA`/`MINA` (OXP-188) and `SUM` (OXP-006), but AVERAGEA
/// keeps its own text→`0` rule (a split distinct from `MAXA`, which drops text).
fn averagea_array_constant_cell(v: &Value) -> RangeCell {
    match v {
        // OXP-193: a logical inside an array constant is dropped, not 1/0.
        Value::Bool(_) => RangeCell::Skip,
        // BC-6 (RFC-0012, B2 ROUTE): a lambda in an array constant must NOT
        // silently flow into `averagea_cell` (a compute path) — refuse at the
        // leak point so this match makes its own decision.
        Value::Lambda(_) => RangeCell::Error(ErrorKind::Unsupported),
        _ => averagea_cell(v),
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // --- numbers only ------------------------------------------------------

    #[test]
    fn scalar_numbers_mean() {
        // (10 + 7 + 9 + 2) / 4 = 7.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(10.0)),
                    Scalar(num(7.0)),
                    Scalar(num(9.0)),
                    Scalar(num(2.0)),
                ]
            ),
            num(7.0)
        );
    }

    #[test]
    fn range_numbers_mean() {
        // (2 + 4 + 6) / 3 = 4.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(2.0), num(4.0), num(6.0)])]),
            num(4.0)
        );
    }

    // --- the AVERAGEA worked example: range text = 0 AND counted -----------

    #[test]
    fn worked_example_range_text_is_zero_and_counted() {
        // Microsoft Learn AVERAGEA page: A2:A6 = {10, 7, 9, 2, "Not available"},
        // =AVERAGEA(A2:A6) = 5.6. That is 28/5 — the text cell contributes 0 and
        // IS counted (denominator 5). If text were IGNORED it would be 28/4 = 7.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(10.0),
                    num(7.0),
                    num(9.0),
                    num(2.0),
                    txt("Not available"),
                ])]
            ),
            num(5.6)
        );
    }

    #[test]
    fn range_text_counted_diverges_from_average() {
        // {4, "x"} → sum 4, count 2 → 2. AVERAGE would skip the text → 4/1 = 4.
        // This locks in the AVERAGEA side of the divergence: text is a counted 0.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(4.0), txt("x")])]),
            num(2.0)
        );
    }

    #[test]
    fn range_empty_text_is_zero_and_counted() {
        // MS Learn: "Empty text ("") evaluates as 0 (zero)." {6, ""} → 6/2 = 3.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(6.0), txt("")])]),
            num(3.0)
        );
    }

    // --- booleans counted as 1/0 -------------------------------------------

    #[test]
    fn range_true_counts_as_one() {
        // {TRUE, 3} → (1 + 3) / 2 = 2.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![Value::Bool(true), num(3.0)])]),
            num(2.0)
        );
    }

    #[test]
    fn range_false_counts_as_zero() {
        // {FALSE, 3} → (0 + 3) / 2 = 1.5. FALSE is a counted 0, not skipped.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![Value::Bool(false), num(3.0)])]),
            num(1.5)
        );
    }

    #[test]
    fn array_constant_drops_logicals_keeps_text_zero_oxp193() {
        // OXP-193 (RUN-2026-07-13): a logical inside an ARRAY CONSTANT is
        // DROPPED (not counted 1/0), unlike a range/reference. Excel pins
        // AVERAGEA({4,TRUE}) = 4 (TRUE dropped → mean of {4}) and
        // AVERAGEA({-5,FALSE}) = -5. But text still counts as 0:
        // AVERAGEA({4,"x"}) = 2 (mean of {4,0}).
        assert_eq!(
            eval_direct(eval, vec![Array(vec![num(4.0), Value::Bool(true)])]),
            num(4.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Array(vec![num(-5.0), Value::Bool(false)])]),
            num(-5.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Array(vec![num(4.0), txt("x")])]),
            num(2.0),
            "text still counts as 0 in an array constant"
        );
        // Regression guard: a RANGE with the same contents still counts TRUE=1.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(4.0), Value::Bool(true)])]),
            num(2.5)
        );
        // Numeric text inside an array constant still DEFERS loudly (unpinned —
        // must not silently start counting it for the Array shape either).
        assert_eq!(
            eval_direct(eval, vec![Array(vec![num(4.0), txt("5")])]),
            Value::Error(ErrorKind::Unsupported),
            "numeric text in an array constant defers, not counts"
        );
    }

    #[test]
    fn scalar_bools_counted() {
        // AVERAGEA(TRUE, FALSE, 4) → (1 + 0 + 4) / 3 = 5/3.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(true)),
                    Scalar(Value::Bool(false)),
                    Scalar(num(4.0)),
                ]
            ),
            num(5.0 / 3.0)
        );
    }

    // --- blank excluded from a range ---------------------------------------

    #[test]
    fn range_blank_is_excluded_not_counted() {
        // A genuinely blank cell in a range is ignored (not a counted 0).
        // {8, Blank, 4} → (8 + 4) / 2 = 6, NOT (8 + 0 + 4) / 3.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(8.0), Value::Blank, num(4.0)])]),
            num(6.0)
        );
    }

    // --- mixed --------------------------------------------------------------

    #[test]
    fn mixed_number_text_bool_blank_in_range() {
        // {10, "x", TRUE, FALSE, Blank, 4} → counted: 10, 0(text), 1, 0, 4
        // (blank skipped) → sum 15, count 5 → 3.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(10.0),
                    txt("x"),
                    Value::Bool(true),
                    Value::Bool(false),
                    Value::Blank,
                    num(4.0),
                ])]
            ),
            num(3.0)
        );
    }

    #[test]
    fn scalar_numeric_text_counts_as_its_value() {
        // Directly-typed numeric text is counted as its value (OXP-130 scalar
        // rule): ("5", 3) → (5 + 3) / 2 = 4.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("5")), Scalar(num(3.0))]),
            num(4.0)
        );
    }

    // --- empty aggregate → #DIV/0! -----------------------------------------

    #[test]
    fn empty_aggregate_is_div0() {
        // No countable value anywhere → #DIV/0! (a mean of nothing), like
        // AVERAGE — NOT MINA/MAXA's "no values → 0".
        assert_eq!(
            eval_direct(eval, vec![Range(vec![])]),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn all_blank_range_is_div0() {
        assert_eq!(
            eval_direct(eval, vec![Range(vec![Value::Blank, Value::Blank])]),
            Value::Error(ErrorKind::Div0)
        );
    }

    // --- error propagation --------------------------------------------------

    #[test]
    fn range_error_cell_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), Value::Error(ErrorKind::Div0)])]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn scalar_error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(2.0)), Scalar(Value::Error(ErrorKind::Na))]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn scalar_non_numeric_text_is_value_error() {
        // Directly-typed non-numeric text → #VALUE! (OXP-130 scalar rule;
        // MS Learn "text that cannot be translated into numbers cause errors").
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("abc")), Scalar(num(2.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // --- deferrals (never guess — Recalc Principle 2) --------------------

    #[test]
    fn scalar_blank_is_deferred() {
        // No AVERAGEA probe for a scalar blank; AVERAGE (OXP-083) and MINA/MAXA
        // (OXP-131) answers don't transfer. → #UNSUPPORTED!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Blank), Scalar(num(5.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn omitted_slot_is_deferred() {
        assert_eq!(
            eval_direct(eval, vec![Omitted, Scalar(num(5.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn numeric_text_in_range_is_deferred() {
        // Numeric-looking text drawn from a range is the untested edge
        // (0-and-counted vs its value vs ignored). Defer, don't guess.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(10.0), txt("20")])]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // RFC 0010: a single-cell *reference* to non-numeric text now rides
    // AVERAGEA's range rule — it evaluates to a counted `0` (NOT `#VALUE!`), so
    // `AVERAGEA(text_ref, 6)` = (0 + 6)/2 = 3.
    #[test]
    fn rfc0010_text_reference_counts_as_zero_not_value_error() {
        assert_eq!(
            eval_direct(eval, vec![CellRef(txt("x")), CellRef(num(6.0))]),
            num(3.0)
        );
    }

    // RFC 0010: a scalar *literal* still coerces under `CoercionMode::Scalar`,
    // so non-numeric text typed directly is `#VALUE!` (the sharp contrast with
    // the counted-0 reference above).
    #[test]
    fn rfc0010_scalar_literal_still_coerces() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("x")), Scalar(num(6.0))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // RFC 0010 explicit consequence (the contract review): a single-cell NUMERIC-text
    // reference routes through AVERAGEA's range rule, whose numeric-text handling
    // is oracle-deferred (unpinned whether "5" counts as 5 or 0) → the aggregate
    // defers LOUDLY (`#UNSUPPORTED!`) instead of the old silent scalar-coerced
    // value. This is the correct Principle-2 outcome, not a regression.
    #[test]
    fn rfc0010_numeric_text_reference_defers_loudly() {
        assert_eq!(
            eval_direct(eval, vec![CellRef(txt("5")), CellRef(num(6.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

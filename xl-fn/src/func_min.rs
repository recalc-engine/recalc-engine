//! `MIN` — the smallest numeric value across all arguments.
//!
//! # Provenance
//! Behavior contract: `docs/specs/MIN.md` (which cites the Microsoft Learn
//! MIN function page). Coercion is deferred entirely to `xl-value`
//! ([`coerce_number_arg`] with the two [`CoercionMode`]s), the same split
//! `SUM` uses.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Direct **scalar** arguments coerce under [`CoercionMode::Scalar`]:
//!   numbers pass through, `TRUE`/`FALSE` → 1/0, numeric text → its number
//!   (MIN.md §Coercion, mirroring the SUM/COUNT direct-arg family). A
//!   scalar text that cannot parse as a number is `#VALUE!`.
//! - **Range / array** arguments aggregate under
//!   [`CoercionMode::RangeAggregate`]: only real numbers participate; blank,
//!   boolean, and text cells inside the range are ignored entirely
//!   (MIN.md §1/§2, §Coercion).
//! - Any argument that evaluates to an error propagates it — first error in
//!   left-to-right argument order / first cell scanned within a range wins,
//!   the same short-circuit policy `SUM` uses (`OXP-082`; MIN.md
//!   §Error behavior explicitly notes "all-error argument set precedence
//!   order" as its own open oracle question, covered by the same policy).
//! - With no numeric values found anywhere, the result is `0`, never an
//!   error — this is the documented contrast with `AVERAGE`'s `#DIV/0!`
//!   (MIN.md §3, §Error behavior).
//!
//! # Scalar blank argument (`OXP-086`, RUN-2026-07-16-oracle01) — split resolution
//! MIN.md's "Oracle experiments needed" asked whether a **scalar** argument
//! evaluating to [`Value::Blank`] counts as `0` (SUM-like, potentially becoming
//! the minimum) or is excluded (range-like). `OXP-086` H1 probed `=MIN(A1,5)`
//! with `A1` a **bare reference to an empty cell** → `5`: the blank is
//! **excluded**. This oracle-confirms the RFC-0010 bare-reference-aggregates
//! path already implemented below — a single-cell reference is routed through
//! the range arm by [`eff_shape`], so its blank cell is skipped (had the blank
//! counted as `0`, the result would be `0`, not `5`).
//!
//! The probe used a bare **reference**, so it pins ONLY the reference case. A
//! scalar **literal** blank or an **elided argument slot** (`MIN(,5)`) is a
//! DIFFERENT shape OXP-086 did not probe; extrapolating the reference result to
//! it would be a guess (a hard design rule), so that path stays
//! `#UNSUPPORTED!` pending its own probe. Every other scalar shape (number,
//! bool, numeric/non-numeric text, error) is fully supported.
//!
//! # `-0`/`+0` tie-break (`OXP-086`, RUN-2026-07-16-oracle01) — probe INCONCLUSIVE
//! MIN.md flags that Excel's sign-of-zero tie-break for equal-magnitude
//! `-0`/`0` candidates is not citable from public docs. `OXP-086` probed it
//! (`MIN(-0,0)`/`MIN(0,-0)` plus `1/MIN(...)` sign probes) but the result is
//! **inconclusive on the sign**: both `MIN(-0,0)` and `MIN(0,-0)` render as `0`
//! (H2/H3 — magnitude only; Excel displays both `±0` as "0"), and both
//! `1/MIN(-0,0)` and `1/MIN(0,-0)` are `#DIV/0!` (H4/H5), because Excel maps
//! division by *either* sign of zero to `#DIV/0!`. The sign of the zero MIN
//! keeps is therefore **not observable** through this probe. The implementation
//! keeps the natural running-minimum choice (the *first* candidate wins ties — a
//! later value replaces the running minimum only when *strictly* less — so
//! whichever sign of zero appears first is kept); Recalc reproduces every
//! recorded OXP-086 output (0 / 0 / #DIV/0! / #DIV/0!) regardless of that
//! internal sign, so nothing here is guessed. The sign itself stays unpinned.

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::{CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Evaluate a `MIN(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut min: Option<f64> = None;

    for i in 0..args.count() {
        // RFC 0010: a single-cell REFERENCE aggregates like a range (text/logical
        // ignored, a blank cell skipped — resolving the bare-ref half of
        // OXP-086); only a scalar LITERAL / omitted slot takes the scalar path,
        // where a blank stays oracle-deferred.
        match eff_shape(args, i) {
            // An omitted argument slot evaluates to `Blank` per
            // `CallArgs::eval_scalar`'s contract, so it hits the same
            // oracle-deferred scalar-blank case as a bare reference to an
            // empty cell (`OXP-086`, see the module docs).
            EffShape::Omitted | EffShape::ScalarLiteral => {
                let v = args.eval_scalar_array_arg(i);
                if v.is_blank() {
                    return Value::Error(ErrorKind::Unsupported);
                }
                match coerce_number_arg(&v, CoercionMode::Scalar) {
                    NumericArg::Number(n) => {
                        if min.is_none_or(|m| n < m) {
                            min = Some(n);
                        }
                    }
                    // CoercionMode::Scalar never yields Skip.
                    NumericArg::Skip => {}
                    NumericArg::Error(k) => return Value::Error(k),
                }
            }
            EffShape::Aggregate => {
                let mut err: Option<ErrorKind> = None;
                let min_ref = &mut min;
                args.for_each_cell(i, &mut |v| match coerce_number_arg(
                    v,
                    CoercionMode::RangeAggregate,
                ) {
                    NumericArg::Number(n) => {
                        if min_ref.is_none_or(|m| n < m) {
                            *min_ref = Some(n);
                        }
                        ControlFlow::Continue(())
                    }
                    NumericArg::Skip => ControlFlow::Continue(()),
                    NumericArg::Error(k) => {
                        err = Some(k);
                        ControlFlow::Break(())
                    }
                });
                if let Some(k) = err {
                    return Value::Error(k);
                }
            }
        }
    }

    Value::number(min.unwrap_or(0.0))
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn scalar_numbers_min() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(4.0)), Scalar(num(2.0)), Scalar(num(7.0))]
            ),
            num(2.0)
        );
    }

    #[test]
    fn range_skips_text_logical_blank() {
        // Only real numbers participate: {3, "x", TRUE, Blank, 5} → min 3.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(3.0),
                    txt("x"),
                    Value::Bool(true),
                    Value::Blank,
                    num(5.0),
                ])]
            ),
            num(3.0)
        );
    }

    // OXP-086 (RUN-2026-07-16-oracle01, Excel 16.0) — CONFIRMS the RFC-0010
    // bare-ref half: H1 =MIN(A1,5) with A1 a bare reference to an empty cell → 5
    // (the blank is EXCLUDED, not counted as 0). A single-cell reference is
    // routed through the range arm by `eff_shape`, so its blank is skipped.
    #[test]
    fn oxp086_bare_ref_blank_excluded_confirmed() {
        assert_eq!(
            eval_direct(eval, vec![CellRef(Value::Blank), Scalar(num(5.0))]),
            num(5.0)
        );
        // H2/H3: MIN(-0,0) / MIN(0,-0) → a zero-magnitude number. The oracle is
        // INCONCLUSIVE on the sign of zero (see module docs: both display as 0
        // and both 1/MIN(...) are #DIV/0!), so we assert only the magnitude —
        // `num(0.0) == num(-0.0)` under `f64` equality — never the sign.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-0.0)), Scalar(num(0.0))]),
            num(0.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0)), Scalar(num(-0.0))]),
            num(0.0)
        );
    }

    // RFC 0010: a single-cell *reference* to text/logical is ignored (the range
    // rule), so `MIN(num_ref, text_ref)` = the number, not `#VALUE!`.
    #[test]
    fn rfc0010_text_reference_is_ignored() {
        assert_eq!(
            eval_direct(eval, vec![CellRef(num(3.0)), CellRef(txt("x"))]),
            num(3.0)
        );
    }

    // RFC 0010: the scalar *literal* / omitted blank path is UNCHANGED — a
    // literal blank stays oracle-deferred (`OXP-086`), never guessed.
    #[test]
    fn rfc0010_scalar_literal_blank_still_deferred() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Blank), Scalar(num(5.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn scalar_literal_numeric_text_still_coerces() {
        // A directly-typed numeric text still coerces: MIN("5", 2) = 2.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("5")), Scalar(num(2.0))]),
            num(2.0)
        );
    }
}

//! `COUNT` — counts the cells/values that hold an actual number.
//!
//! # Provenance
//! Behavior contract: `docs/specs/COUNT.md` (which cites the Microsoft
//! Learn COUNT function page, verified 2026-07-05).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - **Range / array** arguments: only cells holding an actual number
//!   (including date/time serials, which are just numbers) are counted;
//!   blank, text, and logical cells are skipped without incrementing the
//!   count (COUNT.md §1/§2). This is `xl-value`'s
//!   [`CoercionMode::RangeAggregate`] `Number`/`Skip` split.
//! - **Range / array errors never propagate and are never counted.**
//!   COUNT.md §Error behavior states this in prose (not oracle-flagged):
//!   "COUNT is one of the few aggregate functions documented as *not*
//!   necessarily propagating errors encountered inside range arguments —
//!   they are simply not counted." So a cell holding `#DIV/0!` inside a
//!   range/array argument is silently skipped, exactly like a text cell —
//!   `coerce_number_arg`'s `RangeAggregate::Error` outcome is deliberately
//!   *not* propagated here (unlike `SUM`/`AVERAGE`/`MIN`/`MAX`).
//! - **Scalar number**: counts (trivially — it already is a number).
//! - **Scalar numeric text**: counts. COUNT.md §Semantics bullet 3 quotes
//!   the docs' explicit list of countable *direct* argument forms —
//!   "a number, a date, a text representation of a number" — so a scalar
//!   text that parses as a number (`to_number` succeeds) counts.
//! - **Scalar non-numeric text**: does not count, and does not error. This
//!   is the complement of the exhaustively-documented countable-forms list
//!   above (not itself independently oracle-flagged), consistent with
//!   COUNT's overall "only numbers, never an error" character.
//! - **Scalar blank** (a bare reference to an empty cell, or an elided
//!   argument slot — both evaluate to [`Value::Blank`] per
//!   [`CallArgs::eval_scalar`]'s contract): does not count. Not itself
//!   oracle-flagged by COUNT.md; it is the natural extension of the range
//!   rule (a blank cell is not a number) to the scalar case.
//!
//! # Scalar logical argument (`OXP-084`, RESOLVED RUN-2026-07-11-oracle01)
//! COUNT.md §Coercion left open whether a **direct** `TRUE`/`FALSE`
//! argument counts. The farm ran the probe (`RUN-2026-07-11-oracle01`):
//! `=COUNT(TRUE, FALSE, 1)` = 3 and `=COUNT(TRUE)` = 1 — a direct logical
//! argument **is counted** (unlike a logical *cell* inside a range, which
//! is skipped). Implemented as `count += 1` for a scalar `Bool`.
//!
//! # Scalar error argument (`OXP-085`, RESOLVED RUN-2026-07-11-oracle01)
//! COUNT.md §Error behavior left open whether a **direct** error argument
//! (e.g. `COUNT(1, #DIV/0!)`) propagates the error or is silently skipped
//! like a range error cell. The farm ran the probe
//! (`RUN-2026-07-11-oracle01`): `=COUNT(1, #DIV/0!)` = 1, `=COUNT(1, 1/0)`
//! = 1, `=COUNT(1/0, 1)` = 1 — a direct error argument is **silently
//! skipped** (neither counted nor propagated), matching the range-error
//! rule. Implemented as a no-op for a scalar `Error`.
//!
//! # Recalc sentinels (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`) propagate
//! The two "errors are silently skipped" rules above (range-error and
//! OXP-085's scalar-error) are about **genuine Excel errors** — cells/args
//! where Excel itself really would show `#DIV/0!` etc. A Recalc sentinel
//! ([`xl_value::ErrorKind::is_recalc_sentinel`]) is different in kind: it is
//! Recalc's own admission that a cell's true value was never computed at
//! all, so whether real Excel would have found a number there is unknowable.
//! Silently skipping it (the old, uniform "any error is skipped" behavior)
//! would launder that gap into a specific, possibly-wrong count. Per
//! Recalc Principle 2 ("never silently wrong"), COUNT instead
//! **propagates the sentinel's exact kind** (never collapsed to a different
//! sentinel), at the first sentinel cell/argument encountered in left-to-
//! right scan order — covering both a range/array cell and a scalar direct
//! argument. Real (non-sentinel) errors are completely unaffected: they keep
//! the exact COUNT.md §Error-behavior / OXP-085 skip.
//!
//! This also closes a related gap for free: an unresolvable range/ref
//! surfaces to [`CallArgs::for_each_cell`] as a synthetic
//! `Value::Error(ErrorKind::Unsupported)` visit (see its doc contract).
//! Before this fix that synthetic visit was silently skipped like any other
//! error (yielding a wrong `0` for e.g. an unresolvable 3-D span); it is now
//! correctly propagated as `#UNSUPPORTED!`.

use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value, to_number};

use crate::args::{CallArgs, EffShape, eff_shape};
use crate::context::EvalContext;

/// Evaluate a `COUNT(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let mut count = 0u64;

    for i in 0..args.count() {
        // RFC 0010: a single-cell REFERENCE is counted by the range rule (only a
        // real *number* cell counts — a logical/text/blank cell does not), while
        // a scalar LITERAL keeps COUNT's direct-argument rules (a typed logical
        // counts, OXP-084). `eff_shape` splits the two.
        match eff_shape(args, i) {
            // An omitted argument slot evaluates to `Blank` per
            // `CallArgs::eval_scalar`'s contract — not counted, same as a
            // scalar blank cell reference (see the module docs).
            EffShape::Omitted | EffShape::ScalarLiteral => {
                let v = args.eval_scalar_array_arg(i);
                match v {
                    Value::Number(_) => count += 1,
                    // OXP-084 (RUN-2026-07-11-oracle01, RESOLVED): a direct
                    // logical argument IS counted. Observed
                    // `=COUNT(TRUE, FALSE, 1)` = 3, `=COUNT(TRUE)` = 1.
                    Value::Bool(_) => count += 1,
                    // OXP-085 (RUN-2026-07-11-oracle01, RESOLVED): a direct
                    // *genuine* error argument is silently skipped — neither
                    // counted nor propagated, exactly like a range error
                    // cell. Observed `=COUNT(1, #DIV/0!)` = 1,
                    // `=COUNT(1, 1/0)` = 1, `=COUNT(1/0, 1)` = 1. A Recalc
                    // sentinel is different: propagate it (kind preserved)
                    // instead of laundering the gap into a silent skip (see
                    // the module docs).
                    Value::Error(k) => {
                        if k.is_recalc_sentinel() {
                            return Value::Error(k);
                        }
                    }
                    // A direct text arg counts only if it parses as a
                    // number (COUNT.md §Semantics bullet 3); genuinely
                    // non-numeric text is silently not counted. But a
                    // *deferral* (`to_number` returns a Recalc sentinel, e.g.
                    // `"$ 5"` / a no-year date text we refuse to pin) is NOT a
                    // definite "not a number" — treating it as uncounted
                    // launders an explicit gap into a silent count, so propagate
                    // it (kind preserved), mirroring the `Value::Error` arm
                    // above (coercion-consumer audit, 2026-07-12).
                    Value::Text(_) => match to_number(&v) {
                        Ok(_) => count += 1,
                        Err(k) if k.is_recalc_sentinel() => return Value::Error(k),
                        Err(_) => {}
                    },
                    // Blank (bare or omitted) is not a number: not counted.
                    Value::Blank => {}
                    // Load-bearing under M2 lane 6 (RFC-0011 array context):
                    // a genuine multi-cell consumed array can now reach COUNT's
                    // scalar arm via the materialization gate. COUNT over a
                    // consumed array is UNPINNED (only SUM's fold is pinned by
                    // OXP-201) → refuse loudly (Principle 2) rather than fold it
                    // into COUNT's silent non-numeric skip. Also covers the base
                    // case (a bare multi-cell array / unresolved ref reaching
                    // here outside array context).
                    Value::Array(_) | Value::Ref(_) => {
                        return Value::Error(ErrorKind::Unsupported);
                    }
                    // BC-6 (RFC-0012): a lambda is not a number and is not a
                    // COUNT-countable value — refuse loudly rather than fold it
                    // into COUNT's silent non-numeric skip. Its own arm.
                    Value::Lambda(_) => {
                        return Value::Error(ErrorKind::Unsupported);
                    }
                }
            }
            EffShape::Aggregate => {
                let mut sentinel: Option<ErrorKind> = None;
                args.for_each_cell(i, &mut |v| {
                    // A Recalc sentinel propagates (kind preserved) instead
                    // of being silently skipped — see the module docs. This
                    // also covers the synthetic `Error(Unsupported)` visit
                    // `for_each_cell` surfaces for an unresolvable range.
                    if let Value::Error(k) = v {
                        if k.is_recalc_sentinel() {
                            sentinel = Some(*k);
                            return ControlFlow::Break(());
                        }
                        // A genuine error cell is silently skipped — COUNT
                        // never propagates a range error (COUNT.md §Error
                        // behavior).
                        return ControlFlow::Continue(());
                    }
                    // Only real numbers count; text/logical/blank cells are
                    // silently skipped.
                    if matches!(v, Value::Number(_)) {
                        count += 1;
                    }
                    ControlFlow::Continue(())
                });
                if let Some(k) = sentinel {
                    return Value::Error(k);
                }
            }
        }
    }

    Value::number(count as f64)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // OXP-084 (RUN-2026-07-11-oracle01): a direct logical argument IS counted.
    // Observed `=COUNT(TRUE, FALSE, 1)` = 3.
    #[test]
    fn scalar_logicals_counted() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(true)),
                    Scalar(Value::Bool(false)),
                    Scalar(num(1.0)),
                ]
            ),
            num(3.0)
        );
    }

    // OXP-084 (RUN-2026-07-11-oracle01): observed `=COUNT(TRUE)` = 1.
    #[test]
    fn single_scalar_logical_counted() {
        assert_eq!(eval_direct(eval, vec![Scalar(Value::Bool(true))]), num(1.0));
    }

    // OXP-085 (RUN-2026-07-11-oracle01): a direct error argument is silently
    // skipped — neither counted nor propagated. Observed
    // `=COUNT(1, #DIV/0!)` = 1, `=COUNT(1, 1/0)` = 1, `=COUNT(1/0, 1)` = 1.
    #[test]
    fn scalar_error_skipped_trailing() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(1.0)), Scalar(Value::Error(ErrorKind::Div0))]
            ),
            num(1.0)
        );
    }

    #[test]
    fn scalar_error_skipped_leading() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(Value::Error(ErrorKind::Div0)), Scalar(num(1.0))]
            ),
            num(1.0)
        );
    }

    // ---- Recalc sentinels propagate (Principle 2 fix) -----------------

    #[test]
    fn scalar_sentinel_propagates_kind_preserved() {
        // Unlike a genuine error (skipped, OXP-085), a scalar sentinel
        // argument propagates its exact kind.
        for k in [ErrorKind::Unsupported, ErrorKind::Blocked] {
            assert_eq!(
                eval_direct(eval, vec![Scalar(num(1.0)), Scalar(Value::Error(k))]),
                Value::Error(k),
                "{k:?} should propagate, not be skipped"
            );
        }
    }

    #[test]
    fn scalar_deferral_text_propagates_not_uncounted() {
        // A direct text arg whose numeric parse DEFERS (`to_number` returns a
        // Recalc sentinel, e.g. `"$ 5"`) is not a definite "not a number" —
        // propagate it rather than launder the gap into a silent skip
        // (coercion-consumer audit, 2026-07-12).
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::text("$ 5"))]),
            Value::Error(ErrorKind::Unsupported),
        );
        // A numeric-text arg is still counted (unchanged, COUNT.md §Semantics).
        assert_eq!(eval_direct(eval, vec![Scalar(Value::text("5"))]), num(1.0));
    }

    #[test]
    fn range_sentinel_propagates_kind_preserved() {
        // A sentinel cell inside a range propagates instead of being
        // silently skipped like a genuine range error.
        for k in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                eval_direct(eval, vec![Range(vec![num(1.0), Value::Error(k), num(2.0)])]),
                Value::Error(k),
                "{k:?} should propagate out of the range walk"
            );
        }
    }

    #[test]
    fn range_genuine_error_still_skipped_unchanged() {
        // Control: a genuine error cell in a range keeps the exact
        // COUNT.md §Error-behavior skip (unaffected by this fix).
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![
                    num(1.0),
                    Value::Error(ErrorKind::Div0),
                    num(2.0)
                ])]
            ),
            num(2.0)
        );
    }

    #[test]
    fn unresolvable_range_synthetic_sentinel_propagates() {
        // The synthetic `Error(Unsupported)` visit `for_each_cell` surfaces
        // for an unresolvable range (e.g. a bad 3-D span) now propagates
        // instead of being silently counted as 0.
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![Value::Error(ErrorKind::Unsupported)])]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // ---- RFC 0010: single-cell reference counted by the range rule ----------

    // A genuine behavior fix: a single-cell *reference* holding a logical or a
    // (numeric-looking) text is NOT counted — only a real number cell is — while
    // a *literal* keeps COUNT's direct-argument rules (a typed logical counts,
    // OXP-084; numeric text counts, COUNT.md §Semantics).
    #[test]
    fn rfc0010_reference_counts_only_numbers() {
        // A logical CELL reference: not counted (contrast the counted literal).
        assert_eq!(
            eval_direct(eval, vec![CellRef(Value::Bool(true))]),
            num(0.0)
        );
        // A numeric-text CELL reference: not counted.
        assert_eq!(eval_direct(eval, vec![CellRef(txt("5"))]), num(0.0));
        // A number CELL reference: counted.
        assert_eq!(eval_direct(eval, vec![CellRef(num(5.0))]), num(1.0));
    }

    #[test]
    fn rfc0010_literal_still_counts_logical_and_numeric_text() {
        // Direct logical literal counts (OXP-084).
        assert_eq!(eval_direct(eval, vec![Scalar(Value::Bool(true))]), num(1.0));
        // Direct numeric-text literal counts (COUNT.md §Semantics).
        assert_eq!(eval_direct(eval, vec![Scalar(txt("5"))]), num(1.0));
    }
}

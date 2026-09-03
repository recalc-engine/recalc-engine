//! `RANK` / `RANK.EQ` — the rank of a number within a list of numbers.
//!
//! # Provenance
//! Behavior contract: `docs/specs/RANK.md`, which cites the Microsoft Learn
//! RANK / RANK.EQ function pages
//! (`https://support.microsoft.com/en-us/office/rank-function-6a2fc49d-1831-4a03-9d8c-c279cf99f723`,
//! `https://support.microsoft.com/en-us/office/rank-eq-function-284858ce-8ef6-450e-b662-26245be04a40`).
//! `RANK.EQ` (2010+) is the renamed `RANK`; for the top-of-tie behavior they
//! are **identical** (only `RANK.AVG`, which averages tied ranks, differs —
//! not implemented here), so one evaluator backs both.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `RANK(number, ref, [order])` returns the 1-based rank of `number` among
//!   the numbers in `ref` (RANK.md §1). `order = 0` or omitted ranks
//!   **descending** (largest → rank 1); any **nonzero** `order` ranks
//!   **ascending** (smallest → rank 1) (RANK.md §Order).
//! - **Ties share the top rank of the tie group** (the min rank), so the next
//!   distinct value's rank skips by the tie count — the documented `RANK`/
//!   `RANK.EQ` behavior (RANK.md §Ties). Concretely: descending rank =
//!   `1 + count(v > number)`; ascending rank = `1 + count(v < number)`.
//! - **Nonnumeric values in `ref` are ignored** (RANK.md §Coercion): a
//!   range/array argument keeps only real `Number` cells (blank/bool/text
//!   skipped), exactly like `MAX`/`LARGE`'s `RangeAggregate` inclusion; a
//!   scalar `ref` coerces under `Scalar`.
//! - If `number` is **not present** in `ref`, `RANK` returns `#N/A`
//!   (RANK.md §1).
//! - Arguments are evaluated left-to-right (`number`, then `ref`, then
//!   `order`); the first error (a non-coercible `number`, an error cell in
//!   `ref`, or a bad `order`) propagates (RANK.md §Error behavior).
//!
//! # Oracle experiments (OXP-216) — RESOLVED 2026-07-19, no code change
//! Two edges were confirmed by **OXP-216** (Excel 16.0, job `1846a9fc` /
//! sidecar `f5f4eb386afc`, `tools/oracle/manual_probes.py::oxp_216`). Both
//! pin the implementation *as written* — exact `==` and coercing `number`:
//! - **Equality/tie comparison precision — EXACT, not the `=`-operator fuzz.**
//!   `RANK` compares `number`/`ref` with **exact IEEE-754 equality**, NOT the
//!   ~15-significant-figure fuzzy rule the comparison operators use
//!   (OXP-179/180/181/182, RFC-0009). Probed with `A1 = 16/9`
//!   (1.7777777777777777) and a stored literal `A2 = 1.77777777777778`
//!   (1.777…780) — bit-distinct but 15-sig-EQUAL (Δ ≈ 2.22e-15): Excel gave
//!   `RANK(A1,{A1,A2,1},0) = 2` (a fuzzy tie would be 1) and
//!   `RANK(A1,{A2,1},0) = #N/A` (fuzzy would find it), and
//!   `RANK(A2,{A1,A2,1},1) = 3` (fuzzy would be 2). So the exact `==` here is
//!   **oracle-confirmed correct**, not just a defensible default; applying the
//!   RFC-0009 fuzz to `RANK` would REGRESS it. Locked by
//!   `oxp_216_rank_uses_exact_not_fuzzy_compare`.
//! - **Text-number / logical `number` — coerced, then matched.** Excel coerces
//!   a text/logical `number` before matching a genuine-numeric `ref`:
//!   `RANK("7",{7,3,10},0) = 2` and `RANK(TRUE,{1,0,2},0) = 2`, matching this
//!   module's scalar `to_number` (`"7"` → 7, `TRUE` → 1).
//!
//! OXP-216 therefore warrants **no change** to `func_rank`; the 52 corpus
//! mismatches attributed to RANK (36 `expected_value_got_error` + 16
//! `numeric_gross`, `docs/oracle-run-status.md`) are **not** a compare-precision
//! or coercion bug and remain an open, separate root-cause investigation
//! (likely `ref`-shape / cascade — needs corpus mismatch mining, not this probe).
//!
//! # Array-position arguments (M2 lane 6 follow-up, 2026-09-04)
//! An argument in a range/array position is evaluated under the consumed-array
//! gate (RFC-0011; `docs/plans/2026-07-14-consumed-array-eval-spec.md` §2).
//! A materialized multi-cell array reaching this function is **refused** with a
//! loud `#UNSUPPORTED!` plus an engine diagnostic (spec §4, born-refusing
//! boundary): only the SUM/SUMPRODUCT consumers are oracle-pinned (OXP-201), and
//! the legacy alternative — a silent, host-row-dependent implicit intersection —
//! is a "never silently wrong" violation. Plain ranges are unchanged.

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg, to_number};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Collect the participating numbers from the `ref` argument (index 1) using
/// `MAX`'s inclusion rules: a scalar coerces (booleans / numeric text
/// included), a range/array keeps only real number cells. Returns the
/// propagated [`ErrorKind`] for an erroring cell/argument.
fn collect_ref_numbers(args: &mut dyn CallArgs) -> Result<Vec<f64>, ErrorKind> {
    let mut xs: Vec<f64> = Vec::new();
    match args.shape(1) {
        ArgShape::Omitted | ArgShape::Scalar => {
            // Array position: evaluate under the array-context gate, so an operator
            // expression over a range materializes (and the scalar coercion below
            // refuses it loudly — unpinned for this function) instead of being
            // implicit-intersected into a silent host-row-dependent value.
            match coerce_number_arg(&args.eval_scalar_array_arg(1), CoercionMode::Scalar) {
                NumericArg::Number(n) => xs.push(n),
                NumericArg::Skip => {}
                NumericArg::Error(k) => return Err(k),
            }
        }
        ArgShape::Range | ArgShape::Array => {
            let mut err: Option<ErrorKind> = None;
            let xs_ref = &mut xs;
            args.for_each_cell(
                1,
                &mut |v| match coerce_number_arg(v, CoercionMode::RangeAggregate) {
                    NumericArg::Number(n) => {
                        xs_ref.push(n);
                        ControlFlow::Continue(())
                    }
                    NumericArg::Skip => ControlFlow::Continue(()),
                    NumericArg::Error(k) => {
                        err = Some(k);
                        ControlFlow::Break(())
                    }
                },
            );
            if let Some(k) = err {
                return Err(k);
            }
        }
    }
    Ok(xs)
}

/// Evaluate a `RANK(number, ref, [order])` / `RANK.EQ(...)` call. See the
/// module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // 1. `number` (arg 0), left-most, coerced scalar; its error wins.
    let number = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    // 2. `ref` (arg 1): the participating numbers; an error cell propagates.
    let xs = match collect_ref_numbers(args) {
        Ok(xs) => xs,
        Err(k) => return Value::Error(k),
    };
    // 3. `order` (arg 2): 0/omitted → descending, nonzero → ascending.
    let order = match to_number(&args.eval_scalar(2)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let ascending = order != 0.0;

    // `number` must be present in `ref` (exact ==); otherwise #N/A.
    // (slice `contains` compares with `==`, i.e. exact IEEE-754 equality.)
    if !xs.contains(&number) {
        return Value::Error(ErrorKind::Na);
    }

    // Ties share the top rank: rank = 1 + count on the "better" side.
    let better = if ascending {
        xs.iter().filter(|&&v| v < number).count()
    } else {
        xs.iter().filter(|&&v| v > number).count()
    };
    Value::number((better + 1) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    fn rank(number: Value, refs: Vec<Value>, order: Option<Value>) -> Value {
        let mut args = vec![Scalar(number), Range(refs)];
        if let Some(o) = order {
            args.push(Scalar(o));
        }
        eval_direct(eval, args)
    }

    #[test]
    fn descending_default_order() {
        // ref {7,3,10,3,5}; descending: 10→1, 7→2, 5→3, 3→4 (tie), 3→4.
        let r = vec![num(7.0), num(3.0), num(10.0), num(3.0), num(5.0)];
        assert_eq!(rank(num(10.0), r.clone(), None), num(1.0));
        assert_eq!(rank(num(7.0), r.clone(), None), num(2.0));
        assert_eq!(rank(num(5.0), r.clone(), None), num(3.0));
        assert_eq!(rank(num(3.0), r.clone(), None), num(4.0));
    }

    #[test]
    fn descending_explicit_zero_order() {
        let r = vec![num(7.0), num(3.0), num(10.0), num(3.0), num(5.0)];
        assert_eq!(rank(num(7.0), r, Some(num(0.0))), num(2.0));
    }

    #[test]
    fn ascending_nonzero_order() {
        // ref {7,3,10,3,5}; ascending: 3→1 (tie), 3→1, 5→3, 7→4, 10→5.
        let r = vec![num(7.0), num(3.0), num(10.0), num(3.0), num(5.0)];
        assert_eq!(rank(num(3.0), r.clone(), Some(num(1.0))), num(1.0));
        assert_eq!(rank(num(5.0), r.clone(), Some(num(1.0))), num(3.0));
        assert_eq!(rank(num(7.0), r.clone(), Some(num(1.0))), num(4.0));
        assert_eq!(rank(num(10.0), r.clone(), Some(num(2.0))), num(5.0));
    }

    #[test]
    fn ties_share_top_rank_and_skip() {
        // {5,5,3}: descending 5→1 (both), 3→3 (skips 2).
        let r = vec![num(5.0), num(5.0), num(3.0)];
        assert_eq!(rank(num(5.0), r.clone(), None), num(1.0));
        assert_eq!(rank(num(3.0), r, None), num(3.0));
    }

    #[test]
    fn number_absent_is_na() {
        let r = vec![num(1.0), num(2.0), num(3.0)];
        assert_eq!(rank(num(4.0), r, None), Value::Error(ErrorKind::Na));
    }

    #[test]
    fn nonnumeric_cells_in_ref_are_ignored() {
        // Text/logical/blank are skipped; ranking is over {10, 20} only.
        let r = vec![
            num(10.0),
            txt("hello"),
            Value::bool(true),
            Value::Blank,
            num(20.0),
        ];
        assert_eq!(rank(num(20.0), r.clone(), None), num(1.0));
        assert_eq!(rank(num(10.0), r, None), num(2.0));
    }

    #[test]
    fn single_scalar_ref() {
        // RANK(5, 5) → 1.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(5.0)), Scalar(num(5.0))]),
            num(1.0)
        );
    }

    #[test]
    fn number_text_coerces_then_ranks() {
        // "7" coerces to 7 and is found in the numeric ref.
        let r = vec![num(7.0), num(3.0), num(10.0)];
        assert_eq!(rank(txt("7"), r, None), num(2.0));
    }

    #[test]
    fn non_numeric_number_arg_is_value_error() {
        let r = vec![num(1.0), num(2.0)];
        assert_eq!(rank(txt("x"), r, None), Value::Error(ErrorKind::Value));
    }

    #[test]
    fn error_in_ref_propagates() {
        let r = vec![num(1.0), Value::Error(ErrorKind::Div0), num(3.0)];
        assert_eq!(rank(num(1.0), r, None), Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn error_number_arg_propagates() {
        let r = vec![num(1.0), num(2.0)];
        assert_eq!(
            rank(Value::Error(ErrorKind::Ref), r, None),
            Value::Error(ErrorKind::Ref)
        );
    }

    #[test]
    fn all_text_ref_makes_number_absent_na() {
        // No numbers participate → `number` cannot be present → #N/A.
        let r = vec![txt("a"), txt("b")];
        assert_eq!(rank(num(1.0), r, None), Value::Error(ErrorKind::Na));
    }

    #[test]
    fn oxp_216_rank_uses_exact_not_fuzzy_compare() {
        // OXP-216 (RUN 2026-07-19, Excel 16.0, job 1846a9fc / sidecar
        // f5f4eb386afc): RANK compares with EXACT IEEE-754 equality, NOT the
        // `=`-operator's ~15-sig-fig fuzz (OXP-179/182). `16/9` and the stored
        // literal `1.77777777777778` are 15-sig-EQUAL but bit-distinct
        // (Δ ≈ 2.22e-15); Excel does NOT tie them. This locks RANK to exact so
        // it is never silently switched to fuzzy (which would regress it).
        let a1 = 16.0 / 9.0; // 1.7777777777777777
        let a2 = 1.777_777_777_777_78_f64; // 1.777…780, +2.22e-15, bit-distinct
        assert_ne!(a1, a2, "scaffold must be bit-distinct");
        // desc RANK(a1, {a1,a2,1}) = 2 (a2 is strictly greater); fuzzy tie → 1.
        assert_eq!(
            rank(num(a1), vec![num(a1), num(a2), num(1.0)], Some(num(0.0))),
            num(2.0)
        );
        // desc RANK(a2, {a1,a2,1}) = 1 (a2 is the max).
        assert_eq!(
            rank(num(a2), vec![num(a1), num(a2), num(1.0)], Some(num(0.0))),
            num(1.0)
        );
        // presence: a1 is ABSENT from {a2,1} under exact ==; fuzzy would find it.
        assert_eq!(
            rank(num(a1), vec![num(a2), num(1.0)], Some(num(0.0))),
            Value::Error(ErrorKind::Na)
        );
        // asc RANK(a2, {a1,a2,1}) = 3 (two strictly-less values); fuzzy → 2.
        assert_eq!(
            rank(num(a2), vec![num(a1), num(a2), num(1.0)], Some(num(1.0))),
            num(3.0)
        );
        // logical `number` TRUE coerces to 1 and matches a numeric ref (H6).
        assert_eq!(
            rank(
                Value::bool(true),
                vec![num(1.0), num(0.0), num(2.0)],
                Some(num(0.0))
            ),
            num(2.0)
        );
    }
}

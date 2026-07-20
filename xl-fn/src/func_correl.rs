//! `CORREL` — Pearson product-moment correlation coefficient of two arrays.
//!
//! # Provenance
//! Behavior contract: `docs/specs/CORREL.md` (which cites the Microsoft Learn
//! `CORREL` page, verified 2026-07-11). Three rules are quoted verbatim from that
//! page and drive this evaluator:
//! - *"If array1 and array2 have a different number of data points, CORREL
//!   returns a #N/A error."*
//! - *"If either array1 or array2 is empty, or if s (the standard deviation) of
//!   their values equals zero, CORREL returns a #DIV/0! error."*
//! - *"If an array or reference argument contains text, logical values, or empty
//!   cells, those values are ignored; however, cells with zero values are
//!   included."*
//!
//! The three questions the page left open — error-value precedence, whole-column
//! semantics, and the exact non-numeric mechanic — were resolved on the Excel
//! farm by **RUN-2026-07-11-oracle01** (OXP-145 / OXP-146 / OXP-147) and are now
//! implemented as observed, not deferred. See the sections below.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Computes `r = Σ((x-x̄)(y-ȳ)) / sqrt(Σ(x-x̄)² · Σ(y-ȳ)²)` over the surviving
//!   paired data points of `array1`/`array2` (CORREL.md §Equation). Two bounded
//!   operands are paired **positionally in row-major order**; the pairing gate is
//!   the **number of data points** (`rows·cols`), per the verbatim `#N/A` rule.
//! - **Different data-point counts → `#N/A`** (CORREL.md §Dimensions,
//!   doc-verified). Compared as total cell counts so a `1×N` row and an `N×1`
//!   column of equal length still pair row-major.
//! - **Error precedence: count first, then error (OXP-145).** The different-count
//!   `#N/A` gate is checked **before** the error scan, so a shorter operand that
//!   also contains an error still yields `#N/A` (OXP-145 H2). When counts match,
//!   an error value in either array propagates as the result (OXP-145 H1). The
//!   error scan runs `array1`-then-`array2` in row-major order and wins over the
//!   pairwise-deletion path below.
//! - **Whole-column / unbounded ranges are computed over their used extent
//!   (OXP-146).** `CORREL(A:A,B:B)` pairs the two columns **by absolute row**
//!   (the sparse used-extent walk), dropping the all-blank trailing rows via the
//!   same pairwise-deletion rule; the farm returned a finite `r`, not an error,
//!   so the former engineering guard is retired for the *both-unbounded* case. A
//!   **mixed** bounded/unbounded pair is an unprobed shape → `#UNSUPPORTED!`.
//! - **Non-numeric cells: pairwise deletion (OXP-147).** For each positional
//!   pair, if **either** cell is non-numeric (text / logical / blank) the whole
//!   pair is dropped; `r` is computed over the surviving all-numeric pairs. This
//!   is Excel's observed mechanic (OXP-147 H1: a text cell in one column and a
//!   blank in the other both drop their pairs, leaving a perfectly-correlated
//!   remainder → `r = 1`). The doc groups "logical values" with text/empty in the
//!   ignored category, so a logical cell drops its pair by the same rule.
//! - **Zero variance / empty → `#DIV/0!`** (CORREL.md §Div0, doc-verified): a
//!   constant surviving array has `s == 0`, so `sqrt(Sxx·Syy)` is `0`; the
//!   degenerate single-surviving-point case (`n == 1`) lands here too, and so
//!   does an operand left empty after pairwise deletion.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// One materialized CORREL operand.
///
/// The two variants carry the two distinct pairing geometries CORREL needs:
/// - [`Operand::Dense`] — a bounded rectangle (or a scalar) flattened row-major.
///   Two dense operands pair by **flat index**, so a `1×N` row and an `N×1`
///   column of equal length still pair data-point-for-data-point, and the
///   different-count `#N/A` gate is a length comparison.
/// - [`Operand::Used`] — the populated rows of an unbounded whole-column range,
///   each tagged with its row index relative to the range top. Two used operands
///   pair **by row index** (absolute-row alignment), which is what a full-column
///   `CORREL(A:A,B:B)` requires (OXP-146).
enum Operand {
    Dense(Vec<Value>),
    Used(Vec<(u32, Vec<Value>)>),
}

/// Evaluate a `CORREL(array1, array2)` call. See the module docs for the
/// semantics and their spec/oracle provenance. Arity (exactly two) is enforced
/// by the engine before dispatch.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let a = match collect(args, 0) {
        Ok(op) => op,
        Err(k) => return Value::Error(k),
    };
    let b = match collect(args, 1) {
        Ok(op) => op,
        Err(k) => return Value::Error(k),
    };

    // Pair the two operands. A different-count gate (`#N/A`) and the
    // mixed-shape guard (`#UNSUPPORTED!`) live inside `pair_up`.
    let pairs = match pair_up(a, b) {
        Ok(p) => p,
        Err(k) => return Value::Error(k),
    };

    // Errors propagate and win over pairwise deletion, but *not* over the
    // different-count `#N/A` (which `pair_up` already resolved). Scan array1
    // (every x) then array2 (every y), row-major, and return the first error —
    // OXP-145 H1.
    for (x, _) in &pairs {
        if let Value::Error(e) = x {
            return Value::Error(*e);
        }
    }
    for (_, y) in &pairs {
        if let Value::Error(e) = y {
            return Value::Error(*e);
        }
    }

    // Pairwise deletion (OXP-147): keep only pairs where *both* cells are real
    // numbers; a text/logical/blank cell in either position drops the whole pair.
    // A `0` is a genuine data point (the page: "cells with zero values are
    // included"), preserved because it is a `Value::Number`.
    let mut xs = Vec::with_capacity(pairs.len());
    let mut ys = Vec::with_capacity(pairs.len());
    for (x, y) in &pairs {
        if let (Value::Number(px), Value::Number(py)) = (x, y) {
            xs.push(*px);
            ys.push(*py);
        }
    }

    // Empty after deletion (or a genuinely empty operand) → #DIV/0! (doc-verified).
    let n = xs.len();
    if n == 0 {
        return Value::Error(ErrorKind::Div0);
    }
    let nf = n as f64;

    let mean_x = xs.iter().sum::<f64>() / nf;
    let mean_y = ys.iter().sum::<f64>() / nf;

    let mut sxy = 0.0_f64;
    let mut sxx = 0.0_f64;
    let mut syy = 0.0_f64;
    for i in 0..n {
        let dx = xs[i] - mean_x;
        let dy = ys[i] - mean_y;
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }

    // Zero variance in either surviving array (a constant array; also the n == 1
    // case) → denominator is zero → #DIV/0! (doc-verified). Guard the divide
    // *before* it can produce a non-finite quotient.
    let denom = (sxx * syy).sqrt();
    if denom == 0.0 {
        return Value::Error(ErrorKind::Div0);
    }

    // Ordinary f64 divide; no fast-math / FMA paths. `Value::number` maps any
    // non-finite backstop to #NUM! per the value invariant.
    Value::number(sxy / denom)
}

/// Materialize argument `index` into an [`Operand`].
///
/// - A **bounded** rectangle (`dims == Some`, including a 1×1 range/array
///   literal) is walked densely with [`CallArgs::for_each_row`] and padded to the
///   reported `cols` width with blanks so ragged rows still line up →
///   [`Operand::Dense`].
/// - A **scalar expression** (`dims == None` **and** [`ArgShape::Scalar`]) is a
///   single-element dense vector holding its evaluated value.
/// - An **unbounded whole-column** range (`dims == None`, non-scalar shape) is
///   walked over its **used extent** with [`CallArgs::for_each_used_row`] →
///   [`Operand::Used`] (OXP-146). A whole-*row* range or an unresolvable range is
///   refused there (`Err`) and surfaces as `#UNSUPPORTED!`.
fn collect(args: &mut dyn CallArgs, index: usize) -> Result<Operand, ErrorKind> {
    match args.dims(index) {
        Some((rows, cols)) => {
            let mut rows_buf: Vec<Vec<Value>> = Vec::new();
            args.for_each_row(index, &mut |row| {
                rows_buf.push(row.to_vec());
                ControlFlow::Continue(())
            })?;
            let mut cells = Vec::with_capacity((rows as usize).saturating_mul(cols as usize));
            for r in 0..rows as usize {
                let row = rows_buf.get(r);
                for c in 0..cols as usize {
                    let v = row
                        .and_then(|rw| rw.get(c))
                        .cloned()
                        .unwrap_or(Value::Blank);
                    cells.push(v);
                }
            }
            Ok(Operand::Dense(cells))
        }
        None => match args.shape(index) {
            ArgShape::Scalar => Ok(Operand::Dense(vec![args.eval_scalar(index)])),
            // Unbounded whole-column → used-extent walk (OXP-146); whole-row /
            // unresolvable ranges refuse and become #UNSUPPORTED!.
            _ => {
                let mut used: Vec<(u32, Vec<Value>)> = Vec::new();
                args.for_each_used_row(index, &mut |rel, cells| {
                    used.push((rel, cells.to_vec()));
                    ControlFlow::Continue(())
                })?;
                Ok(Operand::Used(used))
            }
        },
    }
}

/// Combine two operands into the positional `(x, y)` pair list CORREL folds over.
///
/// - **Both dense**: pair by flat row-major index; a different data-point count
///   is the `#N/A` gate (doc-verified).
/// - **Both used** (two unbounded whole-columns): pair **by absolute row**
///   (OXP-146). A row present in only one operand pairs the other side against a
///   blank, which pairwise deletion then drops — so the all-blank trailing rows
///   of two full columns contribute nothing, matching Excel's used-extent result.
///   Two full columns have equal data-point counts, so there is no `#N/A` gate.
/// - **Mixed** bounded/unbounded: an unprobed shape → `#UNSUPPORTED!` rather than
///   guess how Excel reconciles a bounded count against a full-column one.
fn pair_up(a: Operand, b: Operand) -> Result<Vec<(Value, Value)>, ErrorKind> {
    match (a, b) {
        (Operand::Dense(av), Operand::Dense(bv)) => {
            // Different number of data points → #N/A (doc-verified). Checked
            // before the error scan, so a short operand carrying an error still
            // yields #N/A (OXP-145 H2).
            if av.len() != bv.len() {
                return Err(ErrorKind::Na);
            }
            Ok(av.into_iter().zip(bv).collect())
        }
        (Operand::Used(ar), Operand::Used(br)) => {
            // Merge the two used-extent walks by row index, then pair column-wise
            // within each row (absent cells → blank).
            let mut merged: BTreeMap<u32, (Vec<Value>, Vec<Value>)> = BTreeMap::new();
            for (r, cells) in ar {
                merged
                    .entry(r)
                    .or_insert_with(|| (Vec::new(), Vec::new()))
                    .0 = cells;
            }
            for (r, cells) in br {
                merged
                    .entry(r)
                    .or_insert_with(|| (Vec::new(), Vec::new()))
                    .1 = cells;
            }
            let mut pairs = Vec::new();
            for (_, (arow, brow)) in merged {
                let width = arow.len().max(brow.len());
                for c in 0..width {
                    let x = arow.get(c).cloned().unwrap_or(Value::Blank);
                    let y = brow.get(c).cloned().unwrap_or(Value::Blank);
                    pairs.push((x, y));
                }
            }
            Ok(pairs)
        }
        // Mixed bounded/unbounded — unprobed; refuse rather than guess.
        _ => Err(ErrorKind::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    /// Extract the number from a `CORREL` result or panic (tests assert against
    /// an expected `r` at a 1e-9 tolerance).
    fn r_of(v: Value) -> f64 {
        match v {
            Value::Number(x) => x,
            other => panic!("expected a numeric CORREL result, got {other:?}"),
        }
    }

    #[test]
    fn canonical_ms_example() {
        // MS Learn CORREL example: array1 {3,2,4,5,6}, array2 {9,7,12,15,17}.
        // x̄=4, ȳ=12; Sxy=26, Sxx=10, Syy=68; r = 26/sqrt(680).
        let expected = 26.0 / 680.0_f64.sqrt(); // 0.9970544855015815
        let r = r_of(eval_direct(
            eval,
            vec![
                Array(vec![num(3.0), num(2.0), num(4.0), num(5.0), num(6.0)]),
                Array(vec![num(9.0), num(7.0), num(12.0), num(15.0), num(17.0)]),
            ],
        ));
        assert!((r - expected).abs() < 1e-9, "r = {r}, expected {expected}");
    }

    #[test]
    fn perfect_positive_correlation_is_one() {
        // y = 2x is perfectly correlated → r = 1.
        let r = r_of(eval_direct(
            eval,
            vec![
                Array(vec![num(1.0), num(2.0), num(3.0)]),
                Array(vec![num(2.0), num(4.0), num(6.0)]),
            ],
        ));
        assert!((r - 1.0).abs() < 1e-9, "r = {r}, expected 1");
    }

    #[test]
    fn perfect_negative_correlation_is_minus_one() {
        // A strictly decreasing linear relation → r = -1.
        // x {1,2,3,4}, y {4,3,2,1}: Sxy=-5, Sxx=Syy=5 → r = -1.
        let r = r_of(eval_direct(
            eval,
            vec![
                Range(vec![num(1.0), num(2.0), num(3.0), num(4.0)]),
                Range(vec![num(4.0), num(3.0), num(2.0), num(1.0)]),
            ],
        ));
        assert!((r - (-1.0)).abs() < 1e-9, "r = {r}, expected -1");
    }

    #[test]
    fn known_partial_correlation() {
        // x {1,2,3,4,5}, y {2,1,4,3,5}: x̄=3, ȳ=3;
        // dx=[-2,-1,0,1,2], dy=[-1,-2,1,0,2]; Sxy = 2+2+0+0+4 = 8;
        // Sxx = 4+1+0+1+4 = 10; Syy = 1+4+1+0+4 = 10; r = 8/10 = 0.8.
        let r = r_of(eval_direct(
            eval,
            vec![
                Array(vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0)]),
                Array(vec![num(2.0), num(1.0), num(4.0), num(3.0), num(5.0)]),
            ],
        ));
        assert!((r - 0.8).abs() < 1e-9, "r = {r}, expected 0.8");
    }

    #[test]
    fn mismatched_lengths_is_na() {
        // 3 data points vs 2 → #N/A (doc-verified).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Array(vec![num(1.0), num(2.0), num(3.0)]),
                    Array(vec![num(4.0), num(5.0)]),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn zero_variance_is_div0() {
        // A constant array1 has s = 0 → denominator 0 → #DIV/0! (doc-verified).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(1.0), num(1.0)]),
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn error_element_propagates() {
        // An error in either array propagates as the result (and wins over the
        // pairwise-deletion path). Use #VALUE! to distinguish from #DIV/0!.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Array(vec![num(1.0), Value::Error(ErrorKind::Value), num(3.0)]),
                    Array(vec![num(4.0), num(5.0), num(6.0)]),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn oxp_145_error_in_array_propagates_div0() {
        // RUN-2026-07-11-oracle01 · OXP-145 H1: =CORREL(A1:A3,B1:B3) with
        // A={1,2,3}, B={2,#DIV/0!,6} — counts match (3==3), so the error scan
        // fires and #DIV/0! propagates.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![num(2.0), Value::Error(ErrorKind::Div0), num(6.0)]),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn oxp_145_different_count_na_wins_over_error() {
        // RUN-2026-07-11-oracle01 · OXP-145 H2: =CORREL(A1:A3,B1:B2) with
        // A={1,2,3}, B={2,#DIV/0!} — the different data-point count (3 vs 2) is
        // decided *before* the error scan, so #N/A wins over the #DIV/0! cell.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Range(vec![num(1.0), num(2.0), num(3.0)]),
                    Range(vec![num(2.0), Value::Error(ErrorKind::Div0)]),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn oxp_146_whole_column_computed_over_used_extent() {
        // RUN-2026-07-11-oracle01 · OXP-146 H1: =CORREL(A:A,B:B) with
        // A={1,2,3,4}, B={3,5,7,9} (B = 2A+1) → Excel returns 0.9999999999999998,
        // i.e. it computes over the used extent rather than erroring. Two
        // whole-columns pair by absolute row; the all-blank trailing rows drop out.
        let observed = 0.9999999999999998_f64;
        let r = r_of(eval_direct(
            eval,
            vec![
                Unbounded(vec![num(1.0), num(2.0), num(3.0), num(4.0)]),
                Unbounded(vec![num(3.0), num(5.0), num(7.0), num(9.0)]),
            ],
        ));
        assert!((r - observed).abs() < 1e-9, "r = {r}, observed {observed}");
    }

    #[test]
    fn oxp_147_pairwise_deletion_of_non_numeric() {
        // RUN-2026-07-11-oracle01 · OXP-147 H1: =CORREL(A1:A5,B1:B5) with
        // A={1,2,"x",4,5}, B={2,4,6,8,<blank>} → 1.0. The text at A3 and the
        // blank at B5 each drop their whole positional pair (pairwise deletion),
        // leaving the perfectly-correlated pairs (1,2),(2,4),(4,8) → r = 1.
        let observed = 1.0_f64;
        let r = r_of(eval_direct(
            eval,
            vec![
                Range(vec![num(1.0), num(2.0), txt("x"), num(4.0), num(5.0)]),
                Range(vec![num(2.0), num(4.0), num(6.0), num(8.0), Value::Blank]),
            ],
        ));
        assert!((r - observed).abs() < 1e-9, "r = {r}, observed {observed}");
    }

    #[test]
    fn logical_cell_drops_its_pair() {
        // The MS Learn page groups "logical values" with text/empty in the
        // ignored category; under the OXP-147-confirmed pairwise-deletion
        // mechanic a logical cell drops its whole pair. Here TRUE at A1 drops the
        // (TRUE,5) pair, leaving (2,6),(3,7) → r = 1.
        let r = r_of(eval_direct(
            eval,
            vec![
                Array(vec![Value::Bool(true), num(2.0), num(3.0)]),
                Array(vec![num(5.0), num(6.0), num(7.0)]),
            ],
        ));
        assert!((r - 1.0).abs() < 1e-9, "r = {r}, expected 1");
    }

    #[test]
    fn error_wins_over_pairwise_deletion() {
        // Both a text cell (would drop its pair) and an error cell present: the
        // error scan runs before deletion, so #DIV/0! propagates rather than the
        // computed r over the surviving pairs.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Array(vec![num(1.0), txt("x"), num(3.0)]),
                    Array(vec![num(4.0), Value::Error(ErrorKind::Div0), num(6.0)]),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn mixed_bounded_and_unbounded_is_unsupported() {
        // One whole-column (used-extent) operand paired against one bounded
        // rectangle is an unprobed shape: refuse rather than guess how Excel
        // reconciles a full-column count against a bounded one (never silently
        // wrong). The both-unbounded case is computed (see OXP-146 above).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Unbounded(vec![num(1.0), num(2.0), num(3.0)]),
                    Array(vec![num(4.0), num(5.0), num(6.0)]),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

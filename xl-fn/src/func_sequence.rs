//! `SEQUENCE` — generate a sequence of numbers into a spilled array.
//!
//! # Provenance
//! Behavior contract: Microsoft support "SEQUENCE function"
//! (<https://support.microsoft.com/en-us/office/sequence-function-57467a98-57e0-4817-9f14-2eb78519ca90>,
//! verified by WebFetch 2026-07-15). No `docs/specs/SEQUENCE.md` exists; this is
//! a clean-room implementation of the documented behavior only. The `_xlfn.`
//! authoring prefix is stripped by the parser (`xl-ast::parser::parse_func_name`)
//! before registry lookup, so the registry name is the bare uppercase
//! `SEQUENCE`.
//!
//! # Behavior contract (one line)
//! `SEQUENCE(rows, [columns], [start], [step])` returns a `rows × columns`
//! array whose values start at `start` and increase by `step`.
//!
//! # Semantics implemented (MS page wording in parentheses)
//! - `rows` (arg 0, "Required"): the row count. `columns` (arg 1), `start`
//!   (arg 2), `step` (arg 3) are optional; "Any missing optional arguments will
//!   default to 1."
//! - A **one-dimensional** sequence — `rows == 1` *or* `columns == 1` — is
//!   filled with `start + i*step` for `i = 0, 1, …`. In one dimension the fill
//!   order is unambiguous (a single row or single column), so it is implemented.
//! - `start`/`step` may be any finite number (fractional included, e.g.
//!   `SEQUENCE(4,1,1,0.5)`).
//!
//! # Refused edges (loud), pending probes — see `docs/plans/2026-07-15-lane3a-probe-needed.md`
//! - **Two-dimensional fill order** (L3A-SEQ2D): when `rows > 1` *and*
//!   `columns > 1`, whether the array fills row-major or column-major is shown
//!   only in the page's example *images*, never in its text. Rather than guess
//!   from memory (forbidden), the 2-D case is refused (`#UNSUPPORTED!`). The
//!   common 1-D forms (`SEQUENCE(n)`, `SEQUENCE(1,n)`) are unaffected.
//! - **Non-positive `rows`/`columns`** (L3A-SEQNP): `rows < 1` or `columns < 1`.
//!   The page documents no error code for this (`#CALC!`? `#NUM!`? unknown), so
//!   it is refused rather than guessed.
//! - **Non-integer `rows`/`columns`** (L3A-FRAC): the rounding direction of a
//!   fractional count is undocumented → refused.
//! - **Elided `rows` (`SEQUENCE(,3)`)** (L3A-SEQROWS): the page says "If you omit
//!   the rows argument, you must provide at least one other argument," but does
//!   not pin what an elided `rows` defaults to → refused.
//! - **Array-valued `rows`/`columns`** (L3A-ARRIDX): undocumented → refused.
//! - **Over the materialization cap** (L3A-CAP): see `arrayshape`.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::{ArgShape, CallArgs};
use crate::arrayshape::{IntArg, MAX_MATERIALIZED_ELEMS, int_arg, over_cap, spill};
use crate::context::EvalContext;

/// Evaluate a `SEQUENCE(rows, [columns], [start], [step])` call. See the module
/// docs for the semantics and their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // --- rows (arg 0) & columns (arg 1): positive integer counts.
    let rows = match count_arg(args, 0) {
        Ok(n) => n,
        Err(v) => return v,
    };
    let cols = match count_arg(args, 1) {
        Ok(n) => n,
        Err(v) => return v,
    };

    // --- 2-D fill order is undocumented (image-only) → refuse loudly.
    if rows > 1 && cols > 1 {
        return Value::Error(ErrorKind::Unsupported);
    }

    // --- start (arg 2) & step (arg 3): any finite number, default 1.
    let start = match number_arg(args, 2) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let step = match number_arg(args, 3) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };

    // Guard each dimension against the cap *before* narrowing to `usize`: an
    // `i64 -> usize` cast truncates (not saturates) on 32-bit / wasm32, so an
    // over-huge dimension (e.g. `SEQUENCE(2^32+5)`) could otherwise slip under
    // the cap. Any single dim over the cap over-caps the whole result (the other
    // dim is ≥ 1). `rows`/`cols` are already validated ≥ 1, so `as u64` is exact.
    if rows as u64 > MAX_MATERIALIZED_ELEMS || cols as u64 > MAX_MATERIALIZED_ELEMS {
        return Value::Error(ErrorKind::Unsupported);
    }
    let rows_u = rows as usize;
    let cols_u = cols as usize;
    if over_cap(rows_u, cols_u) {
        return Value::Error(ErrorKind::Unsupported);
    }

    // Row-major fill; in one dimension this is the only ordering.
    let total = rows_u * cols_u;
    let mut data: Vec<Value> = Vec::with_capacity(total);
    for i in 0..total {
        data.push(Value::number(start + (i as f64) * step));
    }
    spill(rows_u, cols_u, data)
}

/// Read a positive-integer count argument (`rows` default is none; `columns`
/// default is 1). Returns the count on success, or the [`Value`] to return.
///
/// `index == 1` (columns) treats an omitted slot as the documented default `1`;
/// `index == 0` (rows) refuses an omitted slot (elided `rows` is unpinned).
fn count_arg(args: &mut dyn CallArgs, index: usize) -> Result<i64, Value> {
    match int_arg(args, index) {
        IntArg::Value(n) => {
            if n < 1 {
                // Non-positive count: undocumented error type → refuse.
                Err(Value::Error(ErrorKind::Unsupported))
            } else {
                Ok(n)
            }
        }
        IntArg::Omitted => {
            if index == 0 {
                // Elided `rows` is unpinned (L3A-SEQROWS).
                Err(Value::Error(ErrorKind::Unsupported))
            } else {
                Ok(1)
            }
        }
        // Fractional count, array-valued count → refuse.
        IntArg::NonInteger | IntArg::NonScalar => Err(Value::Error(ErrorKind::Unsupported)),
        IntArg::Err(k) => Err(Value::Error(k)),
    }
}

/// Read a `start`/`step` value argument: any finite number, default `1`. A
/// multi-cell range/array is undocumented → `#UNSUPPORTED!`.
fn number_arg(args: &mut dyn CallArgs, index: usize) -> Result<f64, ErrorKind> {
    match args.shape(index) {
        ArgShape::Omitted => Ok(1.0),
        ArgShape::Range | ArgShape::Array => Err(ErrorKind::Unsupported),
        ArgShape::Scalar => to_number(&args.eval_scalar(index)),
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num};
    use xl_value::{Array, ErrorKind, Value};

    fn arr(rows: usize, cols: usize, data: Vec<Value>) -> Value {
        Value::Array(Array::new(rows, cols, data).unwrap())
    }

    // SEQUENCE(5) → a 5×1 column 1..5 (columns/start/step default to 1).
    #[test]
    fn column_default() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(5.0))]),
            arr(5, 1, vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0)])
        );
    }

    // SEQUENCE(1,4) → a 1×4 row 1..4.
    #[test]
    fn single_row() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0)), Scalar(num(4.0))]),
            arr(1, 4, vec![num(1.0), num(2.0), num(3.0), num(4.0)])
        );
    }

    // SEQUENCE(4,1,1001,1000) → the documented GL-code column {1001;2001;3001;4001}.
    #[test]
    fn start_and_step() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(4.0)),
                    Scalar(num(1.0)),
                    Scalar(num(1001.0)),
                    Scalar(num(1000.0)),
                ]
            ),
            arr(
                4,
                1,
                vec![num(1001.0), num(2001.0), num(3001.0), num(4001.0)]
            )
        );
    }

    // Fractional step is allowed on start/step (not a count).
    #[test]
    fn fractional_step() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(3.0)),
                    Scalar(num(1.0)),
                    Scalar(num(1.0)),
                    Scalar(num(0.5)),
                ]
            ),
            arr(3, 1, vec![num(1.0), num(1.5), num(2.0)])
        );
    }

    // L3A-SEQ2D: a genuinely 2-D sequence refuses (fill order image-only).
    #[test]
    fn two_dimensional_refused() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(4.0)), Scalar(num(5.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // L3A-SEQNP: non-positive rows refuses.
    #[test]
    fn non_positive_rows_refused() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-2.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // L3A-FRAC: a fractional row count refuses.
    #[test]
    fn fractional_rows_refused() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(2.5))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // L3A-SEQROWS: an elided rows argument refuses.
    #[test]
    fn elided_rows_refused() {
        assert_eq!(
            eval_direct(eval, vec![Omitted, Scalar(num(3.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // A coercion error in a count propagates.
    #[test]
    fn count_error_propagates() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Ref))]),
            Value::Error(ErrorKind::Ref)
        );
    }

    // B2 / L3A-CAP (32-bit/wasm32 guard): a dimension above 2^32 refuses, caught
    // *before* the `i64 -> usize` narrowing that would truncate 2^32+5 to 5 on
    // wasm32 (passes trivially on 64-bit; documents the intent and guards wasm32).
    #[test]
    fn over_u32_dimension_refused() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(4_294_967_301.0))]), // 2^32 + 5
            Value::Error(ErrorKind::Unsupported)
        );
    }
}

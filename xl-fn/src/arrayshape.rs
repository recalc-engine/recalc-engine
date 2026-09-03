//! Shared helpers for the M2 **Lane 3a** dynamic-array *reshape / generate*
//! functions (`SEQUENCE`, `TOCOL`, `TOROW`, `VSTACK`, `HSTACK`, `TAKE`, `DROP`,
//! `EXPAND`, `WRAPROWS`, `WRAPCOLS`, `CHOOSEROWS`, `CHOOSECOLS`, `RANDARRAY`).
//!
//! One source of truth for the two operations every reshaper shares:
//! **materialising an array/reference argument into a dense row-major [`Grid`]**
//! (via [`CallArgs::dims`] + [`CallArgs::for_each_row`]) and **building a spilled
//! [`Value::Array`] result** under a materialization element cap. Kept out of the
//! per-function modules so the cap policy and the unbounded-range refusal are
//! written once (mirrors `date_common` for the date family).
//!
//! # Materialization element cap (engineering guardrail, not an Excel semantic)
//! [`MAX_MATERIALIZED_ELEMS`] is a defensive array-materialization cap
//! (`1 << 20` = 1,048,576 elements). Any input we would have to walk, or any
//! result we would have to build, whose element count exceeds this cap is
//! **refused loudly** with `#UNSUPPORTED!` ([`ErrorKind::Unsupported`]). We
//! deliberately do **not** reproduce Excel's own "#NUM! when the array is too
//! large" threshold: that threshold is the full worksheet grid
//! (`1,048,576 × 16,384` ≈ 1.7e10 cells), far above our cap, so returning
//! `#NUM!` at *our* cap would be silently wrong for every array between the two
//! limits (Excel would compute it; we cannot). A distinguishable `#UNSUPPORTED!`
//! at the cap honours Principle 2 (never silently wrong); the
//! `#NUM!`-when-too-large path is queued as a probe. See
//! `docs/plans/2026-07-15-lane3a-probe-needed.md` (L3A-CAP).
//!
//! # Unbounded whole-column/row inputs — refused
//! [`CallArgs::for_each_row`]/[`CallArgs::dims`] refuse an unbounded
//! whole-column/row range (the documented dense-walk guardrail in `args.rs`).
//! A reshape over such a range would materialise up to 1,048,576 cells, so we
//! propagate the refusal as `#UNSUPPORTED!` rather than guess a used extent —
//! the same policy `INDEX`/`SUMIF` use for their dense paths.

use std::ops::ControlFlow;

use xl_value::{Array, ErrorKind, Value, to_bool, to_number};

use crate::args::{ArgShape, CallArgs};

/// The array-materialization element cap (`1 << 20` = 1,048,576). See the module
/// docs for why exceeding it is a loud `#UNSUPPORTED!` rather than Excel's
/// `#NUM!`.
pub(crate) const MAX_MATERIALIZED_ELEMS: u64 = 1 << 20;

/// The `#N/A` value used to pad ragged stacks / expansions / wraps (the
/// documented default `pad_with` for `VSTACK`/`HSTACK`/`EXPAND`/`WRAPROWS`/
/// `WRAPCOLS`).
pub(crate) const PAD_NA: Value = Value::Error(ErrorKind::Na);

/// A densely-materialised rectangle of cell values, row-major.
///
/// Always `rows >= 1` and `cols >= 1`, with `data.len() == rows * cols`
/// (absent cells surfaced as [`Value::Blank`] at their position, exactly as
/// [`CallArgs::for_each_row`] delivers them).
pub(crate) struct Grid {
    pub rows: usize,
    pub cols: usize,
    /// Row-major, length `rows * cols`.
    pub data: Vec<Value>,
}

impl Grid {
    /// Borrow the value at 0-based `(row, col)`.
    pub fn at(&self, row: usize, col: usize) -> &Value {
        &self.data[row * self.cols + col]
    }
}

/// The outcome of [`materialize`] for one argument.
pub(crate) enum Materialized {
    /// A bounded rectangle was materialised.
    Grid(Grid),
    /// The argument slot was elided (`ArgShape::Omitted`). Each function decides
    /// what an omitted array argument means (usually: refuse).
    Omitted,
    /// The argument could not be materialised — an unbounded whole-column/row
    /// range, an over-cap rectangle, or a range that resolves to nothing. Carries
    /// the [`ErrorKind`] the caller should return (`#UNSUPPORTED!`).
    Refused(ErrorKind),
}

/// Materialise argument `index` into a dense row-major [`Grid`].
///
/// - A **scalar** argument (literal, single-cell reference, computed scalar,
///   1×1 range/array) becomes a `1 × 1` grid holding its value — so every
///   reshaper accepts a lone scalar uniformly.
/// - A **bounded** range/array becomes its full rectangle (blanks surfaced
///   positionally), provided its element count is within [`MAX_MATERIALIZED_ELEMS`].
/// - An **unbounded** whole-column/row range, an **over-cap** rectangle, or an
///   unresolvable range is [`Materialized::Refused`] with `#UNSUPPORTED!`.
/// - An **omitted** slot is [`Materialized::Omitted`].
pub(crate) fn materialize(args: &mut dyn CallArgs, index: usize) -> Materialized {
    if args.shape(index) == ArgShape::Omitted {
        return Materialized::Omitted;
    }
    match args.dims(index) {
        Some((rows, cols)) => {
            if rows == 0 || cols == 0 {
                return Materialized::Refused(ErrorKind::Unsupported);
            }
            let total = u64::from(rows).saturating_mul(u64::from(cols));
            if total > MAX_MATERIALIZED_ELEMS {
                return Materialized::Refused(ErrorKind::Unsupported);
            }
            let expected = total as usize;
            let mut data: Vec<Value> = Vec::with_capacity(expected);
            let mut lambda_seen = false;
            let walk = args.for_each_row(index, &mut |row| {
                // A lambda is engine-internal (RFC-0012 BC-6); relocating it
                // verbatim through a reshape would silently keep it in the
                // output, so refuse loudly (integration guard, the contract review lane-3a
                // review — the choke point that covers every relocation path).
                if row.iter().any(|v| matches!(v, Value::Lambda(_))) {
                    lambda_seen = true;
                    return ControlFlow::Break(());
                }
                data.extend(row.iter().cloned());
                ControlFlow::Continue(())
            });
            if lambda_seen {
                return Materialized::Refused(ErrorKind::Unsupported);
            }
            if walk.is_err() {
                // The dense walk refused (unexpected once `dims` reported a
                // bounded rectangle, but stay loud rather than guess).
                return Materialized::Refused(ErrorKind::Unsupported);
            }
            // Normalise to the declared rectangle: a bounded range delivers
            // exactly `expected` full-width rows; pad/truncate defensively so a
            // short final row can never shift the row-major layout downstream.
            if data.len() < expected {
                data.resize(expected, Value::Blank);
            } else {
                data.truncate(expected);
            }
            Materialized::Grid(Grid {
                rows: rows as usize,
                cols: cols as usize,
                data,
            })
        }
        None => {
            // `dims` returns `None` for a true scalar expression *and* for an
            // unbounded whole-column/row range. Distinguish via `shape`.
            match args.shape(index) {
                ArgShape::Scalar => {
                    // Array position: evaluate under the array-context gate, so
                    // an operator expression over a range materializes instead
                    // of being implicit-intersected. A multi-cell array here is
                    // not a 1×1 grid cell — its element-wise consumption by the
                    // array-shaping functions is unpinned → refuse loudly.
                    let v = args.eval_scalar_array_arg(index);
                    if let Value::Array(a) = &v
                        && a.as_scalar().is_none()
                    {
                        return Materialized::Refused(ErrorKind::Unsupported);
                    }
                    // A lambda is engine-internal (RFC-0012 BC-6); refuse rather
                    // than relocate it (integration guard, the contract review lane-3a review).
                    if matches!(v, Value::Lambda(_)) {
                        return Materialized::Refused(ErrorKind::Unsupported);
                    }
                    Materialized::Grid(Grid {
                        rows: 1,
                        cols: 1,
                        data: vec![v],
                    })
                }
                // Unbounded range (Range/Array with no bounded dims), or an
                // otherwise-unmaterialisable argument.
                ArgShape::Range | ArgShape::Array | ArgShape::Omitted => {
                    Materialized::Refused(ErrorKind::Unsupported)
                }
            }
        }
    }
}

/// Materialise **every** argument into a [`Grid`], in order — the `VSTACK` /
/// `HSTACK` input walk.
///
/// An elided argument slot (undocumented in a variadic stack) or an
/// unmaterialisable argument (unbounded / over-cap) short-circuits to the
/// [`Value`] the caller should return.
pub(crate) fn collect_grids(args: &mut dyn CallArgs) -> Result<Vec<Grid>, Value> {
    let count = args.count();
    let mut grids: Vec<Grid> = Vec::with_capacity(count);
    for i in 0..count {
        match materialize(args, i) {
            Materialized::Grid(g) => grids.push(g),
            Materialized::Omitted => return Err(Value::Error(ErrorKind::Unsupported)),
            Materialized::Refused(k) => return Err(Value::Error(k)),
        }
    }
    Ok(grids)
}

/// A scalar integer argument (a count / index), classified for the reshape
/// functions' shared coercion.
pub(crate) enum IntArg {
    /// A finite, integral value.
    Value(i64),
    /// The slot was elided.
    Omitted,
    /// The value coerced to a finite **non-integral** number. Rounding direction
    /// is undocumented for these functions, so callers refuse loudly rather than
    /// guess (probe L3A-FRAC).
    NonInteger,
    /// The argument is a multi-cell range/array where a scalar count/index is
    /// expected. Array-valued counts/indices are undocumented on the basic pages
    /// → callers refuse loudly (probe L3A-ARRIDX).
    NonScalar,
    /// A coercion error to propagate (leftmost-wins semantics of the caller).
    Err(ErrorKind),
}

/// Read argument `index` as a scalar integer count/index. See [`IntArg`].
pub(crate) fn int_arg(args: &mut dyn CallArgs, index: usize) -> IntArg {
    match args.shape(index) {
        ArgShape::Omitted => IntArg::Omitted,
        ArgShape::Range | ArgShape::Array => IntArg::NonScalar,
        ArgShape::Scalar => match to_number(&args.eval_scalar(index)) {
            Err(k) => IntArg::Err(k),
            Ok(n) => {
                if !n.is_finite() {
                    IntArg::Err(ErrorKind::Num)
                } else if n.fract() != 0.0 {
                    IntArg::NonInteger
                } else {
                    // Rust float→int casts saturate, so an absurd magnitude
                    // clamps to i64::MAX/MIN and fails a later bound/cap check.
                    IntArg::Value(n as i64)
                }
            }
        },
    }
}

/// Read a `pad_with` argument at `index`: default `#N/A`, otherwise the scalar
/// value placed verbatim (any type — a number, text, blank, or an error).
///
/// A multi-cell range/array `pad_with` is undocumented on the basic pages, so it
/// is refused (`#UNSUPPORTED!`, probe L3A-PADARR); the `Err` carries the [`Value`]
/// to return.
pub(crate) fn read_pad(args: &mut dyn CallArgs, index: usize) -> Result<Value, Value> {
    match args.shape(index) {
        ArgShape::Omitted => Ok(PAD_NA),
        ArgShape::Range | ArgShape::Array => Err(Value::Error(ErrorKind::Unsupported)),
        ArgShape::Scalar => {
            let v = args.eval_scalar(index);
            // A lambda is engine-internal (RFC-0012 BC-6) and must never be
            // placed verbatim as a pad value — refuse loudly rather than relocate
            // it into the result array (integration guard, the contract review lane-3a review).
            if matches!(v, Value::Lambda(_)) {
                return Err(Value::Error(ErrorKind::Unsupported));
            }
            Ok(v)
        }
    }
}

/// Collect the 1-based `CHOOSEROWS`/`CHOOSECOLS` index arguments (args `1..`)
/// into 0-based offsets against an axis of length `dim`.
///
/// Per the pages: a negative index counts from the end (`-1` → the last), and
/// "Excel returns a #VALUE error if the absolute value of any of the … arguments
/// is zero or exceeds the number of rows/columns." An elided, fractional, or
/// array-valued index is undocumented → refused (`#UNSUPPORTED!`).
pub(crate) fn collect_indices(args: &mut dyn CallArgs, dim: usize) -> Result<Vec<usize>, Value> {
    let count = args.count();
    let mut out: Vec<usize> = Vec::with_capacity(count.saturating_sub(1));
    for i in 1..count {
        match int_arg(args, i) {
            IntArg::Value(n) => {
                if n == 0 {
                    // |index| == 0 (documented #VALUE!).
                    return Err(Value::Error(ErrorKind::Value));
                }
                let idx = if n > 0 { n } else { dim as i64 + n + 1 };
                if idx < 1 || idx > dim as i64 {
                    // |index| exceeds the axis (documented #VALUE!).
                    return Err(Value::Error(ErrorKind::Value));
                }
                out.push((idx - 1) as usize);
            }
            IntArg::Omitted | IntArg::NonInteger | IntArg::NonScalar => {
                return Err(Value::Error(ErrorKind::Unsupported));
            }
            IntArg::Err(k) => return Err(Value::Error(k)),
        }
    }
    Ok(out)
}

/// `true` if a `rows × cols` result would exceed [`MAX_MATERIALIZED_ELEMS`].
pub(crate) fn over_cap(rows: usize, cols: usize) -> bool {
    (rows as u64).saturating_mul(cols as u64) > MAX_MATERIALIZED_ELEMS
}

/// Flatten a [`Grid`] into a linear value list for `TOCOL`/`TOROW`, applying the
/// documented `ignore` filter and scan direction.
///
/// - `by_column == false` (default): scan by row (row-major).
/// - `by_column == true`: scan by column (column-major).
/// - `ignore`: `0` keep all, `1` ignore blanks, `2` ignore errors, `3` both.
pub(crate) fn flatten(grid: &Grid, ignore: i64, by_column: bool) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    if by_column {
        for c in 0..grid.cols {
            for r in 0..grid.rows {
                let v = grid.at(r, c);
                if keep(v, ignore) {
                    out.push(v.clone());
                }
            }
        }
    } else {
        for r in 0..grid.rows {
            for c in 0..grid.cols {
                let v = grid.at(r, c);
                if keep(v, ignore) {
                    out.push(v.clone());
                }
            }
        }
    }
    out
}

/// Read the `TOCOL`/`TOROW` `ignore` argument (arg 1): default `0`, must be one
/// of `{0,1,2,3}`. On refusal returns the [`Value`] to return (`#UNSUPPORTED!`),
/// or propagates a coercion error.
pub(crate) fn read_ignore(args: &mut dyn CallArgs) -> Result<i64, Value> {
    match int_arg(args, 1) {
        IntArg::Omitted => Ok(0),
        IntArg::Value(n) if (0..=3).contains(&n) => Ok(n),
        // Out-of-range, fractional, or array-valued `ignore` is undocumented.
        IntArg::Value(_) | IntArg::NonInteger | IntArg::NonScalar => {
            Err(Value::Error(ErrorKind::Unsupported))
        }
        IntArg::Err(k) => Err(Value::Error(k)),
    }
}

/// Read the `TOCOL`/`TOROW` `scan_by_column` argument (arg 2): default FALSE,
/// coerced via [`to_bool`]. An array-valued flag is undocumented → refused.
pub(crate) fn read_scan(args: &mut dyn CallArgs) -> Result<bool, Value> {
    match args.shape(2) {
        ArgShape::Omitted => Ok(false),
        ArgShape::Range | ArgShape::Array => Err(Value::Error(ErrorKind::Unsupported)),
        ArgShape::Scalar => to_bool(&args.eval_scalar(2)).map_err(Value::Error),
    }
}

/// Whether a value survives the `TOCOL`/`TOROW` `ignore` filter. Matches are
/// explicit (`matches!`) rather than a bare wildcard so a new `Value` variant is
/// a compile decision, not a silent absorb (BC-11 discipline).
fn keep(v: &Value, ignore: i64) -> bool {
    let ignore_blanks = ignore == 1 || ignore == 3;
    let ignore_errors = ignore == 2 || ignore == 3;
    if ignore_blanks && matches!(v, Value::Blank) {
        return false;
    }
    if ignore_errors && matches!(v, Value::Error(_)) {
        return false;
    }
    true
}

/// The validated inputs shared by `WRAPROWS` / `WRAPCOLS`.
pub(crate) enum WrapInputs {
    /// A 1-D `vector` (in reading order), a wrap count `<= elems.len()`, and the
    /// pad value.
    Ready {
        elems: Vec<Value>,
        wrap: usize,
        pad: Value,
    },
    /// An error/refusal to return directly (`#VALUE!` non-vector, `#NUM!`
    /// wrap_count < 1, refusals, propagated coercion errors).
    Return(Value),
}

/// Validate the shared `WRAPROWS`/`WRAPCOLS` arguments: a one-dimensional
/// `vector` (arg 0), a `wrap_count >= 1` (arg 1), and an optional `pad_with`
/// (arg 2, default `#N/A`).
///
/// - Non-vector `vector` (both dims > 1) → `#VALUE!` (documented).
/// - `wrap_count < 1` → `#NUM!` (documented).
/// - `wrap_count > len(vector)` → refused (`#UNSUPPORTED!`, L3A-WRAP-SINGLE): the
///   page's "the vector is simply returned in a single row/column" contradicts
///   its general "the row/column is padded" rule for this case, so it is left to
///   a probe rather than guessed. `wrap_count == len` (a single exactly-filled
///   line) *is* supported.
pub(crate) fn wrap_inputs(args: &mut dyn CallArgs) -> WrapInputs {
    let grid = match materialize(args, 0) {
        Materialized::Grid(g) => g,
        Materialized::Omitted => return WrapInputs::Return(Value::Error(ErrorKind::Unsupported)),
        Materialized::Refused(k) => return WrapInputs::Return(Value::Error(k)),
    };
    // A vector is one-dimensional: a single row or a single column.
    if grid.rows > 1 && grid.cols > 1 {
        return WrapInputs::Return(Value::Error(ErrorKind::Value));
    }
    let n = grid.data.len();

    let wrap_i = match int_arg(args, 1) {
        IntArg::Value(w) => {
            if w < 1 {
                return WrapInputs::Return(Value::Error(ErrorKind::Num));
            }
            w
        }
        // Elided (required), fractional, or array-valued wrap_count → refuse.
        IntArg::Omitted | IntArg::NonInteger | IntArg::NonScalar => {
            return WrapInputs::Return(Value::Error(ErrorKind::Unsupported));
        }
        IntArg::Err(k) => return WrapInputs::Return(Value::Error(k)),
    };
    if wrap_i > n as i64 {
        // wrap_count > element count: the "single row/column" ambiguity.
        return WrapInputs::Return(Value::Error(ErrorKind::Unsupported));
    }
    let wrap = wrap_i as usize;

    let pad = match read_pad(args, 2) {
        Ok(v) => v,
        Err(v) => return WrapInputs::Return(v),
    };
    WrapInputs::Ready {
        elems: grid.data,
        wrap,
        pad,
    }
}

/// Build a spilled sub-rectangle of `grid`: rows `[r0, r1)` × columns
/// `[c0, c1)` (both half-open, `r0 < r1 <= grid.rows`, `c0 < c1 <= grid.cols`).
/// The `TAKE` / `DROP` contiguous-window builder.
pub(crate) fn subrect(grid: &Grid, r0: usize, r1: usize, c0: usize, c1: usize) -> Value {
    let rows = r1 - r0;
    let cols = c1 - c0;
    let mut data: Vec<Value> = Vec::with_capacity(rows * cols);
    for r in r0..r1 {
        for c in c0..c1 {
            data.push(grid.at(r, c).clone());
        }
    }
    spill(rows, cols, data)
}

/// Build a spilled [`Value::Array`] from a `rows × cols` row-major `data`.
///
/// `data.len()` must equal `rows * cols`. A shape mismatch (which should never
/// happen if the caller sized `data` correctly) surfaces as `#UNSUPPORTED!`
/// rather than a panic.
pub(crate) fn spill(rows: usize, cols: usize, data: Vec<Value>) -> Value {
    match Array::new(rows, cols, data) {
        Ok(a) => Value::Array(a),
        Err(_) => Value::Error(ErrorKind::Unsupported),
    }
}

//! Shared helpers for the M2 **Lane 3b** dynamic-array functions — `XLOOKUP`,
//! `XMATCH`, `FILTER`, `SORT`, `SORTBY`, `UNIQUE`.
//!
//! These functions all begin the same way: pull a range/array argument into a
//! materialized, padded rectangle, then reason about its shape. This module
//! centralises that so each `func_*` stays about *its* semantics, not about
//! walking [`CallArgs`].
//!
//! # Whole-column/row inputs are refused, not spilled
//! Every materializer here routes through the dense [`CallArgs::for_each_row`]
//! walk, which **refuses an unbounded whole-column/row range** (the RFC-0001
//! guardrail — see `args.rs`). A Lane 3b function over `A:A` would have to spill
//! up to 1,048,576 rows, which is out of v0 scope, so the refusal is surfaced
//! as a loud `#UNSUPPORTED!` rather than a guessed extent (the Recalc design rules
//! Principle 2). Unlike `INDEX`/`VLOOKUP`, none of these functions can serve the
//! whole-column case with an O(1) absolute read — their result is intrinsically
//! the whole (filtered/sorted/deduped) axis — so there is no RFC-0006 fast path
//! to fall back to.

use std::ops::ControlFlow;

use xl_value::{Array, ErrorKind, Value, to_number};

use crate::args::{ArgShape, CallArgs};
use crate::lookup::{LookupEq, exact_eq};

/// A materialized rectangle: `height` rows × `width` columns, stored row-major,
/// every row padded on the right to `width` with [`Value::Blank`] so callers can
/// index `rows[r][c]` uniformly.
pub(crate) struct Grid {
    /// Row-major cells; `rows.len() == height`, every inner `Vec` is `width` long.
    pub(crate) rows: Vec<Vec<Value>>,
    /// Number of rows (`0` only for an omitted/empty argument).
    pub(crate) height: usize,
    /// Number of columns (`0` only for an omitted/empty argument).
    pub(crate) width: usize,
}

impl Grid {
    /// Column `c` as a top-to-bottom vector (`c` must be `< width`).
    pub(crate) fn column(&self, c: usize) -> Vec<Value> {
        self.rows.iter().map(|r| r[c].clone()).collect()
    }
}

/// Materialize argument `index` into a padded [`Grid`] via the dense
/// [`CallArgs::for_each_row`] walk.
///
/// Returns `Err(ErrorKind::Unsupported)` when the walk refuses — an unbounded
/// whole-column/row range (the RFC-0001 guardrail) or an argument that resolves
/// to no range (bad sheet, 3-D span, unresolved name). Lane 3b callers surface
/// that unchanged as a loud `#UNSUPPORTED!` (see the module docs).
pub(crate) fn materialize(args: &mut dyn CallArgs, index: usize) -> Result<Grid, ErrorKind> {
    let mut rows: Vec<Vec<Value>> = Vec::new();
    args.for_each_row(index, &mut |row| {
        rows.push(row.to_vec());
        ControlFlow::Continue(())
    })?;
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    for row in &mut rows {
        if row.len() < width {
            row.resize(width, Value::Blank);
        }
    }
    let height = rows.len();
    Ok(Grid {
        rows,
        height,
        width,
    })
}

/// The orientation of a 1-D lookup vector flattened from a [`Grid`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Orient {
    /// A single column (`N×1`) — positions run down the rows. A `1×1` grid is
    /// reported as `Vertical` (length 1).
    Vertical,
    /// A single row (`1×N`) — positions run across the columns.
    Horizontal,
}

/// Flatten a lookup/search grid to a 1-D vector, reporting its [`Orient`].
///
/// `XLOOKUP`/`XMATCH` document their `lookup_array` as a single row *or* a
/// single column; a genuinely 2-D grid (more than one row **and** more than one
/// column) has no documented flattening order, so it returns
/// `Err(ErrorKind::Unsupported)` rather than guess row-major vs column-major
/// (mirrors `func_match`'s 2-D refusal).
pub(crate) fn flatten_1d(grid: &Grid) -> Result<(Vec<Value>, Orient), ErrorKind> {
    if grid.height == 0 || grid.width == 0 {
        return Ok((Vec::new(), Orient::Vertical));
    }
    if grid.height > 1 && grid.width > 1 {
        return Err(ErrorKind::Unsupported);
    }
    if grid.width == 1 {
        Ok((grid.column(0), Orient::Vertical))
    } else {
        Ok((grid.rows[0].clone(), Orient::Horizontal))
    }
}

/// The leftmost (row-major) [`Value::Error`] in `grid`, if any.
///
/// Used by `SORT`/`SORTBY`/`UNIQUE` to propagate a data error **leftmost-first**
/// rather than fold it into an unpinned comparison/dedup verdict (the Recalc design rules
/// Principle 2; the brief's "propagate leftmost-first, don't silently drop").
/// `FILTER`/`XLOOKUP` do **not** use this: they carry data errors through into
/// their result in place (Excel-faithful), erroring only on a value they must
/// actually *compare* or *coerce*.
pub(crate) fn first_error(grid: &Grid) -> Option<ErrorKind> {
    for row in &grid.rows {
        for v in row {
            if let Value::Error(k) = v {
                return Some(*k);
            }
        }
    }
    None
}

/// Pre-validate a comparison key line for `SORT`/`SORTBY` before it is handed to
/// [`xl_value::compare`].
///
/// [`xl_value::compare`] can only fail on: an **error** cell (returned here so
/// the caller propagates it leftmost-first), **non-ASCII text** (locale
/// collation is unpinned — OXP-031, so `#UNSUPPORTED!`), or an unresolved
/// **array/ref** cell. Once this returns `Ok(())`, every cell is
/// `Number`/ASCII-`Text`/`Bool`/`Blank` and `compare` is total (never errors) —
/// so the sort comparator can safely treat a residual `Err` as
/// `Ordering::Equal`. Mixed types and `Blank`s pass the precheck: their relative
/// order follows `compare`'s frozen total order (unpinned placement — OXP-040).
///
/// The line is scanned left-to-right, so the returned error is the leftmost
/// offending cell's.
pub(crate) fn precheck_compare_line(line: &[Value]) -> Result<(), ErrorKind> {
    for v in line {
        match v {
            Value::Number(_) | Value::Bool(_) | Value::Blank => {}
            Value::Text(t) => {
                if !t.as_str().is_ascii() {
                    return Err(ErrorKind::Unsupported);
                }
            }
            Value::Error(k) => return Err(*k),
            Value::Array(_) | Value::Ref(_) | Value::Lambda(_) => {
                return Err(ErrorKind::Unsupported);
            }
        }
    }
    Ok(())
}

/// The `match_mode` XLOOKUP/XMATCH modes this tranche can serve. The **wildcard**
/// mode (`2`) is refused up front (its `*`/`?`/`~` collation is undocumented on
/// the MS page — the OXP-089-class hazard), so it never becomes a variant here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum XMatchMode {
    /// `match_mode = 0` (default): exact match only.
    Exact,
    /// `match_mode = -1`: exact match, else the next smaller item.
    ExactOrSmaller,
    /// `match_mode = 1`: exact match, else the next larger item.
    ExactOrLarger,
}

/// Resolve the shared XLOOKUP/XMATCH `match_mode` (arg `mm_idx`) and `search_mode`
/// (arg `sm_idx`) into an [`XMatchMode`] plus the linear scan direction
/// (`forward == true` for `search_mode = 1`, `false` for `-1`).
///
/// Refused loudly (`#UNSUPPORTED!`), per Recalc Principle 2 and the MS-page
/// gaps recorded in `docs/plans/2026-07-15-lane3b-probe-needed.md`:
/// - `match_mode = 2` — wildcard collation (`*`/`?`/`~`) is not documented.
/// - `search_mode = 2` / `-2` — binary search's behavior on unsorted data and
///   its duplicate tie-break are not pinned (the OXP-088-class hazard).
/// - any other `match_mode`/`search_mode` value — undocumented.
///
/// An **Omitted** mode arg (absent OR an elided `,` slot — via
/// [`ArgShape::Omitted`], never conflated with a provided `Blank`) takes its
/// documented default (`match_mode = 0`, `search_mode = 1`). A **provided** mode
/// is coerced (a provided `Blank` → `0`, which is a valid `match_mode` but an
/// invalid `search_mode` → refused); an error in a mode arg propagates.
pub(crate) fn resolve_modes(
    args: &mut dyn CallArgs,
    mm_idx: usize,
    sm_idx: usize,
) -> Result<(XMatchMode, bool), ErrorKind> {
    let match_mode = if args.shape(mm_idx) == ArgShape::Omitted {
        0.0
    } else {
        to_number(&args.eval_scalar(mm_idx))?
    };
    let search_mode = if args.shape(sm_idx) == ArgShape::Omitted {
        1.0
    } else {
        to_number(&args.eval_scalar(sm_idx))?
    };

    let mode = if match_mode == 0.0 {
        XMatchMode::Exact
    } else if match_mode == -1.0 {
        XMatchMode::ExactOrSmaller
    } else if match_mode == 1.0 {
        XMatchMode::ExactOrLarger
    } else {
        // `2` (wildcard) and every other value: undocumented / unpinned.
        return Err(ErrorKind::Unsupported);
    };

    let forward = if search_mode == 1.0 {
        true
    } else if search_mode == -1.0 {
        false
    } else {
        // `2`/`-2` binary and every other value: unpinned.
        return Err(ErrorKind::Unsupported);
    };

    Ok((mode, forward))
}

/// Exact-match linear scan of a 1-D vector for `key`, honouring the search
/// direction (`forward` = first-to-last; otherwise last-to-first). Equality is
/// the shared LOOKUP-family [`exact_eq`], so `XLOOKUP`/`XMATCH` inherit the
/// oracle-scoped `Blank` handling (OXP-104/165) and cross-type strictness that
/// `VLOOKUP`/`MATCH` use, and a `Defer` becomes a loud `#UNSUPPORTED!`.
///
/// **No wildcard pre-check** (unlike `func_match::exact_search`): in XLOOKUP's
/// exact modes `*`/`?` are *literal* characters — wildcards are active only in
/// the refused `match_mode = 2`. An error cell reached before a match propagates
/// (leftmost-in-scan-order).
pub(crate) fn exact_scan(
    vec: &[Value],
    key: &Value,
    forward: bool,
) -> Result<Option<usize>, ErrorKind> {
    let n = vec.len();
    for step in 0..n {
        let i = if forward { step } else { n - 1 - step };
        match exact_eq(&vec[i], key) {
            Ok(LookupEq::Match) => return Ok(Some(i)),
            Ok(LookupEq::NoMatch) => {}
            Ok(LookupEq::Defer) => return Err(ErrorKind::Unsupported),
            Err(k) => return Err(k),
        }
    }
    Ok(None)
}

/// Interpret a numeric `sort_order` for `SORT`/`SORTBY`: `1` → ascending
/// (`Some(false)`), `-1` → descending (`Some(true)`); any other value is invalid
/// (`None`, which the caller maps to `#VALUE!`, per SORTBY's documented rule).
pub(crate) fn sort_order_descending(order: f64) -> Option<bool> {
    if order == 1.0 {
        Some(false)
    } else if order == -1.0 {
        Some(true)
    } else {
        None
    }
}

/// Wrap a bounded row-major `data` of `rows`×`cols` into a spillable
/// [`Value::Array`], or `#CALC!` if the shape is degenerate (`0` on either
/// axis). A top-level `Value::Array` auto-spills via the engine's spill
/// machinery — Lane 3b never touches `xl-engine`.
pub(crate) fn spill(rows: usize, cols: usize, data: Vec<Value>) -> Value {
    match Array::new(rows, cols, data) {
        Ok(a) => Value::Array(a),
        // The only way `Array::new` fails here is an empty dimension; every Lane
        // 3b caller routes a genuinely-empty result to its own documented error
        // (`#CALC!` for FILTER/UNIQUE) first, so this is a backstop.
        Err(_) => Value::Error(ErrorKind::Calc),
    }
}

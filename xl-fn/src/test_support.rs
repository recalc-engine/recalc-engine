//! Shared `#[cfg(test)]` mock [`CallArgs`] for per-function unit tests.
//!
//! One source of truth for the argument-mock used by `func_*` unit tests, so a
//! new function's tests can exercise its `eval` in isolation — without standing
//! up the engine and without each file re-deriving the trait boilerplate. Lifted
//! from the original inline mock in `lib.rs`'s test module; see [`CallArgs`] for
//! the contract each method models.
//!
//! Use [`eval_direct`] to invoke a not-yet-registered function's evaluator
//! directly (the registry-based `call` helper in `lib.rs` can't, since the
//! function isn't in `REGISTRY` until wired). Construct arguments with
//! [`TestArgs::new`] over [`TestArg`] shapes; [`num`]/[`txt`] are value shorthands.
#![cfg(test)]
// A complete `CallArgs` fixture mirroring the trait's full shape space; not every
// arg-shape variant or helper is exercised by every consumer, which is expected
// for shared test scaffolding.
#![allow(dead_code)]

use std::ops::ControlFlow;

use xl_value::{ErrorKind, SheetId, Value};

use crate::args::{ArgShape, CallArgs, RefRect};
use crate::context::EvalContext;
use crate::registry::EvalFn;

// range that the dense row walk must refuse.
pub(crate) enum TestArg {
    Scalar(Value),
    Range(Vec<Value>),
    Array(Vec<Value>),
    Rect {
        rows: u32,
        cols: u32,
        data: Vec<Value>,
    },
    /// A whole-column range with **contiguous** single-column data at
    /// relative rows `0..n`: `for_each_cell` streams its cells and the
    /// used-extent `for_each_used_row` yields `(i, [vs[i]])`, but
    /// `dims`/`for_each_row` refuse it (the documented dense-walk policy).
    Unbounded(Vec<Value>),
    /// A whole-column range with **explicit** used-extent rows
    /// `(relative_row, cells)` — models gaps between populated rows and
    /// multi-column column spans. `dims`/`for_each_row` refuse it (unbounded);
    /// `for_each_used_row` yields the rows verbatim (they must be ascending).
    UsedRows(Vec<(u32, Vec<Value>)>),
    /// A whole-**row** range (unbounded *columns*) with **explicit** used-extent
    /// columns `(relative_col, cells)`, each `cells` a row-span-tall column slice
    /// (RFC 0008) — models gaps between populated columns and multi-row row spans.
    /// `dims`/`for_each_row`/`for_each_used_row` all refuse it (it is unbounded on
    /// the column axis, and the row iterator serves only whole-column ranges);
    /// `for_each_used_col` yields the columns verbatim (they must be ascending).
    UsedCols(Vec<(u32, Vec<Value>)>),
    /// A *reference* argument whose position + full extent is surfaced through
    /// the RFC-0005 [`CallArgs::arg_ref_extent`] channel (not `dims`):
    /// `arg_ref_extent` returns `Some(rect)` while `dims`/`for_each_row` refuse
    /// it. Models how `EngineArgs` serves a whole-column/row reference
    /// (`ROWS(A:A)` reads the full extent — OXP-116) and how `ROW`/`COLUMN` read
    /// a reference's top-left position.
    Reference(RefRect),
    /// A *whole-column/row reference* whose geometry is surfaced through
    /// [`CallArgs::arg_ref_extent`] **and** whose individual cells are served at
    /// **absolute** positions through the RFC-0006 [`CallArgs::cell_at`] channel —
    /// the way `EngineArgs` serves `INDEX(A:A, n)` / a whole-column `HLOOKUP`
    /// without materialising the rectangle. `dims`/`for_each_row` refuse it
    /// (unbounded), so a consumer is forced onto the in-rectangle absolute-read
    /// path. `cells` are `(abs_row, abs_col, value)` inside `rect`; a position
    /// inside `rect` with no listed cell reads as `Value::Blank`.
    RefCells {
        rect: RefRect,
        cells: Vec<(u32, u32, Value)>,
    },
    /// A *single-cell reference* argument — the RFC-0010 case that must be
    /// treated as a reference, not a scalar literal. Its
    /// [`shape`](CallArgs::shape) is [`ArgShape::Scalar`] (a 1×1 reference is
    /// scalar-shaped) while [`arg_ref_extent`](CallArgs::arg_ref_extent) returns
    /// `Some(1×1)`, so [`eff_shape`](crate::args::eff_shape) routes it through
    /// the aggregate arm exactly as `EngineArgs` does for a bare `A1` reference.
    /// `for_each_cell` streams the single value **once** (a `Blank` included,
    /// matching `EngineArgs`' single-cell-`Ref` arm, which does *not* elide it);
    /// `eval_scalar` lifts it to that value. This is the
    /// literal-vs-single-cell-reference expressibility RFC-0010 condition 4 asks
    /// for — the existing [`TestArg::Scalar`] variant models the literal.
    CellRef(Value),
    Omitted,
    /// Panics if forced; proves a branch/argument is *not* evaluated.
    Poison,
}

pub(crate) struct TestArgs {
    args: Vec<TestArg>,
    /// The calling cell's position for the RFC-0005 [`CallArgs::anchor`] channel
    /// (`ROW()`/`COLUMN()` no-arg forms). `None` models a mock with no anchor
    /// infra (`anchor()` refuses).
    anchor: Option<(SheetId, u32, u32)>,
}

impl CallArgs for TestArgs {
    fn count(&self) -> usize {
        self.args.len()
    }
    fn shape(&mut self, index: usize) -> ArgShape {
        match self.args.get(index) {
            Some(TestArg::Scalar(_)) | Some(TestArg::CellRef(_)) | Some(TestArg::Poison) => {
                ArgShape::Scalar
            }
            Some(TestArg::Range(_))
            | Some(TestArg::Unbounded(_))
            | Some(TestArg::UsedRows(_))
            | Some(TestArg::UsedCols(_))
            | Some(TestArg::Reference(_))
            | Some(TestArg::RefCells { .. })
            | Some(TestArg::Rect { .. }) => ArgShape::Range,
            Some(TestArg::Array(_)) => ArgShape::Array,
            Some(TestArg::Omitted) | None => ArgShape::Omitted,
        }
    }
    fn eval_scalar(&mut self, index: usize) -> Value {
        match self.args.get(index) {
            Some(TestArg::Scalar(v)) => v.clone(),
            // A 1×1 reference in scalar context lifts to its element (Blank if
            // the referenced cell is empty), matching `EngineArgs`.
            Some(TestArg::CellRef(v)) => v.clone(),
            Some(TestArg::Omitted) | None => Value::Blank,
            // A multi-cell range/array in scalar context is #UNSUPPORTED!,
            // matching xl-value's rule.
            Some(TestArg::Range(_))
            | Some(TestArg::Array(_))
            | Some(TestArg::Rect { .. })
            | Some(TestArg::Unbounded(_))
            | Some(TestArg::UsedRows(_))
            | Some(TestArg::UsedCols(_))
            | Some(TestArg::Reference(_))
            | Some(TestArg::RefCells { .. }) => Value::Error(ErrorKind::Unsupported),
            Some(TestArg::Poison) => panic!("poison argument {index} was evaluated"),
        }
    }
    fn for_each_cell(&mut self, index: usize, visit: &mut dyn FnMut(&Value) -> ControlFlow<()>) {
        match self.args.get(index) {
            Some(TestArg::Range(vs)) | Some(TestArg::Array(vs)) | Some(TestArg::Unbounded(vs)) => {
                for v in vs {
                    if visit(v).is_break() {
                        return;
                    }
                }
            }
            Some(TestArg::Rect { data, .. }) => {
                for v in data {
                    if visit(v).is_break() {
                        return;
                    }
                }
            }
            Some(TestArg::UsedRows(rows)) | Some(TestArg::UsedCols(rows)) => {
                for (_, cells) in rows {
                    for v in cells {
                        if visit(v).is_break() {
                            return;
                        }
                    }
                }
            }
            Some(TestArg::Scalar(v)) => {
                let _ = visit(v);
            }
            // A single-cell reference streams its one value once — a `Blank`
            // included, not elided — mirroring `EngineArgs`' single-cell-`Ref`
            // arm (a bare `A1`, unlike a multi-cell range, surfaces its blank).
            Some(TestArg::CellRef(v)) => {
                let _ = visit(v);
            }
            // A `Reference` carries only geometry (position + extent), no cell
            // values — its consumers (ROW/COLUMN/ROWS/COLUMNS) never stream it.
            // A `RefCells` is served through the absolute `cell_at` path, not by
            // streaming, so it likewise yields nothing here.
            Some(TestArg::Reference(_)) | Some(TestArg::RefCells { .. }) => {}
            Some(TestArg::Poison) => panic!("poison argument {index} was streamed"),
            Some(TestArg::Omitted) | None => {}
        }
    }
    fn dims(&mut self, index: usize) -> Option<(u32, u32)> {
        match self.args.get(index) {
            Some(TestArg::Range(vs)) => Some((vs.len() as u32, 1)),
            Some(TestArg::Array(vs)) => Some((1, vs.len() as u32)),
            Some(TestArg::Rect { rows, cols, .. }) => Some((*rows, *cols)),
            // Unbounded / scalar / lazy / omitted have no materializable
            // bounded rectangle. A `Reference` returns `None` here too so its
            // consumer routes through `arg_ref_extent` (which alone surfaces the
            // whole-axis extent — mirrors `EngineArgs` refusing unbounded dims).
            Some(TestArg::Unbounded(_))
            | Some(TestArg::UsedRows(_))
            | Some(TestArg::UsedCols(_))
            | Some(TestArg::Reference(_))
            | Some(TestArg::RefCells { .. })
            | Some(TestArg::Scalar(_))
            | Some(TestArg::CellRef(_))
            | Some(TestArg::Poison)
            | Some(TestArg::Omitted)
            | None => None,
        }
    }
    fn for_each_row(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(&[Value]) -> ControlFlow<()>,
    ) -> Result<(), ErrorKind> {
        match self.args.get(index) {
            // A column: one value per row.
            Some(TestArg::Range(vs)) => {
                for v in vs {
                    if visit(std::slice::from_ref(v)).is_break() {
                        break;
                    }
                }
                Ok(())
            }
            // A single row.
            Some(TestArg::Array(vs)) => {
                let _ = visit(vs);
                Ok(())
            }
            Some(TestArg::Rect { cols, data, .. }) => {
                for row in data.chunks(*cols as usize) {
                    if visit(row).is_break() {
                        break;
                    }
                }
                Ok(())
            }
            // A scalar is a 1×1 rectangle.
            Some(TestArg::Scalar(v)) => {
                let _ = visit(std::slice::from_ref(v));
                Ok(())
            }
            // A single-cell reference is also a 1×1 rectangle (one value).
            Some(TestArg::CellRef(v)) => {
                let _ = visit(std::slice::from_ref(v));
                Ok(())
            }
            // Documented policy: the dense walk refuses an unbounded range; a
            // `Reference`/`RefCells` (whole-column geometry) is likewise not
            // densely walkable — its consumer must use `arg_ref_extent`/`cell_at`.
            Some(TestArg::Unbounded(_))
            | Some(TestArg::UsedRows(_))
            | Some(TestArg::UsedCols(_))
            | Some(TestArg::Reference(_))
            | Some(TestArg::RefCells { .. }) => Err(ErrorKind::Unsupported),
            Some(TestArg::Poison) => panic!("poison argument {index} was streamed"),
            Some(TestArg::Omitted) | None => Ok(()),
        }
    }
    fn for_each_used_row(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
    ) -> Result<(), ErrorKind> {
        match self.args.get(index) {
            // Whole-column single-column data at contiguous relative rows.
            Some(TestArg::Unbounded(vs)) => {
                for (i, v) in vs.iter().enumerate() {
                    if visit(i as u32, std::slice::from_ref(v)).is_break() {
                        break;
                    }
                }
                Ok(())
            }
            // Explicit used-extent rows (relative_row, cells), verbatim.
            Some(TestArg::UsedRows(rows)) => {
                for (rel, cells) in rows {
                    if visit(*rel, cells).is_break() {
                        break;
                    }
                }
                Ok(())
            }
            // Bounded shapes carry their dense relative index, mirroring
            // `EngineArgs` so a test can exercise the method directly.
            Some(TestArg::Range(vs)) => {
                for (i, v) in vs.iter().enumerate() {
                    if visit(i as u32, std::slice::from_ref(v)).is_break() {
                        break;
                    }
                }
                Ok(())
            }
            Some(TestArg::Array(vs)) => {
                let _ = visit(0, vs);
                Ok(())
            }
            Some(TestArg::Rect { cols, data, .. }) => {
                for (i, row) in data.chunks(*cols as usize).enumerate() {
                    if visit(i as u32, row).is_break() {
                        break;
                    }
                }
                Ok(())
            }
            Some(TestArg::Scalar(v)) => {
                let _ = visit(0, std::slice::from_ref(v));
                Ok(())
            }
            Some(TestArg::CellRef(v)) => {
                let _ = visit(0, std::slice::from_ref(v));
                Ok(())
            }
            // A whole-**row** range (unbounded columns) is not served by the
            // row-oriented used-extent walk; it refuses, forcing the caller onto
            // the column iterator `for_each_used_col` (RFC 0008 §2).
            Some(TestArg::UsedCols(_)) => Err(ErrorKind::Unsupported),
            // A `Reference`/`RefCells` is served through `cell_at`, not by a
            // used-extent row walk.
            Some(TestArg::Reference(_)) | Some(TestArg::RefCells { .. }) => {
                Err(ErrorKind::Unsupported)
            }
            Some(TestArg::Poison) => panic!("poison argument {index} was streamed"),
            Some(TestArg::Omitted) | None => Ok(()),
        }
    }
    fn for_each_used_col(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
    ) -> Result<(), ErrorKind> {
        match self.args.get(index) {
            // Whole-row range with explicit used-extent columns (rel_col, cells),
            // verbatim (they must be ascending) — the RFC 0008 column walk.
            Some(TestArg::UsedCols(cols)) => {
                for (rel, cells) in cols {
                    if visit(*rel, cells).is_break() {
                        break;
                    }
                }
                Ok(())
            }
            // Bounded shapes carry columns transposed from their dense layout,
            // mirroring `EngineArgs` so a test can exercise the method directly
            // (production paths reach the column walk only for a whole-row range,
            // never for these bounded shapes). `Range` is an N×1 column → one
            // column (rel 0) of all values; `Array` is a 1×N row → N single-cell
            // columns; `Rect` is transposed row-major → column-major.
            Some(TestArg::Range(vs)) => {
                let _ = visit(0, vs);
                Ok(())
            }
            Some(TestArg::Array(vs)) => {
                for (i, v) in vs.iter().enumerate() {
                    if visit(i as u32, std::slice::from_ref(v)).is_break() {
                        break;
                    }
                }
                Ok(())
            }
            Some(TestArg::Rect { rows, cols, data }) => {
                let cols = *cols as usize;
                let rows = *rows as usize;
                let mut col_buf: Vec<Value> = Vec::with_capacity(rows);
                for c in 0..cols {
                    col_buf.clear();
                    for r in 0..rows {
                        col_buf.push(data.get(r * cols + c).cloned().unwrap_or(Value::Blank));
                    }
                    if visit(c as u32, &col_buf).is_break() {
                        break;
                    }
                }
                Ok(())
            }
            Some(TestArg::Scalar(v)) => {
                let _ = visit(0, std::slice::from_ref(v));
                Ok(())
            }
            Some(TestArg::CellRef(v)) => {
                let _ = visit(0, std::slice::from_ref(v));
                Ok(())
            }
            // Whole-**column** shapes (`Unbounded`/`UsedRows`) are served by the
            // row iterator, not this one: refuse, mirroring `EngineArgs`. A
            // `Reference`/`RefCells` is served through `cell_at`.
            Some(TestArg::Unbounded(_))
            | Some(TestArg::UsedRows(_))
            | Some(TestArg::Reference(_))
            | Some(TestArg::RefCells { .. }) => Err(ErrorKind::Unsupported),
            Some(TestArg::Poison) => panic!("poison argument {index} was streamed"),
            Some(TestArg::Omitted) | None => Ok(()),
        }
    }
    fn anchor(&self) -> Option<(SheetId, u32, u32)> {
        self.anchor
    }
    fn arg_ref_extent(&mut self, index: usize) -> Option<RefRect> {
        match self.args.get(index) {
            Some(TestArg::Reference(rect)) => Some(*rect),
            Some(TestArg::RefCells { rect, .. }) => Some(*rect),
            // A single-cell reference: a 1×1 rectangle whose ref-extent is
            // `Some` (the RFC-0010 signal `eff_shape` reads to route it as an
            // aggregate, not a scalar literal).
            Some(TestArg::CellRef(_)) => Some(RefRect {
                row: 0,
                col: 0,
                height: 1,
                width: 1,
            }),
            _ => None,
        }
    }
    fn cell_at(&mut self, index: usize, row: u32, col: u32) -> Option<Value> {
        match self.args.get(index) {
            // Confine the read to the declared rectangle (RFC 0006): a position
            // outside `rect` yields `None`; a position inside with no listed cell
            // reads as `Value::Blank`.
            Some(TestArg::RefCells { rect, cells }) => {
                if row < rect.row
                    || row >= rect.row + rect.height
                    || col < rect.col
                    || col >= rect.col + rect.width
                {
                    return None;
                }
                Some(
                    cells
                        .iter()
                        .find(|(r, c, _)| *r == row && *c == col)
                        .map(|(_, _, v)| v.clone())
                        .unwrap_or(Value::Blank),
                )
            }
            _ => None,
        }
    }
}

impl TestArgs {
    /// Build a mock arg list from explicit [`TestArg`] shapes (no anchor).
    pub(crate) fn new(args: Vec<TestArg>) -> TestArgs {
        TestArgs { args, anchor: None }
    }

    /// Build a mock arg list with an explicit calling-cell anchor for the
    /// RFC-0005 [`CallArgs::anchor`] channel (`ROW()`/`COLUMN()` no-arg forms).
    pub(crate) fn with_anchor(args: Vec<TestArg>, anchor: Option<(SheetId, u32, u32)>) -> TestArgs {
        TestArgs { args, anchor }
    }
}

/// Invoke a function's `eval` directly (default date system), bypassing the
/// registry so an unregistered/in-progress function can be unit-tested.
pub(crate) fn eval_direct(f: EvalFn, args: Vec<TestArg>) -> Value {
    let ctx = EvalContext::new();
    let mut ta = TestArgs::new(args);
    f(&ctx, &mut ta)
}

/// Like [`eval_direct`], but with an explicit calling-cell anchor for the
/// RFC-0005 [`CallArgs::anchor`] channel — used by `ROW()`/`COLUMN()` no-arg
/// tests (`anchor == None` models a mock with no anchor infra).
pub(crate) fn eval_anchored(
    f: EvalFn,
    args: Vec<TestArg>,
    anchor: Option<(SheetId, u32, u32)>,
) -> Value {
    let ctx = EvalContext::new();
    let mut ta = TestArgs::with_anchor(args, anchor);
    f(&ctx, &mut ta)
}

/// A [`Value::Number`] shorthand for tests.
pub(crate) fn num(x: f64) -> Value {
    Value::number(x)
}

/// A [`Value::Text`] shorthand for tests.
pub(crate) fn txt(s: &str) -> Value {
    Value::text(s)
}

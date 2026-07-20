//! Node identity ([`CellId`]) and the dependency-declaration types
//! ([`SheetRange`], [`Precedent`]).
//!
//! # Provenance
//! These are structural (graph-shape) types, not Excel-semantic ones. They
//! reuse the frozen coordinate types from `xl-value` ([`SheetId`],
//! [`RectRange`]) so the graph and the value model agree on what a cell
//! address is (`implementation-plan.md` §2, "`xl-value`'s types are the frozen
//! contract between lanes").

use core::cmp::Ordering;

use xl_value::{RectRange, SheetId};

/// Identity of a single cell — the graph's node key.
///
/// v1 uses a **concrete** key rather than a generic `NodeKey` trait (the seam
/// the engine, Task 9, plugs into). Rationale: every node in a workbook graph
/// *is* a cell, the key is `Copy` and cheap to order, and a concrete type keeps
/// the range index (which reasons about row/column geometry) explicit. If a
/// future need arises for non-cell nodes (e.g. defined-name or spill-anchor
/// pseudo-nodes) this becomes an `enum` by RFC.
///
/// Coordinates are **0-based** (matching [`RectRange`]). The derived [`Ord`] is
/// lexicographic on `(sheet, row, col)` — this is the canonical tie-break used
/// for every deterministic ordering the graph produces (recalc order, cycle
/// membership, plan steps). Determinism of that order is a **product feature**
/// (`implementation-plan.md` §2: "stable reduction order — cross-platform float
/// identity is a feature").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellId {
    /// Sheet the cell lives on.
    pub sheet: SheetId,
    /// Row index (0-based).
    pub row: u32,
    /// Column index (0-based).
    pub col: u32,
}

impl CellId {
    /// Convenience constructor.
    #[must_use]
    pub fn new(sheet: SheetId, row: u32, col: u32) -> CellId {
        CellId { sheet, row, col }
    }
}

/// A rectangular range on one sheet — the target of a *range* dependency.
///
/// A formula depending on `A1:B100` declares **one** `Range` precedent, not 200
/// `Cell` precedents; the graph's per-sheet range index ([`crate::DepGraph`])
/// resolves "changed cell → dependent range-nodes" without materialising an
/// edge per contained cell.
///
/// Coordinates are 0-based and inclusive on both axes (as [`RectRange`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SheetRange {
    /// Sheet the range lives on (may differ from a dependent cell's own
    /// sheet — cross-sheet references are ordinary range precedents).
    pub sheet: SheetId,
    /// The rectangle covered (0-based, inclusive).
    pub range: RectRange,
}

impl SheetRange {
    /// Convenience constructor.
    #[must_use]
    pub fn new(sheet: SheetId, range: RectRange) -> SheetRange {
        SheetRange { sheet, range }
    }

    /// Whether `cell` lies inside this range.
    ///
    /// A range and a cell on different sheets never intersect. A range whose
    /// `start > end` on either axis (degenerate, not normally produced by the
    /// parser) contains nothing.
    #[must_use]
    pub fn contains(&self, cell: CellId) -> bool {
        cell.sheet == self.sheet
            && cell.row >= self.range.row_start
            && cell.row <= self.range.row_end
            && cell.col >= self.range.col_start
            && cell.col <= self.range.col_end
    }
}

impl From<xl_value::Ref> for SheetRange {
    fn from(r: xl_value::Ref) -> SheetRange {
        SheetRange {
            sheet: r.sheet,
            range: r.range,
        }
    }
}

/// A single declared dependency of a formula cell — either one cell or a
/// rectangular range.
///
/// The engine (Task 9) extracts these from a parsed formula's references
/// (`xl-ast`) and hands the full set to [`crate::DepGraph::set_deps`]. The
/// graph never parses formulas itself (`implementation-plan.md` §2: the graph
/// "owns node identity, edges, ordering, dirtiness, cycles, volatility";
/// evaluation and reference extraction are the engine's job).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Precedent {
    /// Depends on a single cell.
    Cell(CellId),
    /// Depends on every cell in a rectangular range.
    Range(SheetRange),
}

/// `Ord` on `CellId` is lexicographic `(sheet, row, col)`; this helper spells
/// that out for readers of the tie-break logic and guards against a field
/// reorder silently changing the canonical order.
#[inline]
#[must_use]
pub(crate) fn cell_order(a: &CellId, b: &CellId) -> Ordering {
    (a.sheet, a.row, a.col).cmp(&(b.sheet, b.row, b.col))
}

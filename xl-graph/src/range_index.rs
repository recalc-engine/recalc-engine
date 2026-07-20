//! Per-sheet range index: maps a changed cell to the nodes whose *range*
//! precedents cover it, without one edge per contained cell.
//!
//! # Structure & complexity
//! Each sheet owns a [`SheetRanges`]: a flat `Vec` of `(range, dependent)`
//! entries kept **sorted by `row_start`** (then the rest of the rectangle,
//! then the dependent, for a total order → deterministic scans). A query for
//! cell `(row, col)`:
//!
//! 1. binary-searches for the first entry with `row_start > row` — every
//!    candidate whose top edge is at or above `row` is in the prefix before it;
//! 2. linearly scans that prefix, keeping entries that also satisfy
//!    `row_end >= row` and `col_start <= col <= col_end`.
//!
//! So a query is `O(log n + k)` where `k` is the number of ranges beginning at
//! or above the query row, and a rebuild-sort is `O(n log n)`. Insertion is
//! `O(1)` amortised (push + mark dirty); the sort is paid lazily on the next
//! query. This is the "sorted vec of range entries with binary search" option
//! the lane spec offers. It is deliberately simple and fully deterministic;
//! its weakness is the `k` term — many ranges anchored near the top of a sheet
//! (a common shape, e.g. `A$1:A$100` filled down) degrade a query toward
//! `O(n)`. A proper interval tree / segment structure would give
//! `O(log n + output)`; that upgrade is deferred and does not change the public
//! API. The point the lane spec insists on — that `A1:B100` is **one** entry,
//! not 200 edges — holds regardless.
//!
//! # Provenance
//! Standard sorted-array + binary-search interval filtering. No Excel semantics
//! live here (this maps geometry to node ids); the value/error a dependent
//! ultimately takes is the engine's concern.

use core::cmp::Ordering;

use xl_value::RectRange;

use crate::cell::{CellId, cell_order};

/// One `(range, dependent)` registration: the cell `dependent` has a range
/// precedent covering `range` (on the sheet that owns this [`SheetRanges`]).
#[derive(Clone, Copy, Debug)]
struct Entry {
    range: RectRange,
    dependent: CellId,
}

/// Total order over entries: by rectangle geometry first (so `row_start` is the
/// binary-search key), then by dependent, giving a deterministic layout
/// regardless of insertion order.
fn entry_order(a: &Entry, b: &Entry) -> Ordering {
    (
        a.range.row_start,
        a.range.row_end,
        a.range.col_start,
        a.range.col_end,
    )
        .cmp(&(
            b.range.row_start,
            b.range.row_end,
            b.range.col_start,
            b.range.col_end,
        ))
        .then_with(|| cell_order(&a.dependent, &b.dependent))
}

/// The range entries registered on a single sheet.
#[derive(Clone, Debug, Default)]
pub(crate) struct SheetRanges {
    entries: Vec<Entry>,
    /// `false` after an insertion invalidates the sort; a query re-sorts.
    sorted: bool,
}

impl SheetRanges {
    /// Register that `dependent` depends on `range`.
    pub(crate) fn insert(&mut self, range: RectRange, dependent: CellId) {
        self.entries.push(Entry { range, dependent });
        self.sorted = false;
    }

    /// Drop **every** entry naming `dependent`. Used when a node's precedents
    /// are replaced or the node is removed; because `set_deps` replaces the
    /// whole precedent set, removing all of a dependent's range entries and
    /// re-inserting the survivors is correct. `retain` preserves relative
    /// order, so a previously-sorted vec stays sorted.
    pub(crate) fn remove_dependent(&mut self, dependent: CellId) {
        self.entries.retain(|e| e.dependent != dependent);
    }

    /// Whether any entries remain (an empty per-sheet index is pruned).
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Append the dependents of every range covering `(row, col)` to `out`.
    ///
    /// Results are appended in the index's sorted order; the caller dedups /
    /// re-sorts as needed. Takes `&mut self` because a query lazily restores
    /// the sort invariant — this is why range queries run during the
    /// `&mut self` dirty-marking pass, never during read-only plan building.
    pub(crate) fn query(&mut self, row: u32, col: u32, out: &mut Vec<CellId>) {
        if !self.sorted {
            self.entries.sort_by(entry_order);
            self.sorted = true;
        }
        // First index whose `row_start > row`; all candidates precede it.
        let cut = self.entries.partition_point(|e| e.range.row_start <= row);
        for e in &self.entries[..cut] {
            if row <= e.range.row_end && col >= e.range.col_start && col <= e.range.col_end {
                out.push(e.dependent);
            }
        }
    }
}

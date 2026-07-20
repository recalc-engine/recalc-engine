//! `xl-graph` — the Recalc dependency graph.
//!
//! Builds and maintains the formula dependency graph over a workbook and
//! answers the two questions the engine needs: **in what order** must cells be
//! recalculated, and **which** cells must be recalculated after an edit. It
//! owns node identity, edges, ordering, dirtiness, cycle detection, and
//! volatility. It does **not** evaluate formulas or parse them — the engine
//! (Task 9) extracts each formula's references (via `xl-ast`) and reports them
//! through [`DepGraph::set_deps`], then consumes the [`Plan`] this crate
//! produces. That callback seam keeps evaluation out of the graph
//! (`implementation-plan.md` §2).
//!
//! # What lives here
//! - **Graph build/maintenance** — [`DepGraph::set_deps`],
//!   [`DepGraph::remove_node`]; range dependencies are stored as *one* entry in
//!   a per-sheet range index, never one edge per contained cell.
//! - **Full recalc order** — [`DepGraph::full_plan`]: a topological order over
//!   the whole graph with deterministic `CellId` tie-breaking.
//! - **Incremental recalc** — [`DepGraph::mark_dirty`] /
//!   [`DepGraph::mark_volatile_dirty`] then [`DepGraph::recalc_plan`]: the plan
//!   is the full order restricted to the transitively-dirty cells.
//! - **Cycles** — Tarjan SCC (iterative) groups circular references; with
//!   iteration off they become [`Step::Cycle`], on they become
//!   [`Step::Iterate`] carrying the [`CalcSettings`].
//! - **Volatility** — [`DepGraph::register_volatile`] and dynamic-dependency
//!   flagging [`DepGraph::set_dynamic_deps`] for `OFFSET`/`INDIRECT`.
//!
//! # Determinism
//! Recalc order is a **product feature** (`implementation-plan.md` §2: "stable
//! reduction order, cross-platform float identity"). Every ordered output is a
//! deterministic function of the graph: all internal maps/sets are
//! `BTreeMap`/`BTreeSet`, adjacency lists are sorted before traversal, and plan
//! tie-breaks use [`CellId`] order. No `HashMap` iteration order can leak into a
//! schedule. The unit test `plan_is_build_order_independent` and the property
//! test `plan_determinism_under_build_order` pin this.
//!
//! # No recursion
//! Workbooks chain 1,000,000+ cells. Every traversal here is **iterative** with
//! an explicit stack/worklist — dirty propagation, Tarjan SCC, and Kahn's
//! topological sort — so none can overflow the call stack (the failure class of
//! the parser bug fixed in commit `afb5141`). See the scale tests.
//!
//! # Module-header provenance
//! The graph machinery is algorithmic, not Excel-semantic: it cites standard
//! sources by name — **Tarjan 1972** (SCC, `src/scc.rs`) and **Kahn 1962**
//! (topological sort, `src/schedule.rs`). The few Excel-semantic facts — the
//! volatile-function set and the iterative-calc defaults (100 iterations /
//! 0.001 change) — cite `implementation-plan.md` §2 and Microsoft's calculation
//! documentation (see [`CalcSettings`]). Recalc-order questions that are
//! Excel-observable but undocumented (evaluation order *within* a cycle; the
//! exact value a circular reference takes with iteration off) are **not
//! guessed**: they are queued as oracle probe `OXP-070` and the graph exposes
//! only deterministic membership, leaving values to the engine
//! (`implementation-plan.md` §0, "Never silently wrong").

#![forbid(unsafe_code)]

mod cell;
mod plan;
mod range_index;
mod scc;
mod schedule;

use std::collections::{BTreeMap, BTreeSet};

pub use cell::{CellId, Precedent, SheetRange};
pub use plan::{CalcSettings, CycleGroup, IterativeGroup, Plan, Step};

use range_index::SheetRanges;

/// A formula cell's stored dependencies (its *precedents*): what it reads.
///
/// Split into single-cell and range precedents so ranges stay compact (one
/// entry, resolved lazily through the range index) rather than exploding into
/// an edge per contained cell.
#[derive(Clone, Debug, Default)]
pub(crate) struct Node {
    /// Single-cell precedents.
    pub(crate) cell_precedents: BTreeSet<CellId>,
    /// Range precedents (may target other sheets).
    pub(crate) range_precedents: BTreeSet<SheetRange>,
}

/// The dependency graph of a workbook.
///
/// A *node* is a formula cell registered via [`set_deps`](DepGraph::set_deps).
/// Plain input cells (constants) are not nodes: they never need recalculation,
/// but editing one still dirties the formula cells that read it. Reverse edges
/// (dependents) and the per-sheet range index are maintained incrementally as
/// dependencies are set and removed.
///
/// # Typical use
/// ```
/// use xl_graph::{CalcSettings, CellId, DepGraph, Precedent, Step};
/// use xl_value::SheetId;
///
/// let s = SheetId(0);
/// let a1 = CellId::new(s, 0, 0);
/// let b1 = CellId::new(s, 0, 1);
/// let c1 = CellId::new(s, 0, 2);
///
/// let mut g = DepGraph::new();
/// // B1 = A1 + 1 ;  C1 = B1 * 2
/// g.set_deps(b1, &[Precedent::Cell(a1)]);
/// g.set_deps(c1, &[Precedent::Cell(b1)]);
///
/// // Edit A1 -> only B1 and C1 need recomputing, B1 before C1.
/// g.mark_dirty(&[a1]);
/// let plan = g.recalc_plan(CalcSettings::default());
/// assert_eq!(plan.steps, vec![Step::Eval(b1), Step::Eval(c1)]);
/// ```
#[derive(Clone, Debug, Default)]
pub struct DepGraph {
    /// Formula cells and their precedents.
    nodes: BTreeMap<CellId, Node>,
    /// Reverse edges: `dependents[p]` = cells that directly name `p` as a
    /// single-cell precedent. Kept for cells that are not (yet) nodes too, so an
    /// edit to a plain input cell finds its formula dependents.
    dependents: BTreeMap<CellId, BTreeSet<CellId>>,
    /// Per-sheet range index for range precedents.
    ranges: BTreeMap<xl_value::SheetId, SheetRanges>,
    /// Registered volatile cells (always dirty at recalc start).
    volatile: BTreeSet<CellId>,
    /// Cells whose precedents may change after evaluation (`OFFSET`/`INDIRECT`).
    dynamic: BTreeSet<CellId>,
    /// Accumulated transitively-dirty nodes awaiting an incremental plan.
    dirty: BTreeSet<CellId>,
}

impl DepGraph {
    /// Create an empty graph.
    #[must_use]
    pub fn new() -> DepGraph {
        DepGraph::default()
    }

    // ----- graph build / maintenance -------------------------------------

    /// Set (replacing any previous set) the precedents of formula cell `cell`.
    ///
    /// Registers `cell` as a node, rewires its outgoing edges (reverse edges and
    /// range-index entries), and leaves its volatile/dynamic flags untouched.
    /// Duplicate precedents in the slice are coalesced. Idempotent for a given
    /// precedent set.
    ///
    /// # Mid-recalc contract (dynamic dependencies)
    /// This may be called **during** plan execution — the intended path for
    /// [`set_dynamic_deps`](DepGraph::set_dynamic_deps) cells (`OFFSET`,
    /// `INDIRECT`) whose true precedents are only known after evaluation. The
    /// call leaves the graph fully consistent (edges, reverse edges, and range
    /// index updated together). It does **not** retroactively reorder the plan
    /// already being executed: if evaluation reveals a new precedent that has
    /// not yet been computed this pass, the engine is responsible for
    /// re-marking (`mark_dirty`) the affected cells and requesting a fresh
    /// [`recalc_plan`](DepGraph::recalc_plan) for the remainder. The graph
    /// guarantees consistency; ordering across a dependency discovered mid-pass
    /// is the engine's to resolve.
    pub fn set_deps(&mut self, cell: CellId, precedents: &[Precedent]) {
        self.detach_precedents(cell);

        let mut node = Node::default();
        for p in precedents {
            match *p {
                Precedent::Cell(c) => {
                    node.cell_precedents.insert(c);
                }
                Precedent::Range(r) => {
                    node.range_precedents.insert(r);
                }
            }
        }
        // Install reverse edges / range entries for the new precedent set.
        for &p in &node.cell_precedents {
            self.dependents.entry(p).or_default().insert(cell);
        }
        for r in &node.range_precedents {
            self.ranges
                .entry(r.sheet)
                .or_default()
                .insert(r.range, cell);
        }
        self.nodes.insert(cell, node);
    }

    /// Remove `cell` as a formula node.
    ///
    /// Drops its outgoing edges (it no longer reads anything) and its
    /// volatile/dynamic/dirty membership. Reverse edges *into* `cell` are kept:
    /// cells that referenced `cell` still depend on it (it becomes a blank input
    /// cell, exactly as deleting a formula in Excel leaves the cell blank and
    /// recomputes its dependents). Removing a cell that is not a node is a
    /// no-op for the node table but still clears any flags.
    pub fn remove_node(&mut self, cell: CellId) {
        self.detach_precedents(cell);
        self.nodes.remove(&cell);
        self.volatile.remove(&cell);
        self.dynamic.remove(&cell);
        self.dirty.remove(&cell);
    }

    /// Tear down `cell`'s outgoing edges (reverse edges + range entries) for its
    /// current precedent set, if it is a node. Leaves the node record itself in
    /// place for the caller to replace or remove.
    fn detach_precedents(&mut self, cell: CellId) {
        if let Some(node) = self.nodes.get(&cell) {
            for p in &node.cell_precedents {
                if let Some(set) = self.dependents.get_mut(p) {
                    set.remove(&cell);
                    if set.is_empty() {
                        self.dependents.remove(p);
                    }
                }
            }
            let sheets: Vec<xl_value::SheetId> =
                node.range_precedents.iter().map(|r| r.sheet).collect();
            for sheet in sheets {
                if let Some(sr) = self.ranges.get_mut(&sheet) {
                    sr.remove_dependent(cell);
                    if sr.is_empty() {
                        self.ranges.remove(&sheet);
                    }
                }
            }
        }
    }

    // ----- volatility & dynamic deps -------------------------------------

    /// Register (or clear) `cell` as volatile.
    ///
    /// Volatile cells — those calling `NOW`, `TODAY`, `RAND`/`RANDBETWEEN`,
    /// `OFFSET`, `INDIRECT`, `CELL`, `INFO` (`implementation-plan.md` §2) —
    /// recompute on *every* recalculation. [`mark_volatile_dirty`] seeds them
    /// into the dirty set. Whether a given function is volatile is `xl-fn`'s
    /// declaration; the graph only records the outcome.
    ///
    /// [`mark_volatile_dirty`]: DepGraph::mark_volatile_dirty
    pub fn register_volatile(&mut self, cell: CellId, volatile: bool) {
        if volatile {
            self.volatile.insert(cell);
        } else {
            self.volatile.remove(&cell);
        }
    }

    /// Flag (or clear) `cell` as having dynamic dependencies.
    ///
    /// A dynamic-dependency cell's precedents may change after it is evaluated
    /// (`OFFSET`/`INDIRECT` compute their targets at run time). The flag is
    /// advisory metadata for the engine: after evaluating such a cell it should
    /// re-report the discovered precedents via [`set_deps`](DepGraph::set_deps).
    /// See the mid-recalc contract on `set_deps`.
    pub fn set_dynamic_deps(&mut self, cell: CellId, dynamic: bool) {
        if dynamic {
            self.dynamic.insert(cell);
        } else {
            self.dynamic.remove(&cell);
        }
    }

    // ----- dirty marking -------------------------------------------------

    /// Mark `cells` edited: they and all their transitive dependents become
    /// dirty (accumulated for the next [`recalc_plan`](DepGraph::recalc_plan)).
    ///
    /// Propagation is an iterative forward traversal over reverse edges and the
    /// range index. Seeds that are themselves nodes are included; seeds that are
    /// plain input cells are not scheduled but their dependents are. The dirty
    /// set is always closed under "is a dependent of" — the invariant that makes
    /// an incremental plan a valid sub-schedule of the full plan.
    pub fn mark_dirty(&mut self, cells: &[CellId]) {
        let mut stack: Vec<CellId> = Vec::with_capacity(cells.len());
        for &c in cells {
            if self.nodes.contains_key(&c) {
                self.dirty.insert(c);
            }
            stack.push(c);
        }
        self.propagate(stack);
    }

    /// Mark every registered volatile cell (and its dependents) dirty.
    ///
    /// The engine calls this at the start of each recalculation so volatile
    /// cells always recompute (`implementation-plan.md` §2).
    pub fn mark_volatile_dirty(&mut self) {
        let seeds: Vec<CellId> = self.volatile.iter().copied().collect();
        for &c in &seeds {
            if self.nodes.contains_key(&c) {
                self.dirty.insert(c);
            }
        }
        self.propagate(seeds);
    }

    /// Mark **every** node dirty (equivalent to requesting a full recalc through
    /// the incremental path).
    pub fn mark_all_dirty(&mut self) {
        self.dirty = self.nodes.keys().copied().collect();
    }

    /// Discard the accumulated dirty set without producing a plan.
    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    /// Iterative dirty propagation from `seeds` over reverse edges + ranges.
    fn propagate(&mut self, mut stack: Vec<CellId>) {
        let mut neighbours: Vec<CellId> = Vec::new();
        while let Some(c) = stack.pop() {
            neighbours.clear();
            if let Some(deps) = self.dependents.get(&c) {
                neighbours.extend(deps.iter().copied());
            }
            if let Some(sr) = self.ranges.get_mut(&c.sheet) {
                sr.query(c.row, c.col, &mut neighbours);
            }
            for d in neighbours.drain(..) {
                if self.dirty.insert(d) {
                    stack.push(d);
                }
            }
        }
    }

    // ----- plans ---------------------------------------------------------

    /// Produce a full-recalc plan: a topological order over the whole graph,
    /// with cycles grouped per `settings`. Independent of the dirty set.
    #[must_use]
    pub fn full_plan(&self, settings: CalcSettings) -> Plan {
        schedule::build_full_plan(self, settings)
    }

    /// Produce an incremental plan: the canonical full order restricted to the
    /// accumulated transitively-dirty cells, with cycles grouped per
    /// `settings`.
    ///
    /// # Restriction guarantee
    /// The returned plan is **literally a filter** of
    /// [`full_plan`](DepGraph::full_plan)`(settings)`: the canonical full order
    /// is computed and every step whose cell (or cycle group) is not dirty is
    /// dropped. It is therefore an order-preserving sub-sequence of the full
    /// plan — two cells scheduled by both plans are always in the same relative
    /// order. This is load-bearing for seeded determinism
    /// (`implementation-plan.md` §5 M2): with a seeded `RAND`, cross-cell
    /// evaluation order determines the draw sequence, so an edit-recalc and a
    /// full rebuild must not reorder shared cells or seeded workbooks would
    /// diverge between the two paths. (Re-running the topological sort on the
    /// dirty *subgraph* would violate this: the `CellId` tie-break is
    /// scope-dependent — clean nodes absent from the subgraph change when
    /// components become ready.)
    ///
    /// A cycle group is kept when any member is dirty; members of a circular
    /// group are mutually dependent, so the dirty closure normally contains
    /// all of them or none.
    ///
    /// The plan schedules only cells that need recomputing (every `Eval` is
    /// dirty). It does **not** clear the dirty set — call
    /// [`clear_dirty`](DepGraph::clear_dirty) once the plan has been executed,
    /// or use [`take_recalc_plan`](DepGraph::take_recalc_plan), which does both
    /// atomically (forgetting to clear re-schedules already-executed cells,
    /// double-drawing seeded `RAND`s on the next recalc).
    ///
    /// # Complexity
    /// `O(V + E)` — the full canonical order is rebuilt and filtered, not
    /// `O(dirty)`. At the 100k-formula scale target this is well within budget
    /// (see the scale tests).
    // TODO(perf): maintain a cached canonical topological numbering
    // (invalidated on structural `set_deps`/`remove_node`, or an
    // order-maintenance structure) so incremental planning costs
    // O(dirty · log dirty) instead of O(V + E), per the <100ms p95
    // incremental-edit target in `implementation-plan.md` §8.
    #[must_use]
    pub fn recalc_plan(&self, settings: CalcSettings) -> Plan {
        let full = self.full_plan(settings);
        let steps = full
            .steps
            .into_iter()
            .filter(|s| match s {
                Step::Eval(c) => self.dirty.contains(c),
                Step::Cycle(g) => g.members.iter().any(|m| self.dirty.contains(m)),
                Step::Iterate(g) => g.members.iter().any(|m| self.dirty.contains(m)),
            })
            .collect();
        Plan { steps }
    }

    /// Compute the incremental plan **and clear the dirty set** in one call —
    /// the intended engine entry point for an edit-recalc.
    ///
    /// Equivalent to [`recalc_plan`](DepGraph::recalc_plan) followed by
    /// [`clear_dirty`](DepGraph::clear_dirty). The split API exists for
    /// callers that want to inspect a plan without committing to executing it,
    /// but it makes forgetting `clear_dirty` possible — and re-executing an
    /// already-executed plan on the next recalc double-draws seeded `RAND`
    /// cells, diverging from a single Excel recalculation (seeded determinism,
    /// `implementation-plan.md` §5 M2). Engine usage:
    ///
    /// ```
    /// # use xl_graph::{CalcSettings, CellId, DepGraph, Precedent};
    /// # use xl_value::SheetId;
    /// # let mut graph = DepGraph::new();
    /// # let edited = CellId::new(SheetId(0), 0, 0);
    /// graph.mark_volatile_dirty();
    /// graph.mark_dirty(&[edited]);
    /// let plan = graph.take_recalc_plan(CalcSettings::default());
    /// // execute `plan`; the dirty set is already clear for the next edit
    /// # assert_eq!(graph.dirty_len(), 0);
    /// ```
    #[must_use]
    pub fn take_recalc_plan(&mut self, settings: CalcSettings) -> Plan {
        let plan = self.recalc_plan(settings);
        self.dirty.clear();
        plan
    }

    /// Antichain **wave index** for each step of `plan`, parallel to
    /// `plan.steps` (RFC-0014).
    ///
    /// `wave[i]` is `0` when `plan.steps[i]` has no precedent *scheduled inside
    /// `plan`*; otherwise `1 + max(wave[precedent step])`. Cells sharing a wave
    /// are mutually independent (safe to evaluate concurrently); processing
    /// waves in ascending order respects every dependency edge.
    ///
    /// This is a **pure, additive read** over an existing plan — it does not
    /// change the canonical order [`full_plan`](DepGraph::full_plan) produced,
    /// and the default (serial) engine never calls it. The precedent scan uses
    /// the same cell + range edge set as the plan builder (range precedents
    /// expanded through the range index), so a reader read via a range is
    /// levelled correctly — unlike [`direct_dependents`](DepGraph::direct_dependents),
    /// which by design carries no per-cell reverse edge for ranges.
    ///
    /// Deterministic in `(self, plan)`: same graph + same plan → same vector.
    /// `O(V + E)` over the plan's cells.
    #[must_use]
    pub fn waves(&self, plan: &Plan) -> Vec<u32> {
        schedule::plan_waves(self, plan)
    }

    // ----- accessors -----------------------------------------------------

    /// Number of formula nodes.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Whether `cell` is a formula node.
    #[must_use]
    pub fn is_node(&self, cell: CellId) -> bool {
        self.nodes.contains_key(&cell)
    }

    /// Whether `cell` is currently marked dirty.
    #[must_use]
    pub fn is_dirty(&self, cell: CellId) -> bool {
        self.dirty.contains(&cell)
    }

    /// Number of cells currently marked dirty.
    #[must_use]
    pub fn dirty_len(&self) -> usize {
        self.dirty.len()
    }

    /// Whether `cell` is registered volatile.
    #[must_use]
    pub fn is_volatile(&self, cell: CellId) -> bool {
        self.volatile.contains(&cell)
    }

    /// Whether `cell` is flagged as having dynamic dependencies.
    #[must_use]
    pub fn is_dynamic(&self, cell: CellId) -> bool {
        self.dynamic.contains(&cell)
    }

    /// The cells that directly name `cell` as a single-cell precedent, sorted.
    ///
    /// Range dependents are resolved through the range index and are not
    /// included here (they have no per-cell reverse edge by design).
    #[must_use]
    pub fn direct_dependents(&self, cell: CellId) -> Vec<CellId> {
        self.dependents
            .get(&cell)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    // ----- crate-internal helpers used by `schedule` --------------------

    /// Borrow a node's stored precedents.
    pub(crate) fn node(&self, cell: CellId) -> Option<&Node> {
        self.nodes.get(&cell)
    }

    /// All formula-node ids in `CellId` order.
    pub(crate) fn node_ids(&self) -> impl Iterator<Item = CellId> + '_ {
        self.nodes.keys().copied()
    }

    /// Iterate the **nodes** whose address falls inside `range`.
    ///
    /// Uses a `BTreeMap` range scan over the row band `[row_start, row_end]`
    /// then filters columns — `CellId` orders as `(sheet, row, col)`, so the
    /// scan is contiguous. Cost is `O(band + output)`; this is how a range
    /// precedent is expanded into topological edges (only over cells that are
    /// themselves formula nodes — plain inputs need no ordering).
    ///
    /// A **degenerate** range (`start > end` on either axis) contains nothing
    /// and yields nothing, mirroring [`SheetRange::contains`]. This must be
    /// short-circuited *before* the scan: `BTreeMap::range` panics on an
    /// inverted bound, and reversed refs (Excel accepts `A10:A1`, normalizing
    /// internally) reach here un-normalized from unvetted workbooks — a panic
    /// would be a DoS surface for the batch server.
    pub(crate) fn nodes_in_range(&self, range: &SheetRange) -> impl Iterator<Item = CellId> + '_ {
        let r = range.range;
        let proper = r.row_start <= r.row_end && r.col_start <= r.col_end;
        proper
            .then(|| {
                let lo = CellId {
                    sheet: range.sheet,
                    row: r.row_start,
                    col: 0,
                };
                let hi = CellId {
                    sheet: range.sheet,
                    row: r.row_end,
                    col: u32::MAX,
                };
                self.nodes
                    .range(lo..=hi)
                    .map(|(k, _)| *k)
                    .filter(move |c| c.col >= r.col_start && c.col <= r.col_end)
            })
            .into_iter()
            .flatten()
    }
}

#[cfg(test)]
mod tests;

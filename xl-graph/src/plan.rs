//! The recalc [`Plan`] the graph hands to the engine, and the
//! iterative-calculation [`CalcSettings`] that shape cycle handling.
//!
//! # Provenance
//! The plan shape is structural. The iterative-calc defaults (100 iterations,
//! 0.001 maximum change) are Excel semantics, taken from Microsoft's "Change
//! formula recalculation, iteration, or precision" documentation and the
//! `<calcPr iterateCount="100" iterateDelta="0.001">` OOXML defaults
//! (ISO/IEC 29500 / `implementation-plan.md` §2, "iterative mode
//! (100 iters / 0.001 default)").
//!
//! ## Circular-reference value (iteration off) — OXP-070
//! What *value* a cell in a circular reference takes when iteration is **off**
//! was probed by `OXP-070` (`RUN-2026-07-11-oracle01`): a 2-cycle
//! (`A1=B1+1`, `B1=A1+1`), a self-loop (`C1=C1`), and a text-shaped member
//! (`D1=IF(TRUE,D1,"x")&"t"`) all dumped as the **empty text string** (an
//! empty value, `value_type=text`) — uniformly, regardless of whether the
//! formula is numeric- or text-shaped. The graph only detects and groups the
//! cycle; assigning the members' value is the engine's job (see
//! [`Step::Cycle`]).
//!
//! **Current engine behavior:** the engine assigns `#UNSUPPORTED!` (loud, with
//! a `CircularReference` diagnostic) to every cycle member — the
//! never-silently-wrong default. Adopting OXP-070's pinned empty-text *dumped*
//! value is a tracked follow-up, **not** yet implemented: the probe pinned the
//! members' own cached value, but how a **dependent** cell observes a broken
//! cycle (Excel treats an uncomputed precedent as `0` during calc, distinct
//! from the members' cached `""`) is a separate, still-unpinned question, so
//! flipping the member value without that second pin would be a guess. The
//! intra-group *evaluation order* under iteration **on** was likewise not
//! dumped by this run and stays deferred — see [`CycleGroup`].

use crate::cell::CellId;

/// Workbook calculation settings that affect how the graph schedules cycles.
///
/// Mirrors Excel's iterative-calculation options. Defaults match Excel's:
/// iteration **off**, and when enabled, 100 iterations / 0.001 maximum change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CalcSettings {
    /// Whether iterative calculation is enabled (Excel: *Enable iterative
    /// calculation*). When `false`, cycles are scheduled as [`Step::Cycle`];
    /// when `true`, as [`Step::Iterate`].
    pub iterate: bool,
    /// Maximum iterations for a cycle group (Excel default 100).
    pub max_iters: u32,
    /// Maximum change below which iteration stops early (Excel default 0.001).
    pub max_change: f64,
}

impl Default for CalcSettings {
    fn default() -> CalcSettings {
        CalcSettings {
            iterate: false,
            max_iters: 100,
            max_change: 0.001,
        }
    }
}

/// A group of cells that form a strongly-connected component (a cycle) in the
/// scheduled subgraph — a circular reference.
///
/// `members` are the cells in the cycle, sorted by [`CellId`] order. This is a
/// **deterministic** membership list, but the order in which Excel *evaluates*
/// cells inside a circular group (when iteration is on) is Excel-observable and
/// **not** pinned down here — the graph provides sorted membership and lets the
/// engine decide values. `OXP-070`'s `RUN-2026-07-11-oracle01` pinned the
/// iteration-**off** value (empty text; see [`Step::Cycle`]) but did not dump
/// the iteration-**on** intra-group order, so that part stays deferred.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CycleGroup {
    /// Cells in the cycle, sorted by [`CellId`].
    pub members: Vec<CellId>,
}

/// A cycle scheduled under **iterative** calculation: the engine loops over
/// `members` up to `settings.max_iters` times (or until every member changes by
/// less than `settings.max_change`).
#[derive(Clone, Debug, PartialEq)]
pub struct IterativeGroup {
    /// Cells in the cycle, sorted by [`CellId`]. The intra-group order is
    /// deterministic; whether it matches Excel's own within-cycle evaluation
    /// order is oracle territory (`OXP-070`).
    pub members: Vec<CellId>,
    /// The iteration settings in force.
    pub settings: CalcSettings,
}

/// One step of a recalc plan, in the order the engine must execute it.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Evaluate a single cell. All of its scheduled precedents appear in
    /// earlier steps.
    Eval(CellId),
    /// A circular reference with iteration **off**. The engine assigns the
    /// circular-reference result. OXP-070 (`RUN-2026-07-11-oracle01`) dumped
    /// that result as the **empty text string** for every probed member
    /// (numeric- and text-shaped alike), even though the Excel UI displays `0`
    /// — but the engine currently assigns `#UNSUPPORTED!` (loud) instead;
    /// adopting the pinned empty-text value is a tracked follow-up (see the
    /// module-level "Circular-reference value" note). Includes self-loops (a
    /// 1-cell cycle).
    Cycle(CycleGroup),
    /// A circular reference with iteration **on**. Iterative *convergence* is
    /// not yet implemented, so the engine assigns `#UNSUPPORTED!` to every
    /// member (it does **not** loop the group); this variant reserves the shape
    /// for that later work. OXP-070 pinned only the iteration-**off** value, not
    /// the intra-group evaluation order iteration-on would need.
    Iterate(IterativeGroup),
}

/// A complete recalc plan: ordered steps that respect every dependency edge
/// (each precedent is evaluated before its dependents; cycle members are
/// grouped into one step).
///
/// Consumed by the engine in `steps` order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// Steps in execution order.
    pub steps: Vec<Step>,
}

impl Plan {
    /// Number of steps (each cycle group counts as one step).
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether the plan is empty (nothing to recalculate).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Every cell scheduled by the plan, in step order (cycle members expanded
    /// in their sorted intra-group order). Handy for tests and for engines that
    /// want a flat cell list.
    pub fn cells(&self) -> impl Iterator<Item = CellId> + '_ {
        self.steps.iter().flat_map(|s| match s {
            Step::Eval(c) => std::slice::from_ref(c).iter().copied(),
            Step::Cycle(g) => g.members.as_slice().iter().copied(),
            Step::Iterate(g) => g.members.as_slice().iter().copied(),
        })
    }
}

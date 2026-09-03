//! `xl-engine` — Recalc orchestration.
//!
//! The top-level driver that wires `xl-io` (loading), `xl-ast` (parsing),
//! `xl-value` (the value contract), `xl-graph` (dependency/recalc scheduling),
//! and `xl-fn` (function evaluation) into a single recalc pipeline. This is the
//! v0 "minimal evaluator": it makes the io→ast→graph→eval path end-to-end for
//! `SUM`, `IF`, and the scalar operators so the Task-10 diff harness has
//! something to measure.
//!
//! # Pipeline
//! 1. [`Engine::load`] parses every formula cell (A1 mode), records a diagnostic
//!    (never a panic) for any parse error, extracts precedents into a
//!    [`xl_graph::DepGraph`], and marks volatile cells.
//! 2. [`Engine::recalc`] takes the graph's full plan and evaluates it step by
//!    step; [`Engine::edit`] rewrites one cell and recomputes only the affected
//!    cells via the graph's incremental plan.
//! 3. [`Engine::value`] reads a computed cell; [`Engine::diagnostics`] lists
//!    everything the engine refused to compute.
//!
//! # Determinism (a product feature)
//! Evaluation is **single-threaded, in plan order**. Every ordered structure is
//! a `BTreeMap`/`Vec` in `CellId` order — no `HashMap` iteration influences a
//! result. Two engines loaded from the same workbook produce identical values,
//! and `recalc` is idempotent. Parallel recalc with a stable reduction order is
//! a later task (`implementation-plan.md` §2) — the single-threaded order is the
//! reference that parallel evaluation must reproduce bit-for-bit.
//!
//! # Never silently wrong
//! Any construct the engine cannot faithfully compute — an unsupported function,
//! a parse error, a circular reference, a static-range implicit intersection, a
//! 3-D reference — becomes [`xl_value::ErrorKind::Unsupported`] with a
//! diagnostic, never a guessed value (the project's scope rules §0).
//!
//! # Dynamic-array spill (M2 lane 4, compute-only)
//! A top-level [`Value::Array`] result **spills** into neighbouring cells
//! ([`Engine::write_cell_result`]); an obstruction yields `#SPILL!`; a
//! lambda-valued element yields `#VALUE!` across the spill (OXP-203); the
//! spilled-range operator `A1#` / `_xlfn.ANCHORARRAY(A1)` resolves through the
//! reference seam ([`refx::resolve_ref_expr`]); `@` implicit-intersects a
//! computed array to its top-left (OXP-201). Because a spill's eval-time
//! footprint is invisible to the static graph, a **fixpoint planner**
//! ([`Engine::drive_recalc`], RFC-0012 §3 / BC-1 protocol B) re-dirties the
//! readers of grown/shrunk/re-valued spilled cells until stable; non-convergence
//! (a spill-dependency cycle / two-anchor race, BC-5) is a loud refusal (BC-4b),
//! never stale values. Determinism (BTree order, order-preserving filtered
//! passes) is preserved (BC-4c/§5). **Compute-only**: spills are *not* written
//! back to the `.xlsx` (that is `BLOCKED-PENDING-USER`; see
//! `docs/plans/2026-07-15-spill-writer-spec.md`). Provenance: RFC-0012 (spine),
//! OXP-201/202/203/204, disposable evidence `spike/spill-sequence`.

#![forbid(unsafe_code)]

mod analyze;
mod eval;
mod lambda;
mod refx;
mod shared;

use std::collections::{BTreeMap, BTreeSet};

use xl_ast::Expr;
use xl_fn::{DateSystem, EvalContext};
use xl_graph::{CalcSettings, DepGraph, Step};
use xl_io::{FormulaKind, Workbook};
use xl_value::{Array, ErrorKind, RectRange};

use analyze::{
    NameScope, ScopedName, WbIndex, collect_precedents, contains_volatile, head_is_subtotal,
};
use eval::{ValueStore, eval_expr};

// M2 lane 9 / RFC-0014 §6 (the `wasm-bindgen` dependency policy, condition 4): the parallel
// recalc feature must NEVER be enabled on wasm32. rayon threads are unavailable
// there, and the single-threaded / no-network wasm guarantee forbids it. This
// is the compile-time half of the promise; the CI `test` job additionally
// asserts (via `cargo tree`) that the wasm target's dependency tree carries no
// `rayon` — see the "Assert the wasm build has no rayon" step in ci.yml.
#[cfg(all(feature = "parallel", target_family = "wasm"))]
compile_error!(
    "the `parallel` feature must never be enabled on a wasm target (RFC-0014 §6; \
     the Recalc design rules wasm-bindgen condition 4): rayon threads are unavailable and the \
     single-threaded, no-network wasm build forbids it"
);

// Re-export the identity/value types callers need to drive the engine
// (`value`, `edit`, `value_at`), so a consumer need not also depend on
// `xl-graph`/`xl-value` just to name a cell.
pub use xl_graph::CellId;
pub use xl_value::{SheetId, Value};

/// A formula cell's compiled program: either a parsed AST or the reason it
/// could not be parsed (surfaced as `#UNSUPPORTED!` with a diagnostic on every
/// recalc).
/// Load-level mirror of Excel's open-time `LET` validation (OXP-200: a
/// duplicate `LET` parameter — `LET(x,1,x,2,x)` — load-rejects the workbook,
/// it is **not** a computed error). Returns the refusing [`CellProgram`] if
/// `expr` contains any `LET` call with a duplicate parameter, else `None`.
/// Every path that compiles an [`Expr`] into a runnable program (workbook
/// load, shared-formula expansion, programmatic edit) consults this so the
/// duplicate never reaches evaluation as a silently-shadowed binding.
fn duplicate_let_rejection(expr: &Expr) -> Option<CellProgram> {
    lambda::find_duplicate_let_param(expr).map(|dup| {
        CellProgram::Unparsed(
            DiagnosticKind::ParseError,
            format!(
                "duplicate LET parameter `{dup}`: Excel load-rejects a workbook \
                 whose LET re-binds a name (validated at open, OXP-200); \
                 refusing the cell at load"
            ),
        )
    })
}

enum CellProgram {
    /// Successfully parsed. The `bool` is the cell's **array-entry** status —
    /// `true` for a legacy CSE array formula (`<f t="array">`,
    /// [`xl_io::RawFormula::is_array_entered`]), `false` for an ordinary
    /// (including `t="shared"`) formula. It selects the evaluator's array vs
    /// scalar context: a non-array formula does legacy implicit intersection on
    /// a range reaching scalar context, an array-entered one does not
    /// (OXP-004/163). Threaded to `eval` via `WbIndex::is_array_formula`.
    Parsed(Expr, bool),
    /// Could not be compiled (a parse error, an orphan shared follow-on whose
    /// `si` master is missing, or an out-of-scope array/dataTable follow-on with
    /// no body); the [`DiagnosticKind`] classifies which, and the string is the
    /// human-readable message. A **resolvable** shared follow-on is expanded from
    /// its master and stored as [`CellProgram::Parsed`] instead (ECMA-376
    /// §18.17.2; see [`shared`]).
    Unparsed(DiagnosticKind, String),
}

/// A machine-readable classification of a [`Diagnostic`], so consumers can
/// branch on *why* a cell was refused without substring-matching the message.
///
/// The message remains the human-readable detail; the kind is the stable,
/// matchable category. New categories may be added as the engine grows, so
/// consumers should treat this as non-exhaustive in spirit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiagnosticKind {
    /// The formula text could not be parsed by `xl-ast`.
    ParseError,
    /// A called function is not in the registry (`xl-fn::lookup` miss).
    UnknownFunction,
    /// A call supplied an argument count outside the function's declared arity.
    ArityError,
    /// A construct the engine refuses to evaluate rather than guess: an
    /// unsupported reference (3-D, R1C1, whole-col/row in scalar context), a
    /// non-simple defined name, implicit intersection/union, a multi-cell range
    /// or multi-element array in scalar context, a shared/array follow-on cell,
    /// or any `xl-ast` `Unsupported` node.
    UnsupportedConstruct,
    /// The cell participates in a circular reference (non-iterative, or an
    /// iterative group pending the iterative-calc task).
    CircularReference,
}

/// A record of something the engine refused to compute, keyed by the cell it
/// happened in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// The cell whose evaluation produced the diagnostic.
    pub cell: CellId,
    /// The machine-readable category (branch on this, not on [`message`](Diagnostic::message)).
    pub kind: DiagnosticKind,
    /// A human-readable explanation (often naming the oracle probe that would
    /// resolve it).
    pub message: String,
}

/// The sink `eval` pushes `(kind, message)` pairs into while evaluating one
/// cell; [`Engine::run_cell`] stamps each with the cell to form a [`Diagnostic`].
/// All of a cell's diagnostics are retained (a cell can refuse more than one
/// construct — e.g. `BAD1()+BAD2()`).
pub(crate) type DiagnosticSink = Vec<(DiagnosticKind, String)>;

/// The result of a recalculation pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecalcResult {
    /// Number of cells (re)evaluated this pass — for a full recalc, every
    /// formula node; for an incremental one, only the dirty closure.
    pub evaluated: usize,
    /// Number of diagnostics currently recorded across the workbook.
    pub diagnostics: usize,
}

/// The new contents to assign to a cell in [`Engine::edit`].
pub enum CellInput {
    /// A formula (with or without a leading `=`), reparsed and re-wired into the
    /// dependency graph.
    Formula(String),
    /// A literal value; the cell becomes a plain input (no longer a formula
    /// node), and its dependents recompute.
    Literal(Value),
}

/// A loaded workbook with its dependency graph, ready to recalc and edit.
pub struct Engine {
    /// ASCII-lowercased sheet name → id (0-based tab index).
    sheet_ids: BTreeMap<String, SheetId>,
    /// Original-case sheet display names in tab order (index == `SheetId`).
    /// Kept alongside `sheet_ids` because that map lowercases its keys for
    /// case-insensitive lookup and so cannot reproduce the display casing;
    /// [`Engine::sheet_names`] returns a clone of this.
    sheet_names: Vec<String>,
    /// Workbook defined names, scope-translated at load (ECMA-376 §18.2.6:
    /// `localSheetId` collection indices → engine [`SheetId`]s); bodies stay
    /// raw and are resolved on demand.
    defined_names: Vec<ScopedName>,
    /// Compiled formula programs, one per formula cell.
    programs: BTreeMap<CellId, CellProgram>,
    /// Current computed/loaded cell values.
    values: ValueStore,
    /// Cells whose own formula head is a `SUBTOTAL` call (RFC 0002): the
    /// nested-exclusion tag set, consulted by the evaluator's provenance-tagged
    /// cell walk so a `SUBTOTAL` over these cells does not double-count them.
    /// Maintained alongside `programs` on load and every edit.
    subtotals: BTreeSet<CellId>,
    /// The `(sheet, 0-based row)` pairs Excel is not displaying — the OOXML
    /// `<row hidden="1">` rows `xl-io` parsed (OXP-121). Consulted by the same
    /// provenance-tagged cell walk so `SUBTOTAL`'s `101`–`111` forms can drop
    /// hidden-row cells. Sourced once from the workbook at load: a row's hidden
    /// state is a static property of the file, not something a formula edit
    /// changes (v1 does not add/remove hidden rows — see `implementation-plan.md`
    /// §1 non-goals), so unlike `subtotals` it is not maintained per edit.
    hidden_rows: BTreeSet<(SheetId, u32)>,
    /// The dependency graph.
    graph: DepGraph,
    /// Graph calc settings (iterative-calc options), derived from the workbook.
    calc: CalcSettings,
    /// Diagnostics, keyed by cell for deterministic, per-cell replacement. All
    /// diagnostics a cell produced are kept (a cell may refuse several
    /// constructs), in the order `eval` emitted them.
    diagnostics: BTreeMap<CellId, Vec<Diagnostic>>,
    /// The sandbox authority seam handed to every function evaluation.
    ctx: EvalContext,
    /// Monotonic count of cell evaluations, for observing incremental recalc.
    eval_count: u64,
    /// Cells evaluated by the most recent recalc/edit, in plan order (a
    /// fixpoint re-evaluation appends a cell again — see [`Engine::execute`]).
    last_recalc_cells: Vec<CellId>,
    /// **M2 lane 4 (dynamic-array spill).** Each spill anchor → the rectangle it
    /// currently spills into (on the anchor's own sheet, always containing the
    /// anchor as its top-left). A **1×1** dynamic-array result IS registered here
    /// (OXP-204/RFC-0012 BC-10). This is the write-side spill footprint the static
    /// graph cannot know; it is materialized state derived from evaluation (like
    /// the value store), so it lives on the engine, not the graph (RFC-0012 §1 —
    /// the graph stays a pure structural index and gets only the dynamic-deps
    /// signal). Consulted by the evaluator via [`analyze::WbIndex::spills`] to
    /// resolve `A1#`; maintained by [`Engine::write_cell_result`] on every
    /// (re)eval.
    spills: BTreeMap<CellId, RectRange>,
    /// **M2 lane 4.** Reverse index: each spilled-into cell (including the anchor)
    /// → its owning anchor (RFC-0012 §1 hardening). Lets the obstruction check
    /// distinguish "a blank/value **I** already own, reclaimable" from "a foreign
    /// value → `#SPILL!`" in O(log n) without rescanning the old rectangle, and
    /// keeps a full recalc idempotent (an anchor re-spilling over its own prior
    /// region is not self-obstructed).
    spill_owner: BTreeMap<CellId, CellId>,
    /// **M2 lane 4 — fixpoint scratch.** Cells whose spilled **value** changed
    /// during the pass currently executing: a grow writes an element into a
    /// previously-blank slot, a shrink blanks a vacated slot, and a same-shape
    /// re-spill overwrites with different contents — the symmetric difference of
    /// old vs new footprints by value. (A *pure* ownership flip with no value
    /// change cannot occur: after the B1 obstruction fix a foreign-owned slot
    /// blocks the spill, so an anchor never quietly re-owns another's cell.)
    /// After each pass the fixpoint planner ([`Engine::drive_recalc`]) re-dirties
    /// these so a formula reading a spilled cell recomputes — the RFC-0012 §3 /
    /// BC-1 protocol B "grow/shrink dirtying" that closes the dirty-set-closure
    /// gap the spike flagged. Drained between passes.
    spill_redirty: BTreeSet<CellId>,
    /// **M2 lane 4 — fixpoint scratch.** Anchors whose spill footprint or content
    /// changed during the pass currently executing. Used only to name the
    /// still-unstable anchors when the fixpoint fails to converge within
    /// [`SPILL_FIXPOINT_CAP`] (BC-4b loud refusal). Drained between passes.
    spill_changed_anchors: BTreeSet<CellId>,
    /// **M2 lane 4 — fixpoint refusal set.** Anchors refused for non-convergence
    /// (BC-4b) during the recalc currently in flight. Cleared at
    /// [`Engine::drive_recalc`] entry. Once an anchor is here, [`Engine::run_cell`]
    /// **skips** re-evaluating it for the rest of the recalc — re-spilling it
    /// would restart the very oscillation the refusal broke — so its
    /// `#UNSUPPORTED!` value stays put while the readers of the cells it vacated
    /// are reconciled to non-stale values (review B2: a refusal must not strand
    /// stale readers). A refused anchor cannot re-enter [`spill_changed_anchors`]
    /// (its `spills` entry is gone, so `retract_spill` no-ops and `run_cell`
    /// never re-spills it), which bounds the number of refusals to the anchor
    /// count and makes the fixpoint terminate.
    refused_spills: BTreeSet<CellId>,
}

/// **M2 lane 4 (BC-4a).** Structural cap on spill-planning fixpoint iterations.
///
/// **Decoupled** from the workbook's iterative-calc setting: a workbook with
/// iteration OFF must still converge its spill footprints, and `iterations = 1`
/// must not truncate spill planning. A legitimate spill-anchor dependency chain
/// (anchor A's size feeds anchor B's size, …) is a DAG and stabilizes in at most
/// its depth; real chains are shallow. Exceeding this bound signals a
/// **spill-dependency cycle** or a two-anchor oscillation (RFC-0012 BC-5) — a
/// **loud refusal** (`#UNSUPPORTED!` + diagnostic on the unstable anchors, per
/// BC-4b), never stale values. The bound is generous so a deep-but-legitimate
/// chain is not falsely refused while a true cycle still terminates quickly.
const SPILL_FIXPOINT_CAP: u32 = 64;

/// Whether `(row, col)` lies inside `rect` (inclusive). M2 lane-4 spill geometry
/// helper — the spill rectangles are small (real dynamic arrays are a handful of
/// cells), so a direct bounds test is used rather than the `xl-graph`
/// range-index machinery.
fn rect_contains(rect: &RectRange, row: u32, col: u32) -> bool {
    row >= rect.row_start && row <= rect.row_end && col >= rect.col_start && col <= rect.col_end
}

/// Translate `xl-io`'s workbook date-system flag into `xl-fn`'s equivalent.
/// The two enums are deliberately separate — `xl-fn` must not depend on
/// `xl-io` — so `xl-engine` (which sees both) bridges them here.
fn map_date_system(system: xl_io::DateSystem) -> DateSystem {
    match system {
        xl_io::DateSystem::Excel1900 => DateSystem::Excel1900,
        xl_io::DateSystem::Excel1904 => DateSystem::Excel1904,
    }
}

impl Engine {
    /// Load a workbook: parse every formula, build the dependency graph, and
    /// seed the value store with the file's cached values. Never panics — parse
    /// errors and unsupported constructs become diagnostics/`#UNSUPPORTED!`.
    ///
    /// Call [`recalc`](Engine::recalc) to compute fresh values (the seeded
    /// values are whatever the file last stored).
    #[must_use]
    pub fn load(workbook: Workbook) -> Engine {
        let mut sheet_ids = BTreeMap::new();
        for (idx, sheet) in workbook.sheets.iter().enumerate() {
            sheet_ids.insert(sheet.name.to_ascii_lowercase(), SheetId(idx as u32));
        }
        // Retain the original-case display names in tab order; `sheet_ids`
        // above discards case for its lookup keys.
        let sheet_names: Vec<String> = workbook.sheets.iter().map(|s| s.name.clone()).collect();
        // Translate each defined name's `localSheetId` scope into an engine
        // `SheetId` (ECMA-376 §18.2.6; lane L2-D). `localSheetId` indexes the
        // FULL `<sheets>` collection (`Sheet::sheets_index`), which diverges
        // from the loaded tab order whenever the loader skipped an entry
        // (chartsheet/dialogsheet/macrosheet/veryHidden no-part sheet) — so
        // the mapping keys on `sheets_index`, never on the vector position. A
        // scope index with no loaded sheet becomes `LocalUnmapped`: such a
        // name can never resolve (its sheet hosts no formulas) and must not
        // shadow or stand in for a same-named global.
        let collection_to_tab: BTreeMap<u32, SheetId> = workbook
            .sheets
            .iter()
            .enumerate()
            .map(|(idx, sheet)| (sheet.sheets_index, SheetId(idx as u32)))
            .collect();
        let defined_names: Vec<ScopedName> = workbook
            .defined_names
            .iter()
            .map(|d| ScopedName {
                name: d.name.clone(),
                formula: d.formula.clone(),
                scope: match d.sheet_scope {
                    None => NameScope::Global,
                    Some(li) => match collection_to_tab.get(&li) {
                        Some(&sid) => NameScope::Local(sid),
                        None => NameScope::LocalUnmapped,
                    },
                },
            })
            .collect();
        let calc = CalcSettings {
            iterate: workbook.calc_settings.iterate,
            max_iters: workbook.calc_settings.iterate_count,
            max_change: workbook.calc_settings.iterate_delta,
        };

        let mut values: ValueStore = BTreeMap::new();
        let mut programs: BTreeMap<CellId, CellProgram> = BTreeMap::new();
        let mut subtotals: BTreeSet<CellId> = BTreeSet::new();
        // OXP-121: flatten each sheet's `<row hidden="1">` set (0-based rows)
        // into `(sheet, row)` pairs keyed by tab-order `SheetId`, so the tagged
        // cell walk can flag a cell's `is_hidden_row` by a single lookup.
        let mut hidden_rows: BTreeSet<(SheetId, u32)> = BTreeSet::new();
        for (idx, sheet) in workbook.sheets.iter().enumerate() {
            let sid = SheetId(idx as u32);
            for &row in &sheet.hidden_rows {
                hidden_rows.insert((sid, row));
            }
        }
        let mut graph = DepGraph::new();

        // First pass (ECMA-376 §18.17.2): collect every sheet's shared-formula
        // masters, keyed by `(sheet, si)`. The `si` namespace is per-worksheet,
        // so a bodyless follow-on is expanded against the master of its *own*
        // sheet's group. Built before the compile loop so a follow-on can be
        // translated the moment it is visited.
        let shared_masters = shared::collect_masters(&workbook);

        {
            // Precedent extraction never consults the SUBTOTAL or hidden-row
            // tag sets; pass empty ones so the real `subtotals` stays free to be
            // mutated in the loop below (the eval-time env supplies the live sets
            // instead).
            let no_subtotals: BTreeSet<CellId> = BTreeSet::new();
            let no_hidden_rows: BTreeSet<(SheetId, u32)> = BTreeSet::new();
            // Precedent extraction never reads the spill map (the `#` operand's
            // anchor is a plain `Ref` precedent — RFC-0012 finding 4); pass empty.
            let no_spills: BTreeMap<CellId, RectRange> = BTreeMap::new();
            let env = WbIndex {
                sheet_ids: &sheet_ids,
                defined_names: &defined_names,
                subtotals: &no_subtotals,
                hidden_rows: &no_hidden_rows,
                spills: &no_spills,
                // Precedent extraction is array-entry-agnostic (it never reaches
                // the Range scalar seam); the real value is stamped per-cell in
                // `run_cell` from the stored `CellProgram::Parsed` flag.
                is_array_formula: false,
            };
            for (idx, sheet) in workbook.sheets.iter().enumerate() {
                let sid = SheetId(idx as u32);
                for (&(row, col), cell) in &sheet.cells {
                    let cid = CellId::new(sid, row, col);
                    // Seed the store with the file's cached value.
                    values.insert(cid, cell.value.clone());

                    let Some(raw) = &cell.formula else {
                        continue;
                    };
                    match &raw.text {
                        Some(text) => match xl_ast::parse(text) {
                            // Load-level LET validation (OXP-200): a duplicate
                            // LET parameter is a load rejection, mirrored per
                            // cell — never a silently-shadowed binding.
                            Ok(expr) if duplicate_let_rejection(&expr).is_some() => {
                                graph.set_deps(cid, &[]);
                                let p = duplicate_let_rejection(&expr)
                                    .expect("checked by the guard above");
                                programs.insert(cid, p);
                            }
                            Ok(expr) => {
                                let mut prec = Vec::new();
                                collect_precedents(&expr, sid, &env, &mut prec);
                                graph.set_deps(cid, &prec);
                                if contains_volatile(&expr) {
                                    graph.register_volatile(cid, true);
                                }
                                // RFC 0002: tag this cell if its formula head is a
                                // SUBTOTAL call, so aggregates over it can exclude it.
                                if head_is_subtotal(&expr) {
                                    subtotals.insert(cid);
                                }
                                // Record whether this cell was array-entered
                                // (legacy CSE `<f t="array">`), so eval knows to
                                // suppress legacy implicit intersection (OXP-163).
                                programs
                                    .insert(cid, CellProgram::Parsed(expr, raw.is_array_entered()));
                            }
                            Err(e) => {
                                graph.set_deps(cid, &[]);
                                programs.insert(
                                    cid,
                                    CellProgram::Unparsed(
                                        DiagnosticKind::ParseError,
                                        format!("formula parse error: {e}"),
                                    ),
                                );
                            }
                        },
                        None => {
                            // A formula cell with no body: a shared-formula
                            // follow-on, or an array/dataTable follow-on.
                            //
                            // ECMA-376 §18.17.2: expand a `t="shared"` follow-on
                            // by translating its group master's formula by the
                            // follow-on's relative `(row, col)` offset from the
                            // master, then compile the result through the SAME
                            // path a normal parsed formula uses. Array (`t=
                            // "array"`, legacy CSE) and dataTable (`t="dataTable"`)
                            // materialization is a separate lane and out of scope,
                            // so those keep refusing loudly (`#UNSUPPORTED!`).
                            let expanded = if raw.kind == FormulaKind::Shared {
                                raw.shared_index.and_then(|si| {
                                    shared_masters.get(&(sid, si)).map(|m| {
                                        let drow = row as i64 - m.row as i64;
                                        let dcol = col as i64 - m.col as i64;
                                        shared::translate(&m.expr, drow, dcol)
                                    })
                                })
                            } else {
                                None
                            };
                            match expanded {
                                // Load-level LET validation applies to an
                                // expanded follow-on exactly as to its master
                                // (same compile path — OXP-200).
                                Some(translated)
                                    if duplicate_let_rejection(&translated).is_some() =>
                                {
                                    graph.set_deps(cid, &[]);
                                    let p = duplicate_let_rejection(&translated)
                                        .expect("checked by the guard above");
                                    programs.insert(cid, p);
                                }
                                Some(translated) => {
                                    // Compile exactly like a freshly-parsed
                                    // formula: precedents, volatility, SUBTOTAL
                                    // tag. A shared follow-on is never
                                    // array-entered, so the array-entry flag is
                                    // `false`.
                                    let mut prec = Vec::new();
                                    collect_precedents(&translated, sid, &env, &mut prec);
                                    graph.set_deps(cid, &prec);
                                    if contains_volatile(&translated) {
                                        graph.register_volatile(cid, true);
                                    }
                                    if head_is_subtotal(&translated) {
                                        subtotals.insert(cid);
                                    }
                                    programs.insert(cid, CellProgram::Parsed(translated, false));
                                }
                                None => {
                                    // Could not expand: an orphan shared follow-on
                                    // (its `si` master is missing / on another
                                    // sheet / failed to parse), or an out-of-scope
                                    // array/dataTable follow-on. Refuse loudly with
                                    // an accurate, distinguishable message.
                                    graph.set_deps(cid, &[]);
                                    let message = match raw.kind {
                                        FormulaKind::Shared => match raw.shared_index {
                                            Some(si) => format!(
                                                "shared follow-on: master si={si} not \
                                                 found or unparseable on this sheet \
                                                 (orphan); cannot expand (ECMA-376 \
                                                 §18.17.2)"
                                            ),
                                            None => "shared follow-on with no si \
                                                     attribute (malformed); cannot expand"
                                                .to_string(),
                                        },
                                        FormulaKind::Array => "array follow-on (no body): \
                                             CSE array materialization is out of scope \
                                             (separate lane)"
                                            .to_string(),
                                        FormulaKind::DataTable => "dataTable follow-on \
                                             (no body): data-table materialization is \
                                             out of scope (separate lane)"
                                            .to_string(),
                                        FormulaKind::Normal => "formula cell with no body \
                                             and no sharing metadata (malformed); \
                                             unsupported"
                                            .to_string(),
                                    };
                                    programs.insert(
                                        cid,
                                        CellProgram::Unparsed(
                                            DiagnosticKind::UnsupportedConstruct,
                                            message,
                                        ),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // Surface load-time refusals immediately, before any `recalc`. A cell
        // whose program could not be compiled — a parse error, an unsupported
        // construct, or an out-of-scope array/dataTable (or orphan shared)
        // follow-on with no body — is a *known*
        // refusal the moment the workbook loads; it needs no evaluation to
        // discover. Seeding these into `diagnostics` at load makes
        // [`diagnostics`](Engine::diagnostics) mean "every refusal known so
        // far": load-time refusals now, evaluation-time refusals (unknown
        // function, in-eval unsupported construct, cycles) added by `recalc`.
        // Without this seed, a consumer that reads `value`/`diagnostics`
        // *before* calling `recalc` would see the file's cached values with an
        // empty diagnostics list — a workbook full of unparseable formulas
        // would read as "clean values, zero refusals", the exact silently-wrong
        // window `#UNSUPPORTED!` exists to close (never-guess; the Recalc design rules §0).
        // `run_cell` removes then re-inserts a cell's diagnostics on recalc, so
        // this seed is consistent with — never double-counted against — the
        // post-recalc set.
        let mut diagnostics: BTreeMap<CellId, Vec<Diagnostic>> = BTreeMap::new();
        for (&cell, program) in &programs {
            if let CellProgram::Unparsed(kind, message) = program {
                diagnostics.insert(
                    cell,
                    vec![Diagnostic {
                        cell,
                        kind: *kind,
                        message: message.clone(),
                    }],
                );
            }
        }

        Engine {
            sheet_ids,
            sheet_names,
            defined_names,
            programs,
            values,
            subtotals,
            hidden_rows,
            graph,
            calc,
            diagnostics,
            // Thread the workbook's 1900/1904 date system into the function
            // evaluation context so the date functions (YEAR/MONTH/DAY/DATE/
            // EOMONTH) resolve serials consistently with the file.
            ctx: EvalContext::with_date_system(map_date_system(workbook.date_system)),
            eval_count: 0,
            last_recalc_cells: Vec::new(),
            spills: BTreeMap::new(),
            spill_owner: BTreeMap::new(),
            spill_redirty: BTreeSet::new(),
            spill_changed_anchors: BTreeSet::new(),
            refused_spills: BTreeSet::new(),
        }
    }

    /// Full recalculation: evaluate the whole graph in dependency order.
    ///
    /// Idempotent — running it twice yields identical values and diagnostics.
    pub fn recalc(&mut self) -> RecalcResult {
        let plan = self.graph.full_plan(self.calc);
        // M2 lane 9 (RFC-0014): when the `parallel` feature is on AND the
        // workbook is parallel-safe, evaluate each antichain wave concurrently
        // and apply results in canonical order — bit-identical to the serial
        // path, which the fall-through below runs unchanged. `parallel_unsafe`
        // is the whole-workbook gate: it closes on any cell that can spill (a
        // top-level `Value::Array`, keeping lane-4 semantics serial) OR that
        // reads outside the static edge set (a reference transformer
        // OFFSET/INDIRECT/ANCHORARRAY — RFC-0014 R1), the two ways a same-wave
        // cell could diverge from serial. The staged-array backstop inside
        // `execute_parallel` (R3) additionally converts any spill-gate miss into
        // a serial fallback rather than a wrong answer.
        #[cfg(feature = "parallel")]
        if !self.parallel_unsafe() && self.try_recalc_parallel(&plan) {
            self.graph.clear_dirty();
            return RecalcResult {
                evaluated: self.last_recalc_cells.len(),
                diagnostics: self.diagnostic_count(),
            };
        }
        // Either the `parallel` feature is off, the gate closed, or the parallel
        // attempt bailed on a staged-array gate miss (R3) — all fall through to
        // the authoritative serial driver, which redoes the recalc idempotently.
        // M2 lane 4: run the plan, then iterate the spill-planning fixpoint so a
        // formula reading a cell a dynamic array spilled into recomputes even
        // when the spill footprint was discovered only at eval time (RFC-0012 §3
        // / BC-1 protocol B). `drive_recalc` clears `last_recalc_cells` once and
        // accumulates across the fixpoint passes.
        self.drive_recalc(plan);
        self.graph.clear_dirty();
        RecalcResult {
            evaluated: self.last_recalc_cells.len(),
            diagnostics: self.diagnostic_count(),
        }
    }

    /// The **serial** full recalc, bypassing the parallel branch of
    /// [`recalc`](Engine::recalc). Exists so the determinism test can compare
    /// both paths in one `--features parallel` binary (RFC-0014 R8 / the Recalc design rules
    /// rayon condition 4). Not part of the public contract.
    #[cfg(feature = "parallel")]
    #[doc(hidden)]
    pub fn recalc_serial(&mut self) -> RecalcResult {
        let plan = self.graph.full_plan(self.calc);
        self.drive_recalc(plan);
        self.graph.clear_dirty();
        RecalcResult {
            evaluated: self.last_recalc_cells.len(),
            diagnostics: self.diagnostic_count(),
        }
    }

    /// Whether [`recalc`](Engine::recalc) would take the **parallel branch** for
    /// the workbook in its current state — i.e. the whole-workbook gate
    /// ([`parallel_unsafe`](Engine::parallel_unsafe)) is open. Exposed only so
    /// the `xl-bench` corpus serial-vs-parallel sweep can report **non-vacuity**
    /// (how many workbooks actually engaged the parallel executor rather than
    /// silently falling back to serial) — the deferred half of the Recalc design rules rayon
    /// condition 4 (`docs/parallel-sweep.md`). A `true` here plus a spill-free
    /// run means the concurrent `execute_parallel` wave loop ran; the R3
    /// staged-array backstop can still fall back, but only on a gate miss, which
    /// the gate itself excludes for a non-spilling workbook. Not part of the
    /// stable public contract (mirrors [`recalc_serial`](Engine::recalc_serial)).
    #[cfg(feature = "parallel")]
    #[doc(hidden)]
    #[must_use]
    pub fn parallel_gate_open(&self) -> bool {
        !self.parallel_unsafe()
    }

    /// Edit one cell and incrementally recompute only the affected cells.
    ///
    /// A formula edit reparses the cell and rebuilds its dependency edges; a
    /// literal edit turns the cell into a plain input. Volatile cells are always
    /// rescheduled (via `mark_volatile_dirty`), so a `NOW()`/`OFFSET()` cell
    /// recomputes on every edit even when nothing it reads changed.
    pub fn edit(&mut self, cell: CellId, input: CellInput) {
        self.diagnostics.remove(&cell);
        // M2 lane 4: if the edited cell currently belongs to ANOTHER anchor's
        // spill region, break that ownership and re-dirty the owner so its
        // obstruction check re-runs — the edit now occupies a spilled slot, so
        // the owner must re-evaluate to `#SPILL!` (Excel: typing into a spilled
        // cell blocks the array). Ownership-guarded retraction (below) then leaves
        // this freshly-edited cell untouched.
        if let Some(&owner) = self.spill_owner.get(&cell)
            && owner != cell
        {
            self.spill_owner.remove(&cell);
            self.graph.mark_dirty(&[owner]);
        }
        match input {
            CellInput::Formula(text) => {
                let src = text.strip_prefix('=').unwrap_or(&text);
                match xl_ast::parse(src) {
                    // Load-level LET validation (OXP-200): the programmatic-edit
                    // compile path rejects a duplicate LET parameter exactly as
                    // workbook load does.
                    Ok(expr) if duplicate_let_rejection(&expr).is_some() => {
                        self.graph.set_deps(cell, &[]);
                        self.graph.register_volatile(cell, false);
                        self.subtotals.remove(&cell);
                        let p = duplicate_let_rejection(&expr).expect("checked by the guard above");
                        self.programs.insert(cell, p);
                    }
                    Ok(expr) => {
                        let (prec, volatile) = {
                            let no_spills: BTreeMap<CellId, RectRange> = BTreeMap::new();
                            let env = WbIndex {
                                sheet_ids: &self.sheet_ids,
                                defined_names: &self.defined_names,
                                subtotals: &self.subtotals,
                                hidden_rows: &self.hidden_rows,
                                // Precedent extraction never reads the spill map.
                                spills: &no_spills,
                                // Precedent extraction ignores array-entry.
                                is_array_formula: false,
                            };
                            let mut prec = Vec::new();
                            collect_precedents(&expr, cell.sheet, &env, &mut prec);
                            (prec, contains_volatile(&expr))
                        };
                        // Keep the RFC 0002 tag set in sync with the new formula.
                        let is_subtotal = head_is_subtotal(&expr);
                        self.graph.set_deps(cell, &prec);
                        self.graph.register_volatile(cell, volatile);
                        if is_subtotal {
                            self.subtotals.insert(cell);
                        } else {
                            self.subtotals.remove(&cell);
                        }
                        // A programmatic edit is never CSE array-entered — the
                        // `CellInput::Formula` API carries no array-entry marker —
                        // so the edited cell evaluates in scalar context (does
                        // legacy implicit intersection where applicable).
                        self.programs.insert(cell, CellProgram::Parsed(expr, false));
                    }
                    Err(e) => {
                        self.graph.set_deps(cell, &[]);
                        self.graph.register_volatile(cell, false);
                        // An unparseable cell is no longer a SUBTOTAL.
                        self.subtotals.remove(&cell);
                        self.programs.insert(
                            cell,
                            CellProgram::Unparsed(
                                DiagnosticKind::ParseError,
                                format!("formula parse error: {e}"),
                            ),
                        );
                    }
                }
            }
            CellInput::Literal(v) => {
                // M2 lane 4: a literal edit is never a formula node, so if this
                // cell was a spill anchor, retract its region (vacating + re-
                // dirtying the cells it owned) before it becomes a plain input.
                self.retract_spill(cell);
                self.graph.set_dynamic_deps(cell, false);
                self.graph.remove_node(cell);
                self.programs.remove(&cell);
                // A literal cell is no longer a formula, so it cannot be a SUBTOTAL.
                self.subtotals.remove(&cell);
                self.values.insert(cell, v);
            }
        }
        self.graph.mark_dirty(&[cell]);
        self.graph.mark_volatile_dirty();
        let plan = self.graph.take_recalc_plan(self.calc);
        // M2 lane 4: same fixpoint driver as `recalc`, so an edit that grows or
        // shrinks a spill footprint re-dirties the readers of the cells it
        // gained/vacated (RFC-0012 §3 grow/shrink dirtying).
        self.drive_recalc(plan);
    }

    /// The current value of a cell, or `None` if the cell has no stored value
    /// (a never-populated blank cell).
    #[must_use]
    pub fn value(&self, sheet: SheetId, row: u32, col: u32) -> Option<&Value> {
        self.values.get(&CellId::new(sheet, row, col))
    }

    /// The current value of a cell by its [`CellId`].
    #[must_use]
    pub fn value_at(&self, cell: CellId) -> Option<&Value> {
        self.values.get(&cell)
    }

    /// The spill region **anchored** at (`sheet`, `row`, `col`) as an owned
    /// [`Value::Array`], or `None` if the addressed cell is not a dynamic-array
    /// spill anchor (RFC-0012 BC-10: an obstructed anchor, a spilled-into cell,
    /// a plain value, or a non-formula cell → `None`; a 1×1 dynamic-array
    /// anchor → its 1×1 `Value::Array`). This is the sole read-only query
    /// RFC-0013 §3 specifies for the FFI `spill_region`/`spillRegion` surface,
    /// consumed uniformly by all three bindings (Python/Node/WASM), so the whole
    /// tri-binding surface answers spill queries from this one source of truth.
    ///
    /// The region rectangle comes from the live spill registry
    /// ([`Engine::spills`], populated by [`Engine::spill_anchor`]); the element
    /// values are read back from the materialized value store, where
    /// [`spill_anchor`](Engine::spill_anchor) wrote each element into its own
    /// slot (the anchor holds the top-left element). Never a static guess: a
    /// cell absent from `spills` is genuinely a non-anchor and returns `None`.
    /// Returned by value (not borrowed) because the array is reconstructed from
    /// the scattered per-cell slots on demand.
    #[must_use]
    pub fn spill_region(&self, sheet: SheetId, row: u32, col: u32) -> Option<Value> {
        let anchor = CellId::new(sheet, row, col);
        let rect = *self.spills.get(&anchor)?;
        let rows = (rect.row_end - rect.row_start + 1) as usize;
        let cols = (rect.col_end - rect.col_start + 1) as usize;
        let mut data = Vec::with_capacity(rows * cols);
        for r in rect.row_start..=rect.row_end {
            for c in rect.col_start..=rect.col_end {
                // Each spilled slot holds its element (spill_anchor invariant);
                // a slot the store never populated reads as Blank.
                data.push(self.value(sheet, r, c).cloned().unwrap_or(Value::Blank));
            }
        }
        // `rect` came from the registry, so `rows * cols == data.len()` by
        // construction and `Array::new` cannot fail. A debug build asserts that
        // invariant loudly; a release build degrades to `None` (a "not an anchor"
        // answer) rather than fabricate a wrong-shaped region.
        let arr = Array::new(rows, cols, data);
        debug_assert!(
            arr.is_ok(),
            "spill_region: registry rect and reconstructed data disagree on shape"
        );
        arr.ok().map(Value::Array)
    }

    /// All recorded diagnostics, in deterministic `CellId` order (and, within a
    /// cell, emission order).
    #[must_use]
    pub fn diagnostics(&self) -> Vec<&Diagnostic> {
        self.diagnostics.values().flatten().collect()
    }

    /// The diagnostics recorded for one cell, in emission order (empty if the
    /// cell computed cleanly).
    #[must_use]
    pub fn diagnostics_for(&self, sheet: SheetId, row: u32, col: u32) -> &[Diagnostic] {
        self.diagnostics
            .get(&CellId::new(sheet, row, col))
            .map_or(&[], Vec::as_slice)
    }

    /// Total number of diagnostics across every cell (a cell can hold several).
    #[must_use]
    fn diagnostic_count(&self) -> usize {
        self.diagnostics.values().map(Vec::len).sum()
    }

    /// Resolve a sheet name to its [`SheetId`] (ASCII case-insensitive).
    #[must_use]
    pub fn sheet_id(&self, name: &str) -> Option<SheetId> {
        self.sheet_ids.get(&name.to_ascii_lowercase()).copied()
    }

    /// The workbook's sheet display names, in **tab order** (element `i` is the
    /// sheet whose [`SheetId`] is `SheetId(i as u32)`), preserving the original
    /// casing from `xl/workbook.xml`.
    ///
    /// This is the list-shaped companion to [`sheet_id`](Engine::sheet_id),
    /// which only resolves a single name → id. It exists so a foreign-binding
    /// or reporting consumer can enumerate sheets without depending on `xl-io`
    /// or reaching into the engine's private maps.
    ///
    /// **Public API surface.** This accessor is part of `xl-engine`'s stable
    /// public interface (added for the `xl-ffi` Python binding, M1); its
    /// signature is a human checkpoint per the Recalc design rules.
    #[must_use]
    pub fn sheet_names(&self) -> Vec<String> {
        self.sheet_names.clone()
    }

    /// Total cell evaluations since load — snapshot it before and after an
    /// [`edit`](Engine::edit) to observe that only dependents were recomputed.
    #[must_use]
    pub fn eval_count(&self) -> u64 {
        self.eval_count
    }

    /// The cells evaluated by the most recent recalc/edit, in plan order.
    #[must_use]
    pub fn last_recalc_cells(&self) -> &[CellId] {
        &self.last_recalc_cells
    }

    // ----- internals -----------------------------------------------------

    /// Drive one recalc to a spill-planning fixpoint (RFC-0012 §3 / BC-1
    /// protocol B, with the BC-4 determinism + termination guards).
    ///
    /// Runs `plan`, then repeatedly: drains the cells whose spill footprint or
    /// content changed this pass ([`Engine::spill_redirty`]), re-dirties them
    /// (which, via the graph's reverse edges, pulls in every formula that reads a
    /// grown/shrunk/re-valued spilled cell), and re-executes the resulting
    /// **filtered** incremental plan. Each pass is an order-preserving filter of
    /// the canonical full order (`recalc_plan`), so seeded determinism and
    /// edit≡rebuild identity are preserved (BC-4c/§5). Convergence is when no
    /// footprint/content changed. Non-convergence within [`SPILL_FIXPOINT_CAP`]
    /// (a spill-dependency cycle or two-anchor oscillation, BC-5) is a **loud
    /// refusal** on the unstable anchors (BC-4b) — never stale values.
    fn drive_recalc(&mut self, plan: xl_graph::Plan) {
        self.last_recalc_cells.clear();
        // A refusal set is per-recalc: clear it at entry so a prior recalc's
        // refusals do not suppress this one's cells (review B2).
        self.refused_spills.clear();
        // NB: `spill_redirty`/`spill_changed_anchors` are NOT cleared here — they
        // are always empty at entry (drained on convergence, folded through on
        // refusal, empty at construction) EXCEPT when `edit` pre-seeds them via a
        // retract of an anchor being overwritten. That pre-seed must survive into
        // the first drain below, so clearing here would be a correctness bug.
        self.execute(&plan);

        let mut iters: u32 = 0;
        loop {
            let redirty = std::mem::take(&mut self.spill_redirty);
            let unstable = std::mem::take(&mut self.spill_changed_anchors);
            if redirty.is_empty() {
                return; // fixpoint: no spill footprint or content changed
            }
            iters += 1;
            if iters > SPILL_FIXPOINT_CAP {
                // Non-convergence (BC-4b): refuse the unstable anchors loudly,
                // then KEEP draining so the readers of the cells those refusals
                // vacate are reconciled to non-stale values (review B2). Each
                // bailout refuses ≥1 NEW anchor — a refused anchor is skipped by
                // `run_cell` and so cannot re-oscillate — so the active-anchor
                // set strictly shrinks and this terminates. If a bailout refuses
                // nothing new, no further progress is possible → stop.
                let newly = self.refuse_spill_nonconvergence(&unstable);
                if newly == 0 {
                    return;
                }
                // Fold the pass's still-pending redirty back in alongside the
                // vacated / re-seeded cells `refuse_*` accumulated, and give the
                // reconciliation passes a fresh iteration budget (bounded by the
                // strictly shrinking anchor set).
                for c in redirty {
                    self.spill_redirty.insert(c);
                }
                iters = 0;
                continue;
            }
            let seeds: Vec<CellId> = redirty.into_iter().collect();
            self.graph.mark_dirty(&seeds);
            let plan = self.graph.take_recalc_plan(self.calc);
            if plan.is_empty() {
                // The changed cells have no formula readers — their new values
                // are already written; nothing more to recompute.
                return;
            }
            self.execute(&plan);
        }
    }

    /// Execute a recalc plan step by step, in order. Appends to
    /// `last_recalc_cells` (cleared once per recalc by [`Engine::drive_recalc`]).
    fn execute(&mut self, plan: &xl_graph::Plan) {
        for step in &plan.steps {
            match step {
                Step::Eval(cid) => self.run_cell(*cid),
                Step::Cycle(g) => {
                    for m in &g.members {
                        self.run_cycle_member(*m, false);
                    }
                }
                Step::Iterate(g) => {
                    for m in &g.members {
                        self.run_cycle_member(*m, true);
                    }
                }
            }
        }
    }

    /// Evaluate a single formula cell and store its value + any diagnostic.
    fn run_cell(&mut self, cid: CellId) {
        // A spill anchor refused for non-convergence (BC-4b) stays refused for
        // the remainder of this recalc: re-evaluating it would re-spill and
        // restart the oscillation the refusal just broke. Its `#UNSUPPORTED!`
        // value + diagnostic are already in place, so skip it — while its
        // readers (scheduled after it in the plan) still recompute and observe
        // that refusal rather than a stale spilled value (review B2).
        if self.refused_spills.contains(&cid) {
            return;
        }
        self.eval_count += 1;
        self.last_recalc_cells.push(cid);
        self.diagnostics.remove(&cid);
        let Some((value, raw_diags)) = self.compute_cell(cid) else {
            // Not a formula node (e.g. removed); nothing to recompute. The eval
            // bookkeeping above is done unconditionally, matching the
            // pre-refactor order exactly.
            return;
        };
        self.apply_cell_result(cid, value, raw_diags);
    }

    /// **Pure** evaluation of one formula cell — reads only immutable engine
    /// state and mutates nothing, so the parallel executor (RFC-0014, M2 lane 9)
    /// can call it on `&self` across rayon threads with no aliasing.
    ///
    /// Returns the cell's `(value, raw diagnostics)`, or `None` when `cid` is
    /// not a formula node. All order-sensitive mutation — the value/spill
    /// write, diagnostics insert, and counters — is deferred to the caller
    /// ([`apply_cell_result`](Engine::apply_cell_result) / the parallel apply
    /// phase), so a wave can be evaluated concurrently and committed serially in
    /// canonical order for bit-identical results.
    fn compute_cell(&self, cid: CellId) -> Option<(Value, DiagnosticSink)> {
        // This cell's array-entry status (OXP-163): stamped into the env so
        // the Range scalar-context seam suppresses legacy implicit
        // intersection for a legacy CSE (`<f t="array">`) formula.
        let is_array_formula =
            matches!(self.programs.get(&cid), Some(CellProgram::Parsed(_, true)));
        let env = WbIndex {
            sheet_ids: &self.sheet_ids,
            defined_names: &self.defined_names,
            // The live SUBTOTAL tag set, so `for_each_cell_tagged` can flag
            // nested sub-totals during evaluation (RFC 0002).
            subtotals: &self.subtotals,
            // The workbook's hidden-row set, so `for_each_cell_tagged` can
            // flag hidden-row cells for SUBTOTAL 101–111 (OXP-121).
            hidden_rows: &self.hidden_rows,
            // M2 lane 4: the live anchor→region map, so `A1#` resolves.
            spills: &self.spills,
            is_array_formula,
        };
        match self.programs.get(&cid) {
            Some(CellProgram::Parsed(expr, _)) => {
                let mut diags: DiagnosticSink = Vec::new();
                let v = eval_expr(
                    expr,
                    cid,
                    cid.sheet,
                    &self.values,
                    &env,
                    &self.ctx,
                    &mut diags,
                    // Top-level cell formula — not an array-context aggregator
                    // argument (RFC-0011).
                    false,
                    // No lexical bindings in force at the top level (M2 lane 2).
                    None,
                );
                // M2 lane 2 (OXP-200): a bare lambda that IS a cell's direct
                // result displays as `#CALC!` (distinct from a lambda-valued
                // spilled array *element*, which is `#VALUE!` — OXP-203). A
                // lambda only reaches here as the top-level result; consumed
                // as an argument it never surfaces.
                let v = match v {
                    Value::Lambda(_) => Value::Error(ErrorKind::Calc),
                    other => other,
                };
                Some((v, diags))
            }
            Some(CellProgram::Unparsed(kind, msg)) => Some((
                Value::Error(ErrorKind::Unsupported),
                vec![(*kind, msg.clone())],
            )),
            // Not a formula node (e.g. removed); nothing to recompute.
            None => None,
        }
    }

    /// Commit a computed `(value, raw_diags)` for `cid`: the order-sensitive
    /// mutation tail shared by serial [`run_cell`](Engine::run_cell) and the
    /// parallel apply phase, so the two paths cannot drift.
    fn apply_cell_result(&mut self, cid: CellId, value: Value, raw_diags: DiagnosticSink) {
        // Keep *every* diagnostic the cell produced, not just the first. Inserted
        // BEFORE `write_cell_result` so a spill diagnostic (`#SPILL!`, OXP-203
        // `#VALUE!`) appends after the eval diagnostics for this cell.
        if !raw_diags.is_empty() {
            // Identical (kind, message) pairs collapse to one: an argument walk
            // that retries a lazy array-position expression (dense → used-row →
            // used-col) re-emits the same refusal, and the fidelity report should
            // name each distinct finding once per cell.
            let mut diags: Vec<Diagnostic> = Vec::new();
            for (kind, message) in raw_diags {
                if diags.iter().any(|d| d.kind == kind && d.message == message) {
                    continue;
                }
                diags.push(Diagnostic {
                    cell: cid,
                    kind,
                    message,
                });
            }
            self.diagnostics.insert(cid, diags);
        }

        // M2 lane 4: the write-back seam. A multi-cell (or 1×1 DA) `Value::Array`
        // result spills into neighbouring slots (with an obstruction check); a
        // scalar is a single-slot write that also retracts a prior spill region if
        // this cell used to be an anchor. This is the only place a cell writes
        // outside its own slot — the write footprint is now a function of
        // evaluation, and the fixpoint driver reconciles it with the graph.
        self.write_cell_result(cid, value);
    }

    /// **M2 lane 9 (RFC-0014).** Whole-workbook gate: `true` when the parallel
    /// recalc path is **not** safe to take, so `recalc` must stay serial.
    ///
    /// It closes on the two — and only two — ways a same-antichain-wave cell
    /// could produce a different value under concurrent evaluation than under
    /// serial evaluation:
    /// 1. **Spill.** A cell that can return a top-level `Value::Array` spills
    ///    into neighbouring slots at eval time (lane 4); a later same-pass cell
    ///    reading such a slot sees the fresh spill serially but the pre-wave
    ///    state in parallel. Detected statically as an array literal, a call to
    ///    an [`xl_fn::returns_array`] function, or a spill-range postfix `#`, or
    ///    dynamically by a live spill anchor.
    /// 2. **Out-of-static-edge read.** A reference transformer
    ///    (`OFFSET`/`INDIRECT`/`ANCHORARRAY` — [`refx::is_ref_returning`])
    ///    computes its target at eval time, so its read is absent from the
    ///    static edge set the antichain waves are built from (RFC-0014 R1). It
    ///    could read a same-wave cell whose value differs between the serial
    ///    plan-prefix state and the parallel pre-wave state.
    ///
    /// Conservative by construction — over-inclusion only forgoes parallelism.
    /// Cell ASTs **and** defined-name bodies are scanned (R4b): a name resolves
    /// to an arbitrary expression at eval time. A legacy array-formula CSE
    /// (`<f t="array">`) is also unsafe (it enters as a spilling array).
    #[cfg(feature = "parallel")]
    fn parallel_unsafe(&self) -> bool {
        // A live spill anchor (e.g. from an `edit` pre-seed) ⇒ not spill-free.
        if !self.spills.is_empty() {
            return true;
        }
        for program in self.programs.values() {
            match program {
                CellProgram::Parsed(expr, is_array_formula) => {
                    if *is_array_formula || expr_unsafe_for_parallel(expr) {
                        return true;
                    }
                }
                CellProgram::Unparsed(_, _) => {}
            }
        }
        // R4b: a defined-name body can itself parse to a spill / transformer.
        for dn in &self.defined_names {
            if let Ok(expr) = xl_ast::parse(&dn.formula)
                && expr_unsafe_for_parallel(&expr)
            {
                return true;
            }
        }
        false
    }

    /// **M2 lane 9 (RFC-0014).** Attempt a parallel full recalc for a
    /// parallel-safe workbook. Returns `true` when it completed (results are
    /// committed and bit-identical to [`drive_recalc`](Engine::drive_recalc)),
    /// or `false` when the staged-array backstop (R3) tripped — a spiller the
    /// whole-workbook gate failed to exclude — in which case any partial
    /// application is rolled back to a state from which the caller's serial
    /// `drive_recalc` produces the correct result.
    #[cfg(feature = "parallel")]
    fn try_recalc_parallel(&mut self, plan: &xl_graph::Plan) -> bool {
        let eval_count_entry = self.eval_count;
        self.last_recalc_cells.clear();
        // Always empty on this path (refusals require spills, which the gate
        // excludes), but cleared for parity with `drive_recalc`.
        self.refused_spills.clear();
        if !self.execute_parallel(plan) {
            // R3 gate miss: only spill-free waves were applied (idempotent,
            // correct values already in the store). Undo the two order-sensitive
            // counters the applied prefix bumped so the caller's full serial
            // `drive_recalc` is authoritative (it clears `last_recalc_cells`
            // itself and overwrites every cell as it re-runs the whole plan).
            self.eval_count = eval_count_entry;
            self.last_recalc_cells.clear();
            return false;
        }
        // R2: wave-major application permutes the global commit order, but every
        // wave is precedent-closed so the value store / diagnostics (both keyed
        // maps) are order-insensitive. `last_recalc_cells` is order-*sensitive*
        // public API documented "in plan order" — and serial recalc pushes
        // exactly `plan.cells()` (every scheduled Eval + cycle member, in
        // canonical order, none refused on this path). Rebuild it canonically.
        self.last_recalc_cells.clear();
        self.last_recalc_cells.extend(plan.cells());
        // A completed run had no staged array ⇒ spill-free ⇒ nothing to reconcile.
        debug_assert!(
            self.spill_redirty.is_empty() && self.spill_changed_anchors.is_empty(),
            "parallel path completed on a workbook that produced spill work"
        );
        true
    }

    /// **M2 lane 9 (RFC-0014).** Execute `plan` wave by wave: a pure `par_iter`
    /// map over each antichain, committed serially in canonical order. Returns
    /// `false` (having applied only spill-free prefix waves) if the staged-array
    /// backstop trips; `true` on full completion.
    ///
    /// Correctness rests on: (a) same-wave cells are mutually independent, so
    /// the map reads only state committed by earlier waves; (b)
    /// [`compute_cell`](Engine::compute_cell) mutates nothing, so sharing
    /// `&self` across threads is sound (`Engine: Sync`, asserted below); (c) the
    /// **staged-array backstop** (R3): before applying a wave, its results are
    /// scanned for a `Value::Array` — a spill the whole-workbook gate failed to
    /// exclude. On a hit no further state is applied and the caller re-runs the
    /// whole recalc serially (spill-aware, with the fixpoint), converting any
    /// gate miss into lost parallelism rather than a wrong answer. A wave
    /// containing a cycle group is a serial barrier.
    #[cfg(feature = "parallel")]
    fn execute_parallel(&mut self, plan: &xl_graph::Plan) -> bool {
        use rayon::prelude::*;

        let n = plan.steps.len();
        if n == 0 {
            return true;
        }
        let waves = self.graph.waves(plan);
        let nwaves = waves.iter().copied().max().map_or(0, |m| m as usize + 1);
        // Bucket step indices by wave in one pass (O(V)); push order is ascending
        // index, i.e. canonical plan order within each wave.
        let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); nwaves];
        for (i, &w) in waves.iter().enumerate() {
            buckets[w as usize].push(i);
        }

        for bucket in &buckets {
            let all_eval = bucket
                .iter()
                .all(|&i| matches!(plan.steps[i], Step::Eval(_)));
            if all_eval {
                let cells: Vec<CellId> = bucket
                    .iter()
                    .map(|&i| match &plan.steps[i] {
                        Step::Eval(c) => *c,
                        _ => unreachable!("filtered to Eval above"),
                    })
                    .collect();
                // (a)+(b): pure concurrent evaluation over committed prior waves.
                let staged: Vec<(CellId, Option<(Value, DiagnosticSink)>)> = cells
                    .par_iter()
                    .map(|&cid| (cid, self.compute_cell(cid)))
                    .collect();
                // (c) R3 backstop: a top-level array means the gate missed a
                // spiller. Abandon the parallel attempt (nothing from this wave
                // applied); the caller redoes the whole recalc serially.
                if staged
                    .iter()
                    .any(|(_, r)| matches!(r, Some((Value::Array(_), _))))
                {
                    return false;
                }
                for (cid, res) in staged {
                    self.eval_count += 1;
                    self.diagnostics.remove(&cid);
                    if let Some((value, raw_diags)) = res {
                        self.apply_cell_result(cid, value, raw_diags);
                    }
                }
            } else {
                // A wave carrying a cycle group is a serial barrier.
                for &i in bucket {
                    match &plan.steps[i] {
                        Step::Eval(cid) => self.run_cell(*cid),
                        Step::Cycle(g) => {
                            for m in &g.members {
                                self.run_cycle_member(*m, false);
                            }
                        }
                        Step::Iterate(g) => {
                            for m in &g.members {
                                self.run_cycle_member(*m, true);
                            }
                        }
                    }
                }
            }
        }
        true
    }

    /// **M2 lane 4.** Commit `value` for anchor `cid`, spilling a `Value::Array`.
    ///
    /// A `Value::Array` (any size, including 1×1 — OXP-204) spills; a lambda-valued
    /// element makes the whole spill `#VALUE!` (OXP-203/BC-7); anything else is a
    /// scalar single-slot write.
    fn write_cell_result(&mut self, cid: CellId, value: Value) {
        match value {
            Value::Array(arr) => {
                // OXP-203 / RFC-0012 BC-7: a lambda-valued element in a spilling
                // array makes the WHOLE spill `#VALUE!` (distinct from a *bare*
                // lambda that is a cell's direct result → `#CALC!`, OXP-200, handled
                // in `run_cell` before this). No spill occurs. Checked before the
                // 1×1 path so a 1×1 array-of-lambda is `#VALUE!`, not `#CALC!`.
                if arr.iter().any(|v| matches!(v, Value::Lambda(_))) {
                    self.retract_spill(cid);
                    self.graph.set_dynamic_deps(cid, false);
                    self.values.insert(cid, Value::Error(ErrorKind::Value));
                    self.diagnostics.entry(cid).or_default().push(Diagnostic {
                        cell: cid,
                        kind: DiagnosticKind::UnsupportedConstruct,
                        message: "#VALUE!: a lambda-valued element in a spilling \
                                  dynamic array makes the whole spill #VALUE! \
                                  (OXP-203 / RFC-0012 BC-7)"
                            .to_string(),
                    });
                    return;
                }
                self.spill_anchor(cid, &arr);
            }
            scalar => {
                // If this cell WAS a spill anchor and now yields a scalar, retract
                // its whole region (shrink-to-one / anchor-changed-formula): clear
                // + re-dirty the vacated cells so their static readers recompute.
                self.retract_spill(cid);
                self.graph.set_dynamic_deps(cid, false);
                // Excel never leaves a *formula* result as blank: a formula that
                // evaluates to an empty reference (e.g. `=A1` where A1 is empty) is
                // cached and displayed as 0 (Enron corpus oracle — 88% of value
                // mismatches were exactly this reference-to-empty pattern). Input
                // (non-formula) empty cells never run through here, so they stay
                // `Blank` and ISBLANK is unaffected; arithmetic/aggregates already
                // coerce blanks to 0, so only a bare-reference final result is
                // caught. Array *elements* keep their `Blank` (a spilled blank is
                // not a bare-reference result).
                let scalar = if matches!(scalar, Value::Blank) {
                    Value::Number(0.0)
                } else {
                    scalar
                };
                self.values.insert(cid, scalar);
            }
        }
    }

    /// **M2 lane 4.** Spill `arr` from anchor `cid`, or emit `#SPILL!` on
    /// obstruction. Records footprint/content changes into the fixpoint scratch
    /// ([`Engine::spill_redirty`]/[`Engine::spill_changed_anchors`]).
    fn spill_anchor(&mut self, cid: CellId, arr: &Array) {
        let rows = arr.rows() as u32;
        let cols = arr.cols() as u32;
        // `Array::new` rejects a 0-dimension array, so `rows`/`cols` are ≥ 1 and
        // the `- 1` cannot underflow.
        let new_rect = RectRange::new(cid.row, cid.row + rows - 1, cid.col, cid.col + cols - 1);
        let old_rect = self.spills.get(&cid).copied();

        // Off-sheet: a spill whose footprint would extend past the sheet edge is
        // a `#SPILL!` in Excel, never a phantom write into non-existent rows /
        // columns. `RectRange::new` validates no bounds, so guard here BEFORE the
        // obstruction scan (review B4). The exact off-edge error KIND is not yet
        // OXP-pinned; emit a distinguishable `#SPILL!` + diagnostic saying so,
        // rather than guess or write past the grid.
        if cid.row + rows - 1 > analyze::MAX_ROW0 || cid.col + cols - 1 > analyze::MAX_COL0 {
            self.retract_spill(cid);
            self.graph.set_dynamic_deps(cid, false);
            self.values.insert(cid, Value::Error(ErrorKind::Spill));
            self.diagnostics.entry(cid).or_default().push(Diagnostic {
                cell: cid,
                kind: DiagnosticKind::UnsupportedConstruct,
                message: format!(
                    "#SPILL!: dynamic array at ({}, {}) would spill {rows}×{cols} past \
                     the sheet edge; the exact off-edge error kind is unpinned \
                     (RFC-0012 / OXP follow-up)",
                    cid.row, cid.col
                ),
            });
            return;
        }

        // Obstruction: any target cell (other than the anchor) holding a foreign
        // value — a formula node OR a non-blank literal — that this anchor does
        // not already own (reverse index) blocks the spill (→ `#SPILL!`).
        let mut blocker = None;
        'scan: for r in 0..rows {
            for c in 0..cols {
                if r == 0 && c == 0 {
                    continue; // the anchor's own slot
                }
                let target = CellId::new(cid.sheet, cid.row + r, cid.col + c);
                if self.spill_owner.get(&target) == Some(&cid) {
                    continue; // a cell our own prior spill already owns (reclaimable)
                }
                // A target is obstructed if it holds a formula node, a non-blank
                // literal, OR is owned by ANOTHER anchor's spill region. The
                // own-owner `continue` above already fired, so any remaining
                // owner is foreign — and a foreign-owned slot must block even
                // when its current value is `Blank` (a `Blank` array element the
                // other anchor spilled): otherwise two anchors silently share a
                // cell and `spill_region` would report a foreign element
                // (review B1). The racing-diagnostic branch below then fires.
                let occupied = self.programs.contains_key(&target)
                    || self.spill_owner.contains_key(&target)
                    || matches!(self.values.get(&target), Some(v) if !matches!(v, Value::Blank));
                if occupied {
                    blocker = Some(target);
                    break 'scan;
                }
            }
        }

        if let Some(blocker) = blocker {
            // Retract any prior region (re-dirtying its readers), then mark the
            // anchor `#SPILL!` and DO NOT touch the blocking cell.
            self.retract_spill(cid);
            self.graph.set_dynamic_deps(cid, false);
            self.values.insert(cid, Value::Error(ErrorKind::Spill));
            // A `#SPILL!` from an obstruction is well-established Excel behavior
            // and is emitted loudly here. The specific case RFC-0012 BC-5 / OXP-202
            // leaves *unpinned* is a **pure two-anchor race** (two dynamic-array
            // anchors contending for one rectangle): which one errors, and with
            // what kind. When the blocker is a **formula** cell (a DA anchor is
            // always one) or a cell **owned by another anchor's region**, the block
            // could be that race, so the diagnostic cites the ledger and notes that
            // deterministic claim (= plan) order picked the loser — never a silent
            // guess. A plain literal-data blocker is the clean, pinned case.
            let racing =
                self.programs.contains_key(&blocker) || self.spill_owner.contains_key(&blocker);
            let message = if racing {
                format!(
                    "#SPILL!: dynamic array at ({}, {}) blocked by a formula / \
                     dynamic-array cell at ({}, {}); if that cell is itself a \
                     dynamic-array anchor this is a two-anchor race whose exact \
                     error kind is unpinned — deterministic claim (plan) order \
                     applied (RFC-0012 BC-5 / OXP-202)",
                    cid.row, cid.col, blocker.row, blocker.col
                )
            } else {
                format!(
                    "#SPILL!: dynamic array spill blocked by an existing value at \
                     ({}, {})",
                    blocker.row, blocker.col
                )
            };
            self.diagnostics.entry(cid).or_default().push(Diagnostic {
                cell: cid,
                kind: DiagnosticKind::UnsupportedConstruct,
                message,
            });
            return;
        }

        let mut changed = old_rect != Some(new_rect);

        // Shrink: clear cells that were in the old region but not the new one,
        // re-dirtying them (their readers see `Blank`) and releasing ownership.
        if let Some(old) = old_rect {
            for row in old.row_start..=old.row_end {
                for col in old.col_start..=old.col_end {
                    let cell = CellId::new(cid.sheet, row, col);
                    // Vacate only cells we still own and that fell out of the new
                    // rectangle (an edited-away cell no longer maps to us — leave
                    // its user value alone).
                    if cell != cid
                        && !rect_contains(&new_rect, row, col)
                        && self.spill_owner.get(&cell) == Some(&cid)
                    {
                        if self.values.get(&cell) != Some(&Value::Blank) {
                            changed = true;
                            self.spill_redirty.insert(cell);
                        }
                        self.values.insert(cell, Value::Blank);
                        self.spill_owner.remove(&cell);
                    }
                }
            }
        }

        // Write every element into its slot (anchor gets the top-left element).
        // A non-anchor target whose value actually changes (grow into a new cell,
        // or a same-shape re-spill with different contents) is re-dirtied so its
        // static readers recompute — the symmetric-difference grow-dirtying.
        for r in 0..rows {
            for c in 0..cols {
                let target = CellId::new(cid.sheet, cid.row + r, cid.col + c);
                let v = arr
                    .get(r as usize, c as usize)
                    .cloned()
                    .unwrap_or(Value::Blank);
                if target != cid {
                    if self.values.get(&target) != Some(&v) {
                        changed = true;
                        self.spill_redirty.insert(target);
                    }
                    self.spill_owner.insert(target, cid);
                }
                self.values.insert(target, v);
            }
        }
        self.spill_owner.insert(cid, cid);
        self.spills.insert(cid, new_rect);
        if changed {
            self.spill_changed_anchors.insert(cid);
        }
        // Advertise to the graph that this anchor's footprint is dynamic (the
        // dormant RFC-0012 seam; advisory metadata, consumed by no scheduler yet).
        self.graph.set_dynamic_deps(cid, true);
    }

    /// **M2 lane 4.** If `cid` is a current spill anchor, retract its whole
    /// region: blank every non-anchor cell (re-dirtying its readers), release
    /// ownership, and drop the anchor from the spill map. A no-op if `cid` never
    /// spilled. The anchor's own slot is left for the caller to overwrite.
    fn retract_spill(&mut self, cid: CellId) {
        let Some(rect) = self.spills.remove(&cid) else {
            return;
        };
        for row in rect.row_start..=rect.row_end {
            for col in rect.col_start..=rect.col_end {
                let cell = CellId::new(cid.sheet, row, col);
                if cell == cid {
                    self.spill_owner.remove(&cell);
                    continue;
                }
                // Only reclaim/blank a cell this anchor STILL owns. A cell edited
                // out from under the anchor has had its ownership broken (by
                // `edit`), so it now holds the user's value and must not be
                // clobbered back to `Blank` (Principle 2: never silently wrong).
                if self.spill_owner.get(&cell) == Some(&cid) {
                    self.spill_owner.remove(&cell);
                    if self.values.get(&cell) != Some(&Value::Blank) {
                        self.spill_redirty.insert(cell);
                    }
                    self.values.insert(cell, Value::Blank);
                }
            }
        }
        self.spill_changed_anchors.insert(cid);
    }

    /// **M2 lane 4 (BC-4b).** Loudly refuse the anchors whose spill footprint
    /// never converged within [`SPILL_FIXPOINT_CAP`] (a spill-dependency cycle or
    /// two-anchor oscillation, RFC-0012 BC-5): retract each region and mark the
    /// anchor `#UNSUPPORTED!` with a diagnostic — never leave stale spill values.
    ///
    /// Returns the number of **newly** refused anchors (anchors already in
    /// [`Engine::refused_spills`] are skipped, so this is idempotent). The
    /// retraction scratch is deliberately KEPT — [`Engine::drive_recalc`] drains
    /// it in the reconciliation passes that follow, so the readers of every
    /// vacated cell (and of the refused anchor itself) recompute to non-stale
    /// values rather than being stranded (review B2).
    fn refuse_spill_nonconvergence(&mut self, anchors: &BTreeSet<CellId>) -> usize {
        let mut newly_refused = 0usize;
        for &a in anchors {
            if !self.refused_spills.insert(a) {
                continue; // already refused this recalc — idempotent, no re-diag
            }
            newly_refused += 1;
            self.retract_spill(a);
            self.graph.set_dynamic_deps(a, false);
            self.values.insert(a, Value::Error(ErrorKind::Unsupported));
            self.diagnostics.entry(a).or_default().push(Diagnostic {
                cell: a,
                kind: DiagnosticKind::UnsupportedConstruct,
                message: "spill planning did not converge within the structural \
                          cap — a spill-dependency cycle or two-anchor race \
                          (RFC-0012 BC-4/BC-5, OXP-202); refusing loudly rather \
                          than emitting stale spill values"
                    .to_string(),
            });
            // Re-seed the anchor so its own readers (`=A1`, `SUM(A1#)`, …)
            // recompute against the `#UNSUPPORTED!` we just wrote. `retract_spill`
            // already re-seeded the cells it vacated. We do NOT clear the scratch:
            // drive_recalc keeps draining it (never leaves stale readers).
            self.spill_redirty.insert(a);
        }
        newly_refused
    }

    /// Assign the circular-reference result to a cycle member: `#UNSUPPORTED!`
    /// plus a diagnostic citing `OXP-070`. Iterative calc is a later task, so an
    /// iterative group is treated identically in v0 (documented).
    fn run_cycle_member(&mut self, cid: CellId, iterative: bool) {
        self.eval_count += 1;
        self.last_recalc_cells.push(cid);
        // M2 lane 4: if this cell was a spill anchor before it fell into a cycle,
        // retract its region so no stale spilled values linger (the readers of the
        // vacated cells are re-dirtied through the fixpoint scratch). A no-op for a
        // cell that never spilled.
        self.retract_spill(cid);
        self.graph.set_dynamic_deps(cid, false);
        self.values
            .insert(cid, Value::Error(ErrorKind::Unsupported));
        let message = if iterative {
            "circular reference in an iterative-calc group; iterative \
             convergence is not yet implemented, so the value is loudly \
             #UNSUPPORTED! (the group is not looped)"
                .to_string()
        } else {
            "circular reference (non-iterative); the value is loudly \
             #UNSUPPORTED!. OXP-070 pinned Excel's dumped cached value as the \
             empty text string, but adopting it awaits a second pin on how \
             dependent cells observe the broken cycle (tracked follow-up)"
                .to_string()
        };
        self.diagnostics.insert(
            cid,
            vec![Diagnostic {
                cell: cid,
                kind: DiagnosticKind::CircularReference,
                message,
            }],
        );
    }
}

/// **M2 lane 9 (RFC-0014).** Whether `expr` (or any subexpression) makes a cell
/// unsafe for parallel evaluation — it can spill a top-level array (an array
/// literal, a call to an [`xl_fn::returns_array`] function, or a spilled-range
/// `#` postfix) or it reads outside the static dependency edges (a reference
/// transformer OFFSET/INDIRECT/ANCHORARRAY — [`refx::is_ref_returning`]).
///
/// Conservative: it recurses through every branch (even lazily-unselected `IF`
/// arms), matching the static, over-approximating dependency collection — a
/// construct that merely *appears* in the formula closes the gate.
#[cfg(feature = "parallel")]
fn expr_unsafe_for_parallel(expr: &Expr) -> bool {
    use xl_ast::{ExprKind, PostfixOp};
    match &expr.kind {
        // An array literal spills.
        ExprKind::Array(_) => true,
        ExprKind::Call { name, args } => {
            xl_fn::returns_array(&name.canonical)
                || refx::is_ref_returning(&name.canonical)
                || is_array_special_form(&name.canonical)
                || args.iter().any(expr_unsafe_for_parallel)
        }
        // `A1#` — the spilled-range operator resolves a dynamic array.
        ExprKind::Postfix {
            op: PostfixOp::SpillRange,
            ..
        } => true,
        ExprKind::Postfix { expr, .. } | ExprKind::Unary { expr, .. } => {
            expr_unsafe_for_parallel(expr)
        }
        ExprKind::Paren(inner) | ExprKind::ImplicitIntersection(inner) => {
            expr_unsafe_for_parallel(inner)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            expr_unsafe_for_parallel(lhs) || expr_unsafe_for_parallel(rhs)
        }
        // Leaves — enumerated (not `_`) so a new `ExprKind` variant forces an
        // explicit safe/unsafe decision here. A bare `Name` resolves (v0) only
        // to a Ref/Range, whose target IS a static edge; a name whose body is
        // itself unsafe is caught by the defined-name scan in `parallel_unsafe`.
        ExprKind::Number(_)
        | ExprKind::Text(_)
        | ExprKind::Bool(_)
        | ExprKind::Error(_)
        | ExprKind::Missing
        | ExprKind::Ref(_)
        | ExprKind::Name(_)
        | ExprKind::Unsupported { .. } => false,
    }
}

/// **M2 lane 9 (RFC-0014).** The M2 lane-2 lambda **special forms** that return
/// a top-level array (and so can spill), dispatched in
/// [`eval`](crate::eval)'s `eval_special_form` — NOT registry functions, so
/// [`xl_fn::returns_array`] does not see them. `REDUCE` returns a scalar and
/// `LET`/`LAMBDA` inherit array-ness from a sub-expression the gate already
/// recurses into, so none of those are listed. The
/// `gate_closes_on_spill_and_transformers` test (tests.rs) checks a MAKEARRAY
/// workbook stays serial; a runtime miss of any future array special form is
/// still caught by the staged-array backstop (R3).
#[cfg(feature = "parallel")]
fn is_array_special_form(canonical: &str) -> bool {
    matches!(canonical, "MAP" | "SCAN" | "BYROW" | "BYCOL" | "MAKEARRAY")
}

// RFC-0014 R5: the parallel executor shares `&Engine` across rayon threads, so
// the engine — and in particular the `EvalContext` sandbox seam — must be
// `Sync`. This static assertion fails the *parallel build* the instant a future
// change breaks it (e.g. the interior-mutability seeded RNG reserved in
// `xl-fn/src/context.rs`), forcing an explicit re-evaluation of parallel
// determinism rather than a silent thread-order RNG draw.
#[cfg(feature = "parallel")]
const _: fn() = || {
    fn assert_sync<T: Sync>() {}
    assert_sync::<Engine>();
    assert_sync::<EvalContext>();
    assert_sync::<Value>();
};

#[cfg(test)]
mod tests;

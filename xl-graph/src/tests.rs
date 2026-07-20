//! Unit tests for the dependency graph: build/maintenance, dirty propagation
//! minimality, range index, volatility, cycles (self-loop / 2-cycle /
//! figure-eight), iterative-mode plan shape, and plan determinism.
//!
//! Property tests (random DAGs, SCC vs naive reference, incremental ⊆ full)
//! live in `tests/props.rs`; scale tests in `tests/scale.rs`.

use xl_value::{RectRange, SheetId};

use crate::{CalcSettings, CellId, DepGraph, Plan, Precedent, SheetRange, Step};

/// Cell on sheet 0.
fn cell(row: u32, col: u32) -> CellId {
    CellId::new(SheetId(0), row, col)
}

/// Range on sheet 0, 0-based inclusive.
fn range(r0: u32, r1: u32, c0: u32, c1: u32) -> SheetRange {
    SheetRange::new(SheetId(0), RectRange::new(r0, r1, c0, c1))
}

fn settings() -> CalcSettings {
    CalcSettings::default()
}

/// Flat cell list of a plan (cycle members expanded).
fn cells_of(plan: &Plan) -> Vec<CellId> {
    plan.cells().collect()
}

/// Index of the step scheduling `c`, if any.
fn step_of(plan: &Plan, c: CellId) -> Option<usize> {
    plan.steps.iter().position(|s| match s {
        Step::Eval(x) => *x == c,
        Step::Cycle(g) => g.members.contains(&c),
        Step::Iterate(g) => g.members.contains(&c),
    })
}

/// Assert `before` is scheduled strictly before `after`.
fn assert_before(plan: &Plan, before: CellId, after: CellId) {
    let (b, a) = (step_of(plan, before), step_of(plan, after));
    assert!(
        b.is_some() && a.is_some() && b < a,
        "expected {before:?} (step {b:?}) before {after:?} (step {a:?}) in {plan:#?}"
    );
}

// ----- chains & diamonds ---------------------------------------------------

#[test]
fn chain_orders_precedents_first() {
    // a -> b -> c -> d  (b = f(a), etc.); a is a plain input.
    let (a, b, c, d) = (cell(0, 0), cell(0, 1), cell(0, 2), cell(0, 3));
    let mut g = DepGraph::new();
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(c, &[Precedent::Cell(b)]);
    g.set_deps(d, &[Precedent::Cell(c)]);

    let plan = g.full_plan(settings());
    assert_eq!(
        plan.steps,
        vec![Step::Eval(b), Step::Eval(c), Step::Eval(d)],
        "input cell `a` is not a node and must not be scheduled"
    );

    g.mark_dirty(&[a]);
    let inc = g.recalc_plan(settings());
    assert_eq!(inc.steps, plan.steps);
}

#[test]
fn diamond_schedules_join_after_both_arms() {
    // b and c both read a; d reads b and c.
    let (a, b, c, d) = (cell(0, 0), cell(0, 1), cell(0, 2), cell(0, 3));
    let mut g = DepGraph::new();
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(c, &[Precedent::Cell(a)]);
    g.set_deps(d, &[Precedent::Cell(b), Precedent::Cell(c)]);

    g.mark_dirty(&[a]);
    let plan = g.recalc_plan(settings());
    assert_eq!(cells_of(&plan).len(), 3);
    assert_before(&plan, b, d);
    assert_before(&plan, c, d);
    // Tie between the independent arms breaks by CellId order: b < c.
    assert_before(&plan, b, c);
}

#[test]
fn independent_nodes_schedule_in_cell_id_order() {
    let (x, y, z) = (cell(5, 0), cell(1, 3), cell(1, 0));
    let mut g = DepGraph::new();
    for c in [x, y, z] {
        g.set_deps(c, &[]);
    }
    let plan = g.full_plan(settings());
    assert_eq!(
        plan.steps,
        vec![Step::Eval(z), Step::Eval(y), Step::Eval(x)],
        "Kahn tie-break must be (sheet, row, col) order"
    );
}

// ----- dirty minimality ------------------------------------------------------

#[test]
fn dirty_marking_is_minimal() {
    // Diamond a->{b,c}->d, plus an untouched branch e->f.
    let (a, b, c, d) = (cell(0, 0), cell(0, 1), cell(0, 2), cell(0, 3));
    let (e, f) = (cell(9, 0), cell(9, 1));
    let mut g = DepGraph::new();
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(c, &[Precedent::Cell(a)]);
    g.set_deps(d, &[Precedent::Cell(b), Precedent::Cell(c)]);
    g.set_deps(f, &[Precedent::Cell(e)]);

    g.mark_dirty(&[a]);
    let plan = g.recalc_plan(settings());
    let cells = cells_of(&plan);
    assert_eq!(cells, vec![b, c, d]);
    assert!(
        !cells.contains(&f),
        "untouched branch must not be scheduled"
    );
}

#[test]
fn editing_mid_chain_schedules_only_downstream() {
    let (a, b, c, d) = (cell(0, 0), cell(0, 1), cell(0, 2), cell(0, 3));
    let mut g = DepGraph::new();
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(c, &[Precedent::Cell(b)]);
    g.set_deps(d, &[Precedent::Cell(c)]);

    g.mark_dirty(&[c]);
    let plan = g.recalc_plan(settings());
    assert_eq!(plan.steps, vec![Step::Eval(c), Step::Eval(d)]);
}

#[test]
fn clear_dirty_resets_incremental_state() {
    let (a, b) = (cell(0, 0), cell(0, 1));
    let mut g = DepGraph::new();
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.mark_dirty(&[a]);
    assert_eq!(g.dirty_len(), 1);
    g.clear_dirty();
    assert!(g.recalc_plan(settings()).is_empty());
}

// ----- range dependencies ----------------------------------------------------

#[test]
fn range_dep_fires_inside_not_outside() {
    // d = SUM(A1:B100) -> rows 0..=99, cols 0..=1.
    let d = cell(0, 5);
    let mut g = DepGraph::new();
    g.set_deps(d, &[Precedent::Range(range(0, 99, 0, 1))]);

    // Edit inside the range.
    g.mark_dirty(&[cell(4, 0)]);
    assert_eq!(cells_of(&g.recalc_plan(settings())), vec![d]);
    g.clear_dirty();

    // Edits outside: past the last row, past the last column, other sheet.
    g.mark_dirty(&[cell(100, 0)]);
    g.mark_dirty(&[cell(4, 2)]);
    g.mark_dirty(&[CellId::new(SheetId(1), 4, 0)]);
    assert!(
        g.recalc_plan(settings()).is_empty(),
        "edits outside the range must not dirty the range dependent"
    );
}

#[test]
fn range_boundary_cells_are_inclusive() {
    let d = cell(200, 0);
    let mut g = DepGraph::new();
    g.set_deps(d, &[Precedent::Range(range(2, 5, 3, 7))]);
    for corner in [cell(2, 3), cell(2, 7), cell(5, 3), cell(5, 7)] {
        g.mark_dirty(&[corner]);
        assert_eq!(
            cells_of(&g.recalc_plan(settings())),
            vec![d],
            "corner {corner:?}"
        );
        g.clear_dirty();
    }
}

#[test]
fn range_precedent_orders_formula_cells_inside_the_range() {
    // b (in row 1) is a formula; s = SUM over rows 0..=1 must evaluate after b.
    let (a, b, s) = (cell(0, 0), cell(1, 0), cell(9, 9));
    let mut g = DepGraph::new();
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(s, &[Precedent::Range(range(0, 1, 0, 0))]);

    g.mark_dirty(&[a]);
    let plan = g.recalc_plan(settings());
    assert_eq!(plan.steps, vec![Step::Eval(b), Step::Eval(s)]);
}

#[test]
fn cross_sheet_range_dep() {
    let other = SheetId(3);
    let d = cell(0, 0);
    let mut g = DepGraph::new();
    g.set_deps(
        d,
        &[Precedent::Range(SheetRange::new(
            other,
            RectRange::new(0, 9, 0, 9),
        ))],
    );
    g.mark_dirty(&[CellId::new(other, 5, 5)]);
    assert_eq!(cells_of(&g.recalc_plan(settings())), vec![d]);
    g.clear_dirty();
    g.mark_dirty(&[cell(5, 5)]); // same coordinates, wrong sheet
    assert!(g.recalc_plan(settings()).is_empty());
}

#[test]
fn overlapping_ranges_each_fire_once() {
    let (d1, d2) = (cell(50, 0), cell(50, 1));
    let mut g = DepGraph::new();
    g.set_deps(d1, &[Precedent::Range(range(0, 9, 0, 9))]);
    g.set_deps(d2, &[Precedent::Range(range(5, 14, 5, 14))]);

    g.mark_dirty(&[cell(7, 7)]); // in both
    assert_eq!(cells_of(&g.recalc_plan(settings())), vec![d1, d2]);
    g.clear_dirty();
    g.mark_dirty(&[cell(1, 1)]); // only d1
    assert_eq!(cells_of(&g.recalc_plan(settings())), vec![d1]);
}

/// Degenerate (reversed) ranges: Excel accepts reversed refs like `A10:A1`
/// (normalizing internally); nothing upstream normalizes yet, so unvetted
/// workbooks can reach the graph with `start > end`. They must contain
/// nothing (per `SheetRange::contains`) and must never panic —
/// `BTreeMap::range` panics on inverted bounds, which would be a DoS surface
/// for the batch server. Exercises both the plan path (`nodes_in_range`) and
/// the dirty-marking path (range index).
#[test]
fn degenerate_ranges_never_panic_and_contain_nothing() {
    let reversed_rows = range(10, 1, 0, 0);
    let reversed_cols = range(0, 5, 7, 2);
    let reversed_both = range(9, 3, 8, 4);
    for r in [reversed_rows, reversed_cols, reversed_both] {
        // b = f(a) keeps the node map non-empty (BTreeMap::range only panics
        // on inverted bounds when the map is non-empty).
        let (a, b, d) = (cell(4, 0), cell(5, 0), cell(100, 100));
        let mut g = DepGraph::new();
        g.set_deps(b, &[Precedent::Cell(a)]);
        g.set_deps(d, &[Precedent::Range(r)]);

        // full_plan expands range precedents -> must not panic, no edges.
        let plan = g.full_plan(settings());
        assert_eq!(plan.cells().count(), 2, "range {r:?}");
        assert!(plan.steps.iter().all(|s| matches!(s, Step::Eval(_))));

        // Dirty-marking through the range index: a cell inside the
        // would-be-normalized rectangle must NOT dirty the range dependent.
        g.mark_dirty(&[cell(5, 5)]);
        assert!(!g.is_dirty(d), "degenerate {r:?} must contain nothing");
        g.clear_dirty();

        // The dependent itself still schedules when directly dirtied.
        g.mark_dirty(&[d]);
        assert_eq!(cells_of(&g.recalc_plan(settings())), vec![d]);
    }
}

#[test]
fn take_recalc_plan_clears_dirty_atomically() {
    let (a, b) = (cell(0, 0), cell(0, 1));
    let mut g = DepGraph::new();
    g.set_deps(b, &[Precedent::Cell(a)]);

    g.mark_dirty(&[a]);
    let plan = g.take_recalc_plan(settings());
    assert_eq!(plan.steps, vec![Step::Eval(b)]);
    assert_eq!(g.dirty_len(), 0, "dirty set cleared with the plan");
    assert!(
        g.take_recalc_plan(settings()).is_empty(),
        "nothing re-scheduled"
    );
}

// ----- volatile --------------------------------------------------------------

#[test]
fn volatile_cells_always_enter_the_plan() {
    // v = NOW(); w = v + 1; unrelated x = f(y).
    let (v, w, x, y) = (cell(0, 0), cell(0, 1), cell(9, 0), cell(9, 1));
    let mut g = DepGraph::new();
    g.set_deps(v, &[]);
    g.register_volatile(v, true);
    g.set_deps(w, &[Precedent::Cell(v)]);
    g.set_deps(x, &[Precedent::Cell(y)]);

    // No edits at all — volatile seeding alone schedules v and its dependents.
    g.mark_volatile_dirty();
    let plan = g.recalc_plan(settings());
    assert_eq!(plan.steps, vec![Step::Eval(v), Step::Eval(w)]);

    // Deregistering stops the seeding.
    g.clear_dirty();
    g.register_volatile(v, false);
    g.mark_volatile_dirty();
    assert!(g.recalc_plan(settings()).is_empty());
}

// ----- cycles ----------------------------------------------------------------

#[test]
fn self_loop_is_a_cycle_group() {
    let a = cell(0, 0);
    let mut g = DepGraph::new();
    g.set_deps(a, &[Precedent::Cell(a)]);
    let plan = g.full_plan(settings());
    assert_eq!(plan.steps.len(), 1);
    match &plan.steps[0] {
        Step::Cycle(grp) => assert_eq!(grp.members, vec![a]),
        other => panic!("self-loop must be a Cycle step, got {other:?}"),
    }
}

#[test]
fn two_cycle_is_grouped_and_sorted() {
    let (a, b) = (cell(0, 0), cell(0, 1));
    let mut g = DepGraph::new();
    g.set_deps(a, &[Precedent::Cell(b)]);
    g.set_deps(b, &[Precedent::Cell(a)]);
    let plan = g.full_plan(settings());
    assert_eq!(plan.steps.len(), 1);
    match &plan.steps[0] {
        Step::Cycle(grp) => assert_eq!(grp.members, vec![a, b], "members sorted by CellId"),
        other => panic!("expected Cycle, got {other:?}"),
    }
}

#[test]
fn oxp070_probe_topology_groups_as_cycles() {
    // OXP-070 (RUN-2026-07-11-oracle01), iteration off. The probe workbook's
    // four circular cells: a 2-cycle A1<->B1 (`A1=B1+1`, `B1=A1+1`), a bare
    // self-loop C1 (`C1=C1`), and a text-shaped self-loop D1
    // (`D1=IF(TRUE,D1,"x")&"t"`, which depends on D1). All must be detected as
    // Cycle groups. The engine assigns the observed empty-text value; the graph
    // only groups membership, which is what this pins.
    let (a1, b1) = (cell(0, 0), cell(0, 1));
    let (c1, d1) = (cell(0, 2), cell(0, 3));
    let mut g = DepGraph::new();
    g.set_deps(a1, &[Precedent::Cell(b1)]);
    g.set_deps(b1, &[Precedent::Cell(a1)]);
    g.set_deps(c1, &[Precedent::Cell(c1)]);
    g.set_deps(d1, &[Precedent::Cell(d1)]);

    let plan = g.full_plan(settings());
    // Three independent cycle groups, all scheduled as Step::Cycle.
    let cycle_members: Vec<Vec<CellId>> = plan
        .steps
        .iter()
        .map(|s| match s {
            Step::Cycle(grp) => grp.members.clone(),
            other => panic!("every step must be a Cycle, got {other:?}"),
        })
        .collect();
    assert_eq!(
        cycle_members,
        vec![vec![a1, b1], vec![c1], vec![d1]],
        "A1<->B1 group, C1 self-loop, D1 self-loop"
    );
}

#[test]
fn figure_eight_is_one_scc() {
    // Two loops sharing b: a <-> b and b <-> c. One SCC {a, b, c}.
    let (a, b, c) = (cell(0, 0), cell(0, 1), cell(0, 2));
    let mut g = DepGraph::new();
    g.set_deps(a, &[Precedent::Cell(b)]);
    g.set_deps(b, &[Precedent::Cell(a), Precedent::Cell(c)]);
    g.set_deps(c, &[Precedent::Cell(b)]);
    let plan = g.full_plan(settings());
    assert_eq!(plan.steps.len(), 1);
    match &plan.steps[0] {
        Step::Cycle(grp) => assert_eq!(grp.members, vec![a, b, c]),
        other => panic!("expected one Cycle of 3, got {other:?}"),
    }
}

#[test]
fn separate_cycles_stay_separate_and_ordered() {
    // Cycle {a, b} feeds x, which feeds cycle {c, d}.
    let (a, b, x, c, d) = (cell(0, 0), cell(0, 1), cell(0, 2), cell(0, 3), cell(0, 4));
    let mut g = DepGraph::new();
    g.set_deps(a, &[Precedent::Cell(b)]);
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(x, &[Precedent::Cell(b)]);
    g.set_deps(c, &[Precedent::Cell(d), Precedent::Cell(x)]);
    g.set_deps(d, &[Precedent::Cell(c)]);

    let plan = g.full_plan(settings());
    assert_eq!(plan.steps.len(), 3);
    match (&plan.steps[0], &plan.steps[1], &plan.steps[2]) {
        (Step::Cycle(g1), Step::Eval(mid), Step::Cycle(g2)) => {
            assert_eq!(g1.members, vec![a, b]);
            assert_eq!(*mid, x);
            assert_eq!(g2.members, vec![c, d]);
        }
        other => panic!("expected Cycle, Eval, Cycle; got {other:?}"),
    }
}

#[test]
fn cycle_plus_downstream_dependent() {
    // Cell outside a cycle that reads a cycle member is scheduled after it.
    let (a, b, e) = (cell(0, 0), cell(0, 1), cell(0, 2));
    let mut g = DepGraph::new();
    g.set_deps(a, &[Precedent::Cell(b)]);
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(e, &[Precedent::Cell(a)]);
    let plan = g.full_plan(settings());
    assert_before(&plan, a, e);
    assert!(matches!(&plan.steps[0], Step::Cycle(_)));
    assert_eq!(plan.steps[1], Step::Eval(e));
}

#[test]
fn iterative_mode_wraps_cycles_with_settings() {
    let (a, b) = (cell(0, 0), cell(0, 1));
    let mut g = DepGraph::new();
    g.set_deps(a, &[Precedent::Cell(b)]);
    g.set_deps(b, &[Precedent::Cell(a)]);

    let s = CalcSettings {
        iterate: true,
        max_iters: 25,
        max_change: 0.5,
    };
    let plan = g.full_plan(s);
    assert_eq!(plan.steps.len(), 1);
    match &plan.steps[0] {
        Step::Iterate(grp) => {
            assert_eq!(grp.members, vec![a, b]);
            assert_eq!(grp.settings, s);
        }
        other => panic!("expected Iterate step, got {other:?}"),
    }
}

#[test]
fn default_calc_settings_match_excel_defaults() {
    // Excel: iteration off; 100 iterations / 0.001 max change when enabled
    // (implementation-plan.md §2; OOXML <calcPr> defaults).
    let s = CalcSettings::default();
    assert!(!s.iterate);
    assert_eq!(s.max_iters, 100);
    assert_eq!(s.max_change, 0.001);
}

// ----- determinism -----------------------------------------------------------

#[test]
fn plan_is_build_order_independent() {
    // Same graph assembled in two different insertion orders -> identical plan.
    let (a, b, c, d, e) = (cell(0, 0), cell(0, 1), cell(0, 2), cell(0, 3), cell(0, 4));
    let deps: Vec<(CellId, Vec<Precedent>)> = vec![
        (b, vec![Precedent::Cell(a)]),
        (
            c,
            vec![Precedent::Cell(a), Precedent::Range(range(0, 0, 1, 1))],
        ),
        (d, vec![Precedent::Cell(b), Precedent::Cell(c)]),
        (e, vec![Precedent::Cell(e)]), // self-loop, independent
    ];

    let mut g1 = DepGraph::new();
    for (cellid, ps) in &deps {
        g1.set_deps(*cellid, ps);
    }
    let mut g2 = DepGraph::new();
    for (cellid, ps) in deps.iter().rev() {
        g2.set_deps(*cellid, ps);
    }
    // Churn g2 with an overwrite to prove replacement leaves no residue.
    g2.set_deps(d, &[Precedent::Cell(e)]);
    g2.set_deps(d, &[Precedent::Cell(b), Precedent::Cell(c)]);

    assert_eq!(g1.full_plan(settings()), g2.full_plan(settings()));
}

// ----- dep replacement & removal ----------------------------------------------

#[test]
fn dep_replacement_retargets_indirect() {
    // x = INDIRECT(...) initially resolving to a; later retargeted to b.
    let (a, b, x) = (cell(0, 0), cell(0, 1), cell(0, 2));
    let mut g = DepGraph::new();
    g.set_deps(x, &[Precedent::Cell(a)]);
    g.set_dynamic_deps(x, true);
    assert!(g.is_dynamic(x));

    g.mark_dirty(&[a]);
    assert_eq!(cells_of(&g.recalc_plan(settings())), vec![x]);
    g.clear_dirty();

    // Engine re-reports deps after evaluating the INDIRECT.
    g.set_deps(x, &[Precedent::Cell(b)]);
    g.mark_dirty(&[a]);
    assert!(
        g.recalc_plan(settings()).is_empty(),
        "old edge must be gone after replacement"
    );
    g.clear_dirty();
    g.mark_dirty(&[b]);
    assert_eq!(cells_of(&g.recalc_plan(settings())), vec![x]);
    // Dynamic flag survives dep replacement.
    assert!(g.is_dynamic(x));
}

#[test]
fn range_dep_replacement_removes_old_range_entry() {
    let d = cell(100, 0);
    let mut g = DepGraph::new();
    g.set_deps(d, &[Precedent::Range(range(0, 9, 0, 0))]);
    g.set_deps(d, &[Precedent::Range(range(20, 29, 0, 0))]);

    g.mark_dirty(&[cell(5, 0)]); // old range only
    assert!(g.recalc_plan(settings()).is_empty());
    g.mark_dirty(&[cell(25, 0)]); // new range
    assert_eq!(cells_of(&g.recalc_plan(settings())), vec![d]);
}

#[test]
fn remove_node_detaches_edges_but_keeps_dependents_on_it() {
    // Chain a -> b -> c; delete b's formula.
    let (a, b, c) = (cell(0, 0), cell(0, 1), cell(0, 2));
    let mut g = DepGraph::new();
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(c, &[Precedent::Cell(b)]);
    g.register_volatile(b, true);
    g.remove_node(b);

    assert!(!g.is_node(b));
    assert!(!g.is_volatile(b));
    assert_eq!(g.node_count(), 1);

    // Editing a no longer dirties anything (b's outgoing edge is gone).
    g.mark_dirty(&[a]);
    assert!(g.recalc_plan(settings()).is_empty());
    g.clear_dirty();

    // But c still reads b (now a plain input cell): editing b dirties c.
    g.mark_dirty(&[b]);
    assert_eq!(cells_of(&g.recalc_plan(settings())), vec![c]);
}

#[test]
fn remove_node_with_range_precedents_clears_range_index() {
    let d = cell(100, 0);
    let mut g = DepGraph::new();
    g.set_deps(d, &[Precedent::Range(range(0, 9, 0, 9))]);
    g.remove_node(d);
    g.mark_dirty(&[cell(5, 5)]);
    assert!(g.recalc_plan(settings()).is_empty());
    assert_eq!(g.dirty_len(), 0);
}

// ----- misc API ---------------------------------------------------------------

#[test]
fn mark_all_dirty_equals_full_plan() {
    let (a, b, c) = (cell(0, 0), cell(0, 1), cell(0, 2));
    let mut g = DepGraph::new();
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(c, &[Precedent::Cell(b), Precedent::Cell(c)]); // self-loop too
    g.mark_all_dirty();
    assert_eq!(g.recalc_plan(settings()), g.full_plan(settings()));
}

#[test]
fn duplicate_precedents_coalesce() {
    let (a, b) = (cell(0, 0), cell(0, 1));
    let mut g = DepGraph::new();
    g.set_deps(
        b,
        &[Precedent::Cell(a), Precedent::Cell(a), Precedent::Cell(a)],
    );
    assert_eq!(g.direct_dependents(a), vec![b]);
    g.mark_dirty(&[a]);
    assert_eq!(cells_of(&g.recalc_plan(settings())), vec![b]);
}

#[test]
fn direct_dependents_are_sorted() {
    let a = cell(0, 0);
    let (x, y, z) = (cell(3, 0), cell(1, 0), cell(2, 0));
    let mut g = DepGraph::new();
    for c in [x, y, z] {
        g.set_deps(c, &[Precedent::Cell(a)]);
    }
    assert_eq!(g.direct_dependents(a), vec![y, z, x]);
}

/// Spec-review counterexample: `recalc_plan` must be a *filter* of the
/// canonical full order, not a Kahn re-run on the dirty subgraph — the CellId
/// tie-break is scope-dependent, so a subgraph re-run can flip the relative
/// order of cells shared with the full plan (breaking seeded determinism,
/// `implementation-plan.md` §5 M2).
#[test]
fn subsequence_holds_for_scope_dependent_tiebreak() {
    // Nodes n0..n3, all formulas; n1 reads n3; n2 reads n0.
    let n: Vec<CellId> = (0..4).map(|i| cell(0, i)).collect();
    let mut g = DepGraph::new();
    g.set_deps(n[0], &[]);
    g.set_deps(n[1], &[Precedent::Cell(n[3])]);
    g.set_deps(n[2], &[Precedent::Cell(n[0])]);
    g.set_deps(n[3], &[]);

    // Canonical full order: n0 (releases n2), n2, n3 (releases n1), n1.
    let full = g.full_plan(settings());
    assert_eq!(
        full.steps,
        vec![
            Step::Eval(n[0]),
            Step::Eval(n[2]),
            Step::Eval(n[3]),
            Step::Eval(n[1]),
        ]
    );

    // Dirty {n0, n1} closes over dependents to {n0, n1, n2}. A Kahn re-run on
    // that subgraph would emit [n0, n1, n2] (n1 is immediately ready once the
    // clean n3 leaves the scope), flipping n1/n2 relative to the full plan.
    // Restriction semantics must yield the full order filtered: [n0, n2, n1].
    g.mark_dirty(&[n[0], n[1]]);
    let inc = g.recalc_plan(settings());
    assert_eq!(
        inc.steps,
        vec![Step::Eval(n[0]), Step::Eval(n[2]), Step::Eval(n[1])]
    );
}

// ----- antichain waves (RFC-0014, parallel recalc) --------------------------

/// The wave index of the step scheduling `c`.
fn wave_of(plan: &Plan, waves: &[u32], c: CellId) -> u32 {
    waves[step_of(plan, c).expect("cell scheduled")]
}

#[test]
fn waves_empty_plan() {
    let g = DepGraph::new();
    let plan = g.full_plan(settings());
    assert!(g.waves(&plan).is_empty());
}

#[test]
fn waves_chain_is_strictly_increasing() {
    // a -> b -> c -> d : a plain input; b,c,d each one wave apart.
    let (a, b, c, d) = (cell(0, 0), cell(0, 1), cell(0, 2), cell(0, 3));
    let mut g = DepGraph::new();
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(c, &[Precedent::Cell(b)]);
    g.set_deps(d, &[Precedent::Cell(c)]);
    let plan = g.full_plan(settings());
    let w = g.waves(&plan);
    assert_eq!(w.len(), plan.steps.len());
    // b has only the plain input `a` (not scheduled) as precedent -> wave 0.
    assert_eq!(wave_of(&plan, &w, b), 0);
    assert_eq!(wave_of(&plan, &w, c), 1);
    assert_eq!(wave_of(&plan, &w, d), 2);
}

#[test]
fn waves_diamond_join_is_one_past_arms() {
    // b,c both read a; d reads b and c. b,c share a wave; d is one past.
    let (a, b, c, d) = (cell(0, 0), cell(0, 1), cell(0, 2), cell(0, 3));
    let mut g = DepGraph::new();
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(c, &[Precedent::Cell(a)]);
    g.set_deps(d, &[Precedent::Cell(b), Precedent::Cell(c)]);
    let plan = g.full_plan(settings());
    let w = g.waves(&plan);
    // b and c are mutually independent -> same wave (0, since `a` is a plain
    // input and not scheduled).
    assert_eq!(wave_of(&plan, &w, b), 0);
    assert_eq!(wave_of(&plan, &w, c), 0);
    assert_eq!(wave_of(&plan, &w, d), 1);
}

#[test]
fn waves_independent_nodes_all_wave_zero() {
    let (x, y, z) = (cell(5, 0), cell(1, 3), cell(1, 0));
    let mut g = DepGraph::new();
    for c in [x, y, z] {
        g.set_deps(c, &[]);
    }
    let plan = g.full_plan(settings());
    let w = g.waves(&plan);
    assert_eq!(
        w,
        vec![0, 0, 0],
        "no dependencies -> a single parallel wave"
    );
}

#[test]
fn waves_expand_range_precedents() {
    // s = SUM(A1:A3): reading a scheduled cell via a RANGE must raise the wave,
    // even though `direct_dependents` carries no per-cell reverse range edge.
    let (a1, a2, a3) = (cell(0, 0), cell(1, 0), cell(2, 0));
    let s = cell(0, 5);
    let mut g = DepGraph::new();
    // a1 is itself a formula node (reads a plain input) so it is scheduled.
    g.set_deps(a1, &[Precedent::Cell(cell(9, 9))]);
    g.set_deps(a2, &[Precedent::Cell(cell(9, 9))]);
    g.set_deps(a3, &[Precedent::Cell(cell(9, 9))]);
    g.set_deps(s, &[Precedent::Range(range(0, 2, 0, 0))]);
    let plan = g.full_plan(settings());
    let w = g.waves(&plan);
    // a1..a3 are independent (wave 0); s reads them via a range -> wave 1.
    assert_eq!(wave_of(&plan, &w, a1), 0);
    assert_eq!(wave_of(&plan, &w, a2), 0);
    assert_eq!(wave_of(&plan, &w, a3), 0);
    assert_eq!(wave_of(&plan, &w, s), 1);
}

#[test]
fn waves_cycle_group_is_a_barrier() {
    // A 2-cycle {a,b} feeding c. The cycle is one step; c is one wave past it.
    let (a, b, c) = (cell(0, 0), cell(0, 1), cell(0, 2));
    let mut g = DepGraph::new();
    g.set_deps(a, &[Precedent::Cell(b)]);
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(c, &[Precedent::Cell(a)]);
    let plan = g.full_plan(settings());
    let w = g.waves(&plan);
    // The cycle group's intra-group edges do not raise its own wave.
    assert_eq!(wave_of(&plan, &w, a), 0);
    assert_eq!(wave_of(&plan, &w, b), 0);
    assert_eq!(wave_of(&plan, &w, c), 1);
}

#[test]
fn waves_are_relative_to_incremental_plan_scope() {
    // a -> b -> c -> d ; edit only c: the incremental plan is [c, d], and c's
    // precedent b is clean (absent from the plan) so c is wave 0 within scope.
    let (a, b, c, d) = (cell(0, 0), cell(0, 1), cell(0, 2), cell(0, 3));
    let mut g = DepGraph::new();
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(c, &[Precedent::Cell(b)]);
    g.set_deps(d, &[Precedent::Cell(c)]);
    g.mark_dirty(&[c]);
    let inc = g.recalc_plan(settings());
    assert_eq!(inc.steps, vec![Step::Eval(c), Step::Eval(d)]);
    let w = g.waves(&inc);
    assert_eq!(wave_of(&inc, &w, c), 0);
    assert_eq!(wave_of(&inc, &w, d), 1);
}

#[test]
fn plan_len_and_cells_agree() {
    let (a, b) = (cell(0, 0), cell(0, 1));
    let mut g = DepGraph::new();
    g.set_deps(a, &[Precedent::Cell(b)]);
    g.set_deps(b, &[Precedent::Cell(a)]);
    let plan = g.full_plan(settings());
    assert_eq!(plan.len(), 1);
    assert!(!plan.is_empty());
    assert_eq!(plan.cells().count(), 2);
}

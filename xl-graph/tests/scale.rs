//! Scale tests: million-class graphs must build and plan without recursion
//! (no stack overflow) and in reasonable debug-mode time (< 2s each per the
//! lane spec; the perf targets in `implementation-plan.md` §8 are release-mode
//! and stricter).

use std::time::Instant;

use xl_graph::{CalcSettings, CellId, DepGraph, Precedent, Step};
use xl_value::SheetId;

fn cell(row: u32, col: u32) -> CellId {
    CellId::new(SheetId(0), row, col)
}

const N: u32 = 100_000;

/// Debug-mode wall-clock budget for one build+plan pass.
///
/// 2s on a developer machine; widened to 10s when `CI` is set, because
/// shared CI runners are several times slower and an absolute wall-clock
/// assert would flake there (observed: 2.5-3.1s on GitHub's ubuntu-latest
/// for work that takes <1s locally). The assert's real job is catching
/// accidental complexity blowups (quadratic edges, per-node re-sorts), and
/// a 5x-budget regression still trips it on either environment. The strict
/// release-mode product targets live in `implementation-plan.md` §8 and are
/// xl-bench's to enforce, not this smoke bound's.
fn budget_secs() -> f64 {
    if std::env::var_os("CI").is_some() {
        10.0
    } else {
        2.0
    }
}

/// 100k-cell linear chain: the depth-stress case. A recursive DFS would
/// overflow the stack here; the iterative traversals must not.
#[test]
fn chain_100k_builds_and_plans() {
    let t0 = Instant::now();
    let mut g = DepGraph::new();
    for i in 1..N {
        g.set_deps(cell(i, 0), &[Precedent::Cell(cell(i - 1, 0))]);
    }

    // Full plan: strictly ascending chain order.
    let plan = g.full_plan(CalcSettings::default());
    assert_eq!(plan.steps.len(), (N - 1) as usize);
    assert_eq!(plan.steps[0], Step::Eval(cell(1, 0)));
    assert_eq!(plan.steps[(N - 2) as usize], Step::Eval(cell(N - 1, 0)));

    // Incremental from the root input: everything downstream, iteratively.
    g.mark_dirty(&[cell(0, 0)]);
    let inc = g.recalc_plan(CalcSettings::default());
    assert_eq!(inc.steps.len(), (N - 1) as usize);

    let elapsed = t0.elapsed();
    assert!(
        elapsed.as_secs_f64() < budget_secs(),
        "100k chain build+plan took {elapsed:?} (budget {}s)",
        budget_secs()
    );
}

/// 100k-wide diamond: one source feeding 100k independent cells feeding one
/// sink (breadth stress + Kahn ready-set churn).
#[test]
fn diamond_100k_wide_builds_and_plans() {
    let t0 = Instant::now();
    let src = cell(0, 0);
    let sink = cell(2, 0);
    let mut g = DepGraph::new();
    let mut sink_deps: Vec<Precedent> = Vec::with_capacity(N as usize);
    for i in 0..N {
        let mid = cell(1, i);
        g.set_deps(mid, &[Precedent::Cell(src)]);
        sink_deps.push(Precedent::Cell(mid));
    }
    g.set_deps(sink, &sink_deps);

    g.mark_dirty(&[src]);
    let plan = g.recalc_plan(CalcSettings::default());
    assert_eq!(plan.steps.len(), (N + 1) as usize);
    // Middle layer in CellId (column) order; sink last.
    assert_eq!(plan.steps[0], Step::Eval(cell(1, 0)));
    assert_eq!(plan.steps[1], Step::Eval(cell(1, 1)));
    assert_eq!(*plan.steps.last().unwrap(), Step::Eval(sink));

    let elapsed = t0.elapsed();
    assert!(
        elapsed.as_secs_f64() < budget_secs(),
        "100k diamond build+plan took {elapsed:?} (budget {}s)",
        budget_secs()
    );
}

/// 100k-member cycle: the SCC stack, not the call stack, must absorb the
/// depth. One giant cycle group, members sorted.
#[test]
fn cycle_100k_is_one_group_no_overflow() {
    let mut g = DepGraph::new();
    for i in 0..N {
        g.set_deps(cell(i, 0), &[Precedent::Cell(cell((i + 1) % N, 0))]);
    }
    let plan = g.full_plan(CalcSettings::default());
    assert_eq!(plan.steps.len(), 1);
    match &plan.steps[0] {
        Step::Cycle(grp) => {
            assert_eq!(grp.members.len(), N as usize);
            assert_eq!(grp.members[0], cell(0, 0));
            assert_eq!(grp.members[(N - 1) as usize], cell(N - 1, 0));
        }
        other => panic!("expected one giant Cycle, got {other:?}"),
    }
}

/// One range precedent over 100k cells stays one index entry: editing inside
/// fires the dependent; the plan orders formula cells in the range first.
#[test]
fn range_over_100k_cells_is_compact() {
    let mut g = DepGraph::new();
    // b = f(a) inside the summed range; s = SUM(A1:A100000).
    let (a, b, s) = (cell(0, 0), cell(1, 0), cell(0, 5));
    g.set_deps(b, &[Precedent::Cell(a)]);
    g.set_deps(
        s,
        &[Precedent::Range(xl_graph::SheetRange::new(
            SheetId(0),
            xl_value::RectRange::new(0, N - 1, 0, 0),
        ))],
    );

    g.mark_dirty(&[cell(50_000, 0)]);
    let plan = g.recalc_plan(CalcSettings::default());
    assert_eq!(plan.steps, vec![Step::Eval(s)]);

    g.clear_dirty();
    g.mark_dirty(&[a]);
    let plan = g.recalc_plan(CalcSettings::default());
    assert_eq!(plan.steps, vec![Step::Eval(b), Step::Eval(s)]);
}

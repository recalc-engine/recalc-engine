//! Property tests for `xl-graph` plans.
//!
//! Structural guarantees checked over random graphs:
//! 1. **Edge respect** — on a random DAG (cell and range precedents), every
//!    precedent is scheduled strictly before its dependent.
//! 2. **SCC correctness** — on small random digraphs (cycles allowed), the
//!    plan's cycle grouping matches a naive `O(V^3)` reachability-based SCC
//!    reference (u, v share an SCC iff u reaches v and v reaches u).
//! 3. **Incremental ⊆ full** — the incremental plan is an order-preserving
//!    sub-sequence of the full plan and its cell set is closed under
//!    "is a dependent of".
//! 4. **Determinism** — permuting the build order of the same dependency sets
//!    yields the identical plan.

use proptest::prelude::*;
use xl_graph::{CalcSettings, CellId, DepGraph, Plan, Precedent, SheetRange, Step};
use xl_value::{RectRange, SheetId};

fn cell(i: u32) -> CellId {
    // One column per node keeps ids distinct and totally ordered by index.
    CellId::new(SheetId(0), i, 0)
}

fn plan_cells(plan: &Plan) -> Vec<CellId> {
    plan.cells().collect()
}

/// Step index of `c` in `plan`, if scheduled.
fn step_of(plan: &Plan, c: CellId) -> Option<usize> {
    plan.steps.iter().position(|s| match s {
        Step::Eval(x) => *x == c,
        Step::Cycle(g) => g.members.contains(&c),
        Step::Iterate(g) => g.members.contains(&c),
    })
}

// ---------------------------------------------------------------------------
// 1. Random DAG -> plan respects every edge.
// ---------------------------------------------------------------------------

/// A random DAG on `n` nodes: node `i` may only depend on nodes `< i` (cells)
/// or on row-ranges entirely below row `i` — acyclic by construction.
///
/// Some ranges are deliberately emitted **reversed** (`start > end`): the
/// graph treats degenerate ranges as empty (no edges, no panics), so they
/// cannot break acyclicity, and generating them here keeps the whole plan
/// pipeline exercised against un-normalized input (the edge-respect check
/// below iterates `row_start..=row_end`, which is empty for reversed ranges —
/// exactly the required semantics).
fn dag_strategy() -> impl Strategy<Value = (usize, Vec<Vec<Precedent>>)> {
    (2usize..24).prop_flat_map(|n| {
        let node_deps = (0..n).map(move |i| {
            let cells = proptest::collection::vec((0..n).prop_map(|t| cell(t as u32)), 0..4)
                .prop_map(move |targets| {
                    targets
                        .into_iter()
                        .filter(|t| (t.row as usize) < i)
                        .map(Precedent::Cell)
                        .collect::<Vec<_>>()
                });
            let ranges = proptest::collection::vec((0..n, 0..n, any::<bool>()), 0..2).prop_map(
                move |triples| {
                    triples
                        .into_iter()
                        .filter_map(|(a, b, reversed)| {
                            let (lo, hi) = (a.min(b) as u32, a.max(b) as u32);
                            let (r0, r1) = if reversed && lo != hi {
                                (hi, lo) // degenerate: contains nothing
                            } else {
                                (lo, hi)
                            };
                            ((hi as usize) < i).then(|| {
                                Precedent::Range(SheetRange::new(
                                    SheetId(0),
                                    RectRange::new(r0, r1, 0, 0),
                                ))
                            })
                        })
                        .collect::<Vec<_>>()
                },
            );
            (cells, ranges).prop_map(|(mut c, r)| {
                c.extend(r);
                c
            })
        });
        node_deps
            .collect::<Vec<_>>()
            .prop_map(move |deps| (n, deps))
    })
}

proptest! {
    #[test]
    fn dag_plan_respects_all_edges((n, deps) in dag_strategy()) {
        let mut g = DepGraph::new();
        for (i, ps) in deps.iter().enumerate() {
            g.set_deps(cell(i as u32), ps);
        }
        let plan = g.full_plan(CalcSettings::default());
        // Acyclic by construction: no cycle steps, all n nodes scheduled.
        prop_assert!(plan.steps.iter().all(|s| matches!(s, Step::Eval(_))));
        prop_assert_eq!(plan.cells().count(), n);

        for (i, ps) in deps.iter().enumerate() {
            let dep = cell(i as u32);
            let di = step_of(&plan, dep).unwrap();
            for p in ps {
                match *p {
                    Precedent::Cell(c) => {
                        let pi = step_of(&plan, c).unwrap();
                        prop_assert!(pi < di, "cell precedent {c:?} not before {dep:?}");
                    }
                    Precedent::Range(r) => {
                        for row in r.range.row_start..=r.range.row_end {
                            let pi = step_of(&plan, cell(row)).unwrap();
                            prop_assert!(pi < di, "range member {row} not before {dep:?}");
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Random digraph -> cycle grouping matches a naive SCC reference.
// ---------------------------------------------------------------------------

/// Naive SCC partition by transitive closure (Floyd–Warshall-style), fine for
/// n <= 10. Returns each node's component representative (smallest member).
#[allow(clippy::needless_range_loop)] // index form mirrors the textbook algorithm
fn naive_scc(n: usize, adj: &[Vec<bool>]) -> Vec<usize> {
    let mut reach = adj.to_vec();
    for k in 0..n {
        for i in 0..n {
            if reach[i][k] {
                for j in 0..n {
                    if reach[k][j] {
                        reach[i][j] = true;
                    }
                }
            }
        }
    }
    let mut rep = vec![usize::MAX; n];
    for i in 0..n {
        if rep[i] != usize::MAX {
            continue;
        }
        rep[i] = i;
        for j in (i + 1)..n {
            if reach[i][j] && reach[j][i] {
                rep[j] = i;
            }
        }
    }
    rep
}

fn digraph_strategy() -> impl Strategy<Value = (usize, Vec<Vec<bool>>)> {
    (1usize..9).prop_flat_map(|n| {
        proptest::collection::vec(proptest::collection::vec(any::<bool>(), n), n)
            .prop_map(move |adj| (n, adj))
    })
}

proptest! {
    #[test]
    fn scc_grouping_matches_naive_reference((n, adj) in digraph_strategy()) {
        let mut g = DepGraph::new();
        for (i, row) in adj.iter().enumerate() {
            let ps: Vec<Precedent> = (0..n)
                .filter(|&j| row[j])
                .map(|j| Precedent::Cell(cell(j as u32)))
                .collect();
            g.set_deps(cell(i as u32), &ps);
        }
        let plan = g.full_plan(CalcSettings::default());
        prop_assert_eq!(plan.cells().count(), n, "every node scheduled exactly once");

        let rep = naive_scc(n, &adj);
        // Partition the plan's steps into groups and compare against `rep`.
        for step in &plan.steps {
            match step {
                Step::Eval(c) => {
                    let i = c.row as usize;
                    // Singleton non-self-loop: naive rep must be itself and it
                    // must not self-reach.
                    prop_assert_eq!(rep[i], i);
                    prop_assert!(!adj[i][i], "self-loop must be a Cycle step");
                    prop_assert!(
                        (0..n).all(|j| j == i || rep[j] != rep[i]),
                        "Eval'd node shares a component with another node"
                    );
                }
                Step::Cycle(grp) => {
                    let members: Vec<usize> =
                        grp.members.iter().map(|c| c.row as usize).collect();
                    let r = rep[members[0]];
                    // All members share one naive component...
                    prop_assert!(members.iter().all(|&m| rep[m] == r));
                    // ...and the component has no other members.
                    let full: Vec<usize> = (0..n).filter(|&j| rep[j] == r).collect();
                    prop_assert_eq!(&members, &full, "cycle group == naive SCC, sorted");
                    // Size-1 cycle groups must be genuine self-loops.
                    if members.len() == 1 {
                        prop_assert!(adj[members[0]][members[0]]);
                    }
                }
                Step::Iterate(_) => prop_assert!(false, "iterate off"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Incremental plan ⊆ full plan, closed under dependents.
// ---------------------------------------------------------------------------

/// Config for the restriction-guarantee guard below: at least 2048 cases (the
/// `PROPTEST_CASES` env var can only raise it, never lower it). The default
/// 256 cases under-sampled badly enough to let a real defect through.
fn restriction_guard_config() -> ProptestConfig {
    let default = ProptestConfig::default(); // honors PROPTEST_CASES
    ProptestConfig {
        cases: default.cases.max(2048),
        ..default
    }
}

proptest! {
    // This property guards the restriction guarantee that seeded determinism
    // depends on (recalc_plan == full_plan filtered to the dirty set; see
    // DepGraph::recalc_plan). An earlier implementation re-ran Kahn on the
    // dirty subgraph; its scope-dependent tie-break violated the guarantee but
    // slipped past the default 256 cases. Run this one hot enough that
    // under-sampling cannot hide a regression.
    #![proptest_config(restriction_guard_config())]
    #[test]
    fn incremental_plan_is_subsequence_and_closed(
        ((n, adj), seeds) in digraph_strategy().prop_flat_map(|(n, adj)| {
            // Seed indices drawn against this graph's own size so no case is
            // rejected (a prop_assume here starved the 2048-case run).
            let seeds = proptest::collection::vec(0..n, 1..4);
            (Just((n, adj)), seeds)
        }),
    ) {
        let mut g = DepGraph::new();
        for (i, row) in adj.iter().enumerate() {
            let ps: Vec<Precedent> = (0..n)
                .filter(|&j| row[j])
                .map(|j| Precedent::Cell(cell(j as u32)))
                .collect();
            g.set_deps(cell(i as u32), &ps);
        }
        let full = g.full_plan(CalcSettings::default());

        let seeds: Vec<CellId> = seeds.into_iter().map(|s| cell(s as u32)).collect();
        g.mark_dirty(&seeds);
        let inc = g.recalc_plan(CalcSettings::default());

        let full_cells = plan_cells(&full);
        let inc_cells = plan_cells(&inc);

        // Sub-sequence: inc's cells appear in full in the same relative order.
        let mut it = full_cells.iter();
        for c in &inc_cells {
            prop_assert!(
                it.any(|f| f == c),
                "{c:?} out of order or missing from the full plan"
            );
        }

        // Closure: every dependent (via edges) of a scheduled cell is scheduled.
        for c in &inc_cells {
            let i = c.row as usize;
            for (j, row) in adj.iter().enumerate() {
                if row[i] && j != i {
                    prop_assert!(
                        inc_cells.contains(&cell(j as u32)),
                        "dependent {j} of scheduled {i} missing from incremental plan"
                    );
                }
            }
        }

        // Seeds that are nodes must be scheduled.
        for s in &seeds {
            prop_assert!(inc_cells.contains(s));
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Build-order determinism.
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn plan_determinism_under_build_order(
        (n, adj) in digraph_strategy(),
        seed in any::<u64>(),
    ) {
        let deps: Vec<(CellId, Vec<Precedent>)> = (0..n)
            .map(|i| {
                let ps = (0..n)
                    .filter(|&j| adj[i][j])
                    .map(|j| Precedent::Cell(cell(j as u32)))
                    .collect();
                (cell(i as u32), ps)
            })
            .collect();

        let mut g1 = DepGraph::new();
        for (c, ps) in &deps {
            g1.set_deps(*c, ps);
        }

        // Deterministic pseudo-shuffle of the insertion order from `seed`.
        let mut order: Vec<usize> = (0..n).collect();
        let mut s = seed | 1;
        for i in (1..n).rev() {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            order.swap(i, (s >> 33) as usize % (i + 1));
        }
        let mut g2 = DepGraph::new();
        for &i in &order {
            let (c, ps) = &deps[i];
            g2.set_deps(*c, ps);
        }

        prop_assert_eq!(
            g1.full_plan(CalcSettings::default()),
            g2.full_plan(CalcSettings::default())
        );
    }
}

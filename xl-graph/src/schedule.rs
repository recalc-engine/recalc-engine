//! Plan construction: the **canonical full order** over the whole graph.
//!
//! # Algorithm & why this shape
//! Recalc order is a topological sort of the graph, with cycles collapsed into
//! groups. Two well-known algorithms are combined so we get *both* cycle
//! detection *and* a canonical, deterministic linear order:
//!
//! 1. **Tarjan SCC** ([`crate::scc`], iterative) over the graph identifies
//!    every strongly-connected component — the circular-reference groups — in
//!    `O(V + E)`.
//! 2. **Kahn's algorithm** (A. B. Kahn, "Topological sorting of large
//!    networks", *CACM* 5(11), 1962) is then run over the *condensation* (the
//!    DAG of components, guaranteed acyclic). The ready set is a `BTreeSet`
//!    keyed by each component's minimum [`CellId`], so ties between independent
//!    components break by cell order — the canonical tie-break. This yields a
//!    plan that is a deterministic function of the graph: **same graph → same
//!    plan**, no `HashMap` iteration order anywhere (`implementation-plan.md`
//!    §2, "stable reduction order").
//!
//! There is deliberately **no subgraph entry point**: incremental plans are a
//! *filter* of the canonical full order (see `DepGraph::recalc_plan`), never a
//! re-run of Kahn on the dirty subgraph. The `CellId` tie-break is
//! scope-dependent — removing clean nodes from the scope changes when
//! components become ready and can flip the relative order of independent
//! cells, silently breaking the incremental-⊆-full guarantee that seeded
//! determinism relies on (`implementation-plan.md` §5 M2).
//!
//! Both passes are iterative (explicit stacks / worklists); neither recurses, so
//! a million-cell chain cannot overflow the stack.
//!
//! Complexity: `O(V + E)` for Tarjan, plus `O(C log C + E)` for the Kahn pass
//! over `C` components (the `log C` from the `BTreeSet` ready ordering), plus the
//! cost of expanding range precedents into edges (see [`crate::DepGraph`]).
//!
//! # Provenance
//! Pure graph algorithms (Tarjan 1972, Kahn 1962). The only Excel-semantic
//! choices — iterative vs non-iterative cycle handling and the iteration
//! defaults — live in [`crate::plan`] and cite the plan / Microsoft docs.

use std::collections::{BTreeMap, BTreeSet};

use crate::DepGraph;
use crate::cell::CellId;
use crate::plan::{CalcSettings, CycleGroup, IterativeGroup, Plan, Step};
use crate::scc::tarjan;

/// Build the canonical full [`Plan`] scheduling **every** node in `graph`,
/// respecting every dependency edge and grouping cycles per `settings`.
pub(crate) fn build_full_plan(graph: &DepGraph, settings: CalcSettings) -> Plan {
    // Dense indexing of all nodes. `node_ids` is sorted (BTreeMap keys), so
    // index assignment is deterministic and index order == CellId order.
    let nodes: Vec<CellId> = graph.node_ids().collect();
    let n = nodes.len();
    if n == 0 {
        return Plan::default();
    }
    // Deliberate trade-off: `idx` is lookup-only (never iterated), so a std
    // `HashMap` would be equally deterministic here and O(1) instead of
    // O(log V) per probe. We keep `BTreeMap` anyway so the crate-wide
    // "zero HashMap" invariant stays greppable — a single grep proves no map
    // iteration order can leak into a plan. Revisit only if this shows up in
    // profiles (it is a small constant next to the Tarjan + Kahn passes).
    let mut idx: BTreeMap<CellId, u32> = BTreeMap::new();
    for (i, c) in nodes.iter().enumerate() {
        idx.insert(*c, i as u32);
    }

    // Successor lists: edge precedent -> dependent (data-flow direction), so a
    // topological order visits precedents before dependents.
    let mut succ: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut self_loop = vec![false; n];

    for (i, &cell) in nodes.iter().enumerate() {
        let node = graph.node(cell).expect("scheduled cell must be a node");
        let mut add_edge = |p: CellId| {
            if let Some(&j) = idx.get(&p) {
                if j as usize == i {
                    self_loop[i] = true;
                } else {
                    succ[j as usize].push(i as u32);
                }
            }
        };
        for &p in &node.cell_precedents {
            add_edge(p);
        }
        for r in &node.range_precedents {
            for q in graph.nodes_in_range(r) {
                add_edge(q);
            }
        }
    }

    // Canonicalise adjacency (a precedent reachable via both a cell and a range
    // edge would appear twice) so Tarjan is deterministic and edge-count-explicit.
    for s in &mut succ {
        s.sort_unstable();
        s.dedup();
    }

    let sccs = tarjan(n, &succ);
    let comp = &sccs.component;
    let ncomp = sccs.members.len();

    // Condensation DAG + in-degrees, deduped.
    let mut cond_succ: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); ncomp];
    let mut indeg = vec![0u32; ncomp];
    for (i, s) in succ.iter().enumerate() {
        let ci = comp[i];
        for &j in s {
            let cj = comp[j as usize];
            if ci != cj && cond_succ[ci as usize].insert(cj) {
                indeg[cj as usize] += 1;
            }
        }
    }

    // Each component's members as sorted CellIds; first == representative.
    let mut comp_members: Vec<Vec<CellId>> = Vec::with_capacity(ncomp);
    for m in &sccs.members {
        let mut cells: Vec<CellId> = m.iter().map(|&x| nodes[x as usize]).collect();
        cells.sort_unstable();
        comp_members.push(cells);
    }

    // Kahn over the condensation, ties broken by representative CellId.
    let mut ready: BTreeSet<(CellId, u32)> = BTreeSet::new();
    for c in 0..ncomp {
        if indeg[c] == 0 {
            ready.insert((comp_members[c][0], c as u32));
        }
    }

    let mut steps: Vec<Step> = Vec::with_capacity(ncomp);
    while let Some(&(rep, c)) = ready.iter().next() {
        ready.remove(&(rep, c));
        let cu = c as usize;
        let members = &comp_members[cu];

        let is_cycle = members.len() > 1 || {
            // A singleton is a cycle only if it self-references.
            let only = members[0];
            self_loop[idx[&only] as usize]
        };
        if !is_cycle {
            steps.push(Step::Eval(members[0]));
        } else if settings.iterate {
            steps.push(Step::Iterate(IterativeGroup {
                members: members.clone(),
                settings,
            }));
        } else {
            steps.push(Step::Cycle(CycleGroup {
                members: members.clone(),
            }));
        }

        for &d in &cond_succ[cu] {
            let du = d as usize;
            indeg[du] -= 1;
            if indeg[du] == 0 {
                ready.insert((comp_members[du][0], d));
            }
        }
    }

    Plan { steps }
}

/// Antichain **wave index** for each step of `plan`, parallel to `plan.steps`.
///
/// `wave[i]` is `0` when `plan.steps[i]` has no *scheduled* precedent (no
/// precedent that also appears in `plan`); otherwise it is
/// `1 + max(wave[j])` over the steps `j` scheduling this step's precedents.
/// Cells that share a wave are **mutually independent** — none is a precedent
/// of another — so they are safe to evaluate concurrently; ascending wave
/// order respects every dependency edge (RFC-0014).
///
/// This is a pure, additive read layered over an existing [`Plan`]: it does
/// **not** alter the canonical order [`build_full_plan`] produced. It is a
/// deterministic function of `(graph, plan)` — the precedent scan reuses the
/// exact cell + range edge set the plan builder used (range precedents
/// expanded through [`DepGraph::nodes_in_range`]), so no reader read via a
/// range is ever mis-levelled.
///
/// Works for a full plan or an incremental (dirty-filtered) one: a precedent
/// absent from `plan` (already-committed clean value) simply does not raise
/// the wave, so the level structure is relative to the plan's own scope —
/// exactly what a parallel execution of that plan requires.
///
/// # Complexity
/// `O(V + E)` over the plan's cells — one pass, matching plan construction.
pub(crate) fn plan_waves(graph: &DepGraph, plan: &Plan) -> Vec<u32> {
    let steps = &plan.steps;
    if steps.is_empty() {
        return Vec::new();
    }

    // Map every scheduled cell -> its step index. Cycle/iterate members all map
    // to their group's single step, so an intra-group precedent resolves to the
    // same index (excluded below via the `!= i` guard).
    let mut step_of: BTreeMap<CellId, usize> = BTreeMap::new();
    for (i, s) in steps.iter().enumerate() {
        match s {
            Step::Eval(c) => {
                step_of.insert(*c, i);
            }
            Step::Cycle(g) => {
                for &m in &g.members {
                    step_of.insert(m, i);
                }
            }
            Step::Iterate(g) => {
                for &m in &g.members {
                    step_of.insert(m, i);
                }
            }
        }
    }

    let mut wave = vec![0u32; steps.len()];
    // Steps are in canonical topological order, so every scheduled precedent of
    // step `i` sits at an earlier index and its wave is already finalized.
    for i in 0..steps.len() {
        let members: &[CellId] = match &steps[i] {
            Step::Eval(c) => std::slice::from_ref(c),
            Step::Cycle(g) => &g.members,
            Step::Iterate(g) => &g.members,
        };
        let mut w = 0u32;
        for &cell in members {
            let Some(node) = graph.node(cell) else {
                continue;
            };
            // A precedent scheduled by an earlier step raises this step's wave;
            // one absent from the plan (clean / already committed) or inside the
            // same cycle group (`j == i`) does not.
            for &p in &node.cell_precedents {
                if let Some(&j) = step_of.get(&p)
                    && j != i
                {
                    w = w.max(wave[j] + 1);
                }
            }
            for r in &node.range_precedents {
                for q in graph.nodes_in_range(r) {
                    if let Some(&j) = step_of.get(&q)
                        && j != i
                    {
                        w = w.max(wave[j] + 1);
                    }
                }
            }
        }
        wave[i] = w;
    }
    wave
}

//! Iterative Tarjan strongly-connected-components.
//!
//! # Algorithm & why iterative
//! Tarjan's SCC algorithm (R. Tarjan, "Depth-first search and linear graph
//! algorithms", *SIAM J. Comput.* 1(2), 1972) finds every SCC in `O(V + E)` in
//! a single depth-first pass. The textbook form is recursive; a workbook can
//! chain **1,000,000+** cells, so a recursive DFS would blow the call stack —
//! the exact stack-overflow class of the parser recursion bug fixed in commit
//! `afb5141`. This implementation is therefore **iterative**, driving the DFS
//! from an explicit `Vec` stack of `(node, next-neighbour-index)` frames. No
//! call-stack recursion appears anywhere in it.
//!
//! Determinism: components are computed over adjacency lists the caller has
//! sorted, and the DFS visits start nodes `0..n` in order, so the decomposition
//! is a deterministic function of the (canonically ordered) input graph. Tarjan
//! emits components in reverse topological order of the condensation; the caller
//! ([`crate::schedule`]) re-derives a canonical linear order from the
//! condensation, so this reverse order is not relied upon.
//!
//! # Provenance
//! Pure graph algorithm; no Excel semantics. Cited to Tarjan 1972.

/// Sentinel for "not yet visited" in the DFS index array. `u32::MAX` nodes are
/// not addressable (a sheet is at most `2^20` rows × `2^14` cols, and a scheduled
/// set is bounded by node count), so this never collides with a real index.
const UNVISITED: u32 = u32::MAX;

/// Result of an SCC decomposition.
pub(crate) struct Sccs {
    /// `component[v]` = id of the component containing node `v`.
    pub(crate) component: Vec<u32>,
    /// Components, each a list of member node indices. Membership order is
    /// unspecified here; the caller sorts by [`crate::cell::CellId`].
    pub(crate) members: Vec<Vec<u32>>,
}

/// Compute the SCCs of the digraph on nodes `0..n` with successor lists `succ`
/// (each `succ[v]` sorted and deduplicated by the caller).
pub(crate) fn tarjan(n: usize, succ: &[Vec<u32>]) -> Sccs {
    let mut index = vec![UNVISITED; n]; // DFS discovery index
    let mut low = vec![0u32; n]; // low-link
    let mut on_stack = vec![false; n]; // membership of the SCC stack
    let mut component = vec![UNVISITED; n];
    let mut members: Vec<Vec<u32>> = Vec::new();

    let mut scc_stack: Vec<u32> = Vec::new(); // Tarjan's node stack
    let mut call: Vec<(u32, u32)> = Vec::new(); // explicit DFS: (node, next child)
    let mut counter: u32 = 0;

    for start in 0..n as u32 {
        if index[start as usize] != UNVISITED {
            continue;
        }
        // Push `start`.
        index[start as usize] = counter;
        low[start as usize] = counter;
        counter += 1;
        scc_stack.push(start);
        on_stack[start as usize] = true;
        call.push((start, 0));

        while let Some(&(v, child)) = call.last() {
            let vu = v as usize;
            if (child as usize) < succ[vu].len() {
                let w = succ[vu][child as usize];
                // Advance v's cursor before descending.
                call.last_mut().unwrap().1 = child + 1;
                let wu = w as usize;
                if index[wu] == UNVISITED {
                    index[wu] = counter;
                    low[wu] = counter;
                    counter += 1;
                    scc_stack.push(w);
                    on_stack[wu] = true;
                    call.push((w, 0));
                } else if on_stack[wu] && index[wu] < low[vu] {
                    low[vu] = index[wu];
                }
            } else {
                // All neighbours of v explored.
                if low[vu] == index[vu] {
                    // v is an SCC root: pop the component off the SCC stack.
                    let cid = members.len() as u32;
                    let mut scc = Vec::new();
                    loop {
                        let x = scc_stack.pop().unwrap();
                        on_stack[x as usize] = false;
                        component[x as usize] = cid;
                        scc.push(x);
                        if x == v {
                            break;
                        }
                    }
                    members.push(scc);
                }
                call.pop();
                // Relax the parent's low-link with v's.
                if let Some(&(parent, _)) = call.last() {
                    let pu = parent as usize;
                    if low[vu] < low[pu] {
                        low[pu] = low[vu];
                    }
                }
            }
        }
    }

    Sccs { component, members }
}

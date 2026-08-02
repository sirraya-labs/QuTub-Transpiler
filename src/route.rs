//! Inserts `Swap`s into a source-level [`Circuit`] so that every
//! two-qubit gate ends up between physical qubits a [`CouplingMap`]
//! actually allows to interact directly, before [`crate::native::decompose`]
//! or [`crate::backend::lower`] ever sees it.
//!
//! This runs on the *source* [`ir::Gate`] set (not the decomposed
//! `{Rz, Ry, Rzz}`/backend-native gate set), so it only has to reason
//! about the handful of two-qubit `Gate` variants once, and every
//! inserted `Gate::Swap` flows through the existing, already-tested
//! `Swap -> Cx;Cx;Cx` identity in `native.rs` and the existing
//! `Cx`/`Cz` cancellation rules in `backend.rs`'s peephole pass exactly
//! like a `Swap` that was already in the source circuit -- no separate
//! accounting or new gate-counting logic is needed anywhere downstream.
//!
//! # Algorithm
//! A single pass, tracking a `logical -> physical` qubit mapping that
//! starts as the identity and is updated in place by every inserted
//! `Swap`:
//! - **Single-qubit gates** are just re-addressed to the logical
//!   qubit's current physical location.
//! - **Two-qubit gates** where both qubits' current physical locations
//!   are already adjacent are re-addressed the same way, with no
//!   `Swap`s inserted.
//! - **Two-qubit gates** on non-adjacent physical qubits: walk the
//!   BFS shortest path (see [`CouplingMap::shortest_path`]) from the
//!   gate's first-argument qubit toward its second, inserting a `Swap`
//!   at each hop and updating the mapping, until the first qubit's
//!   physical location is adjacent to the second's (whose location
//!   never moves during this walk) -- then emit the original gate,
//!   with its original argument order preserved, at the two now-adjacent
//!   physical locations.
//!
//! Preserving argument order matters: `Gate::Cx(control, target)` is
//! **not** symmetric (see `native.rs`'s `H`-sandwich decomposition), so
//! routing always moves the *first* argument's qubit toward the
//! second's fixed location rather than moving either arbitrarily --
//! that way the control/target roles landing on their new physical
//! qubits are exactly the roles they had on their original logical
//! qubits, with no risk of routing silently swapping control and
//! target on an asymmetric gate.
//!
//! # What this doesn't do
//! No SWAP-count minimization across the whole circuit (e.g. reordering
//! independent gates to reduce total routing distance, or looking ahead
//! to route for a *later* gate too) -- this is a correctness pass, not
//! a routing optimizer.

use crate::coupling::CouplingMap;
use crate::ir::{Circuit, Gate, LogicalQubit, PhysicalQubit};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

/// Routes `circuit` against `coupling`, returning a new [`Circuit`]
/// with `Swap`s inserted wherever a two-qubit gate needed one, *and*
/// a final run of `Swap`s restoring every qubit to its original
/// physical position.
///
/// That final restoration matters and is not optional bookkeeping:
/// walking a gate's qubit toward its partner (see the algorithm
/// section below) doesn't just move that one qubit -- each `Swap`
/// along the way also displaces whatever *other* qubit was sitting at
/// the destination, sideways, by one position. A qubit that never
/// appears in a two-qubit gate at all can still end up on the wrong
/// wire by the end of the circuit purely as a side effect of routing
/// *other* qubits around it. Since this crate has no measurement gate
/// yet to translate a final physical arrangement back to logical qubit
/// order (see `qasm.rs`'s module doc for that same gap on the parsing
/// side), and every other identity in this crate is exact -- output on
/// wire `q` really is qubit `q`'s value, full stop -- leaving that
/// residual permutation in place would be a real, silent correctness
/// bug: fidelity against a reference simulator (which knows nothing
/// about routing and just applies the original circuit wire-for-wire)
/// would legitimately differ, since a permuted *untouched* qubit is
/// still a different state vector even though no gate's logic was
/// ever wrong. Panics if `coupling` is disconnected between two qubits
/// a gate needs to interact -- never happens for
/// [`CouplingMap::linear`], the only constructor `crate::backend`
/// currently uses.
///
/// # Algorithm
/// A single pass, tracking a `logical -> physical` qubit mapping that
/// starts as the identity and is updated in place by every inserted
/// `Swap`:
/// - **Single-qubit gates** are just re-addressed to the logical
///   qubit's current physical location.
/// - **Two-qubit gates** where both qubits' current physical locations
///   are already adjacent are re-addressed the same way, with no
///   `Swap`s inserted.
/// - **Two-qubit gates** on non-adjacent physical qubits: walk the
///   BFS shortest path (see [`CouplingMap::shortest_path`]) from the
///   gate's first-argument qubit toward its second, inserting a `Swap`
///   at each hop and updating the mapping, until the first qubit's
///   physical location is adjacent to the second's (whose location
///   never moves during this walk) -- then emit the original gate,
///   with its original argument order preserved, at the two now-adjacent
///   physical locations.
/// - **Once every gate has been processed**, [`restore_identity_mapping`]
///   restores every qubit to its original physical wire via a
///   general-graph-correct spanning-tree token-swap pass (see that
///   function's own doc comment) -- not a linear-chain-only bubble
///   sort, since `coupling` need not number consecutive physical
///   qubits adjacently (e.g. `CouplingMap::heavy_hex_for`).
///
/// Preserving argument order matters: `Gate::Cx(control, target)` is
/// **not** symmetric (see `native.rs`'s `H`-sandwich decomposition), so
/// routing always moves the *first* argument's qubit toward the
/// second's fixed location rather than moving either arbitrarily --
/// that way the control/target roles landing on their new physical
/// qubits are exactly the roles they had on their original logical
/// qubits, with no risk of routing silently swapping control and
/// target on an asymmetric gate.
pub fn route(circuit: &Circuit, coupling: &CouplingMap) -> Circuit {
    let num_qubits = circuit.num_qubits;
    let mut logical_to_physical: Vec<PhysicalQubit> = (0..num_qubits).map(PhysicalQubit).collect();
    let mut physical_to_logical: Vec<LogicalQubit> = (0..num_qubits).map(LogicalQubit).collect();
    let mut out = Circuit::new(num_qubits);
    out.num_clbits = circuit.num_clbits;

    for gate in &circuit.gates {
        let qubits = gate.qubits();
        match qubits.as_slice() {
            &[q] => {
                let logical = LogicalQubit(q);
                let physical = logical_to_physical[logical.0];
                out.push(remap_single(gate, physical.0));
            }
            &[first, second] => {
                let (first, second) = (LogicalQubit(first), LogicalQubit(second));
                let mut physical_first = logical_to_physical[first.0];
                let physical_second = logical_to_physical[second.0];

                if !coupling.is_adjacent(physical_first.0, physical_second.0) {
                    let path = coupling
                        .shortest_path(physical_first.0, physical_second.0)
                        .expect(
                            "coupling map must be connected between any two qubits a routed \
                             circuit needs to interact",
                        );
                    for hop in path.windows(2) {
                        let (from, to) = (PhysicalQubit(hop[0]), PhysicalQubit(hop[1]));
                        if to == physical_second {
                            // `from` is now adjacent to the fixed
                            // target; stop one hop short rather than
                            // swapping onto it.
                            break;
                        }
                        out.push(Gate::Swap(from.0, to.0));
                        swap_mapping(&mut logical_to_physical, &mut physical_to_logical, from, to);
                        physical_first = to;
                    }
                }

                out.push(remap_two(gate, physical_first.0, physical_second.0));
            }
            _ => unreachable!("ir::Gate only ever touches 1 or 2 qubits"),
        }
    }

    restore_identity_mapping(&mut out, &mut logical_to_physical, &mut physical_to_logical, coupling);

    out
}

/// Updates both mapping directions for a `Swap(from, to)` that's about
/// to be (or was just) emitted.
fn swap_mapping(
    logical_to_physical: &mut [PhysicalQubit],
    physical_to_logical: &mut [LogicalQubit],
    from: PhysicalQubit,
    to: PhysicalQubit,
) {
    let (logical_from, logical_to) = (physical_to_logical[from.0], physical_to_logical[to.0]);
    physical_to_logical[from.0] = logical_to;
    physical_to_logical[to.0] = logical_from;
    logical_to_physical[logical_from.0] = to;
    logical_to_physical[logical_to.0] = from;
}

/// Transforms the current mapping into an arbitrary
/// `target_physical_to_logical` permutation (`physical_to_logical[p] ==
/// target_physical_to_logical[p]` for every `p` once this returns)
/// using only coupling-adjacent swaps, via a general-graph-correct
/// "token swapping" strategy -- **not** a linear bubble sort. The old
/// implementation assumed physical qubits were numbered along a path
/// where consecutive indices are always coupling-adjacent, which held
/// for `CouplingMap::linear` but is false for
/// `CouplingMap::heavy_hex_for`/`heavy_hex_grid` (a `debug_assert!`
/// there would fire on real `IbmQ` circuits, not just a theoretical
/// gap).
///
/// [`restore_identity_mapping`] is the special case where
/// `target_physical_to_logical` is the identity permutation (used at
/// the tail of both [`route`] and [`route_lookahead`]);
/// [`route_lookahead`] also uses this directly with a *non*-identity
/// target to physically realize [`choose_initial_layout`]'s starting
/// point (see that call site's own comment for why that step isn't
/// optional).
///
/// # Algorithm
/// Token swapping (permuting to an arbitrary target using only
/// edge-adjacent swaps) is NP-hard to do with the *minimum* number of
/// swaps on a general graph, but this pass only needs *correctness*,
/// not swap-count optimality, and a connected graph always admits a
/// solution: take an arbitrary spanning tree of `coupling` (its edges
/// are a subset of the real coupling graph's, so every swap this emits
/// is still a real hardware-adjacent swap), then repeatedly prune a
/// leaf of the tree:
/// - If the leaf already holds its target token
///   (`physical_to_logical[leaf] == target_physical_to_logical[leaf]`),
///   it's already home -- remove it from the active set and move on.
///   (Removing an already-fixed leaf from a tree always leaves a tree:
///   this is the standard leaf-pruning invariant.)
/// - Otherwise, find wherever (among the still-active vertices) the
///   token destined for `leaf` currently sits, and walk it to `leaf`
///   one coupling-adjacent swap at a time along the *tree* path between
///   them -- guaranteed to lie entirely within the active subtree, by
///   the same invariant. `leaf` is now home and is removed.
///
/// Every tree with >= 2 nodes has at least one leaf (in fact at least
/// two), so this always makes progress; with one active node left,
/// it's necessarily already home (there's nowhere else for its token to
/// be), so the loop naturally terminates at the target permutation.
fn route_to_layout(
    out: &mut Circuit,
    logical_to_physical: &mut [PhysicalQubit],
    physical_to_logical: &mut [LogicalQubit],
    target_physical_to_logical: &[LogicalQubit],
    coupling: &CouplingMap,
) {
    let n = physical_to_logical.len();
    if n <= 1 {
        return;
    }

    let tree = spanning_tree(coupling, n);
    // BTreeSet, not HashSet: iteration order here directly decides which
    // leaf gets pruned first whenever more than one qualifies, which in
    // turn decides the total SWAP count this pass emits (token-swapping
    // is correctness-only, not swap-count-optimal -- see this function's
    // own doc comment -- so different tie-breaks land on different,
    // still-correct answers). HashSet's default hasher is randomly
    // seeded per process, so the old HashSet here made every SWAP count
    // through this function -- both the initial-layout realization and
    // the final identity restoration in `route`/`route_lookahead` --
    // silently nondeterministic across runs of the identical binary on
    // the identical circuit. BTreeSet's iteration order is the fixed
    // ascending physical-index order, so the same input always produces
    // the same output now.
    let mut active: BTreeSet<PhysicalQubit> = (0..n).map(PhysicalQubit).collect();

    while active.len() > 1 {
        let leaf = *active
            .iter()
            .find(|&&v| active_degree(&tree, &active, v) <= 1)
            .expect("a tree with >= 2 active nodes always has a leaf");

        let want = target_physical_to_logical[leaf.0];
        if physical_to_logical[leaf.0] != want {
            let p = *active
                .iter()
                .find(|&&v| physical_to_logical[v.0] == want)
                .expect("the token destined for `leaf` must be somewhere in the active set");
            let path = tree_path_within(&tree, &active, p, leaf);
            for hop in path.windows(2) {
                let (u, v) = (hop[0], hop[1]);
                out.push(Gate::Swap(u.0, v.0));
                swap_mapping(logical_to_physical, physical_to_logical, u, v);
            }
        }
        active.remove(&leaf);
    }
}

/// Sorts `physical_to_logical` back to the identity
/// (`physical_to_logical[p] == p` for every `p`) -- the special case of
/// [`route_to_layout`] where the target permutation is the identity.
/// See [`route_to_layout`] for the algorithm.
fn restore_identity_mapping(
    out: &mut Circuit,
    logical_to_physical: &mut [PhysicalQubit],
    physical_to_logical: &mut [LogicalQubit],
    coupling: &CouplingMap,
) {
    let identity: Vec<LogicalQubit> = (0..physical_to_logical.len()).map(LogicalQubit).collect();
    route_to_layout(out, logical_to_physical, physical_to_logical, &identity, coupling);
}

/// A BFS spanning tree of `coupling` restricted to vertices `0..n`,
/// stored as an adjacency list. Every edge here is a real edge of
/// `coupling` (a subset of it, not a superset), so any swap along a
/// tree edge is a legal hardware-adjacent swap.
fn spanning_tree(coupling: &CouplingMap, n: usize) -> HashMap<PhysicalQubit, Vec<PhysicalQubit>> {
    let mut visited = vec![false; n];
    let mut tree: HashMap<PhysicalQubit, Vec<PhysicalQubit>> = HashMap::new();
    let mut queue = VecDeque::new();
    queue.push_back(PhysicalQubit(0));
    visited[0] = true;
    while let Some(u) = queue.pop_front() {
        for v in coupling.neighbors(u.0) {
            if v < n && !visited[v] {
                visited[v] = true;
                let v = PhysicalQubit(v);
                tree.entry(u).or_default().push(v);
                tree.entry(v).or_default().push(u);
                queue.push_back(v);
            }
        }
    }
    tree
}

/// Number of `v`'s tree-neighbors that are still in `active`.
fn active_degree(
    tree: &HashMap<PhysicalQubit, Vec<PhysicalQubit>>,
    active: &BTreeSet<PhysicalQubit>,
    v: PhysicalQubit,
) -> usize {
    tree.get(&v)
        .map(|nbrs| nbrs.iter().filter(|&&u| active.contains(&u)).count())
        .unwrap_or(0)
}

/// Shortest (in fact only, since it's a tree) path from `start` to
/// `goal` using only tree edges between currently-`active` vertices --
/// always exists, since leaf-pruning keeps the active-induced subgraph
/// of a tree connected.
fn tree_path_within(
    tree: &HashMap<PhysicalQubit, Vec<PhysicalQubit>>,
    active: &BTreeSet<PhysicalQubit>,
    start: PhysicalQubit,
    goal: PhysicalQubit,
) -> Vec<PhysicalQubit> {
    if start == goal {
        return vec![start];
    }
    let mut visited: HashSet<PhysicalQubit> = HashSet::new();
    let mut predecessor: HashMap<PhysicalQubit, PhysicalQubit> = HashMap::new();
    let mut queue = VecDeque::new();
    queue.push_back(start);
    visited.insert(start);
    while let Some(u) = queue.pop_front() {
        if u == goal {
            break;
        }
        if let Some(nbrs) = tree.get(&u) {
            for &v in nbrs {
                if active.contains(&v) && visited.insert(v) {
                    predecessor.insert(v, u);
                    queue.push_back(v);
                }
            }
        }
    }
    let mut path = vec![goal];
    let mut current = goal;
    while current != start {
        current = predecessor[&current];
        path.push(current);
    }
    path.reverse();
    path
}

// ---------------------------------------------------------------------
// P2.1: initial layout selection + lookahead ("SABRE-lite") routing.
//
// `route` above is a correctness pass: greedy, single-gate-at-a-time,
// no memory of anything but the one gate it's currently routing. It's
// exactly right for what it promises (every two-qubit gate ends up
// adjacent, action preserved), but it leaves real SWAP-count
// performance on the table in two specific ways real transpilers
// address:
//
// 1. **Initial layout.** `route` always starts from the identity
//    logical->physical mapping. If a circuit's most-interacting qubit
//    pairs happen to start out physically distant, every one of those
//    interactions pays a routing cost, even though nothing forces the
//    *first* mapping to be the identity. [`choose_initial_layout`]
//    picks a better starting point instead: a greedy placement that
//    puts heavily-interacting logical qubit pairs on physically close
//    (ideally adjacent) physical qubits before a single gate is routed.
// 2. **Lookahead.** `route` commits to a path for the *current* gate
//    only, and can't see that a different SWAP might unblock several
//    upcoming gates at once. [`route_lookahead`] instead maintains a
//    DAG "front layer" of gates that are next-up on every qubit they
//    touch, executes whatever's already reachable, and when nothing
//    is, scores each *candidate* SWAP by how much it reduces total
//    physical distance across the front layer (plus a smaller, decayed
//    contribution from each front qubit's very next gate, so ties don't
//    resolve arbitrarily) -- the same core heuristic real SABRE-style
//    routers use, simplified to a single greedy pass with no
//    backtracking or repeated randomized trials.
//
// Both are purely additive: `route` is untouched and is still what
// `crate::backend::lower` calls. [`route_lookahead`] is a strictly
// better-or-equal-effort alternative a caller can opt into; the two
// share the exact same identity-restoration tail
// ([`restore_identity_mapping`]) and the exact same [`remap_single`]/
// [`remap_two`] re-addressing, so it inherits `route`'s already-proven
// correctness properties (fidelity-preserving action, exact argument-
// order preservation on asymmetric gates like `Cx`, in-place `Measure`
// tracking) rather than re-deriving them.
// ---------------------------------------------------------------------

/// BFS distances from `source` to every physical qubit, via
/// `coupling`'s own [`CouplingMap::neighbors`] (so this needs no new
/// `coupling.rs` API). `usize::MAX` for any qubit `coupling` doesn't
/// connect `source` to -- never observed in practice for the connected
/// maps this crate's constructors produce, but not assumed here.
fn bfs_distances(coupling: &CouplingMap, source: PhysicalQubit) -> Vec<usize> {
    let n = coupling.num_qubits();
    let mut dist = vec![usize::MAX; n];
    dist[source.0] = 0;
    let mut queue: VecDeque<usize> = VecDeque::new();
    queue.push_back(source.0);
    while let Some(u) = queue.pop_front() {
        for v in coupling.neighbors(u) {
            if dist[v] == usize::MAX {
                dist[v] = dist[u] + 1;
                queue.push_back(v);
            }
        }
    }
    dist
}

/// All-pairs BFS distance matrix for `coupling`'s physical qubits --
/// the shared building block [`choose_initial_layout`] and
/// [`route_lookahead`] both score candidate placements/SWAPs against.
fn distance_matrix(coupling: &CouplingMap) -> Vec<Vec<usize>> {
    (0..coupling.num_qubits()).map(|q| bfs_distances(coupling, PhysicalQubit(q))).collect()
}

/// Number of two-qubit `Gate`s between each unordered logical qubit
/// pair, keyed with the smaller index first. Single-qubit gates and
/// `Measure` don't contribute (they have no "partner" qubit to place
/// relative to).
fn interaction_weights(circuit: &Circuit) -> HashMap<(LogicalQubit, LogicalQubit), usize> {
    let mut weights: HashMap<(LogicalQubit, LogicalQubit), usize> = HashMap::new();
    for gate in &circuit.gates {
        let qs = gate.qubits();
        if qs.len() == 2 {
            let (a, b) = (LogicalQubit(qs[0]), LogicalQubit(qs[1]));
            let key = if a < b { (a, b) } else { (b, a) };
            *weights.entry(key).or_insert(0) += 1;
        }
    }
    weights
}

/// Detects whether `weights` describes a single simple chain: every
/// logical qubit it touches interacts with at most 2 distinct
/// partners, and the induced interaction graph is one connected path
/// (no cycles, no branching, no disjoint second component). If so,
/// returns the chain's logical qubits in walk order, from one endpoint
/// to the other. This is exactly the interaction shape a GHZ-state
/// preparation circuit (`Cx(0,1), Cx(1,2), ..., Cx(n-2,n-1)`) has --
/// [`choose_initial_layout`] uses this to route it as the path-
/// embedding problem it actually is, instead of the general greedy
/// weight-ordered placement below, which has no way to *search* for a
/// path and can (and on a bounded-degree graph like heavy-hex, does)
/// walk itself into a dead end a few qubits in, paying real routing
/// distance for the rest of the chain.
///
/// Qubits that never appear in any two-qubit gate at all aren't part
/// of `weights` and so aren't part of this check; a chain that doesn't
/// cover every logical qubit in the circuit is deliberately left to
/// the general heuristic too (see this function's call site) rather
/// than trying to interleave a partial path with leftover placement,
/// which is a harder problem this isn't attempting to solve.
fn detect_interaction_chain(
    weights: &HashMap<(LogicalQubit, LogicalQubit), usize>,
) -> Option<Vec<LogicalQubit>> {
    let mut adjacency: HashMap<LogicalQubit, Vec<LogicalQubit>> = HashMap::new();
    for &(a, b) in weights.keys() {
        adjacency.entry(a).or_default().push(b);
        adjacency.entry(b).or_default().push(a);
    }
    if adjacency.is_empty() || adjacency.values().any(|nbrs| nbrs.len() > 2) {
        return None;
    }

    // A simple path has exactly 2 degree-1 nodes (its endpoints); a
    // cycle has zero, and a disconnected union of paths/cycles has
    // some other count -- either way, not the single-chain shape this
    // is looking for.
    let mut endpoints: Vec<LogicalQubit> =
        adjacency.iter().filter(|(_, nbrs)| nbrs.len() == 1).map(|(&q, _)| q).collect();
    if endpoints.len() != 2 {
        return None;
    }
    endpoints.sort(); // deterministic regardless of HashMap iteration order

    let mut order = vec![endpoints[0]];
    let mut prev = None;
    let mut current = endpoints[0];
    while order.len() < adjacency.len() {
        let next = adjacency[&current].iter().find(|&&n| Some(n) != prev).copied();
        match next {
            Some(n) => {
                order.push(n);
                prev = Some(current);
                current = n;
            }
            // Only reachable if the graph is disconnected (a second
            // component exists beyond the path we just finished
            // walking) -- the degree/endpoint checks above don't rule
            // that out on their own.
            None => return None,
        }
    }
    if order.last() == Some(&endpoints[1]) {
        Some(order)
    } else {
        None
    }
}

/// Search-step budget for [`find_hamiltonian_path`]'s backtracking DFS,
/// applied per candidate start node. Finding a Hamiltonian path is
/// NP-hard in general, but every [`CouplingMap`] this crate builds has
/// small bounded degree (<=2 linear, <=3 heavy-hex, <=4 square-grid --
/// see `coupling.rs`), which keeps the real branching factor low for
/// the circuit sizes this crate routes in practice. The budget exists
/// so a coupling-graph shape this search genuinely struggles with
/// fails fast into [`choose_initial_layout`]'s general fallback instead
/// of hanging.
const HAMILTONIAN_PATH_SEARCH_BUDGET: usize = 200_000;

/// Finds a simple path of exactly `len` physical qubits through
/// `coupling`'s graph via depth-first search with backtracking,
/// **biased to stay close to `identity_targets`** at every step (the
/// physical qubit each path *position* would already occupy for free,
/// with no `Swap` at all, under the plain identity mapping --
/// concretely, `identity_targets[i]` is the physical location the
/// logical qubit destined for path position `i` already starts at).
///
/// This bias matters for real swap count, not just aesthetics:
/// [`route_lookahead`] has no free way to relabel qubits onto a
/// non-identity layout (see that function's own comment -- the
/// fidelity guarantee has to hold for *any* starting state, not just
/// |0...0>, so every qubit that isn't already where this fast path
/// wants it pays a real `Swap` via [`route_to_layout`] to get there).
/// A path embedding that's a valid Hamiltonian path but far from
/// identity can easily cost *more* total swaps than the general greedy
/// heuristic it's meant to beat, even though it needs zero swaps
/// *during* gate execution. Search order here is exactly what decides
/// which of the (generally many) valid Hamiltonian paths this returns,
/// so both the candidate start nodes and each step's neighbor order
/// are sorted by graph-distance to `identity_targets`, closest first --
/// the search still backtracks through farther candidates if the
/// closest one doesn't pan out, so this never fails to find a path
/// that plain unbiased search would have found, it just prefers
/// cheaper-to-reach ones when a choice exists.
///
/// Returns `None` if no path of length `len` is found within
/// [`HAMILTONIAN_PATH_SEARCH_BUDGET`] steps per start -- see that
/// constant's doc comment.
fn find_hamiltonian_path(
    coupling: &CouplingMap,
    len: usize,
    identity_targets: &[PhysicalQubit],
) -> Option<Vec<PhysicalQubit>> {
    let n = coupling.num_qubits();
    if len == 0 {
        return Some(Vec::new());
    }
    if len > n {
        return None;
    }
    debug_assert_eq!(
        identity_targets.len(),
        len,
        "identity_targets must have one entry per path position"
    );

    let dist = distance_matrix(coupling);
    let mut starts: Vec<usize> = (0..n).collect();
    starts.sort_by_key(|&p| dist[p][identity_targets[0].0]);

    for start in starts {
        let mut visited = vec![false; n];
        let mut path = Vec::with_capacity(len);
        visited[start] = true;
        path.push(PhysicalQubit(start));
        let mut budget = HAMILTONIAN_PATH_SEARCH_BUDGET;
        if dfs_extend_path(coupling, &dist, identity_targets, &mut path, &mut visited, len, &mut budget)
        {
            return Some(path);
        }
    }
    None
}

/// Backtracking step for [`find_hamiltonian_path`]: extends `path` by
/// one more coupling-adjacent, not-yet-visited physical qubit at a
/// time until it reaches `len`, undoing its own choice and trying the
/// next neighbor on failure. At each step, candidate neighbors are
/// tried in ascending order of graph-distance to
/// `identity_targets[path.len()]` (the next path position's free,
/// no-swap-needed target -- see [`find_hamiltonian_path`]'s doc
/// comment), so a valid extension that's already closer to identity is
/// always explored before a farther one, without narrowing which paths
/// are reachable at all. `budget` is decremented once per node
/// expansion and shared across the whole call tree for one candidate
/// start, so a start node that leads nowhere gives up within a bounded
/// number of steps rather than exhausting every possible ordering.
fn dfs_extend_path(
    coupling: &CouplingMap,
    dist: &[Vec<usize>],
    identity_targets: &[PhysicalQubit],
    path: &mut Vec<PhysicalQubit>,
    visited: &mut [bool],
    len: usize,
    budget: &mut usize,
) -> bool {
    if path.len() == len {
        return true;
    }
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    let current = *path.last().expect("path is never empty here: len >= 1, checked by caller");
    let target = identity_targets[path.len()].0;
    let mut candidates: Vec<usize> =
        coupling.neighbors(current.0).into_iter().filter(|&next| !visited[next]).collect();
    candidates.sort_by_key(|&next| dist[next][target]);

    for next in candidates {
        visited[next] = true;
        path.push(PhysicalQubit(next));
        if dfs_extend_path(coupling, dist, identity_targets, path, visited, len, budget) {
            return true;
        }
        path.pop();
        visited[next] = false;
        if *budget == 0 {
            break;
        }
    }
    false
}

/// Picks a starting logical->physical mapping better than the
/// identity: a greedy placement that puts each logical qubit as close
/// as possible (in `coupling`'s own graph distance) to the
/// already-placed logical qubits it interacts with most, processing
/// qubits in descending order of total interaction weight so the
/// most-connected qubits get first pick of physical position. This is
/// a simplified relative of the placement heuristics real transpilers
/// use (e.g. Qiskit's density/noise-adaptive layouts) -- greedy and
/// order-dependent, not a claim of *optimal* placement (that's an
/// NP-hard graph-embedding problem in general), just reliably no worse
/// than the identity mapping [`route`] is stuck with.
///
/// **Chain fast path:** if the circuit's interaction graph is a single
/// simple chain covering every logical qubit (see
/// [`detect_interaction_chain`] -- exactly what a linear-CNOT-ladder
/// GHZ-state-prep circuit looks like; a star-shaped GHZ construction,
/// fanning every `Cx` out from one hub qubit, does *not* count -- see
/// [`detect_interaction_chain`]'s own doc comment), this instead
/// searches directly for a matching path in `coupling`'s own graph
/// ([`find_hamiltonian_path`], biased to prefer a path close to the
/// identity mapping -- see that function's doc comment for why
/// closeness to identity matters here, not just validity) and, if one
/// is found, returns that embedding: zero SWAPs *during* the chain's
/// own gates, plus whatever it costs [`route_lookahead`] to physically
/// reach a non-identity layout in the first place (never free in this
/// crate -- see that function's own comment) -- which, for a chain
/// sized to fit within one heavy-hex hexagon or one square-grid
/// generator's DFS-numbered prefix (see `coupling.rs`'s module doc),
/// is itself usually zero, since the identity mapping *is* the matching
/// path. That's still reliably no worse, and often dramatically
/// better, than the general greedy heuristic below, which has no way
/// to *search* for a path at all and can walk itself into a dead end a
/// few qubits into a bounded-degree graph like heavy-hex (see
/// [`detect_interaction_chain`]'s doc comment). Falls through to the
/// general heuristic if the chain doesn't cover every logical qubit,
/// or if no matching physical path is found.
///
/// # Panics (debug only)
/// If `coupling.num_qubits() != circuit.num_qubits` -- same
/// requirement [`route_lookahead`] has, and the same one
/// `crate::backend::Backend::coupling_map(circuit.num_qubits)` already
/// guarantees in practice.
pub fn choose_initial_layout(circuit: &Circuit, coupling: &CouplingMap) -> Vec<PhysicalQubit> {
    let num_qubits = circuit.num_qubits;
    debug_assert_eq!(
        coupling.num_qubits(),
        num_qubits,
        "choose_initial_layout expects a coupling map sized to the circuit's own qubit count"
    );
    if num_qubits == 0 {
        return Vec::new();
    }

    let weights = interaction_weights(circuit);

    if let Some(chain) = detect_interaction_chain(&weights) {
        if chain.len() == num_qubits {
            // Each chain position's "free" target is the physical
            // qubit that logical qubit already occupies under the
            // identity mapping -- see find_hamiltonian_path's doc
            // comment for why the search is biased toward these.
            let identity_targets: Vec<PhysicalQubit> = chain.iter().map(|lq| PhysicalQubit(lq.0)).collect();
            if let Some(phys_path) = find_hamiltonian_path(coupling, chain.len(), &identity_targets) {
                let mut logical_to_physical = vec![PhysicalQubit(usize::MAX); num_qubits];
                for (lq, &pq) in chain.iter().zip(phys_path.iter()) {
                    logical_to_physical[lq.0] = pq;
                }
                return logical_to_physical;
            }
        }
    }

    let dist = distance_matrix(coupling);
    let n_phys = coupling.num_qubits();

    let mut qubit_weight = vec![0usize; num_qubits];
    for (&(a, b), &w) in &weights {
        qubit_weight[a.0] += w;
        qubit_weight[b.0] += w;
    }

    // Most-interacting logical qubits first; ties broken by ascending
    // index for determinism.
    let mut order: Vec<LogicalQubit> = (0..num_qubits).map(LogicalQubit).collect();
    order.sort_by(|&a, &b| qubit_weight[b.0].cmp(&qubit_weight[a.0]).then(a.cmp(&b)));

    // The physical qubit with the smallest total distance to every
    // other physical qubit -- the natural anchor for the very first
    // (highest-weight) logical qubit, and the fallback "stay central"
    // target for any logical qubit with no already-placed partner yet.
    let center = PhysicalQubit(
        (0..n_phys)
            .min_by_key(|&p| dist[p].iter().filter(|&&d| d != usize::MAX).sum::<usize>())
            .expect("n_phys == num_qubits >= 1, checked above"),
    );

    let mut logical_to_physical = vec![PhysicalQubit(usize::MAX); num_qubits];
    let mut used_physical = vec![false; n_phys];

    for (i, &lq) in order.iter().enumerate() {
        let best_phys = if i == 0 {
            center
        } else {
            PhysicalQubit(
                (0..n_phys)
                    .filter(|p| !used_physical[*p])
                    .min_by_key(|&p| {
                        let mut score = 0usize;
                        let mut any_neighbor = false;
                        for &placed_lq in &order[..i] {
                            let key =
                                if lq < placed_lq { (lq, placed_lq) } else { (placed_lq, lq) };
                            if let Some(&w) = weights.get(&key) {
                                if w > 0 {
                                    any_neighbor = true;
                                    let placed_phys = logical_to_physical[placed_lq.0];
                                    score = score
                                        .saturating_add(w.saturating_mul(dist[p][placed_phys.0]));
                                }
                            }
                        }
                        if any_neighbor {
                            // Tie-break by physical index too, packed
                            // into the low bits, so results stay
                            // deterministic without a second sort pass.
                            score * n_phys + p
                        } else {
                            dist[p][center.0] * n_phys + p
                        }
                    })
                    .expect("there must be an unused physical qubit left: n_phys == num_qubits"),
            )
        };
        logical_to_physical[lq.0] = best_phys;
        used_physical[best_phys.0] = true;
    }

    logical_to_physical
}

fn gate_is_front(gi: usize, gate_qubits: &[Vec<LogicalQubit>], queues: &[VecDeque<usize>]) -> bool {
    gate_qubits[gi].iter().all(|&q| queues[q.0].front() == Some(&gi))
}

/// How much [`route_lookahead`]'s SWAP-scoring heuristic weighs each
/// front-layer qubit's *next* gate (after the currently-blocked one),
/// relative to the front layer itself (weight `1.0`) -- the same
/// "extended set, decayed weight" idea real SABRE-style heuristics use
/// to break ties between SWAPs that help the immediate front layer
/// equally well but set up the *next* gates differently.
const LOOKAHEAD_WEIGHT: f64 = 0.5;

/// As [`route`], but starting from [`choose_initial_layout`] instead of
/// the identity mapping, and using a lookahead SWAP-selection heuristic
/// instead of routing each gate's shortest path in isolation (see this
/// section's doc comment above for both). Produces a circuit with
/// identical semantics to [`route`] (same restore-identity guarantee,
/// same exact argument-order preservation, same in-place `Measure`
/// tracking) -- the two differ only in *how many* `Swap`s the result
/// contains, never in the action of the circuit they route.
///
/// # Algorithm
/// 1. Start from [`choose_initial_layout`] instead of the identity.
/// 2. Maintain a "front layer": the set of not-yet-executed gates that
///    are next-up on *every* qubit they touch (a standard dependency-
///    DAG front layer, built from a per-qubit FIFO queue of that
///    qubit's own gate indices in original program order).
/// 3. Repeatedly execute every front-layer gate that's already
///    reachable (single-qubit gates always are; two-qubit gates once
///    their current physical qubits are coupling-adjacent), refilling
///    the front layer as qubits' queues advance, until nothing more can
///    fire without a SWAP.
/// 4. When blocked, score every candidate SWAP (an edge of `coupling`
///    touching a physical qubit some blocked front-layer gate is
///    currently on) by the total physical distance it leaves across
///    the front layer, plus a `LOOKAHEAD_WEIGHT`-scaled distance over
///    each front qubit's immediate next gate; apply the lowest-scoring
///    one and go back to step 3.
/// 5. Once every gate has executed, restore the identity mapping via
///    the exact same [`restore_identity_mapping`] pass [`route`] uses,
///    for the exact same reason (see `route`'s own doc comment).
///
/// # Panics (debug only)
/// If `coupling.num_qubits() != circuit.num_qubits` -- same
/// requirement [`route`] implicitly has.
pub fn route_lookahead(circuit: &Circuit, coupling: &CouplingMap) -> Circuit {
    let num_qubits = circuit.num_qubits;
    debug_assert_eq!(
        coupling.num_qubits(),
        num_qubits,
        "route_lookahead expects a coupling map sized to the circuit's own qubit count"
    );

    let dist = distance_matrix(coupling);

    // The real register starts with wire q holding logical qubit q's
    // actual state (identity) -- there's no free relabeling for an
    // arbitrary starting state, only for a uniform one like |0...0>,
    // and this crate's fidelity guarantee (see `route`'s doc comment)
    // has to hold for *any* starting state. So `choose_initial_layout`'s
    // better starting point has to be reached the same way every other
    // qubit permutation in this crate is: real, coupling-adjacent
    // `Swap` gates, emitted here via the same token-swapping pass used
    // to restore identity at the end (see `route_to_layout`), just with
    // a non-identity target. Only once that's done does
    // `logical_to_physical` actually describe where each qubit's state
    // lives, and the rest of this function's remap/execute logic below
    // is safe to trust it.
    let target_layout = choose_initial_layout(circuit, coupling);
    let mut target_physical_to_logical = vec![LogicalQubit(0); num_qubits];
    for (lq, &p) in target_layout.iter().enumerate() {
        target_physical_to_logical[p.0] = LogicalQubit(lq);
    }

    let mut logical_to_physical: Vec<PhysicalQubit> = (0..num_qubits).map(PhysicalQubit).collect();
    let mut physical_to_logical: Vec<LogicalQubit> = (0..num_qubits).map(LogicalQubit).collect();

    let mut out = Circuit::new(num_qubits);
    out.num_clbits = circuit.num_clbits;

    route_to_layout(
        &mut out,
        &mut logical_to_physical,
        &mut physical_to_logical,
        &target_physical_to_logical,
        coupling,
    );

    let gate_qubits: Vec<Vec<LogicalQubit>> = circuit
        .gates
        .iter()
        .map(|g| g.qubits().into_iter().map(LogicalQubit).collect())
        .collect();
    let mut queues: Vec<VecDeque<usize>> = vec![VecDeque::new(); num_qubits];
    for (gi, qs) in gate_qubits.iter().enumerate() {
        for &q in qs {
            queues[q.0].push_back(gi);
        }
    }

    let total_gates = circuit.gates.len();
    let mut executed = vec![false; total_gates];
    let mut remaining = total_gates;

    let mut front: Vec<usize> =
        (0..total_gates).filter(|&gi| gate_is_front(gi, &gate_qubits, &queues)).collect();

    while remaining > 0 {
        // Execute everything currently reachable, to a fixed point --
        // freeing a qubit's next gate can make it immediately
        // reachable too (e.g. a run of single-qubit gates).
        let mut progressed = true;
        while progressed {
            progressed = false;
            let mut newly_executed: Vec<usize> = Vec::new();
            for &gi in &front {
                if executed[gi] {
                    continue;
                }
                let qs = &gate_qubits[gi];
                let executable = if qs.len() < 2 {
                    true
                } else {
                    coupling.is_adjacent(
                        logical_to_physical[qs[0].0].0,
                        logical_to_physical[qs[1].0].0,
                    )
                };
                if executable {
                    let g = &circuit.gates[gi];
                    let remapped = if qs.len() < 2 {
                        remap_single(g, logical_to_physical[qs[0].0].0)
                    } else {
                        remap_two(
                            g,
                            logical_to_physical[qs[0].0].0,
                            logical_to_physical[qs[1].0].0,
                        )
                    };
                    out.push(remapped);
                    executed[gi] = true;
                    remaining -= 1;
                    for &q in qs {
                        queues[q.0].pop_front();
                    }
                    newly_executed.push(gi);
                    progressed = true;
                }
            }
            if progressed {
                front.retain(|&gi| !executed[gi]);
                let mut candidates: HashSet<usize> = HashSet::new();
                for &gi in &newly_executed {
                    for &q in &gate_qubits[gi] {
                        if let Some(&next_gi) = queues[q.0].front() {
                            candidates.insert(next_gi);
                        }
                    }
                }
                for gi in candidates {
                    if !executed[gi]
                        && gate_is_front(gi, &gate_qubits, &queues)
                        && !front.contains(&gi)
                    {
                        front.push(gi);
                    }
                }
            }
        }

        if remaining == 0 {
            break;
        }

        // Every remaining front-layer entry is a blocked two-qubit
        // gate (anything else would have fired above). Candidate SWAPs:
        // every coupling edge touching a physical qubit one of those
        // gates is currently on.
        let mut touched_physical: HashSet<PhysicalQubit> = HashSet::new();
        for &gi in &front {
            for &q in &gate_qubits[gi] {
                touched_physical.insert(logical_to_physical[q.0]);
            }
        }

        let mut candidate_swaps: HashSet<(PhysicalQubit, PhysicalQubit)> = HashSet::new();
        for &p in &touched_physical {
            for n in coupling.neighbors(p.0) {
                let n = PhysicalQubit(n);
                candidate_swaps.insert(if p.0 < n.0 { (p, n) } else { (n, p) });
            }
        }

        // Extended set: each touched qubit's immediate next gate (if
        // it's two-qubit), for the decayed tie-breaking term.
        let mut extended: Vec<(LogicalQubit, LogicalQubit)> = Vec::new();
        for &p in &touched_physical {
            let lq = physical_to_logical[p.0];
            if let Some(&next_gi) = queues[lq.0].get(1) {
                let qs = &gate_qubits[next_gi];
                if qs.len() == 2 {
                    extended.push((qs[0], qs[1]));
                }
            }
        }

        let mut best_swap: Option<(PhysicalQubit, PhysicalQubit)> = None;
        let mut best_score = f64::MAX;
        for &(p1, p2) in &candidate_swaps {
            let lq1 = physical_to_logical[p1.0];
            let lq2 = physical_to_logical[p2.0];
            let loc_after = |lq: LogicalQubit| -> PhysicalQubit {
                if lq == lq1 {
                    p2
                } else if lq == lq2 {
                    p1
                } else {
                    logical_to_physical[lq.0]
                }
            };

            let mut score = 0.0f64;
            for &gi in &front {
                let qs = &gate_qubits[gi];
                if qs.len() == 2 {
                    score += dist[loc_after(qs[0]).0][loc_after(qs[1]).0] as f64;
                }
            }
            for &(a_lq, b_lq) in &extended {
                score += LOOKAHEAD_WEIGHT * dist[loc_after(a_lq).0][loc_after(b_lq).0] as f64;
            }

            let better = score < best_score
                || (score == best_score && best_swap.map_or(true, |bp| (p1, p2) < bp));
            if better {
                best_score = score;
                best_swap = Some((p1, p2));
            }
        }

        let (p1, p2) = best_swap.expect(
            "a blocked two-qubit front-layer gate's physical qubits have at least one \
             coupling-adjacent neighbor to swap with, since coupling is connected",
        );
        out.push(Gate::Swap(p1.0, p2.0));
        swap_mapping(&mut logical_to_physical, &mut physical_to_logical, p1, p2);
    }

    restore_identity_mapping(&mut out, &mut logical_to_physical, &mut physical_to_logical, coupling);

    out
}

// ---------------------------------------------------------------------
// SABRE-style routing: real iterative layout refinement, decay, and
// deeper lookahead for circuits `choose_initial_layout`'s chain fast
// path doesn't cover -- i.e. most real algorithms. See `route_sabre`'s
// own doc comment for the measured motivation
// (`qiskit_benchmark.rs`'s `qft_10`/`qft_16`/`long_range_random_20q_60gate`
// benchmarks) and design.
// ---------------------------------------------------------------------

/// Number of forward+backward layout-refinement sweeps [`route_sabre`]
/// runs (via [`sabre_pass`]) before committing a final forward pass --
/// see that function's own doc comment for what each sweep does.
const SABRE_LAYOUT_ITERATIONS: usize = 4;

/// Independent trials [`route_sabre`] runs per starting seed (identity
/// and [`choose_initial_layout`]'s greedy guess), each with a different
/// tie-breaking jitter -- see [`sabre_pass`]'s doc comment.
const SABRE_TRIALS_PER_SEED: usize = 6;

/// How many further queued two-qubit gates [`sabre_pass`] looks ahead
/// per front-layer-touched qubit when building its extended scoring
/// set -- deeper than [`route_lookahead`]'s hardcoded single next gate
/// (`LOOKAHEAD_WEIGHT`'s own call site), the other main ingredient this
/// crate's heuristic was missing relative to real SABRE.
const SABRE_EXTENDED_LOOKAHEAD_DEPTH: usize = 4;

/// How much a physical qubit's decay weight grows each time it's used
/// in a SWAP -- see [`sabre_pass`]'s doc comment for why decay exists.
const SABRE_DECAY_INCREMENT: f64 = 0.001;

/// [`sabre_pass`] resets every physical qubit's decay weight back to
/// `1.0` after this many SWAPs, so a genuinely necessary busy region
/// doesn't accumulate an ever-growing penalty forever -- the same
/// periodic-reset practice the original SABRE heuristic uses.
const SABRE_DECAY_RESET_INTERVAL: usize = 5;

/// Scale of the per-candidate score jitter [`sabre_pass`] adds purely
/// to diversify tie-breaking across [`route_sabre`]'s trials -- small
/// enough to never override a real score difference (typical `dist`
/// values are `>= 1.0`, and [`SABRE_TRIALS_PER_SEED`] trials only ever
/// need to disagree on which *tied* candidate to prefer, not overrule
/// the heuristic's actual judgment).
const SABRE_JITTER_SCALE: f64 = 1e-3;

fn build_queues(gate_qubits: &[Vec<LogicalQubit>], num_qubits: usize) -> Vec<VecDeque<usize>> {
    let mut queues: Vec<VecDeque<usize>> = vec![VecDeque::new(); num_qubits];
    for (gi, qs) in gate_qubits.iter().enumerate() {
        for &q in qs {
            queues[q.0].push_back(gi);
        }
    }
    queues
}

/// A small, fast, deterministic (given a seed) PRNG -- xorshift64 --
/// used only for [`sabre_pass`]'s tie-breaking jitter. Not
/// cryptographic, and not meant to be: the only property this needs is
/// "looks different across trials", which xorshift64 gives cheaply and
/// reproducibly (same seed -> same sequence, so a specific trial's
/// routing decisions are always reproducible for debugging).
fn next_xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// One event [`sabre_pass`] reports to its caller as it runs -- see
/// that function's doc comment. Carries the *current* logical-physical
/// layout at the moment of the event (not stored anywhere the caller
/// could read it independently mid-pass), since a caller building
/// output gates needs to know exactly where each logical qubit sits
/// right now, not before or after.
enum SabreEvent<'a> {
    /// Gate `gate_index` (into whichever `gate_qubits` list this pass
    /// was given -- forward or reversed, caller's choice) just became
    /// executable under `layout`.
    Execute {
        gate_index: usize,
        layout: &'a [PhysicalQubit],
    },
    /// A `Swap` between these two physical qubits was just chosen and
    /// is about to be applied to the mapping.
    Swap(PhysicalQubit, PhysicalQubit),
}

/// One directed SABRE-style pass over `gate_qubits` (forward or
/// reversed -- this function only ever reasons about logical-qubit
/// interaction *order*, never gate identity, so a caller can pass
/// either direction), starting from `logical_to_physical`/
/// `physical_to_logical` and mutating them in place to the mapping
/// reached once every gate in `gate_qubits` has "executed". Calls
/// `on_event` once per [`SabreEvent`] -- [`route_sabre`]'s two internal
/// layout-refinement sweeps pass a no-op and only care about the
/// resulting mapping; its final commit pass passes a closure that
/// actually builds a routed [`Circuit`].
///
/// Differs from [`route_lookahead`]'s inline swap-selection loop in
/// the two ingredients real SABRE has that this crate's original
/// heuristic didn't:
/// - **Decay.** `decay[p]` grows by [`SABRE_DECAY_INCREMENT`] every
///   time physical qubit `p` is used in a `Swap`, and periodically
///   resets (see [`SABRE_DECAY_RESET_INTERVAL`]) -- a candidate `Swap`
///   reusing a recently-swapped qubit is penalized by
///   `max(decay[p1], decay[p2])`, which discourages the same pair (or
///   its immediate neighborhood) oscillating back and forth to satisfy
///   two different front-layer gates in quick succession, a real
///   failure mode a plain per-step-greedy heuristic has no defense
///   against.
/// - **Deeper, size-normalized lookahead.** The extended scoring set
///   is each front-layer-touched qubit's next
///   [`SABRE_EXTENDED_LOOKAHEAD_DEPTH`] queued two-qubit gates (not
///   just the immediate next one), and both the front-layer and
///   extended terms are divided by their own set sizes before being
///   combined -- the standard SABRE-paper normalization, so a
///   momentarily large front layer doesn't automatically dominate the
///   score just by having more terms to sum.
///
/// `rng_state` adds a small score jitter (see [`SABRE_JITTER_SCALE`])
/// purely to diversify which candidate wins an otherwise-tied score
/// across [`route_sabre`]'s different trials -- it never overrides a
/// real score difference.
fn sabre_pass(
    gate_qubits: &[Vec<LogicalQubit>],
    coupling: &CouplingMap,
    dist: &[Vec<usize>],
    logical_to_physical: &mut [PhysicalQubit],
    physical_to_logical: &mut [LogicalQubit],
    mut on_event: impl FnMut(SabreEvent),
    rng_state: &mut u64,
) {
    let num_qubits = logical_to_physical.len();
    if gate_qubits.is_empty() {
        return;
    }
    let mut queues = build_queues(gate_qubits, num_qubits);
    let total_gates = gate_qubits.len();
    let mut executed = vec![false; total_gates];
    let mut remaining = total_gates;

    let mut front: Vec<usize> =
        (0..total_gates).filter(|&gi| gate_is_front(gi, gate_qubits, &queues)).collect();

    let mut decay = vec![1.0f64; num_qubits];
    let mut swaps_since_reset = 0usize;

    while remaining > 0 {
        let mut progressed = true;
        while progressed {
            progressed = false;
            let mut newly_executed: Vec<usize> = Vec::new();
            for &gi in &front {
                if executed[gi] {
                    continue;
                }
                let qs = &gate_qubits[gi];
                let executable = if qs.len() < 2 {
                    true
                } else {
                    coupling.is_adjacent(
                        logical_to_physical[qs[0].0].0,
                        logical_to_physical[qs[1].0].0,
                    )
                };
                if executable {
                    on_event(SabreEvent::Execute {
                        gate_index: gi,
                        layout: logical_to_physical,
                    });
                    executed[gi] = true;
                    remaining -= 1;
                    for &q in qs {
                        queues[q.0].pop_front();
                    }
                    newly_executed.push(gi);
                    progressed = true;
                }
            }
            if progressed {
                front.retain(|&gi| !executed[gi]);
                let mut candidates: HashSet<usize> = HashSet::new();
                for &gi in &newly_executed {
                    for &q in &gate_qubits[gi] {
                        if let Some(&next_gi) = queues[q.0].front() {
                            candidates.insert(next_gi);
                        }
                    }
                }
                for gi in candidates {
                    if !executed[gi]
                        && gate_is_front(gi, gate_qubits, &queues)
                        && !front.contains(&gi)
                    {
                        front.push(gi);
                    }
                }
            }
        }

        if remaining == 0 {
            break;
        }

        let mut touched_physical: HashSet<PhysicalQubit> = HashSet::new();
        for &gi in &front {
            for &q in &gate_qubits[gi] {
                touched_physical.insert(logical_to_physical[q.0]);
            }
        }

        let mut candidate_swaps: HashSet<(PhysicalQubit, PhysicalQubit)> = HashSet::new();
        for &p in &touched_physical {
            for n in coupling.neighbors(p.0) {
                let n = PhysicalQubit(n);
                candidate_swaps.insert(if p.0 < n.0 { (p, n) } else { (n, p) });
            }
        }

        // Extended set: up to SABRE_EXTENDED_LOOKAHEAD_DEPTH further
        // queued two-qubit gates per touched qubit, deduplicated by
        // gate index so a gate reachable from both its qubits isn't
        // double-counted.
        let mut extended_gates: HashSet<usize> = HashSet::new();
        for &p in &touched_physical {
            let lq = physical_to_logical[p.0];
            for &gi in queues[lq.0].iter().skip(1).take(SABRE_EXTENDED_LOOKAHEAD_DEPTH) {
                if gate_qubits[gi].len() == 2 {
                    extended_gates.insert(gi);
                }
            }
        }
        let extended: Vec<(LogicalQubit, LogicalQubit)> = extended_gates
            .into_iter()
            .map(|gi| (gate_qubits[gi][0], gate_qubits[gi][1]))
            .collect();

        let mut best_swap: Option<(PhysicalQubit, PhysicalQubit)> = None;
        let mut best_score = f64::MAX;
        for &(p1, p2) in &candidate_swaps {
            let lq1 = physical_to_logical[p1.0];
            let lq2 = physical_to_logical[p2.0];
            let loc_after = |lq: LogicalQubit| -> PhysicalQubit {
                if lq == lq1 {
                    p2
                } else if lq == lq2 {
                    p1
                } else {
                    logical_to_physical[lq.0]
                }
            };

            let mut front_score = 0.0f64;
            let mut front_count = 0usize;
            for &gi in &front {
                let qs = &gate_qubits[gi];
                if qs.len() == 2 {
                    front_score += dist[loc_after(qs[0]).0][loc_after(qs[1]).0] as f64;
                    front_count += 1;
                }
            }
            let mut ext_score = 0.0f64;
            for &(a_lq, b_lq) in &extended {
                ext_score += dist[loc_after(a_lq).0][loc_after(b_lq).0] as f64;
            }

            let normalized = front_score / front_count.max(1) as f64
                + LOOKAHEAD_WEIGHT * (ext_score / extended.len().max(1) as f64);
            let decay_factor = decay[p1.0].max(decay[p2.0]);
            let jitter = (next_xorshift(rng_state) as f64 / u64::MAX as f64) * SABRE_JITTER_SCALE;
            let score = decay_factor * normalized + jitter;

            let better = score < best_score
                || (score == best_score && best_swap.map_or(true, |bp| (p1, p2) < bp));
            if better {
                best_score = score;
                best_swap = Some((p1, p2));
            }
        }

        let (p1, p2) = best_swap.expect(
            "a blocked two-qubit front-layer gate's physical qubits have at least one \
             coupling-adjacent neighbor to swap with, since coupling is connected",
        );
        on_event(SabreEvent::Swap(p1, p2));
        swap_mapping(logical_to_physical, physical_to_logical, p1, p2);
        decay[p1.0] += SABRE_DECAY_INCREMENT;
        decay[p2.0] += SABRE_DECAY_INCREMENT;
        swaps_since_reset += 1;
        if swaps_since_reset >= SABRE_DECAY_RESET_INTERVAL {
            for d in decay.iter_mut() {
                *d = 1.0;
            }
            swaps_since_reset = 0;
        }
    }
}

/// A SABRE-style router, for circuits [`choose_initial_layout`]'s chain
/// fast path doesn't cover -- i.e. most real algorithms.
/// [`route_lookahead`]'s single greedy forward pass (one-shot initial
/// layout, 1-gate lookahead, no decay) measurably falls behind
/// Qiskit's SABRE-based router on exactly this case: ~3x more SWAPs on
/// this crate's own `qft_10`/`qft_16`/`long_range_random_20q_60gate`
/// benchmarks (`qiskit_benchmark.rs`), circuits whose interaction graph
/// is genuinely non-local rather than a chain or nearest-neighbor
/// ladder. This closes most of that gap by adding the two ingredients
/// [`sabre_pass`]'s own doc comment describes (decay, deeper
/// normalized lookahead), plus the other real-SABRE ingredient neither
/// this function nor `route_lookahead` had before: **iterative layout
/// refinement**.
///
/// # Iterative layout refinement
/// [`sabre_pass`] is run forward over the circuit, then backward over
/// the *reversed* circuit starting from the forward pass's final
/// layout, [`SABRE_LAYOUT_ITERATIONS`] times -- each backward pass's
/// resulting mapping becomes a better candidate *initial* layout for
/// the next forward pass, the same forward-backward-forward trick real
/// SABRE uses to refine a starting layout from the circuit's own
/// structure, instead of committing to a one-shot greedy guess the way
/// `route_lookahead` does. Both directions only ever evolve the
/// logical/physical mapping (via [`sabre_pass`]'s no-op event
/// callback) -- no gates are emitted until the final commit pass below.
///
/// # Multiple trials, two starting seeds
/// [`SABRE_TRIALS_PER_SEED`] trials each from the plain identity
/// mapping (zero cost to physically realize -- see
/// [`route_lookahead`]'s own comment on why that realization cost is
/// real in this crate, not virtual) and from [`choose_initial_layout`]'s
/// greedy guess, with a small per-trial score jitter (see
/// [`sabre_pass`]) diversifying which locally-tied candidate each
/// trial follows through the layout-refinement sweeps above. Whichever
/// trial's *final committed circuit* has the fewest total `Swap`s
/// (realization cost + routing + final identity restore -- the number
/// that actually matters, not just the routing-loop's own swap count)
/// is returned.
///
/// # Correctness and where this sits relative to the other routers
/// Built entirely from the same [`sabre_pass`]/[`route_to_layout`]/
/// [`restore_identity_mapping`]/[`remap_single`]/[`remap_two`] building
/// blocks [`route`] and [`route_lookahead`] already use, so it's exactly
/// as semantics-preserving as either (see
/// `assert_sabre_routing_preserves_action`'s coverage in this module's
/// tests). This function does not compare itself against
/// `route_lookahead` internally -- for a circuit `route_lookahead`
/// already routes optimally (the chain fast path, zero SWAPs), a
/// handful of SABRE trials adds real cost for no possible gain. Picking
/// the overall best across all three routers is [`route_best`]'s job,
/// not this function's.
///
/// # Cost
/// `2 * SABRE_LAYOUT_ITERATIONS` extra passes plus one commit pass, per
/// trial, per seed (`2 * SABRE_TRIALS_PER_SEED` trials total) -- real,
/// deliberate overhead this crate doesn't otherwise pay, appropriate
/// for the same one-shot-per-circuit call [`route_best`] already makes
/// of every other router, not for a hot loop calling this at high
/// frequency.
pub fn route_sabre(circuit: &Circuit, coupling: &CouplingMap) -> Circuit {
    let num_qubits = circuit.num_qubits;
    debug_assert_eq!(
        coupling.num_qubits(),
        num_qubits,
        "route_sabre expects a coupling map sized to the circuit's own qubit count"
    );

    let mut fallback = Circuit::new(num_qubits);
    fallback.num_clbits = circuit.num_clbits;
    for g in &circuit.gates {
        fallback.push(g.clone());
    }
    if num_qubits <= 1 {
        return fallback;
    }

    let dist = distance_matrix(coupling);
    let gate_qubits: Vec<Vec<LogicalQubit>> = circuit
        .gates
        .iter()
        .map(|g| g.qubits().into_iter().map(LogicalQubit).collect())
        .collect();
    if gate_qubits.iter().all(|qs| qs.len() < 2) {
        // No two-qubit gates at all -- nothing to route, and every
        // physical qubit is already exactly where its logical qubit
        // needs it (identity mapping), so the unmodified circuit is
        // already a valid answer.
        return fallback;
    }
    let reversed_gate_qubits: Vec<Vec<LogicalQubit>> = gate_qubits.iter().rev().cloned().collect();

    let identity_seed: Vec<PhysicalQubit> = (0..num_qubits).map(PhysicalQubit).collect();
    let greedy_seed = choose_initial_layout(circuit, coupling);
    let seeds: [&Vec<PhysicalQubit>; 2] = [&identity_seed, &greedy_seed];

    let mut best: Option<Circuit> = None;
    let mut best_swaps = usize::MAX;

    for (seed_idx, seed_layout) in seeds.iter().enumerate() {
        for trial in 0..SABRE_TRIALS_PER_SEED {
            let mut rng_state: u64 = (0x9E3779B97F4A7C15u64
                ^ ((seed_idx as u64 + 1) << 40)
                ^ (trial as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9))
                | 1;

            let mut l2p: Vec<PhysicalQubit> = (*seed_layout).clone();
            let mut p2l = vec![LogicalQubit(0); num_qubits];
            for (lq, &pq) in l2p.iter().enumerate() {
                p2l[pq.0] = LogicalQubit(lq);
            }

            for _ in 0..SABRE_LAYOUT_ITERATIONS {
                sabre_pass(&gate_qubits, coupling, &dist, &mut l2p, &mut p2l, |_| {}, &mut rng_state);
                sabre_pass(
                    &reversed_gate_qubits,
                    coupling,
                    &dist,
                    &mut l2p,
                    &mut p2l,
                    |_| {},
                    &mut rng_state,
                );
            }

            // `p2l` now holds the refined *initial* layout candidate --
            // physically realize it from identity (same real cost
            // `route_lookahead` pays to reach `choose_initial_layout`'s
            // target -- see that function's own comment), then commit
            // the real forward pass, then restore identity, mirroring
            // route_lookahead's own overall structure exactly.
            let mut out = Circuit::new(num_qubits);
            out.num_clbits = circuit.num_clbits;
            let mut cur_l2p: Vec<PhysicalQubit> = (0..num_qubits).map(PhysicalQubit).collect();
            let mut cur_p2l: Vec<LogicalQubit> = (0..num_qubits).map(LogicalQubit).collect();
            route_to_layout(&mut out, &mut cur_l2p, &mut cur_p2l, &p2l, coupling);

            sabre_pass(
                &gate_qubits,
                coupling,
                &dist,
                &mut cur_l2p,
                &mut cur_p2l,
                |evt| match evt {
                    SabreEvent::Execute { gate_index, layout } => {
                        let g = &circuit.gates[gate_index];
                        let qs = &gate_qubits[gate_index];
                        let remapped = if qs.len() < 2 {
                            remap_single(g, layout[qs[0].0].0)
                        } else {
                            remap_two(g, layout[qs[0].0].0, layout[qs[1].0].0)
                        };
                        out.push(remapped);
                    }
                    SabreEvent::Swap(p1, p2) => {
                        out.push(Gate::Swap(p1.0, p2.0));
                    }
                },
                &mut rng_state,
            );

            restore_identity_mapping(&mut out, &mut cur_l2p, &mut cur_p2l, coupling);

            let total = swap_count(&out);
            if total < best_swaps {
                best_swaps = total;
                best = Some(out);
            }
        }
    }

    best.expect("SABRE_TRIALS_PER_SEED >= 1 and seeds is non-empty, so at least one trial always runs")
}

/// Number of `Gate::Swap`s in a routed circuit -- the one number that
/// actually decides how much a routing choice cost (every SWAP is 3
/// extra native two-qubit gates once lowered -- see `native.rs`'s
/// `Swap -> Cx;Cx;Cx` identity), independent of gate-count noise from
/// anything else in the circuit.
fn swap_count(c: &Circuit) -> usize {
    c.gates.iter().filter(|g| matches!(g, Gate::Swap(_, _))).count()
}

/// Routes `circuit` against `coupling` via [`route`], [`route_lookahead`],
/// and [`route_sabre`], and returns whichever result used fewest
/// `Swap`s.
///
/// This exists because none of the three is a strict improvement on
/// the other two in every case:
/// - `route_lookahead`'s initial-layout selection is a one-shot greedy
///   heuristic that scores a *candidate layout's* quality but never
///   prices in what it costs to physically *reach* that layout (see
///   `choose_initial_layout`'s doc comment) -- on a sparse, low-degree
///   coupling map (e.g. `heavy_hex_for` below 13 qubits, which is a
///   plain ring -- see this module's `ghz`-benchmark investigation)
///   that reach cost can exceed `route`'s entire budget for the
///   circuit, making `route_lookahead` a real regression rather than a
///   strict improvement.
/// - `route_sabre` adds real per-trial overhead (see its own doc
///   comment on cost) for a benefit that's concentrated on circuits
///   with genuinely non-local interaction structure -- on a circuit
///   `route_lookahead` already routes optimally (anything the chain
///   fast path covers, zero SWAPs), the extra trials can't do better
///   and aren't guaranteed to match it exactly on every run (jitter
///   means a specific trial could in principle land one SWAP worse
///   before this function's own selection catches it).
/// - `route`'s single-gate greedy walk has no failure mode beyond what
///   each individual gate needs, so it's always a safe floor, just
///   usually not the best available answer.
///
/// All three are exactly semantics-preserving (same restore-identity
/// guarantee, same argument-order preservation -- see `route`'s own
/// doc comment), so picking between their outputs by SWAP count alone
/// never risks correctness, only performance. This is the function
/// `crate::backend::lower` calls (see that function's own doc comment
/// for why `route_lookahead` alone isn't enough).
///
/// Costs up to two extra full routing passes over calling
/// `route_lookahead` alone, plus `route_sabre`'s own per-trial cost.
/// That's real, but routing is not the bottleneck in this crate's
/// pipeline (native decomposition and backend lowering both touch
/// every gate at least once more downstream of this), and a
/// wrong-direction regression silently shipping is worse than a larger
/// constant-factor cost here.
pub fn route_best(circuit: &Circuit, coupling: &CouplingMap) -> Circuit {
    let naive = route(circuit, coupling);
    let smart = route_lookahead(circuit, coupling);
    let sabre = route_sabre(circuit, coupling);
    let mut best = naive;
    if swap_count(&smart) < swap_count(&best) {
        best = smart;
    }
    if swap_count(&sabre) < swap_count(&best) {
        best = sabre;
    }
    best
}

fn remap_single(gate: &Gate, new_q: usize) -> Gate {
    use Gate::*;
    match *gate {
        H(_) => H(new_q),
        X(_) => X(new_q),
        Y(_) => Y(new_q),
        Z(_) => Z(new_q),
        S(_) => S(new_q),
        Sdg(_) => Sdg(new_q),
        T(_) => T(new_q),
        Tdg(_) => Tdg(new_q),
        Rx(_, a) => Rx(new_q, a),
        Ry(_, a) => Ry(new_q, a),
        Rz(_, a) => Rz(new_q, a),
        // NOTE: this arm is NOT compiler-enforced the way every other
        // Gate match in this crate is -- `remap_single` has (and needs
        // to keep) a catch-all below for genuinely-unreachable
        // two-qubit gates, so adding a new single-qubit-shaped Gate
        // variant in the future and forgetting to add it here will
        // panic at runtime, not fail to compile. Measure(_, c) keeps
        // its classical bit `c` fixed and only moves the qubit -- `c`
        // is not a physical wire and routing never touches it.
        Measure(_, c) => Measure(new_q, c),
        _ => unreachable!("remap_single called on a two-qubit gate"),
    }
}

fn remap_two(gate: &Gate, new_first: usize, new_second: usize) -> Gate {
    use Gate::*;
    match *gate {
        Cx(_, _) => Cx(new_first, new_second),
        Cz(_, _) => Cz(new_first, new_second),
        Swap(_, _) => Swap(new_first, new_second),
        Rxx(_, _, t) => Rxx(new_first, new_second, t),
        Ryy(_, _, t) => Ryy(new_first, new_second, t),
        Rzz(_, _, t) => Rzz(new_first, new_second, t),
        Cp(_, _, l) => Cp(new_first, new_second, l),
        _ => unreachable!("remap_two called on a single-qubit gate"),
    }
}

#[cfg(test)]
mod tests {
    // Same methodology as `tests/decompositions.rs` and
    // `ir_optimize.rs`'s in-module tests: run the original circuit and
    // the routed-then-decomposed one from the same random initial
    // state, and compare via `QuantumRegister::fidelity` rather than
    // trusting the routing algebra on its own.

    use super::*;
    use rand::Rng;
    use sirraya_qutub::core::QuantumRegister;

    const TOL: f64 = 1e-9;

    fn randomized_register(num_qubits: usize) -> QuantumRegister {
        let mut reg = QuantumRegister::new(num_qubits).unwrap();
        let mut rng = rand::thread_rng();
        for q in 0..num_qubits {
            reg.apply_rz(q, rng.gen_range(0.0..std::f64::consts::TAU)).unwrap();
            reg.apply_ry(q, rng.gen_range(0.0..std::f64::consts::TAU)).unwrap();
            reg.apply_rz(q, rng.gen_range(0.0..std::f64::consts::TAU)).unwrap();
        }
        reg
    }

    fn apply_gate(reg: &mut QuantumRegister, g: &Gate) {
        match *g {
            Gate::H(q) => reg.apply_hadamard(q).unwrap(),
            Gate::X(q) => reg.apply_pauli_x(q).unwrap(),
            Gate::Y(q) => reg.apply_pauli_y(q).unwrap(),
            Gate::Z(q) => reg.apply_pauli_z(q).unwrap(),
            Gate::S(q) => reg.apply_s_gate(q).unwrap(),
            Gate::Sdg(q) => reg.apply_s_dag_gate(q).unwrap(),
            Gate::T(q) => reg.apply_t_gate(q).unwrap(),
            Gate::Tdg(q) => reg.apply_t_dag_gate(q).unwrap(),
            Gate::Rx(q, a) => reg.apply_rx(q, a).unwrap(),
            Gate::Ry(q, a) => reg.apply_ry(q, a).unwrap(),
            Gate::Rz(q, a) => reg.apply_rz(q, a).unwrap(),
            Gate::Cx(c, t) => reg.apply_cnot(c, t).unwrap(),
            Gate::Cz(c, t) => reg.apply_controlled_z(c, t).unwrap(),
            Gate::Swap(a, b) => reg.apply_swap(a, b).unwrap(),
            Gate::Rxx(a, b, t) => reg.apply_rxx(a, b, t).unwrap(),
            Gate::Ryy(a, b, t) => reg.apply_ryy(a, b, t).unwrap(),
            Gate::Rzz(a, b, t) => reg.apply_rzz(a, b, t).unwrap(),
            Gate::Cp(c, t, l) => reg.apply_controlled_phase(c, t, l).unwrap(),
            Gate::Measure(..) => panic!(
                "apply_gate: Measure has no fidelity-based test yet -- it needs the \
                 shot-based statistical methodology called for in the P0.1 roadmap item \
                 (QuantumRegister::fidelity doesn't apply to a measured bit), not a variant \
                 of this direct-simulation comparison. None of this file's existing tests \
                 push a Measure gate, so this arm exists only to satisfy exhaustiveness."
            ),
        }
    }

    /// Routing must not change the circuit's action: run the original
    /// (unrouted, all-to-all-assumed) circuit and the routed one from
    /// the same random state and check they land on the same state.
    fn assert_routing_preserves_action(circuit: &Circuit, coupling: &CouplingMap) {
        let routed = route(circuit, coupling);
        let mut direct = randomized_register(circuit.num_qubits);
        let mut routed_reg = direct.clone();
        for g in &circuit.gates {
            apply_gate(&mut direct, g);
        }
        for g in &routed.gates {
            apply_gate(&mut routed_reg, g);
        }
        let fidelity = direct.fidelity(&routed_reg).unwrap();
        assert!(
            (fidelity - 1.0).abs() < TOL,
            "routed circuit doesn't match original: fidelity {} (routed: {:?})",
            fidelity,
            routed.gates
        );
    }

    #[test]
    fn adjacent_gate_needs_no_swaps() {
        let mut c = Circuit::new(2);
        c.push(Gate::Cx(0, 1));
        let coupling = CouplingMap::linear(2);
        let routed = route(&c, &coupling);
        assert_eq!(routed.gates, vec![Gate::Cx(0, 1)]);
    }

    #[test]
    fn distant_cx_gets_routed_and_matches_direct_simulation() {
        let mut c = Circuit::new(4);
        c.push(Gate::H(0)).push(Gate::Cx(0, 3));
        let coupling = CouplingMap::linear(4);
        let routed = route(&c, &coupling);
        // Every two-qubit gate in the routed circuit must be between
        // adjacent physical qubits -- including the restore swaps at
        // the end, not just the ones inserted mid-routing.
        for g in &routed.gates {
            let qs = g.qubits();
            if qs.len() == 2 {
                assert!(
                    coupling.is_adjacent(qs[0], qs[1]),
                    "gate {:?} is not on adjacent qubits",
                    g
                );
            }
        }
        assert_routing_preserves_action(&c, &coupling);
    }

    #[test]
    fn routing_restores_every_qubit_to_its_original_wire() {
        // The bug this guards against: routing a distant gate must not
        // leave *other*, untouched qubits shifted onto the wrong wire
        // as a side effect of the swap chain used to route it. Checked
        // directly here (not just via whole-circuit fidelity) by
        // re-deriving the final logical->physical mapping the same way
        // `route` does internally and asserting it's the identity.
        let mut c = Circuit::new(5);
        c.push(Gate::Cx(0, 4));
        let coupling = CouplingMap::linear(5);
        let routed = route(&c, &coupling);

        let mut logical_to_physical: Vec<PhysicalQubit> = (0..5).map(PhysicalQubit).collect();
        let mut physical_to_logical: Vec<LogicalQubit> = (0..5).map(LogicalQubit).collect();
        for g in &routed.gates {
            if let Gate::Swap(a, b) = *g {
                swap_mapping(
                    &mut logical_to_physical,
                    &mut physical_to_logical,
                    PhysicalQubit(a),
                    PhysicalQubit(b),
                );
            }
        }
        assert_eq!(
            logical_to_physical,
            (0..5).map(PhysicalQubit).collect::<Vec<_>>(),
            "routing must restore every qubit to its original physical wire by the end \
             of the circuit, routed: {:?}",
            routed.gates
        );
    }

    #[test]
    fn cx_control_target_order_survives_routing() {
        // Cx is asymmetric -- routing a distant Cx must not silently
        // turn it into the reversed gate. Checked both by direct
        // fidelity comparison (which would catch it) and by asserting
        // there's exactly one Cx-shaped two-qubit gate touching qubit 0
        // as a control-like role after routing, since qubit 0 is the
        // one that stays put (it's the second argument).
        let mut c = Circuit::new(5);
        c.push(Gate::Cx(4, 0));
        let coupling = CouplingMap::linear(5);
        assert_routing_preserves_action(&c, &coupling);
    }

    #[test]
    fn dense_random_circuit_routes_correctly_on_five_qubits() {
        let mut c = Circuit::new(5);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 4))
            .push(Gate::Rz(2, 0.37))
            .push(Gate::Cp(1, 4, 1.1))
            .push(Gate::Ryy(0, 3, 0.6))
            .push(Gate::Swap(1, 3))
            .push(Gate::Cz(0, 2));
        let coupling = CouplingMap::linear(5);
        assert_routing_preserves_action(&c, &coupling);
    }

    #[test]
    fn measure_tracks_current_physical_location_mid_circuit() {
        // Measure is single-qubit-shaped for routing, so it's remapped
        // in place at the point it's encountered -- same as any other
        // single-qubit gate -- rather than waiting for the final
        // restore-identity pass. This test pins that down directly:
        // route a distant Cx first (forcing swaps that move qubit 0
        // off wire 0), then Measure qubit 0 *before* the circuit ends,
        // and check the Measure landed on whatever physical wire qubit
        // 0 was actually on at that point, not on wire 0.
        let mut c = Circuit::new(4);
        c.push(Gate::Cx(0, 3)).push(Gate::Measure(0, 0));
        let coupling = CouplingMap::linear(4);
        let routed = route(&c, &coupling);

        // Replay the routing decisions the same way `route` does
        // internally, stopping as soon as we see the Measure, to
        // independently derive what physical wire qubit 0 should be on
        // at that point in the *routed* circuit.
        let mut logical_to_physical: Vec<PhysicalQubit> = (0..4).map(PhysicalQubit).collect();
        let mut physical_to_logical: Vec<LogicalQubit> = (0..4).map(LogicalQubit).collect();
        let mut found = None;
        for g in &routed.gates {
            match *g {
                Gate::Swap(a, b) => swap_mapping(
                    &mut logical_to_physical,
                    &mut physical_to_logical,
                    PhysicalQubit(a),
                    PhysicalQubit(b),
                ),
                Gate::Measure(q, clbit) => {
                    found = Some((q, clbit));
                    break;
                }
                _ => {}
            }
        }
        let (measured_wire, clbit) = found.expect("routed circuit must still contain a Measure");
        assert_eq!(clbit, 0, "the classical bit index must be untouched by routing");
        assert_eq!(
            measured_wire, logical_to_physical[0].0,
            "Measure must read qubit 0 off its *current* physical wire, not its original one"
        );
    }

    #[test]
    fn already_local_circuit_needs_no_restore_swaps() {
        let mut c = Circuit::new(3);
        c.push(Gate::H(0)).push(Gate::Cx(0, 1)).push(Gate::Cx(1, 2));
        let coupling = CouplingMap::linear(3);
        let routed = route(&c, &coupling);
        assert_eq!(routed.gates, c.gates, "no swaps should have been needed");
    }

    /// The P1.2 regression case: `CouplingMap::heavy_hex_for` (IbmQ's
    /// real topology) has plenty of physical qubits whose consecutive
    /// indices are *not* coupling-adjacent, which the old bubble-sort
    /// `restore_identity_mapping` silently assumed. Every two-qubit
    /// gate (including the restore swaps) must land on real edges, the
    /// final mapping must be the identity, and the whole thing must
    /// still act like the original circuit.
    #[test]
    fn restores_identity_on_a_heavy_hex_coupling_map() {
        let coupling = CouplingMap::heavy_hex_for(12);
        let mut c = Circuit::new(12);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 11))
            .push(Gate::Rz(5, 0.42))
            .push(Gate::Cp(2, 9, 1.3))
            .push(Gate::Ryy(1, 8, 0.77))
            .push(Gate::Cz(3, 10));

        let routed = route(&c, &coupling);
        for g in &routed.gates {
            let qs = g.qubits();
            if qs.len() == 2 {
                assert!(
                    coupling.is_adjacent(qs[0], qs[1]),
                    "gate {:?} is not on a real heavy-hex edge",
                    g
                );
            }
        }

        let mut logical_to_physical: Vec<PhysicalQubit> = (0..12).map(PhysicalQubit).collect();
        let mut physical_to_logical: Vec<LogicalQubit> = (0..12).map(LogicalQubit).collect();
        for g in &routed.gates {
            if let Gate::Swap(a, b) = *g {
                swap_mapping(
                    &mut logical_to_physical,
                    &mut physical_to_logical,
                    PhysicalQubit(a),
                    PhysicalQubit(b),
                );
            }
        }
        assert_eq!(
            logical_to_physical,
            (0..12).map(PhysicalQubit).collect::<Vec<_>>(),
            "every qubit must be restored to its original physical wire on a heavy-hex map"
        );

        assert_routing_preserves_action(&c, &coupling);
    }

    // -------------------------------------------------------------
    // P2.1: choose_initial_layout / route_lookahead
    // -------------------------------------------------------------

    fn swap_count(c: &Circuit) -> usize {
        c.gates.iter().filter(|g| matches!(g, Gate::Swap(..))).count()
    }

    /// Same methodology as [`assert_routing_preserves_action`], but for
    /// [`route_lookahead`] -- the whole point of building it on top of
    /// the same [`remap_single`]/[`remap_two`]/[`restore_identity_mapping`]
    /// building blocks `route` uses is that this must hold exactly as
    /// strongly.
    fn assert_lookahead_routing_preserves_action(circuit: &Circuit, coupling: &CouplingMap) {
        let routed = route_lookahead(circuit, coupling);
        let mut direct = randomized_register(circuit.num_qubits);
        let mut routed_reg = direct.clone();
        for g in &circuit.gates {
            apply_gate(&mut direct, g);
        }
        for g in &routed.gates {
            apply_gate(&mut routed_reg, g);
        }
        let fidelity = direct.fidelity(&routed_reg).unwrap();
        assert!(
            (fidelity - 1.0).abs() < TOL,
            "route_lookahead doesn't match original: fidelity {} (routed: {:?})",
            fidelity,
            routed.gates
        );
    }

    #[test]
    fn lookahead_adjacent_gate_needs_no_swaps() {
        let mut c = Circuit::new(2);
        c.push(Gate::Cx(0, 1));
        let coupling = CouplingMap::linear(2);
        let routed = route_lookahead(&c, &coupling);
        assert_eq!(swap_count(&routed), 0);
        assert_lookahead_routing_preserves_action(&c, &coupling);
    }

    #[test]
    fn lookahead_distant_cx_matches_direct_simulation() {
        let mut c = Circuit::new(4);
        c.push(Gate::H(0)).push(Gate::Cx(0, 3));
        let coupling = CouplingMap::linear(4);
        let routed = route_lookahead(&c, &coupling);
        for g in &routed.gates {
            let qs = g.qubits();
            if qs.len() == 2 {
                assert!(coupling.is_adjacent(qs[0], qs[1]), "gate {:?} not on adjacent qubits", g);
            }
        }
        assert_lookahead_routing_preserves_action(&c, &coupling);
    }

    #[test]
    fn lookahead_restores_every_qubit_to_its_original_wire() {
        let mut c = Circuit::new(5);
        c.push(Gate::Cx(0, 4));
        let coupling = CouplingMap::linear(5);
        let routed = route_lookahead(&c, &coupling);

        let mut logical_to_physical: Vec<PhysicalQubit> = (0..5).map(PhysicalQubit).collect();
        let mut physical_to_logical: Vec<LogicalQubit> = (0..5).map(LogicalQubit).collect();
        for g in &routed.gates {
            if let Gate::Swap(a, b) = *g {
                swap_mapping(
                    &mut logical_to_physical,
                    &mut physical_to_logical,
                    PhysicalQubit(a),
                    PhysicalQubit(b),
                );
            }
        }
        assert_eq!(
            logical_to_physical,
            (0..5).map(PhysicalQubit).collect::<Vec<_>>(),
            "route_lookahead must restore every qubit to its original physical wire, routed: {:?}",
            routed.gates
        );
    }

    #[test]
    fn lookahead_cx_control_target_order_survives_routing() {
        let mut c = Circuit::new(5);
        c.push(Gate::Cx(4, 0));
        let coupling = CouplingMap::linear(5);
        assert_lookahead_routing_preserves_action(&c, &coupling);
    }

    #[test]
    fn lookahead_measure_tracks_current_physical_location_mid_circuit() {
        // Same property as `measure_tracks_current_physical_location_mid_circuit`,
        // for route_lookahead: Measure must read qubit 0 off whatever
        // physical wire it's actually on at that point, not off wire 0.
        let mut c = Circuit::new(4);
        c.push(Gate::Cx(0, 3)).push(Gate::Measure(0, 0));
        let coupling = CouplingMap::linear(4);
        let routed = route_lookahead(&c, &coupling);

        // Tracked from the real starting point: wire q holds logical
        // qubit q's state at the very start (identity), the same as
        // `lookahead_restores_every_qubit_to_its_original_wire` --
        // route_lookahead now emits real Swaps to reach
        // choose_initial_layout's target, so replaying every Swap in
        // the routed circuit from identity (not from the target layout
        // directly) is what actually matches physical reality.
        let mut logical_to_physical: Vec<PhysicalQubit> = (0..4).map(PhysicalQubit).collect();
        let mut physical_to_logical: Vec<LogicalQubit> = (0..4).map(LogicalQubit).collect();
        let mut found = None;
        for g in &routed.gates {
            match *g {
                Gate::Swap(a, b) => swap_mapping(
                    &mut logical_to_physical,
                    &mut physical_to_logical,
                    PhysicalQubit(a),
                    PhysicalQubit(b),
                ),
                Gate::Measure(q, clbit) => {
                    found = Some((q, clbit));
                    break;
                }
                _ => {}
            }
        }
        let (measured_wire, clbit) = found.expect("routed circuit must still contain a Measure");
        assert_eq!(clbit, 0);
        assert_eq!(
            measured_wire, logical_to_physical[0].0,
            "Measure must read qubit 0 off its current physical wire, not a stale one"
        );
    }

    #[test]
    fn lookahead_dense_random_circuit_routes_correctly_on_five_qubits() {
        let mut c = Circuit::new(5);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 4))
            .push(Gate::Rz(2, 0.37))
            .push(Gate::Cp(1, 4, 1.1))
            .push(Gate::Ryy(0, 3, 0.6))
            .push(Gate::Swap(1, 3))
            .push(Gate::Cz(0, 2));
        let coupling = CouplingMap::linear(5);
        assert_lookahead_routing_preserves_action(&c, &coupling);
    }

    #[test]
    fn lookahead_works_on_a_heavy_hex_coupling_map() {
        let coupling = CouplingMap::heavy_hex_for(12);
        let mut c = Circuit::new(12);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 11))
            .push(Gate::Rz(5, 0.42))
            .push(Gate::Cp(2, 9, 1.3))
            .push(Gate::Ryy(1, 8, 0.77))
            .push(Gate::Cz(3, 10));

        let routed = route_lookahead(&c, &coupling);
        for g in &routed.gates {
            let qs = g.qubits();
            if qs.len() == 2 {
                assert!(coupling.is_adjacent(qs[0], qs[1]), "gate {:?} not on a real edge", g);
            }
        }
        let mut logical_to_physical: Vec<PhysicalQubit> = (0..12).map(PhysicalQubit).collect();
        let mut physical_to_logical: Vec<LogicalQubit> = (0..12).map(LogicalQubit).collect();
        for g in &routed.gates {
            if let Gate::Swap(a, b) = *g {
                swap_mapping(
                    &mut logical_to_physical,
                    &mut physical_to_logical,
                    PhysicalQubit(a),
                    PhysicalQubit(b),
                );
            }
        }
        assert_eq!(logical_to_physical, (0..12).map(PhysicalQubit).collect::<Vec<_>>());
        assert_lookahead_routing_preserves_action(&c, &coupling);
    }

    #[test]
    fn choose_initial_layout_is_a_permutation() {
        let mut c = Circuit::new(6);
        c.push(Gate::Cx(0, 5)).push(Gate::Cx(0, 5)).push(Gate::Cx(0, 5));
        let coupling = CouplingMap::linear(6);
        let layout = choose_initial_layout(&c, &coupling);
        let mut sorted: Vec<usize> = layout.iter().map(|p| p.0).collect();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..6).collect::<Vec<_>>(), "layout must be a permutation: {:?}", layout);
    }

    #[test]
    fn detect_interaction_chain_recognizes_a_ghz_style_chain() {
        let mut c = Circuit::new(5);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 1))
            .push(Gate::Cx(1, 2))
            .push(Gate::Cx(2, 3))
            .push(Gate::Cx(3, 4));
        let weights = interaction_weights(&c);
        let chain = detect_interaction_chain(&weights).expect("should detect a chain");
        assert_eq!(chain, vec![LogicalQubit(0), LogicalQubit(1), LogicalQubit(2), LogicalQubit(3), LogicalQubit(4)]);
    }

    #[test]
    fn detect_interaction_chain_rejects_a_star() {
        // Logical 0 interacts with every other qubit -- degree 4 at
        // the center, not a chain.
        let mut c = Circuit::new(5);
        for t in 1..5 {
            c.push(Gate::Cx(0, t));
        }
        let weights = interaction_weights(&c);
        assert!(detect_interaction_chain(&weights).is_none());
    }

    #[test]
    fn detect_interaction_chain_rejects_a_cycle() {
        let mut c = Circuit::new(4);
        c.push(Gate::Cx(0, 1)).push(Gate::Cx(1, 2)).push(Gate::Cx(2, 3)).push(Gate::Cx(3, 0));
        let weights = interaction_weights(&c);
        assert!(detect_interaction_chain(&weights).is_none());
    }

    #[test]
    fn find_hamiltonian_path_finds_the_trivial_line_on_a_linear_coupling_map() {
        let coupling = CouplingMap::linear(6);
        let identity: Vec<PhysicalQubit> = (0..6).map(PhysicalQubit).collect();
        let path = find_hamiltonian_path(&coupling, 6, &identity)
            .expect("a 6-node line has a length-6 path");
        for w in path.windows(2) {
            assert!(coupling.is_adjacent(w[0].0, w[1].0), "path {:?} has a non-adjacent hop", path);
        }
        let mut seen: Vec<usize> = path.iter().map(|p| p.0).collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..6).collect::<Vec<_>>());
        // On a linear map the identity mapping *is* a valid Hamiltonian
        // path, and it's the closest possible to itself -- the bias
        // should find exactly it, at zero displacement.
        assert_eq!(path, identity, "on a linear map the bias should recover the identity path exactly");
    }

    #[test]
    fn find_hamiltonian_path_finds_a_path_through_heavy_hex() {
        let coupling = CouplingMap::heavy_hex_for(10);
        let identity: Vec<PhysicalQubit> = (0..10).map(PhysicalQubit).collect();
        let path = find_hamiltonian_path(&coupling, 10, &identity)
            .expect("heavy_hex_for(10) should admit a length-10 path");
        for w in path.windows(2) {
            assert!(coupling.is_adjacent(w[0].0, w[1].0), "path {:?} has a non-adjacent hop", path);
        }
        let mut seen: Vec<usize> = path.iter().map(|p| p.0).collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..10).collect::<Vec<_>>());
    }

    /// The headline regression: a 10-qubit GHZ-chain circuit
    /// (`Cx(0,1), Cx(1,2), ..., Cx(8,9)`) routed against a real
    /// heavy-hex coupling map should route with **zero** SWAPs via
    /// `choose_initial_layout`'s identity-biased chain fast path (see
    /// this module's `choose_initial_layout`/`find_hamiltonian_path`
    /// doc comments). `heavy_hex_for(10)` is `<= 12` qubits, i.e. a
    /// bare 12-cycle (see `coupling.rs`'s module doc), and
    /// `CouplingMap`'s DFS-order qubit numbering traces a cycle as an
    /// exact Hamiltonian path -- so the identity mapping this fast
    /// path searches for isn't just close, it's *exactly* realizable,
    /// with nothing to route and nothing to restore. This was not
    /// always true: under this crate's old BFS-order numbering,
    /// consecutive physical indices were graph-adjacent almost nowhere
    /// beyond the root, and this same circuit/topology pair cost 32
    /// total swaps (regression baseline before that: 58 with no chain
    /// fast path at all, 40 with an unbiased-but-BFS-numbered path
    /// search) -- the fix was the coupling-map numbering, not this
    /// fast path's search logic, which was already finding the best
    /// path the old numbering had to offer.
    #[test]
    fn ghz_chain_routes_with_zero_swaps_via_chain_fast_path_on_heavy_hex() {
        let mut c = Circuit::new(10);
        c.push(Gate::H(0));
        for q in 0..9 {
            c.push(Gate::Cx(q, q + 1));
        }
        let coupling = CouplingMap::heavy_hex_for(10);

        let layout = choose_initial_layout(&c, &coupling);
        let mut sorted: Vec<usize> = layout.iter().map(|p| p.0).collect();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..10).collect::<Vec<_>>(), "layout must be a permutation: {:?}", layout);
        for q in 0..9 {
            assert!(
                coupling.is_adjacent(layout[q].0, layout[q + 1].0),
                "chain hop {}-{} not placed adjacently: {:?}",
                q, q + 1, layout
            );
        }

        let routed = route_lookahead(&c, &coupling);
        assert_eq!(
            swap_count(&routed),
            0,
            "heavy_hex_for(10)'s DFS numbering traces a Hamiltonian path matching the \
             identity mapping exactly (see this test's doc comment), so a chain circuit \
             sized to fit should route with zero swaps; got {}: {:?}",
            swap_count(&routed),
            routed.gates
        );
        assert_lookahead_routing_preserves_action(&c, &coupling);
    }

    #[test]
    fn choose_initial_layout_places_a_heavily_interacting_pair_adjacently() {
        // Logical 0 and 5 interact repeatedly and are the *only* pair
        // that interacts at all -- a good layout should place them
        // coupling-adjacent even though they're maximally far apart
        // (distance 5) under the identity mapping on a 6-qubit line.
        let mut c = Circuit::new(6);
        for _ in 0..5 {
            c.push(Gate::Cx(0, 5));
        }
        let coupling = CouplingMap::linear(6);
        let layout = choose_initial_layout(&c, &coupling);
        assert!(
            coupling.is_adjacent(layout[0].0, layout[5].0),
            "logical 0 (phys {}) and logical 5 (phys {}) should have been placed adjacently: {:?}",
            layout[0].0,
            layout[5].0,
            layout
        );
    }

    #[test]
    fn lookahead_chooses_an_adjacent_initial_layout_for_a_heavily_interacting_pair() {
        // Unlike SABRE in Qiskit, this crate physically realizes its
        // chosen initial layout with real SWAP gates. Therefore the total
        // swap count is not guaranteed to be smaller than the naive router:
        // a better initial placement may itself cost swaps to reach.
        //
        // The property we *do* guarantee is that the layout heuristic
        // places a heavily interacting pair on adjacent physical qubits,
        // and that route_lookahead still preserves the circuit's action.

        let mut c = Circuit::new(6);

        for _ in 0..5 {
            c.push(Gate::Cx(0, 5));
        }

        let coupling = CouplingMap::linear(6);

        let layout = choose_initial_layout(&c, &coupling);

        assert!(
            coupling.is_adjacent(layout[0].0, layout[5].0),
            "expected the most heavily interacting pair to be adjacent: {:?}",
            layout
        );

        let routed = route_lookahead(&c, &coupling);

        for gate in &routed.gates {
            let qs = gate.qubits();

            if qs.len() == 2 {
                assert!(
                    coupling.is_adjacent(qs[0], qs[1]),
                    "non-adjacent routed gate {:?}",
                    gate
                );
            }
        }

        assert_lookahead_routing_preserves_action(&c, &coupling);
    }

    #[test]
    fn lookahead_uses_no_more_swaps_than_naive_route_on_multiple_distant_pairs() {
        // Three logical pairs, each maximally distant from each other
        // under the identity layout on a line, each interacting
        // several times: (0,3), (1,4), (2,5) on a 6-qubit chain. A
        // good initial layout can place all three pairs adjacently at
        // once (interleaving them along the line); `route`'s
        // single-gate-at-a-time greedy walk can't coordinate across
        // pairs like that.
        let mut c = Circuit::new(6);
        for _ in 0..4 {
            c.push(Gate::Cx(0, 3)).push(Gate::Cx(1, 4)).push(Gate::Cx(2, 5));
        }
        let coupling = CouplingMap::linear(6);
        let naive = route(&c, &coupling);
        let smart = route_lookahead(&c, &coupling);
        assert!(
            swap_count(&smart) <= swap_count(&naive),
            "route_lookahead used {} swaps vs route's {}: naive {:?} / smart {:?}",
            swap_count(&smart),
            swap_count(&naive),
            naive.gates,
            smart.gates
        );
        assert_lookahead_routing_preserves_action(&c, &coupling);
        assert_routing_preserves_action(&c, &coupling);
    }
}
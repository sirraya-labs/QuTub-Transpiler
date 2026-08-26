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
use crate::ir_optimize::commutes;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

/// A routed circuit paired with the exact index into `circuit.gates`
/// where the trailing [`restore_identity_mapping`] block -- if any --
/// begins. Every router in this module computes this for free: it's
/// just `circuit.gates.len()` at the moment right before that router's
/// own final `restore_identity_mapping`/`route_to_layout` call, so
/// there is no reason for code that actually needs the boundary
/// ([`route_best_no_restore`]) to re-derive it heuristically from gate
/// *shape* afterward the way [`restoration_swap_count`] does.
///
/// That heuristic -- "the trailing run of `Gate::Swap`s is
/// restoration" -- silently assumes the source circuit's own last real
/// gate is never itself a `Swap`, and that gates come out in original
/// program order. Both assumptions can be false: a circuit can
/// genuinely end in real `Swap`s (e.g. the standard QFT cascade's
/// trailing bit-reversal, `qft_like`'s own trailing block in this
/// module's tests), and [`route_sabre`]'s commutation-aware front-layer
/// scheduling can legitimately emit gates out of original program
/// order when they provably commute. Either one makes the heuristic
/// mistake real, load-bearing circuit content for restoration -- which
/// `route_best_no_restore` would then silently strip. `RoutedCircuit`
/// sidesteps the guesswork entirely by having each router report its
/// own boundary directly.
struct RoutedCircuit {
    circuit: Circuit,
    /// Number of gates in `circuit.gates` before the restoration tail;
    /// `circuit.gates[..restoration_start]` is real routed content,
    /// `circuit.gates[restoration_start..]` is pure identity-restore
    /// SWAPs (possibly empty).
    restoration_start: usize,
}

impl RoutedCircuit {
    /// SWAPs among the real, pre-restoration content only -- the
    /// number [`route_best_no_restore`] actually wants to minimize
    /// once restoration is going to be discarded regardless (see that
    /// function's own doc comment on why it can't just reuse
    /// `route_best`'s total-SWAP comparison).
    fn routing_swap_count(&self) -> usize {
        self.circuit.gates[..self.restoration_start]
            .iter()
            .filter(|g| matches!(g, Gate::Swap(..)))
            .count()
    }

    /// Drops the restoration tail exactly, using the recorded
    /// boundary rather than re-scanning gate shape.
    fn strip_restoration(&self) -> Circuit {
        let mut out = Circuit::new(self.circuit.num_qubits);
        out.num_clbits = self.circuit.num_clbits;
        for g in &self.circuit.gates[..self.restoration_start] {
            out.push(g.clone());
        }
        out
    }
}

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
    route_boundary(circuit, coupling).circuit
}

/// [`route`], but also returns the exact index into the result's
/// `gates` where the trailing [`restore_identity_mapping`] block
/// begins -- see [`RoutedCircuit`]'s own doc comment for why this
/// exists instead of [`restoration_swap_count`]'s heuristic.
fn route_boundary(circuit: &Circuit, coupling: &CouplingMap) -> RoutedCircuit {
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

    let restoration_start = out.gates.len();
    restore_identity_mapping(&mut out, &mut logical_to_physical, &mut physical_to_logical, coupling);

    RoutedCircuit { circuit: out, restoration_start }
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
    //
    // Deliberately does NOT filter out `usize::MAX` (unreachable)
    // entries before summing: on a coupling map with an isolated/
    // disconnected physical qubit (e.g. `CouplingMap::from_edges` given
    // a real device's edge list with a qubit that has no edges at all),
    // filtering made that qubit's sum artificially the *smallest*
    // (every unreachable pair just vanishes from the sum instead of
    // counting against it), so it looked like the ideal "center" when
    // it's actually the worst possible choice -- every other qubit's
    // distance to it is `usize::MAX`. `saturating_add` instead lets an
    // unreachable neighbor saturate the whole sum to `usize::MAX`,
    // correctly ranking a disconnected qubit last (or tied-last with
    // any other disconnected qubit, broken by ascending index same as
    // every other tie here) without overflowing.
    let center = PhysicalQubit(
        (0..n_phys)
            .min_by_key(|&p| dist[p].iter().fold(0usize, |acc, &d| acc.saturating_add(d)))
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
                        // `saturating_mul`/`saturating_add` rather than
                        // `*`/`+`: `dist[p][..]` (and `score`, built
                        // from it above) can legitimately be
                        // `usize::MAX` when `p` is unreachable from
                        // whatever it's being scored against -- a real,
                        // if rare, possibility on any coupling map with
                        // a disconnected physical qubit, not just a
                        // theoretical one. Saturating keeps such a
                        // qubit correctly ranked as maximally bad
                        // instead of panicking (debug) or silently
                        // wrapping to a tiny, falsely-attractive score
                        // (release).
                        if any_neighbor {
                            // Tie-break by physical index too, packed
                            // into the low bits, so results stay
                            // deterministic without a second sort pass.
                            score.saturating_mul(n_phys).saturating_add(p)
                        } else {
                            dist[p][center.0].saturating_mul(n_phys).saturating_add(p)
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
    route_lookahead_boundary(circuit, coupling).circuit
}

/// [`route_lookahead`], but also returns the real/restoration boundary
/// -- see [`RoutedCircuit`]'s own doc comment.
fn route_lookahead_boundary(circuit: &Circuit, coupling: &CouplingMap) -> RoutedCircuit {
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

    let restoration_start = out.gates.len();
    restore_identity_mapping(&mut out, &mut logical_to_physical, &mut physical_to_logical, coupling);

    RoutedCircuit { circuit: out, restoration_start }
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

/// Target size of [`sabre_pass`]'s extended scoring set -- a single
/// shared frontier expanded via BFS over the whole remaining circuit's
/// gate-dependency DAG (see [`build_successors`]), not a handful of
/// per-touched-qubit local lookaheads the way this crate's first SABRE
/// attempt built it. Matches the scale real SABRE implementations use
/// (Qiskit's own default extended-set size is also 20) -- deliberately
/// not re-tuned smaller or larger without a specific measured reason
/// to.
const SABRE_EXTENDED_SET_SIZE: usize = 20;

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

/// The gate-dependency DAG's direct-successor relation: `successors[gi]`
/// is every gate index that comes immediately after `gi` on at least
/// one of `gi`'s own qubits -- i.e. every gate that becomes one step
/// closer to executable the instant `gi` itself executes. Built once
/// per [`sabre_pass`] call, directly from the static `gate_qubits` list
/// (not from `queues`, which shrinks as execution progresses) -- this
/// relation itself never changes over the course of a pass, only which
/// nodes are already executed does.
///
/// This is what [`sabre_pass`]'s extended scoring set is actually built
/// from now: a real breadth-first frontier expansion across this DAG
/// starting from the current front layer's successors, not (as this
/// crate's first SABRE attempt built it) a handful of independent
/// per-touched-qubit local lookaheads. The difference matters --
/// per-qubit lookahead only ever sees gates involving qubits *already*
/// blocked right now, while a shared DAG frontier also reaches gates
/// one or two qubits removed from the current front layer that are
/// about to matter, the same "what's coming up across the whole
/// circuit, not just at this exact bottleneck" view real SABRE's own
/// extended set gives its heuristic.
/// The gate-dependency DAG's true predecessor relation for [`sabre_pass`]'s
/// front-layer eligibility -- as opposed to [`build_successors`], which
/// only feeds the *heuristic* extended-lookahead set. This is the hard
/// gate: `predecessors[gi]` is every earlier gate `gi` cannot be
/// scheduled ahead of, and until every one of them has executed, `gi`
/// is not front-eligible at all (see [`sabre_pass`]'s own doc comment).
///
/// Before this function existed, `sabre_pass` used the same strict
/// per-qubit FIFO-queue mechanism [`route_lookahead`] still uses today
/// (see [`gate_is_front`]): gate `gi` had to wait for
/// *every* earlier gate on each of its wires, whether or not the two
/// actually had to happen in that order. That's a correct but
/// needlessly conservative dependency -- a diagonal single-qubit gate
/// on a `Cx`'s control wire, say, doesn't actually have to wait for the
/// `Cx` at all (see [`crate::ir_optimize`]'s module doc for the
/// derivation). Reusing that already-proven, already-tested `commutes`
/// predicate here means `sabre_pass`'s front layer can legitimately
/// contain such a gate the moment its *true* predecessors have executed,
/// giving the router more genuinely-independent gates to schedule
/// around a bottleneck instead of one artificially serialized by wire
/// order alone -- more scheduling freedom to route the SWAPs that
/// matter instead of ones forced by a dependency that was never real.
///
/// # Algorithm
/// For every ordered pair `(i, j)` with `i < j` that shares at least
/// one qubit, `i` is a predecessor of `j` unless `commutes(gates[i],
/// gates[j])`. This is the full pairwise check, not just "nearest
/// wire-neighbor" -- deliberately: skipping straight to the nearest
/// same-wire predecessor and assuming transitivity covers the rest is
/// the trap ("silently drop a real ordering constraint three gates
/// back"), because a gate can sit in between two others on a wire that
/// it itself commutes with but that don't commute with *each other*'s
/// intended order relative to the pair on either side. Checking every
/// earlier co-touching gate directly is the version with no missed
/// constraints. It costs O(gates^2) in the worst case (this crate's
/// benchmark circuits top out at a few hundred gates, so this hasn't
/// needed to be revisited); some of the resulting edges are redundant
/// with an already-implied transitive path, but a redundant edge never
/// forbids a schedule that a minimal edge set would have allowed --
/// only a *missing* edge would silently do that -- so redundancy costs
/// a little wasted bookkeeping, never correctness.
///
/// `commutes`'s own coverage is what actually bounds how much slack
/// this finds: today it only has rules for single-qubit-gate-vs-
/// two-qubit-gate pairs (`Cx`-control, `Cx`-target, `Cz`-either-wire --
/// see its own doc comment), no two-qubit/two-qubit rule yet (e.g.
/// `Cx(a,b)`/`Cx(a,c)` sharing a control, which do commute but aren't
/// recognized as such here). That's a real, separate follow-up --
/// widening `commutes` itself, not this function -- left out here
/// deliberately rather than derived and verified in the same pass as
/// this scheduling change.
fn build_commutation_predecessors(
    gates: &[Gate],
    gate_qubits: &[Vec<LogicalQubit>],
) -> Vec<Vec<usize>> {
    let total = gate_qubits.len();
    let qubit_sets: Vec<BTreeSet<usize>> = gate_qubits
        .iter()
        .map(|qs| qs.iter().map(|q| q.0).collect())
        .collect();
    let mut predecessors: Vec<Vec<usize>> = vec![Vec::new(); total];
    for j in 0..total {
        for i in 0..j {
            if !qubit_sets[i].is_disjoint(&qubit_sets[j]) && !commutes(&gates[i], &gates[j]) {
                predecessors[j].push(i);
            }
        }
    }
    predecessors
}

fn build_successors(gate_qubits: &[Vec<LogicalQubit>], num_qubits: usize) -> Vec<Vec<usize>> {
    let mut per_qubit_order: Vec<Vec<usize>> = vec![Vec::new(); num_qubits];
    for (gi, qs) in gate_qubits.iter().enumerate() {
        for &q in qs {
            per_qubit_order[q.0].push(gi);
        }
    }
    let total_gates = gate_qubits.len();
    let mut successors: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); total_gates];
    for order in &per_qubit_order {
        for w in order.windows(2) {
            successors[w[0]].insert(w[1]);
        }
    }
    successors.into_iter().map(|s| s.into_iter().collect()).collect()
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
/// - **Deeper, size-normalized, whole-circuit lookahead.** The
///   extended scoring set is a single shared frontier of up to
///   [`SABRE_EXTENDED_SET_SIZE`] gates, reached via breadth-first
///   search over the gate-dependency DAG starting from the current
///   front layer (see [`build_successors`]) -- not, as this crate's
///   first SABRE attempt built it, a handful of independent
///   per-touched-qubit local lookaheads that only ever see gates
///   involving qubits already blocked right now. Both the front-layer
///   and extended terms are divided by their own set sizes before
///   being combined -- the standard SABRE-paper normalization, so a
///   momentarily large front layer doesn't automatically dominate the
///   score just by having more terms to sum.
///
/// `rng_state` adds a small score jitter (see [`SABRE_JITTER_SCALE`])
/// purely to diversify which candidate wins an otherwise-tied score
/// across [`route_sabre`]'s different trials -- it never overrides a
/// real score difference.
///
/// - **Commutation-aware front layer.** A gate's front-layer
///   eligibility no longer waits on strict per-qubit program order
///   (every earlier gate on each of its wires, full stop) -- it waits
///   only on [`build_commutation_predecessors`]'s true predecessors,
///   which omits an earlier same-wire gate the two are proven (via
///   [`crate::ir_optimize::commutes`]) to commute with. Two gates that
///   commute don't have to execute in their original program order for
///   the circuit's action to come out the same, so this lets a
///   genuinely independent gate become schedulable the moment its real
///   predecessors are done, instead of being artificially serialized
///   behind a same-wire gate it never actually depended on -- more
///   real scheduling freedom for the SWAP-selection heuristic below to
///   route around a bottleneck with, not more candidates chasing the
///   same forced order. See that function's own doc comment for the
///   derivation and what it does and doesn't cover.
fn sabre_pass(
    gates: &[Gate],
    gate_qubits: &[Vec<LogicalQubit>],
    coupling: &CouplingMap,
    dist: &[Vec<usize>],
    logical_to_physical: &mut [PhysicalQubit],
    physical_to_logical: &mut [LogicalQubit],
    mut on_event: impl FnMut(SabreEvent),
    mut on_frontier: impl FnMut(usize, usize),
    rng_state: &mut u64,
) {
    let num_qubits = logical_to_physical.len();
    if gate_qubits.is_empty() {
        return;
    }
    let total_gates = gate_qubits.len();

    // Front-layer eligibility (the hard gate: `remaining` only ever
    // drops when a gate has *no* unexecuted true predecessor left) is
    // commutation-aware, per [`build_commutation_predecessors`] --
    // deliberately a different, more precise relation than
    // `successors` below, which only feeds the heuristic extended-set
    // lookahead and stays on the coarser per-qubit-program-order
    // relation (see that function's own doc comment on why relaxing it
    // isn't needed for correctness there).
    let predecessors = build_commutation_predecessors(gates, gate_qubits);
    let mut pred_remaining: Vec<usize> = predecessors.iter().map(|p| p.len()).collect();
    let mut dependency_successors: Vec<Vec<usize>> = vec![Vec::new(); total_gates];
    for (gi, preds) in predecessors.iter().enumerate() {
        for &p in preds {
            dependency_successors[p].push(gi);
        }
    }

    let successors = build_successors(gate_qubits, num_qubits);
    let mut executed = vec![false; total_gates];
    let mut remaining = total_gates;

    let mut front: Vec<usize> = (0..total_gates).filter(|&gi| pred_remaining[gi] == 0).collect();

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
                    newly_executed.push(gi);
                    progressed = true;
                }
            }
            if progressed {
                front.retain(|&gi| !executed[gi]);
                let mut candidates: BTreeSet<usize> = BTreeSet::new();
                for &gi in &newly_executed {
                    for &s in &dependency_successors[gi] {
                        if executed[s] {
                            continue;
                        }
                        pred_remaining[s] -= 1;
                        if pred_remaining[s] == 0 {
                            candidates.insert(s);
                        }
                    }
                }
                for gi in candidates {
                    if !executed[gi] && !front.contains(&gi) {
                        front.push(gi);
                    }
                }
            }
        }

        if remaining == 0 {
            break;
        }

        let mut touched_physical: BTreeSet<PhysicalQubit> = BTreeSet::new();
        for &gi in &front {
            for &q in &gate_qubits[gi] {
                touched_physical.insert(logical_to_physical[q.0]);
            }
        }

        let mut candidate_swaps: BTreeSet<(PhysicalQubit, PhysicalQubit)> = BTreeSet::new();
        for &p in &touched_physical {
            for n in coupling.neighbors(p.0) {
                let n = PhysicalQubit(n);
                candidate_swaps.insert(if p.0 < n.0 { (p, n) } else { (n, p) });
            }
        }

        // Extended set: a single shared frontier, up to
        // SABRE_EXTENDED_SET_SIZE two-qubit gates, reached via
        // breadth-first search over `successors` starting from the
        // current front layer -- see `build_successors`'s doc comment
        // for why this sees meaningfully further than the old
        // per-touched-qubit local lookahead did. BTreeSet/Vec-based
        // (not HashSet) throughout: this crate learned this lesson
        // once already (see `route_to_layout`'s own doc comment) --
        // Rust's default HashSet hasher is randomly seeded per
        // process, and iteration order here directly decides both the
        // floating-point summation order for `ext_score` below (float
        // addition isn't perfectly associative) and which candidate
        // each `next_xorshift` jitter draw lands on, so a HashSet
        // anywhere in this BFS would make `route_sabre` silently
        // nondeterministic across separate calls on the identical
        // circuit within the same process, not just across process
        // runs.
        let mut visited: BTreeSet<usize> = front.iter().copied().collect();
        let mut bfs_queue: VecDeque<usize> = VecDeque::new();
        for &gi in &front {
            for &s in &successors[gi] {
                if !executed[s] && visited.insert(s) {
                    bfs_queue.push_back(s);
                }
            }
        }
        let mut extended_gates: Vec<usize> = Vec::new();
        while let Some(gi) = bfs_queue.pop_front() {
            if extended_gates.len() >= SABRE_EXTENDED_SET_SIZE {
                break;
            }
            if gate_qubits[gi].len() == 2 {
                extended_gates.push(gi);
            }
            for &s in &successors[gi] {
                if !executed[s] && visited.insert(s) {
                    bfs_queue.push_back(s);
                }
            }
        }
        let extended: Vec<(LogicalQubit, LogicalQubit)> = extended_gates
            .into_iter()
            .map(|gi| (gate_qubits[gi][0], gate_qubits[gi][1]))
            .collect();

        let front_two_qubit_count = front.iter().filter(|&&gi| gate_qubits[gi].len() == 2).count();
        on_frontier(front_two_qubit_count, extended.len());

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
/// # What actually moved the numbers, measured
/// This function has been through three rounds of measurement, not
/// just design -- worth recording plainly rather than leaving only the
/// current code behind:
/// 1. **First cut** (decay + per-touched-qubit local lookahead +
///    iterative refinement + multi-trial): cut SWAPs 12-21% versus
///    `route_lookahead` alone on `qft_10`/`qft_16`/
///    `long_range_random_20q_60gate` (`qiskit_benchmark.rs`). Tripling
///    the trial count and doubling the refinement-iteration count on
///    top of that bought essentially nothing further -- real evidence
///    the heuristic itself, not search budget, was the limit.
/// 2. **Shared-DAG extended set** (this function's current
///    [`build_successors`]-based BFS frontier, replacing the
///    per-touched-qubit local lookahead): only a small further gain on
///    top of (1) (roughly 3%) -- looking further ahead across the
///    whole circuit, not just at qubits already blocked right now,
///    mattered less than expected.
/// 3. **Zero-refinement identity trial** (this function's `configs`
///    array now includes `(identity, 0)` alongside the refined
///    configurations): the real second lever. Every refined layout
///    this crate reaches has to be *physically paid for* in real SWAPs
///    (see the realization-cost comment at `configs`' own definition)
///    -- for a circuit whose interaction graph is close to symmetric
///    across physical qubits (QFT, long-range-random), that
///    realization cost can outweigh the routing savings refinement
///    buys, the same failure mode this crate's own `ghz`-benchmark
///    investigation found once already for a star-shaped circuit. This
///    measured as the largest single gain of the three: an *additional*
///    5-17% on top of (1)+(2), pushing the cumulative reduction versus
///    the pre-SABRE baseline to ~18-35% -- largest on
///    `long_range_random_20q_60gate`, confirming realization cost was
///    a real, disproportionate cost there specifically.
///
/// Cumulative result: `route_sabre` now uses ~18-35% fewer SWAPs than
/// `route_lookahead` alone on this crate's three hardest measured
/// benchmarks, versus Qiskit's own router, which still uses
/// meaningfully fewer still (roughly 2-2.6x fewer than this function,
/// down from roughly 3x before any of this work) -- real, verified
/// progress, honestly still short of parity. The single-swap-at-a-time
/// greedy structure every trial configuration shares is the next
/// suspect: real SABRE-class routers this doesn't yet have any version
/// of a genuinely different strategy (e.g. batch-evaluating short
/// sequences of swaps together, or a proper beam search over partial
/// routings) rather than one locally-best swap at a time, every time.
/// That's a materially larger change than any of the three rounds
/// above and hasn't been attempted here.
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
/// Sweep-only variant of [`route_sabre`] with a caller-supplied trials-
/// per-seed count instead of the fixed [`SABRE_TRIALS_PER_SEED`]
/// constant, so a trial-count sensitivity curve can be measured
/// without hand-editing the constant and recompiling for every point.
/// Not part of the crate's real public API -- exists purely for the
/// `sabre_sweep` experiment.
/// Sweep-only instrumented variant: identical to [`route_sabre_with_trials`]
/// except it also records `(front_two_qubit_count, extended_set_size)`
/// at every swap-decision point of whichever trial ends up winning
/// (fewest total SWAPs) -- i.e. what the frontier actually looked like
/// during the routing this crate would really ship for this circuit,
/// not an average over discarded trials.
pub fn route_sabre_with_frontier_stats(
    circuit: &Circuit,
    coupling: &CouplingMap,
    trials_per_seed: usize,
) -> (Circuit, Vec<(usize, usize)>) {
    let num_qubits = circuit.num_qubits;

    let mut fallback = Circuit::new(num_qubits);
    fallback.num_clbits = circuit.num_clbits;
    for g in &circuit.gates {
        fallback.push(g.clone());
    }
    if num_qubits <= 1 {
        return (fallback, Vec::new());
    }

    let dist = distance_matrix(coupling);
    let gate_qubits: Vec<Vec<LogicalQubit>> = circuit
        .gates
        .iter()
        .map(|g| g.qubits().into_iter().map(LogicalQubit).collect())
        .collect();
    if gate_qubits.iter().all(|qs| qs.len() < 2) {
        return (fallback, Vec::new());
    }
    let reversed_gate_qubits: Vec<Vec<LogicalQubit>> = gate_qubits.iter().rev().cloned().collect();
    let reversed_gates: Vec<Gate> = circuit.gates.iter().rev().cloned().collect();

    let identity_seed: Vec<PhysicalQubit> = (0..num_qubits).map(PhysicalQubit).collect();
    let greedy_seed = choose_initial_layout(circuit, coupling);
    let configs: [(&Vec<PhysicalQubit>, usize); 3] = [
        (&identity_seed, 0),
        (&identity_seed, SABRE_LAYOUT_ITERATIONS),
        (&greedy_seed, SABRE_LAYOUT_ITERATIONS),
    ];

    let mut best: Option<Circuit> = None;
    let mut best_swaps = usize::MAX;
    let mut best_stats: Vec<(usize, usize)> = Vec::new();

    for (seed_idx, (seed_layout, layout_iterations)) in configs.iter().enumerate() {
        for trial in 0..trials_per_seed {
            let mut rng_state: u64 = (0x9E3779B97F4A7C15u64
                ^ ((seed_idx as u64 + 1) << 40)
                ^ (trial as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9))
                | 1;

            let mut l2p: Vec<PhysicalQubit> = (*seed_layout).clone();
            let mut p2l = vec![LogicalQubit(0); num_qubits];
            for (lq, &pq) in l2p.iter().enumerate() {
                p2l[pq.0] = LogicalQubit(lq);
            }

            for _ in 0..*layout_iterations {
                sabre_pass(
                    &circuit.gates, &gate_qubits, coupling, &dist,
                    &mut l2p, &mut p2l, |_| {}, |_, _| {}, &mut rng_state,
                );
                sabre_pass(
                    &reversed_gates, &reversed_gate_qubits, coupling, &dist,
                    &mut l2p, &mut p2l, |_| {}, |_, _| {}, &mut rng_state,
                );
            }

            let mut out = Circuit::new(num_qubits);
            out.num_clbits = circuit.num_clbits;
            let mut cur_l2p: Vec<PhysicalQubit> = (0..num_qubits).map(PhysicalQubit).collect();
            let mut cur_p2l: Vec<LogicalQubit> = (0..num_qubits).map(LogicalQubit).collect();
            route_to_layout(&mut out, &mut cur_l2p, &mut cur_p2l, &p2l, coupling);

            let mut trial_stats: Vec<(usize, usize)> = Vec::new();
            sabre_pass(
                &circuit.gates,
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
                |front_w, ext_w| trial_stats.push((front_w, ext_w)),
                &mut rng_state,
            );

            restore_identity_mapping(&mut out, &mut cur_l2p, &mut cur_p2l, coupling);

            let total = swap_count(&out);
            if total < best_swaps {
                best_swaps = total;
                best_stats = trial_stats;
                best = Some(out);
            }
        }
    }

    (best.expect("at least one trial always runs"), best_stats)
}

/// Scores a hypothetical layout (not necessarily the committed one) by
/// the same normalized front+extended-set distance formula
/// [`sabre_pass`]'s swap loop uses, factored out so [`sabre_pass2`]'s
/// depth-2 search can call it twice per candidate first-swap (once per
/// hypothetical second swap) without duplicating the formula.
fn score_layout(
    l2p: &[PhysicalQubit],
    front: &[usize],
    gate_qubits: &[Vec<LogicalQubit>],
    dist: &[Vec<usize>],
    extended: &[(LogicalQubit, LogicalQubit)],
) -> f64 {
    let mut front_score = 0.0f64;
    let mut front_count = 0usize;
    for &gi in front {
        let qs = &gate_qubits[gi];
        if qs.len() == 2 {
            front_score += dist[l2p[qs[0].0].0][l2p[qs[1].0].0] as f64;
            front_count += 1;
        }
    }
    let mut ext_score = 0.0f64;
    for &(a_lq, b_lq) in extended {
        ext_score += dist[l2p[a_lq.0].0][l2p[b_lq.0].0] as f64;
    }
    front_score / front_count.max(1) as f64 + LOOKAHEAD_WEIGHT * (ext_score / extended.len().max(1) as f64)
}

/// Identical to [`sabre_pass`] except the swap-selection step scores
/// each first candidate swap by its *best available two-swap
/// continuation* (search depth 2), not just its own immediate score --
/// then commits only that first swap and re-plans from scratch, the
/// same "search deeper, act shallow" structure a beam search uses one
/// ply at a time. Everything else (front-layer execution, extended-set
/// construction, decay, jitter) is unchanged from `sabre_pass`.
///
/// Cost: for each of the (typically small, see frontier-width
/// diagnostic) first-level candidate swaps, a second full candidate
/// search is run from the hypothetical post-swap layout -- roughly
/// `O(candidates^2)` scoring evaluations per swap decision instead of
/// `O(candidates)`, which is why this is a separate function rather
/// than folded into `sabre_pass` itself.
#[allow(clippy::too_many_arguments)]
fn sabre_pass2(
    gates: &[Gate],
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
    let total_gates = gate_qubits.len();

    let predecessors = build_commutation_predecessors(gates, gate_qubits);
    let mut pred_remaining: Vec<usize> = predecessors.iter().map(|p| p.len()).collect();
    let mut dependency_successors: Vec<Vec<usize>> = vec![Vec::new(); total_gates];
    for (gi, preds) in predecessors.iter().enumerate() {
        for &p in preds {
            dependency_successors[p].push(gi);
        }
    }

    let successors = build_successors(gate_qubits, num_qubits);
    let mut executed = vec![false; total_gates];
    let mut remaining = total_gates;

    let mut front: Vec<usize> = (0..total_gates).filter(|&gi| pred_remaining[gi] == 0).collect();

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
                    newly_executed.push(gi);
                    progressed = true;
                }
            }
            if progressed {
                front.retain(|&gi| !executed[gi]);
                let mut candidates: BTreeSet<usize> = BTreeSet::new();
                for &gi in &newly_executed {
                    for &s in &dependency_successors[gi] {
                        if executed[s] {
                            continue;
                        }
                        pred_remaining[s] -= 1;
                        if pred_remaining[s] == 0 {
                            candidates.insert(s);
                        }
                    }
                }
                for gi in candidates {
                    if !executed[gi] && !front.contains(&gi) {
                        front.push(gi);
                    }
                }
            }
        }

        if remaining == 0 {
            break;
        }

        let mut touched_physical: BTreeSet<PhysicalQubit> = BTreeSet::new();
        for &gi in &front {
            for &q in &gate_qubits[gi] {
                touched_physical.insert(logical_to_physical[q.0]);
            }
        }
        let mut candidate_swaps: BTreeSet<(PhysicalQubit, PhysicalQubit)> = BTreeSet::new();
        for &p in &touched_physical {
            for n in coupling.neighbors(p.0) {
                let n = PhysicalQubit(n);
                candidate_swaps.insert(if p.0 < n.0 { (p, n) } else { (n, p) });
            }
        }

        let mut visited: BTreeSet<usize> = front.iter().copied().collect();
        let mut bfs_queue: VecDeque<usize> = VecDeque::new();
        for &gi in &front {
            for &s in &successors[gi] {
                if !executed[s] && visited.insert(s) {
                    bfs_queue.push_back(s);
                }
            }
        }
        let mut extended_gates: Vec<usize> = Vec::new();
        while let Some(gi) = bfs_queue.pop_front() {
            if extended_gates.len() >= SABRE_EXTENDED_SET_SIZE {
                break;
            }
            if gate_qubits[gi].len() == 2 {
                extended_gates.push(gi);
            }
            for &s in &successors[gi] {
                if !executed[s] && visited.insert(s) {
                    bfs_queue.push_back(s);
                }
            }
        }
        let extended: Vec<(LogicalQubit, LogicalQubit)> = extended_gates
            .into_iter()
            .map(|gi| (gate_qubits[gi][0], gate_qubits[gi][1]))
            .collect();

        // Depth-2 search: for each first candidate, apply it to a
        // scratch layout, regenerate candidates from the resulting
        // touched-physical set, and take the *best* achievable
        // second-swap score as that first candidate's value. Decay and
        // jitter are applied only to the first (actually committed)
        // swap, matching `sabre_pass`'s own semantics for what decay is
        // for -- spreading real, committed SWAP usage across physical
        // qubits, not penalizing a hypothetical second move that's
        // never actually taken.
        let mut best_swap: Option<(PhysicalQubit, PhysicalQubit)> = None;
        let mut best_score = f64::MAX;
        for &(p1, p2) in &candidate_swaps {
            let mut l2p_1 = logical_to_physical.to_vec();
            let mut p2l_1 = physical_to_logical.to_vec();
            swap_mapping(&mut l2p_1, &mut p2l_1, p1, p2);

            let mut touched_2: BTreeSet<PhysicalQubit> = BTreeSet::new();
            for &gi in &front {
                for &q in &gate_qubits[gi] {
                    touched_2.insert(l2p_1[q.0]);
                }
            }
            let mut candidate_swaps_2: BTreeSet<(PhysicalQubit, PhysicalQubit)> = BTreeSet::new();
            for &p in &touched_2 {
                for n in coupling.neighbors(p.0) {
                    let n = PhysicalQubit(n);
                    candidate_swaps_2.insert(if p.0 < n.0 { (p, n) } else { (n, p) });
                }
            }

            let mut best_continuation = score_layout(&l2p_1, &front, gate_qubits, dist, &extended);
            for &(p3, p4) in &candidate_swaps_2 {
                let mut l2p_2 = l2p_1.clone();
                let mut p2l_2 = p2l_1.clone();
                swap_mapping(&mut l2p_2, &mut p2l_2, p3, p4);
                let s = score_layout(&l2p_2, &front, gate_qubits, dist, &extended);
                if s < best_continuation {
                    best_continuation = s;
                }
            }

            let decay_factor = decay[p1.0].max(decay[p2.0]);
            let jitter = (next_xorshift(rng_state) as f64 / u64::MAX as f64) * SABRE_JITTER_SCALE;
            let score = decay_factor * best_continuation + jitter;

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

/// [`route_sabre_with_trials`], but using [`sabre_pass2`]'s depth-2
/// swap search for the final commit pass (the one that actually emits
/// gates) instead of `sabre_pass`'s depth-1 greedy choice. The layout-
/// refinement sweeps (forward/backward, before the commit pass) still
/// use the cheaper depth-1 `sabre_pass`, since they only ever produce a
/// *candidate initial layout* to be physically realized and re-routed
/// by the commit pass anyway -- spending depth-2 search there wouldn't
/// change what gets emitted, only how the (already realization-costed)
/// candidate layout was chosen.
pub fn route_sabre2_with_trials(circuit: &Circuit, coupling: &CouplingMap, trials_per_seed: usize) -> Circuit {
    route_sabre2_impl(circuit, coupling, trials_per_seed).circuit
}

/// [`route_sabre2_with_trials`]'s implementation, returning the
/// real/restoration boundary alongside the circuit -- see
/// [`RoutedCircuit`]'s own doc comment.
fn route_sabre2_impl(circuit: &Circuit, coupling: &CouplingMap, trials_per_seed: usize) -> RoutedCircuit {
    let num_qubits = circuit.num_qubits;

    let mut fallback = Circuit::new(num_qubits);
    fallback.num_clbits = circuit.num_clbits;
    for g in &circuit.gates {
        fallback.push(g.clone());
    }
    if num_qubits <= 1 {
        let restoration_start = fallback.gates.len();
        return RoutedCircuit { circuit: fallback, restoration_start };
    }

    let dist = distance_matrix(coupling);
    let gate_qubits: Vec<Vec<LogicalQubit>> = circuit
        .gates
        .iter()
        .map(|g| g.qubits().into_iter().map(LogicalQubit).collect())
        .collect();
    if gate_qubits.iter().all(|qs| qs.len() < 2) {
        let restoration_start = fallback.gates.len();
        return RoutedCircuit { circuit: fallback, restoration_start };
    }
    let reversed_gate_qubits: Vec<Vec<LogicalQubit>> = gate_qubits.iter().rev().cloned().collect();
    let reversed_gates: Vec<Gate> = circuit.gates.iter().rev().cloned().collect();

    let identity_seed: Vec<PhysicalQubit> = (0..num_qubits).map(PhysicalQubit).collect();
    let greedy_seed = choose_initial_layout(circuit, coupling);
    let configs: [(&Vec<PhysicalQubit>, usize); 3] = [
        (&identity_seed, 0),
        (&identity_seed, SABRE_LAYOUT_ITERATIONS),
        (&greedy_seed, SABRE_LAYOUT_ITERATIONS),
    ];

    let mut best: Option<RoutedCircuit> = None;
    let mut best_swaps = usize::MAX;

    for (seed_idx, (seed_layout, layout_iterations)) in configs.iter().enumerate() {
        for trial in 0..trials_per_seed {
            let mut rng_state: u64 = (0x9E3779B97F4A7C15u64
                ^ ((seed_idx as u64 + 1) << 40)
                ^ (trial as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9))
                | 1;

            let mut l2p: Vec<PhysicalQubit> = (*seed_layout).clone();
            let mut p2l = vec![LogicalQubit(0); num_qubits];
            for (lq, &pq) in l2p.iter().enumerate() {
                p2l[pq.0] = LogicalQubit(lq);
            }

            for _ in 0..*layout_iterations {
                sabre_pass(
                    &circuit.gates, &gate_qubits, coupling, &dist,
                    &mut l2p, &mut p2l, |_| {}, |_, _| {}, &mut rng_state,
                );
                sabre_pass(
                    &reversed_gates, &reversed_gate_qubits, coupling, &dist,
                    &mut l2p, &mut p2l, |_| {}, |_, _| {}, &mut rng_state,
                );
            }

            let mut out = Circuit::new(num_qubits);
            out.num_clbits = circuit.num_clbits;
            let mut cur_l2p: Vec<PhysicalQubit> = (0..num_qubits).map(PhysicalQubit).collect();
            let mut cur_p2l: Vec<LogicalQubit> = (0..num_qubits).map(LogicalQubit).collect();
            route_to_layout(&mut out, &mut cur_l2p, &mut cur_p2l, &p2l, coupling);

            sabre_pass2(
                &circuit.gates,
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

            let restoration_start = out.gates.len();
            restore_identity_mapping(&mut out, &mut cur_l2p, &mut cur_p2l, coupling);

            let total = swap_count(&out);
            if total < best_swaps {
                best_swaps = total;
                best = Some(RoutedCircuit { circuit: out, restoration_start });
            }
        }
    }

    best.expect("at least one trial always runs")
}

pub fn route_sabre_with_trials(circuit: &Circuit, coupling: &CouplingMap, trials_per_seed: usize) -> Circuit {
    route_sabre_impl(circuit, coupling, trials_per_seed).circuit
}

pub fn route_sabre(circuit: &Circuit, coupling: &CouplingMap) -> Circuit {
    route_sabre_impl(circuit, coupling, SABRE_TRIALS_PER_SEED).circuit
}

/// [`route_sabre`]/[`route_sabre_with_trials`]'s shared implementation,
/// returning the real/restoration boundary alongside the circuit -- see
/// [`RoutedCircuit`]'s own doc comment.
fn route_sabre_impl(circuit: &Circuit, coupling: &CouplingMap, trials_per_seed: usize) -> RoutedCircuit {
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
        // Identity mapping throughout: nothing was ever routed, so
        // every gate here is real content, none of it restoration.
        let restoration_start = fallback.gates.len();
        return RoutedCircuit { circuit: fallback, restoration_start };
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
        let restoration_start = fallback.gates.len();
        return RoutedCircuit { circuit: fallback, restoration_start };
    }
    let reversed_gate_qubits: Vec<Vec<LogicalQubit>> = gate_qubits.iter().rev().cloned().collect();
    // Reversed `Gate`s to match `reversed_gate_qubits`, so
    // `build_commutation_predecessors` run on the reversed direction
    // sees the same gates in the same (reversed) relative order --
    // `commutes` is symmetric (`commutes(a,b) == commutes(b,a)`, see
    // its own doc comment), so this correctly yields the transpose of
    // the forward pass's dependency DAG rather than a second,
    // independently-derived one.
    let reversed_gates: Vec<Gate> = circuit.gates.iter().rev().cloned().collect();

    let identity_seed: Vec<PhysicalQubit> = (0..num_qubits).map(PhysicalQubit).collect();
    let greedy_seed = choose_initial_layout(circuit, coupling);
    // (seed layout, number of forward+backward refinement sweeps to run
    // before committing). `(identity, 0)` is its own deliberate trial
    // configuration, not just a degenerate case of the others: every
    // *other* configuration below refines its layout away from
    // identity, which this crate always has to pay real SWAPs to
    // physically realize (see `route_lookahead`'s own comment on why
    // that cost is real here, not virtual). For a circuit whose
    // interaction graph is genuinely close to symmetric across
    // physical qubits (QFT, or any other near-all-to-all circuit --
    // see `route_sabre`'s own doc comment), no candidate layout is
    // meaningfully better-connected than identity already is, so
    // refinement's realization cost can easily outweigh whatever
    // routing savings it buys -- the exact failure mode this crate's
    // own `ghz`-benchmark investigation already found once for a
    // star-shaped circuit (see `coupling.rs`'s module doc). `(identity,
    // 0)` -- run `sabre_pass`'s decay+deeper-lookahead heuristic
    // straight from identity, paying zero realization cost -- is what
    // actually tests that hypothesis directly, rather than assuming
    // refinement always helps.
    let configs: [(&Vec<PhysicalQubit>, usize); 3] = [
        (&identity_seed, 0),
        (&identity_seed, SABRE_LAYOUT_ITERATIONS),
        (&greedy_seed, SABRE_LAYOUT_ITERATIONS),
    ];

    let mut best: Option<RoutedCircuit> = None;
    let mut best_swaps = usize::MAX;

    for (seed_idx, (seed_layout, layout_iterations)) in configs.iter().enumerate() {
        for trial in 0..trials_per_seed {
            let mut rng_state: u64 = (0x9E3779B97F4A7C15u64
                ^ ((seed_idx as u64 + 1) << 40)
                ^ (trial as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9))
                | 1;

            let mut l2p: Vec<PhysicalQubit> = (*seed_layout).clone();
            let mut p2l = vec![LogicalQubit(0); num_qubits];
            for (lq, &pq) in l2p.iter().enumerate() {
                p2l[pq.0] = LogicalQubit(lq);
            }

            for _ in 0..*layout_iterations {
                sabre_pass(
                    &circuit.gates,
                    &gate_qubits,
                    coupling,
                    &dist,
                    &mut l2p,
                    &mut p2l,
                    |_| {},
                    |_, _| {},
                    &mut rng_state,
                );
                sabre_pass(
                    &reversed_gates,
                    &reversed_gate_qubits,
                    coupling,
                    &dist,
                    &mut l2p,
                    &mut p2l,
                    |_| {},
                    |_, _| {},
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
                &circuit.gates,
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
                |_, _| {},
                &mut rng_state,
            );

            let restoration_start = out.gates.len();
            restore_identity_mapping(&mut out, &mut cur_l2p, &mut cur_p2l, coupling);

            let total = swap_count(&out);
            if total < best_swaps {
                best_swaps = total;
                best = Some(RoutedCircuit { circuit: out, restoration_start });
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

/// Splits a routed circuit's total `Gate::Swap` count into "routing"
/// SWAPs (inserted mid-circuit to get some later real gate onto
/// adjacent physical qubits) vs "restoration" SWAPs (the trailing
/// block every router in this module appends via
/// [`restore_identity_mapping`], after every real gate has already
/// been scheduled, purely to walk physical qubits back to their
/// starting wires before returning) -- inferred from `routed`'s gate
/// *shape* alone: "everything after the last non-`Swap` gate is
/// restoration."
///
/// # This is a heuristic, not exact -- know its failure modes
/// The shape-only inference silently assumes two things that don't
/// always hold: (1) the *source* circuit's own last real gate is never
/// itself a `Swap`, and (2) gates come out of the router in original
/// program order. Both can be false -- a circuit can genuinely end in
/// real `Swap`s (the standard QFT cascade's trailing bit-reversal is
/// exactly this shape; see `qft_like` in this module's own tests), and
/// [`route_sabre`]'s commutation-aware front-layer scheduling can
/// legitimately emit gates out of original program order when they
/// provably commute. On either circuit, this function can mistake real,
/// load-bearing circuit content for restoration.
///
/// For any caller that has the routed circuit fresh from one of this
/// module's own routers (rather than receiving an opaque `Circuit`
/// after the fact) and actually needs an exact split -- as opposed to
/// just an approximate cost estimate, which is this function's only
/// remaining use in this module -- use the router's own
/// [`RoutedCircuit`]-returning `*_boundary`/`*_impl` form instead,
/// which reports the boundary directly rather than inferring it.
/// [`route_best_no_restore`] does exactly this, for exactly this
/// reason.
///
/// Degenerate case: if `routed` is empty or contains only `Swap`s (no
/// real gate at all -- never produced by this module's own routers on
/// a non-empty input circuit, but not otherwise disallowed by
/// `Circuit`'s own type), every SWAP present is counted as
/// `restoration`, since there is no real gate for any of them to have
/// been "routing" toward.
pub fn restoration_swap_count(routed: &Circuit) -> (usize, usize) {
    match routed.gates.iter().rposition(|g| !matches!(g, Gate::Swap(..))) {
        Some(last_real_gate) => {
            let routing = routed.gates[..=last_real_gate]
                .iter()
                .filter(|g| matches!(g, Gate::Swap(..)))
                .count();
            let restoration = routed.gates[last_real_gate + 1..]
                .iter()
                .filter(|g| matches!(g, Gate::Swap(..)))
                .count();
            (routing, restoration)
        }
        None => (0, swap_count(routed)),
    }
}

/// Routes `circuit` against `coupling` via [`route`], [`route_lookahead`],
/// [`route_sabre`], and [`route_sabre2_with_trials`], and returns
/// whichever result used fewest `Swap`s.
///
/// This exists because none of the candidates is a strict improvement
/// on the others in every case:
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
/// - `route_sabre2_with_trials`'s depth-2 commit-pass search costs
///   roughly `O(candidates^2)` scoring evaluations per swap decision
///   instead of `route_sabre`'s `O(candidates)` (see `sabre_pass2`'s
///   own doc comment) for a benefit that isn't guaranteed on every
///   circuit -- looking two swaps ahead can still commit to a first
///   swap that's locally optimal-looking but globally no better (or
///   occasionally worse, before jitter/trial averaging) than the
///   depth-1 choice, so it's compared here rather than assumed to
///   dominate.
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
///
/// # QFT fast path
/// [`route_qft`] is tried alongside the three general-purpose routers:
/// if `circuit` is exactly the textbook QFT gate cascade (see
/// [`detect_qft_cascade`]), it returns a routed circuit built from the
/// dedicated LNN "cascade"/"bubble" QFT construction, whose cost is
/// fixed by the circuit's shape alone (one `Swap` per `Cp`, i.e.
/// exactly `n*(n-1)/2` on an `n`-qubit QFT -- see
/// [`emit_qft_cascade`]'s own doc comment) rather than by heuristic
/// search. That fixed cost comfortably beats all three general routers
/// on a linear coupling map (see this module's
/// `route_qft_uses_exactly_one_swap_per_cp_and_beats_general_routers`
/// test), but is only ever a *candidate* here, scored by [`swap_count`]
/// exactly like the other three, rather than an unconditional
/// short-circuit: on a sparser, higher-degree coupling map (e.g.
/// `heavy_hex_for`) a search-based router can in principle find a
/// shorter path between some pair than the cascade's fixed linear
/// walk affords, so nothing here assumes the cascade is *always*
/// optimal, only that it's worth comparing. [`route_qft`] returns
/// `None` (simply dropping out of the comparison) whenever `circuit`
/// isn't exactly that shape, or [`find_hamiltonian_path`] can't fit a
/// length-`n` path through `coupling`.
pub fn route_best(circuit: &Circuit, coupling: &CouplingMap) -> Circuit {
    let naive = route(circuit, coupling);
    let smart = route_lookahead(circuit, coupling);
    let sabre = route_sabre(circuit, coupling);
    // `route_sabre2_with_trials`'s depth-2 commit-pass search (see its
    // own doc comment) is strictly more expensive per trial than
    // `route_sabre`'s depth-1 pass, never cheaper, so it's added here
    // as one more candidate scored by `swap_count` -- same pattern as
    // every other router in this comparison -- rather than replacing
    // `route_sabre` outright: nothing about depth-2 search guarantees
    // it beats depth-1 on every circuit (a deeper look ahead can still
    // commit to a locally-better swap that's globally worse), so this
    // function's own "let swap_count decide" contract is exactly what
    // settles that question per-circuit instead of assuming an answer.
    let sabre2 = route_sabre2_with_trials(circuit, coupling, SABRE_TRIALS_PER_SEED);
    let mut best = naive;
    if swap_count(&smart) < swap_count(&best) {
        best = smart;
    }
    if swap_count(&sabre) < swap_count(&best) {
        best = sabre;
    }
    if swap_count(&sabre2) < swap_count(&best) {
        best = sabre2;
    }
    if let Some(qft_routed) = route_qft(circuit, coupling) {
        if swap_count(&qft_routed) < swap_count(&best) {
            best = qft_routed;
        }
    }
    best
}

/// [`route_best`], but for callers who don't need the returned
/// circuit's physical layout to match its logical layout once the
/// circuit is done -- the common case being a circuit that ends in
/// `Gate::Measure`s, since a `Measure` already records whichever
/// physical wire its qubit was on *at that point in program order*
/// (see `Gate::Measure`'s own doc comment), not whatever the final
/// layout ends up being. For such a caller, every SWAP
/// `restore_identity_mapping` appends after the last real gate is pure
/// cost with no effect on the result -- exactly the "restoration tax"
/// `restoration_swap_count` measures (concentrated as high as ~29% of
/// a circuit's SWAPs on some of this crate's own benchmarks).
///
/// Callers that don't fit that description -- composing this circuit's
/// output with a second circuit fragment back-to-back, or anything
/// else that relies on physical qubit `i` meaning logical qubit `i` at
/// the end -- should use [`route_best`] instead, which keeps that
/// guarantee.
///
/// # Candidate selection differs from `route_best`
/// `route_best` picks the candidate with fewest *total* SWAPs, which
/// is the right comparison when every candidate pays its own
/// restoration cost. Here, restoration SWAPs are about to be discarded
/// regardless of which candidate has more of them, so candidates are
/// compared by [`RoutedCircuit::routing_swap_count`] instead -- a
/// candidate that has fewer total SWAPs than another purely because it
/// front-loaded more of its cost into (soon to be dropped) restoration
/// is not actually the better choice once restoration is gone, and
/// `route_best`'s own selection would silently pick it anyway if this
/// function just stripped `route_best`'s output instead of re-selecting.
///
/// # Why this doesn't use `restoration_swap_count`/`strip_restoration_swaps`
/// Those two infer the real/restoration boundary heuristically, from
/// gate *shape* (the trailing run of `Gate::Swap`s), which silently
/// assumes the source circuit's own last real gate is never itself a
/// `Swap` and that gates come out in original program order -- both
/// false in general (a circuit can genuinely end in real `Swap`s, e.g.
/// the standard QFT cascade's trailing bit-reversal, and
/// [`route_sabre`]'s commutation-aware scheduling can reorder gates).
/// Getting this wrong here doesn't just miscount -- it makes this
/// function select a worse-but-mislabeled candidate *and* then strip
/// real circuit content from it, silently corrupting the result. Each
/// candidate below instead reports its own exact boundary via
/// [`RoutedCircuit`], so no inference is needed.
pub fn route_best_no_restore(circuit: &Circuit, coupling: &CouplingMap) -> Circuit {
    let mut candidates = vec![
        route_boundary(circuit, coupling),
        route_lookahead_boundary(circuit, coupling),
        route_sabre_impl(circuit, coupling, SABRE_TRIALS_PER_SEED),
        route_sabre2_impl(circuit, coupling, SABRE_TRIALS_PER_SEED),
    ];
    if let Some(qft_routed) = route_qft_boundary(circuit, coupling) {
        candidates.push(qft_routed);
    }
    let best = candidates
        .iter()
        .min_by_key(|c| c.routing_swap_count())
        .expect("candidates always has at least the four unconditional routers pushed above");
    best.strip_restoration()
}

// ---------------------------------------------------------------------
// P4: QFT LNN-cascade fast path.
//
// `route`/`route_lookahead`/`route_sabre` are all general-purpose:
// none of them knows anything about *why* a circuit needs the SWAPs it
// needs, so on a circuit whose two-qubit interaction graph is genuinely
// all-to-all -- which is exactly what a QFT's `Cp` cascade is, every
// qubit `i` paired with every qubit `j > i` -- even `route_sabre`'s
// layout refinement has no non-trivial structure left to exploit (see
// its own doc comment: realization cost for a refined layout tends to
// outweigh the savings on exactly this shape). But an all-to-all QFT
// specifically has a known-optimal-shape answer that has nothing to do
// with general routing search at all: the linear-nearest-neighbor
// "cascade" or "bubble" construction (Fowler, Devitt & Hollenberg,
// *Implementation of Shor's algorithm on a linear nearest neighbour
// qubit array*, 2004; see also Maslov, *Linear depth stabilizer and
// quantum Fourier transformation circuits with no auxiliary qubits in
// finite-neighbor quantum architectures*, 2007), which walks each
// qubit across the register one hop at a time, doing its `Cp` with
// whichever qubit it currently neighbors and then physically swapping
// past it -- one `Swap` per `Cp`, always, regardless of which physical
// qubits the coupling map happens to number where. That's `n*(n-1)/2`
// SWAPs for an `n`-qubit QFT, a fixed cost with no search involved,
// and one this crate's own general routers don't reliably find on
// their own (see `route_sabre`'s doc comment on this exact benchmark
// shape).
//
// [`detect_qft_cascade`] recognizes the shape, [`emit_qft_cascade`]
// re-derives the construction onto whatever physical [`CouplingMap`]
// path is available (not just a bare linear register -- see that
// function's own doc comment), and [`route_qft`] wires both into this
// module's existing [`route_to_layout`]/[`restore_identity_mapping`]
// infrastructure so the fast path is exactly as semantics-preserving
// (fidelity-exact, identity-restoring) as `route`/`route_lookahead`/
// `route_sabre`, not a special-cased correctness island.
// ---------------------------------------------------------------------

/// Per-(i,j) `Cp` angle for a detected QFT cascade: `angles[i][j]` is
/// the angle of the `Cp` gate between logical qubits `i` and `j` for
/// `j > i`; entries with `j <= i` are unused and left `0.0`.
type QftAngles = Vec<Vec<f64>>;

/// Recognizes the exact textbook QFT gate cascade `qiskit_benchmark.rs`'s
/// `qft`/`qft_10`/`qft_16` benchmarks (and this module's own `qft_like`
/// test helper) emit: for each logical qubit `i` in increasing order,
/// `H(i)` followed by `Cp(j, i, angle)` for every `j` from `i+1` to
/// `n-1` in increasing order, where `angle` is whatever this circuit
/// says it is (not re-derived or checked against the standard
/// `PI / 2^(j-i)` QFT angle formula -- this is a *shape* detector, not
/// a QFT-specific-angle validator, so [`route_qft`] correctly routes
/// any circuit with this exact gate structure regardless of which
/// angles it carries). Optionally, the cascade may be followed by
/// exactly the standard trailing reversal `Swap`s
/// (`Swap(q, n-1-q)` for `q` in `0..n/2`) -- if present, they're
/// consumed and *not* re-emitted, because [`emit_qft_cascade`]'s own
/// embedded `Swap`s already realize that exact reversal for free (see
/// its own doc comment) -- but the cascade alone, without any trailing
/// `Swap`s at all, matches too, since a caller may already have
/// dropped that block itself.
///
/// Returns `None` (not this circuit's shape at all) if any gate
/// deviates from the exact expected sequence, in the exact expected
/// order.
fn detect_qft_cascade(circuit: &Circuit) -> Option<QftAngles> {
    let n = circuit.num_qubits;
    if n == 0 {
        return None;
    }
    let gates = &circuit.gates;
    let mut angles: QftAngles = vec![vec![0.0f64; n]; n];
    let mut gi = 0;

    for i in 0..n {
        match gates.get(gi) {
            Some(&Gate::H(q)) if q == i => gi += 1,
            _ => return None,
        }
        for j in (i + 1)..n {
            match gates.get(gi) {
                Some(&Gate::Cp(control, target, lambda)) if control == j && target == i => {
                    angles[i][j] = lambda;
                    gi += 1;
                }
                _ => return None,
            }
        }
    }

    let remaining = &gates[gi..];
    if !remaining.is_empty() {
        let expected: Vec<Gate> = (0..n / 2).map(|q| Gate::Swap(q, n - 1 - q)).collect();
        if remaining != expected.as_slice() {
            return None;
        }
    }
    Some(angles)
}

/// Re-derives the Fowler/Devitt/Hollenberg (2004) / Maslov (2007) LNN
/// QFT "cascade"/"bubble" construction over an explicit physical
/// [`CouplingMap`] path (`path[k]` is the physical qubit occupying
/// line-position `k`), rather than assuming physical labels are
/// numbered `0..n` along the register the way the textbook description
/// does.
///
/// For each logical "anchor" qubit `i` in increasing order: emit `H`
/// on the anchor at its current line position, then walk it one hop at
/// a time toward the far end of the path, at each hop emitting the
/// `Cp` between the anchor and whichever logical qubit currently
/// occupies the next position (looked up by that qubit's *original*
/// logical index, so the angle and control/target roles are exactly
/// what the source circuit specified -- see [`detect_qft_cascade`]'s
/// `angles` matrix), immediately followed by a `Swap` that both
/// realizes the adjacency the gate needed and bubbles the anchor
/// forward one position. One physical `Swap` per `Cp`, always -- this
/// crate's other routers pay a connectivity-dependent, heuristic-search
/// SWAP count on this same all-to-all shape (see this section's module
/// doc comment); this construction's cost is exactly `n*(n-1)/2`
/// SWAPs, fixed by `n` alone, on *any* [`CouplingMap`] that admits a
/// length-`n` Hamiltonian path at all.
///
/// The construction's own embedded `Swap`s happen to realize exactly
/// the permutation that the source circuit's optional trailing
/// reversal `Swap` block exists to undo, so by the time this returns,
/// physical qubit `path[i]` already holds logical qubit `i`'s final,
/// correctly-ordered QFT output -- whether or not the source circuit
/// included that trailing block (see [`detect_qft_cascade`], which
/// drops it if present). [`route_qft`] still calls
/// [`restore_identity_mapping`] unconditionally afterward rather than
/// relying on that algebraic fact procedurally, so this stays correct
/// even if this function's own `Swap` sequence is ever changed
/// independently -- on the identity path (`path[k] == k`), that call
/// finds the mapping already exactly identity and inserts zero
/// additional `Swap`s, so the fact costs nothing when it holds.
fn emit_qft_cascade(n: usize, angles: &QftAngles, path: &[PhysicalQubit]) -> Vec<Gate> {
    debug_assert_eq!(path.len(), n, "path must have exactly one physical qubit per line position");
    let mut at: Vec<usize> = (0..n).collect(); // at[pos] = logical qubit currently at line position `pos`
    let mut pos: Vec<usize> = (0..n).collect(); // pos[logical] = that logical qubit's current line position
    let mut out = Vec::new();

    for i in 0..n {
        let mut p = pos[i];
        out.push(Gate::H(path[p].0));
        for _ in 0..(n - 1 - i) {
            let other = at[p + 1];
            let (target, lambda) = if other > i { (i, angles[i][other]) } else { (other, angles[other][i]) };
            let phys_target = if target == i { path[p] } else { path[p + 1] };
            let phys_control = if target == i { path[p + 1] } else { path[p] };
            out.push(Gate::Cp(phys_control.0, phys_target.0, lambda));
            out.push(Gate::Swap(path[p].0, path[p + 1].0));
            at.swap(p, p + 1);
            pos[i] = p + 1;
            pos[other] = p;
            p += 1;
        }
    }
    out
}

/// Routes `circuit` via the dedicated QFT LNN-cascade construction
/// (see this section's module doc comment) instead of general-purpose
/// SWAP insertion, if and only if `circuit` is exactly the shape
/// [`detect_qft_cascade`] recognizes and `coupling` admits a
/// length-`n` Hamiltonian path ([`find_hamiltonian_path`], biased
/// toward the identity mapping -- free on any coupling map that admits
/// the identity path itself, e.g. [`CouplingMap::linear`]). Returns
/// `None` otherwise, so callers (currently just [`route_best`]) can
/// fall through to the general routers unconditionally.
///
/// # Algorithm
/// 1. [`detect_qft_cascade`] the circuit's `Cp` angles; bail to `None`
///    if it isn't this exact shape.
/// 2. [`find_hamiltonian_path`] a physical path through `coupling`,
///    biased toward the identity mapping; bail to `None` if none
///    exists within budget.
/// 3. Physically realize "logical `i` starts at physical `path[i]`"
///    via [`route_to_layout`] -- the same real, non-optional
///    realization cost `route_lookahead`/`route_sabre` pay to reach a
///    non-identity starting layout (see their own doc comments),
///    priced in here exactly the same way; a genuine no-op whenever
///    `path` is already the identity (e.g. [`CouplingMap::linear`]),
///    since `route_to_layout` only emits `Swap`s for tokens not
///    already home.
/// 4. Emit [`emit_qft_cascade`]'s gates as-is.
/// 5. [`restore_identity_mapping`] back from `path`'s layout to the
///    identity -- **not** derived by tracking the cascade's own
///    `Swap`s as ordinary routing relocations (they are not that --
///    see the important note below), but from the same closed-form
///    `path_physical_to_logical` mapping step 3 already computed.
///    Also a genuine no-op whenever `path` is the identity, symmetric
///    with step 3.
///
/// # Why step 5 doesn't track the cascade's own `Swap`s
/// Every other router in this module (`route`/`route_lookahead`/
/// `route_sabre`) treats every `Swap` it emits as a data relocation:
/// [`swap_mapping`] updates the logical/physical bookkeeping after
/// each one, and whatever that bookkeeping says at the end is what
/// [`restore_identity_mapping`] undoes. [`emit_qft_cascade`]'s `Swap`s
/// are *not* that kind of `Swap` -- they're an inseparable part of the
/// QFT construction's own algebra (interleaving a `Swap` between two
/// single-qubit gates that would otherwise land on different qubits
/// turns out, by the operator identity `SWAP · (I \otimes H) = (H
/// \otimes I) \cdot SWAP` compounded across the whole cascade, to
/// reproduce the *original* circuit's action directly on physical
/// wire `path[i]` for each logical `i` -- see
/// [`emit_qft_cascade`]'s own doc comment). Bookkeeping that tracked
/// those `Swap`s as ordinary relocations would conclude the final
/// physical/logical mapping is scrambled (in fact, exactly reversed)
/// when the circuit's actual action already matches the un-scrambled
/// target directly -- calling `restore_identity_mapping` against that
/// mistaken belief would insert a real, unwanted extra permutation and
/// silently corrupt the circuit's action. The correct post-cascade
/// mapping is the same closed-form fact used to realize the starting
/// layout in step 3 (`physical path[i]` holds logical `i`), not
/// something derived procedurally from the cascade's gate sequence.
///
/// # Panics (debug only)
/// If `coupling.num_qubits() != circuit.num_qubits` -- same
/// requirement every other router in this module has.
pub fn route_qft(circuit: &Circuit, coupling: &CouplingMap) -> Option<Circuit> {
    route_qft_boundary(circuit, coupling).map(|rc| rc.circuit)
}

/// [`route_qft`], but also returns the real/restoration boundary -- see
/// [`RoutedCircuit`]'s own doc comment. Note that unlike the general
/// routers, `route_qft`'s own trailing `Swap`s from
/// [`emit_qft_cascade`] are *not* at risk of being mistaken for
/// restoration here, since [`emit_qft_cascade`]'s own last emitted gate
/// is always a single-qubit `H` (its outer loop's final iteration has
/// an empty inner loop) -- but this function still reports the real
/// boundary explicitly rather than relying on that fact, for the same
/// reason every other router in this module does.
fn route_qft_boundary(circuit: &Circuit, coupling: &CouplingMap) -> Option<RoutedCircuit> {
    let n = circuit.num_qubits;
    let angles = detect_qft_cascade(circuit)?;

    if n == 0 {
        let mut out = Circuit::new(0);
        out.num_clbits = circuit.num_clbits;
        return Some(RoutedCircuit { circuit: out, restoration_start: 0 });
    }
    debug_assert_eq!(
        coupling.num_qubits(),
        n,
        "route_qft expects a coupling map sized to the circuit's own qubit count"
    );

    let identity_targets: Vec<PhysicalQubit> = (0..n).map(PhysicalQubit).collect();
    let path = find_hamiltonian_path(coupling, n, &identity_targets)?;

    // physical_to_logical[path[i]] = i: the layout the cascade itself
    // is built around (logical `i` starts, and -- see this function's
    // own doc comment -- ends, at physical `path[i]`). Used unchanged
    // for both the initial realization and the final restore below.
    let mut path_physical_to_logical = vec![LogicalQubit(0); n];
    for (i, &p) in path.iter().enumerate() {
        path_physical_to_logical[p.0] = LogicalQubit(i);
    }

    let mut out = Circuit::new(n);
    out.num_clbits = circuit.num_clbits;
    let mut logical_to_physical: Vec<PhysicalQubit> = (0..n).map(PhysicalQubit).collect();
    let mut physical_to_logical: Vec<LogicalQubit> = (0..n).map(LogicalQubit).collect();

    route_to_layout(
        &mut out,
        &mut logical_to_physical,
        &mut physical_to_logical,
        &path_physical_to_logical,
        coupling,
    );

    for gate in emit_qft_cascade(n, &angles, &path) {
        out.push(gate);
    }

    // `logical_to_physical`/`physical_to_logical` are untouched since
    // the call above, so they're still exactly `path`'s layout -- see
    // this function's doc comment for why that (not a tracked replay
    // of the cascade's own Swaps) is the correct mapping to restore
    // identity from.
    let restoration_start = out.gates.len();
    restore_identity_mapping(&mut out, &mut logical_to_physical, &mut physical_to_logical, coupling);

    Some(RoutedCircuit { circuit: out, restoration_start })
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
        // `If`'s own qubit(s) are wherever `inner`'s are (see
        // `Gate::qubits`), so remapping it means recursing into
        // `inner` with the same `remap_single`/`remap_two` split the
        // caller already used to get here -- `conditions` are
        // classical, untouched by routing, same as `Measure`'s `c`
        // above.
        If(ref conditions, ref inner) => {
            If(conditions.clone(), Box::new(remap_single(inner, new_q)))
        }
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
        If(ref conditions, ref inner) => {
            If(conditions.clone(), Box::new(remap_two(inner, new_first, new_second)))
        }
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
            Gate::If(..) => panic!(
                "apply_gate: If has no fidelity-based test yet, for the same reason as \
                 Measure above. None of this file's existing tests push an If gate, so this \
                 arm exists only to satisfy exhaustiveness."
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

    // -------------------------------------------------------------
    // P3: route_sabre / route_best
    // -------------------------------------------------------------

    /// Same methodology as [`assert_routing_preserves_action`]/
    /// [`assert_lookahead_routing_preserves_action`], but for
    /// [`route_sabre`] -- built from the same primitives, must hold
    /// exactly as strongly, and this is the one router new enough
    /// (iterative layout refinement, decay, jitter) that a subtle bug
    /// wouldn't necessarily show up as an obviously-wrong SWAP count.
    fn assert_sabre_routing_preserves_action(circuit: &Circuit, coupling: &CouplingMap) {
        let routed = route_sabre(circuit, coupling);
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
            "route_sabre doesn't match original: fidelity {} (routed: {:?})",
            fidelity,
            routed.gates
        );
    }

    /// Same methodology again, for [`route_best`] -- confirms picking
    /// by SWAP count alone across all three routers never accidentally
    /// picks a semantically-wrong result.
    fn assert_route_best_preserves_action(circuit: &Circuit, coupling: &CouplingMap) {
        let routed = route_best(circuit, coupling);
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
            "route_best doesn't match original: fidelity {} (routed: {:?})",
            fidelity,
            routed.gates
        );
    }

    /// A QFT-shaped circuit: genuinely all-to-all interaction (qubit
    /// `i` pairs with every qubit `j > i`), the same structural reason
    /// `qiskit_benchmark.rs`'s real `qft` benchmark is the case that
    /// exposed `route_lookahead`'s gap against Qiskit in the first
    /// place -- see `route_sabre`'s own doc comment.
    fn qft_like(num_qubits: usize) -> Circuit {
        let mut c = Circuit::new(num_qubits);
        for i in 0..num_qubits {
            c.push(Gate::H(i));
            for j in (i + 1)..num_qubits {
                let lambda = std::f64::consts::PI / (1u64 << (j - i)) as f64;
                c.push(Gate::Cp(j, i, lambda));
            }
        }
        for q in 0..num_qubits / 2 {
            c.push(Gate::Swap(q, num_qubits - 1 - q));
        }
        c
    }

    #[test]
    fn sabre_adjacent_gate_needs_no_swaps() {
        let mut c = Circuit::new(2);
        c.push(Gate::Cx(0, 1));
        let coupling = CouplingMap::linear(2);
        let routed = route_sabre(&c, &coupling);
        assert_eq!(swap_count(&routed), 0);
        assert_sabre_routing_preserves_action(&c, &coupling);
    }

    #[test]
    fn sabre_preserves_action_on_a_dense_random_circuit() {
        let mut c = Circuit::new(5);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 4))
            .push(Gate::Rz(2, 0.37))
            .push(Gate::Cp(1, 4, 1.1))
            .push(Gate::Ryy(0, 3, 0.6))
            .push(Gate::Swap(1, 3))
            .push(Gate::Cz(0, 2));
        let coupling = CouplingMap::linear(5);
        assert_sabre_routing_preserves_action(&c, &coupling);
    }

    #[test]
    fn sabre_preserves_action_on_a_qft_shaped_circuit_over_heavy_hex() {
        let c = qft_like(8);
        let coupling = CouplingMap::heavy_hex_for(8);
        assert_sabre_routing_preserves_action(&c, &coupling);
    }

    /// The headline regression this router exists for: on a genuinely
    /// non-local (QFT-shaped) circuit, `route_sabre` should use
    /// meaningfully fewer SWAPs than `route_lookahead`'s single greedy
    /// pass -- not just tie it. This is the concrete, locked-in proof
    /// that the iterative layout refinement + decay + deeper lookahead
    /// actually buys something, mirroring the real gap
    /// `qiskit_benchmark.rs`'s `qft_10`/`qft_16` benchmarks measured
    /// against Qiskit (route_lookahead alone: ~3x more SWAPs than
    /// Qiskit on those circuits).
    #[test]
    fn sabre_beats_lookahead_on_a_qft_shaped_circuit_over_heavy_hex() {
        let c = qft_like(10);
        let coupling = CouplingMap::heavy_hex_for(10);
        let lookahead_swaps = swap_count(&route_lookahead(&c, &coupling));
        let sabre_swaps = swap_count(&route_sabre(&c, &coupling));
        assert!(
            sabre_swaps < lookahead_swaps,
            "expected route_sabre to beat route_lookahead on a QFT-shaped circuit \
             (no chain fast path applies here -- see detect_interaction_chain_rejects_a_star, \
             a QFT's interaction graph is even less chain-like than a star); \
             lookahead used {} swaps, sabre used {}",
            lookahead_swaps,
            sabre_swaps
        );
    }

    /// Same headline regression, for a long-range random circuit
    /// (mirrors `qiskit_benchmark.rs`'s `long_range_random_20q_60gate`)
    /// -- confirms the improvement isn't specific to QFT's particular
    /// structure.
    #[test]
    fn sabre_beats_lookahead_on_a_long_range_random_circuit_over_heavy_hex() {
        let num_qubits = 16;
        let mut c = Circuit::new(num_qubits);
        for q in 0..num_qubits {
            c.push(Gate::H(q));
        }
        let mut state: u64 = 0x2545F4914F6CDD1D;
        let mut next_u64 = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..40 {
            let a = (next_u64() as usize) % num_qubits;
            let mut b = (next_u64() as usize) % num_qubits;
            while b == a {
                b = (next_u64() as usize) % num_qubits;
            }
            c.push(Gate::Cx(a, b));
        }
        let coupling = CouplingMap::heavy_hex_for(num_qubits);
        let lookahead_swaps = swap_count(&route_lookahead(&c, &coupling));
        let sabre_swaps = swap_count(&route_sabre(&c, &coupling));
        assert!(
            sabre_swaps < lookahead_swaps,
            "expected route_sabre to beat route_lookahead on a long-range random circuit; \
             lookahead used {} swaps, sabre used {}",
            lookahead_swaps,
            sabre_swaps
        );
    }

    #[test]
    fn route_best_never_worse_than_any_individual_router() {
        let cases: Vec<(Circuit, CouplingMap)> = vec![
            (qft_like(8), CouplingMap::heavy_hex_for(8)),
            (qft_like(10), CouplingMap::heavy_hex_for(10)),
            {
                let mut c = Circuit::new(10);
                c.push(Gate::H(0));
                for q in 0..9 {
                    c.push(Gate::Cx(q, q + 1));
                }
                (c, CouplingMap::heavy_hex_for(10))
            },
        ];
        for (c, coupling) in &cases {
            let naive = swap_count(&route(c, coupling));
            let lookahead = swap_count(&route_lookahead(c, coupling));
            let sabre = swap_count(&route_sabre(c, coupling));
            let best = swap_count(&route_best(c, coupling));
            assert!(
                best <= naive && best <= lookahead && best <= sabre,
                "route_best ({}) should never exceed any individual router \
                 (route: {}, route_lookahead: {}, route_sabre: {})",
                best, naive, lookahead, sabre
            );
            assert_route_best_preserves_action(c, coupling);
        }
    }

    #[test]
    fn route_best_still_achieves_zero_swaps_on_the_chain_case() {
        // Confirms route_best's extra candidates (route_sabre included)
        // never accidentally regress the case route_lookahead already
        // solves optimally.
        let mut c = Circuit::new(10);
        c.push(Gate::H(0));
        for q in 0..9 {
            c.push(Gate::Cx(q, q + 1));
        }
        let coupling = CouplingMap::heavy_hex_for(10);
        let routed = route_best(&c, &coupling);
        assert_eq!(swap_count(&routed), 0, "routed: {:?}", routed.gates);
    }

    #[test]
    fn route_best_no_restore_never_exceeds_route_best_swap_count() {
        let cases: Vec<(Circuit, CouplingMap)> = vec![
            (qft_like(8), CouplingMap::heavy_hex_for(8)),
            (qft_like(12), CouplingMap::heavy_hex_for(12)),
            {
                let mut c = Circuit::new(8);
                for i in 0..8 {
                    for j in (i + 1)..8 {
                        c.push(Gate::Cx(i, j));
                    }
                }
                (c, CouplingMap::heavy_hex_for(8))
            },
        ];
        for (c, coupling) in &cases {
            let with_restore = swap_count(&route_best(c, coupling));
            let no_restore = swap_count(&route_best_no_restore(c, coupling));
            assert!(
                no_restore <= with_restore,
                "route_best_no_restore ({}) should never need more swaps than \
                 route_best ({}), since it's free to reuse route_best's own \
                 candidates minus their trailing restoration block",
                no_restore,
                with_restore
            );
        }
    }

    #[test]
    fn route_best_no_restore_output_has_no_restoration_swaps_left() {
        // By construction (RoutedCircuit::strip_restoration truncates
        // right at the winning candidate's own recorded boundary),
        // route_best_no_restore's own output should never itself
        // contain a trailing restoration block -- confirmed here via
        // restoration_swap_count directly, rather than just trusting
        // the doc comment. This circuit has no real trailing `Swap`s
        // of its own (all-to-all `Cx`, not a QFT), so
        // restoration_swap_count's shape-based heuristic is safe to
        // use for this check -- see its own doc comment for the cases
        // where it isn't.
        let mut c = Circuit::new(8);
        for i in 0..8 {
            for j in (i + 1)..8 {
                c.push(Gate::Cx(i, j));
            }
        }
        let coupling = CouplingMap::heavy_hex_for(8);
        let routed = route_best_no_restore(&c, &coupling);
        let (_, restoration) = restoration_swap_count(&routed);
        assert_eq!(
            restoration, 0,
            "route_best_no_restore's own output must never contain a trailing \
             restoration block, routed: {:?}",
            routed.gates
        );
    }

    /// Test-only mirror of `route_best_no_restore`'s own candidate
    /// selection, except it hands back the winning [`RoutedCircuit`]
    /// itself -- real routed content *and* its real restoration tail,
    /// as computed by whichever router actually won -- instead of just
    /// the already-stripped `Circuit` the public function returns. See
    /// `route_best_no_restore_action_matches_original_once_restored_back`
    /// for why the test needs this instead of reconstructing a mapping
    /// on its own.
    fn route_best_no_restore_boundary(circuit: &Circuit, coupling: &CouplingMap) -> RoutedCircuit {
        let mut candidates = vec![
            route_boundary(circuit, coupling),
            route_lookahead_boundary(circuit, coupling),
            route_sabre_impl(circuit, coupling, SABRE_TRIALS_PER_SEED),
            route_sabre2_impl(circuit, coupling, SABRE_TRIALS_PER_SEED),
        ];
        if let Some(qft_routed) = route_qft_boundary(circuit, coupling) {
            candidates.push(qft_routed);
        }
        candidates
            .into_iter()
            .min_by_key(|c| c.routing_swap_count())
            .expect("candidates always has at least the four unconditional routers pushed above")
    }

    #[test]
    fn route_best_no_restore_action_matches_original_once_restored_back() {
        // route_best_no_restore's whole point is to skip the trailing
        // identity-restore swaps, so its output's final physical
        // layout is *not* identity -- a direct fidelity comparison
        // against `circuit` (the pattern every other preserves_action
        // test in this file uses, which assumes identity at the end)
        // isn't the right check here.
        //
        // This test used to replay `no_restore`'s own Swaps from
        // identity via generic `swap_mapping` bookkeeping to guess
        // whatever layout it left qubits on, then restore identity from
        // *that* guessed layout. That's exactly the heuristic
        // `route_qft_boundary`'s own doc comment (see its "Why step 5
        // doesn't track the cascade's own Swaps" section) warns is
        // invalid for a `route_qft` winner: `emit_qft_cascade`'s
        // embedded Swaps are not ordinary data relocations, and naively
        // replaying them concludes an "exactly reversed" mapping
        // relative to the circuit's real action -- restoring identity
        // from that wrong mapping then silently corrupts the result.
        // That's exactly what this test used to catch, as a false
        // positive: ~2% fidelity on `qft_like(8)`, whose `route_best`
        // winner is `route_qft`.
        //
        // So instead of reconstructing a mapping at all, mirror
        // `route_best_no_restore`'s own selection
        // (`route_best_no_restore_boundary` above) to get the winning
        // `RoutedCircuit` directly. Its restoration tail was already
        // appended by the router that produced it, using that router's
        // own correct, non-heuristic bookkeeping -- for `route_qft`
        // that's the closed-form `path_physical_to_logical` mapping,
        // not a replay of its cascade Swaps. Re-appending that exact
        // tail to the stripped circuit is therefore guaranteed
        // action-preserving by construction, for every candidate.
        let cases: Vec<(Circuit, CouplingMap)> = vec![
            (qft_like(8), CouplingMap::heavy_hex_for(8)),
            {
                let mut c = Circuit::new(6);
                for q in 0..5 {
                    c.push(Gate::Cx(q, (q + 3) % 6));
                }
                (c, CouplingMap::heavy_hex_for(6))
            },
        ];
        for (c, coupling) in &cases {
            let winner = route_best_no_restore_boundary(c, coupling);
            let no_restore = winner.strip_restoration();
            assert_eq!(
                no_restore.gates,
                route_best_no_restore(c, coupling).gates,
                "test's mirrored selection must agree with the real route_best_no_restore"
            );

            let mut restored = no_restore.clone();
            for g in &winner.circuit.gates[winner.restoration_start..] {
                restored.push(g.clone());
            }

            let mut direct = randomized_register(c.num_qubits);
            let mut restored_reg = direct.clone();
            for g in &c.gates {
                apply_gate(&mut direct, g);
            }
            for g in &restored.gates {
                apply_gate(&mut restored_reg, g);
            }
            let fidelity = direct.fidelity(&restored_reg).unwrap();
            assert!(
                (fidelity - 1.0).abs() < TOL,
                "route_best_no_restore's action (once identity-restored back) doesn't \
                 match original: fidelity {} (no_restore: {:?}, restored: {:?})",
                fidelity,
                no_restore.gates,
                restored.gates
            );
        }
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

    // -------------------------------------------------------------
    // P4: QFT LNN-cascade fast path.
    // -------------------------------------------------------------

    /// Same methodology as [`assert_route_best_preserves_action`], but
    /// calling [`route_qft`] directly rather than through
    /// [`route_best`] -- exercises the fast path itself even on a
    /// coupling map or circuit shape where [`route_best`] might not
    /// end up calling it.
    fn assert_qft_routing_preserves_action(circuit: &Circuit, coupling: &CouplingMap) {
        let routed = route_qft(circuit, coupling)
            .expect("route_qft should recognize this circuit's exact QFT-cascade shape");
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
            "route_qft doesn't match original: fidelity {} (routed: {:?})",
            fidelity,
            routed.gates
        );
    }

    #[test]
    fn detect_qft_cascade_recognizes_qft_like_with_and_without_trailing_swaps() {
        for n in [3usize, 4, 5, 8, 10, 16] {
            let with_swaps = qft_like(n);
            assert!(
                detect_qft_cascade(&with_swaps).is_some(),
                "n={}: cascade + standard trailing reversal swaps should be recognized",
                n
            );

            let mut without_swaps = Circuit::new(n);
            for i in 0..n {
                without_swaps.push(Gate::H(i));
                for j in (i + 1)..n {
                    let lambda = std::f64::consts::PI / (1u64 << (j - i)) as f64;
                    without_swaps.push(Gate::Cp(j, i, lambda));
                }
            }
            assert!(
                detect_qft_cascade(&without_swaps).is_some(),
                "n={}: bare cascade with no trailing swaps should also be recognized",
                n
            );
        }
    }

    #[test]
    fn detect_qft_cascade_rejects_non_qft_shapes() {
        // Wrong gate order (Cp before its H).
        let mut c = Circuit::new(3);
        c.push(Gate::Cp(1, 0, 0.5)).push(Gate::H(0));
        assert!(detect_qft_cascade(&c).is_none());

        // An otherwise-plausible two-qubit circuit that just isn't a
        // QFT cascade at all.
        let mut c2 = Circuit::new(4);
        c2.push(Gate::H(0)).push(Gate::Cx(0, 1)).push(Gate::Cx(1, 2)).push(Gate::Cx(2, 3));
        assert!(detect_qft_cascade(&c2).is_none());

        // Right gates, wrong trailing block (not the standard reversal).
        let mut c3 = qft_like(4);
        c3.gates.pop();
        c3.push(Gate::Swap(0, 1));
        assert!(detect_qft_cascade(&c3).is_none());
    }

    #[test]
    fn route_qft_preserves_action_on_linear_coupling() {
        for n in [3usize, 4, 5, 8, 10, 16] {
            let c = qft_like(n);
            let coupling = CouplingMap::linear(n);
            assert_qft_routing_preserves_action(&c, &coupling);
        }
    }

    /// The headline efficiency claim: on a linear coupling map (the
    /// identity path is already Hamiltonian), `route_qft` should use
    /// exactly `n*(n-1)/2` SWAPs -- one per `Cp` -- and that should
    /// meaningfully beat `route_best`'s general-purpose search on the
    /// same circuit (see this module's `route_sabre` doc comment on
    /// why QFT specifically exposes the general routers' weakest
    /// case).
    #[test]
    fn route_qft_uses_exactly_one_swap_per_cp_and_beats_general_routers() {
        for n in [8usize, 10, 16] {
            let c = qft_like(n);
            let coupling = CouplingMap::linear(n);
            let cp_count = c.gates.iter().filter(|g| matches!(g, Gate::Cp(..))).count();

            let qft_routed = route_qft(&c, &coupling).expect("qft_like must match detect_qft_cascade");
            assert_eq!(
                swap_count(&qft_routed),
                cp_count,
                "n={}: expected exactly one swap per Cp ({}), got {}: {:?}",
                n,
                cp_count,
                swap_count(&qft_routed),
                qft_routed.gates
            );

            let sabre = route_sabre(&c, &coupling);
            assert!(
                swap_count(&qft_routed) <= swap_count(&sabre),
                "n={}: route_qft used {} swaps, route_sabre used {} -- expected the dedicated \
                 cascade to be no worse",
                n,
                swap_count(&qft_routed),
                swap_count(&sabre)
            );
        }
    }

    /// On a linear coupling map the identity path is itself Hamiltonian
    /// (`find_hamiltonian_path` finds it directly, biased toward
    /// identity -- see that function's own doc comment), so
    /// `route_qft` shouldn't need [`route_to_layout`]'s realization
    /// step at all: the very first gate in the routed circuit should
    /// already be `H(0)`, exactly as in the source circuit, not a
    /// `Swap`.
    #[test]
    fn route_qft_needs_no_initial_layout_realization_on_linear_coupling() {
        let c = qft_like(6);
        let coupling = CouplingMap::linear(6);
        let routed = route_qft(&c, &coupling).unwrap();
        assert_eq!(
            routed.gates.first(),
            Some(&Gate::H(0)),
            "expected no realization swaps before the first H on a linear coupling map: {:?}",
            routed.gates
        );
    }

    #[test]
    fn route_qft_preserves_action_on_heavy_hex_coupling() {
        // A non-trivial coupling map where the identity permutation is
        // not itself guaranteed to be a Hamiltonian path in physical-
        // label order, exercising route_to_layout's realization step
        // (see route_qft's own doc comment, step 3) as well as the
        // cascade itself.
        for n in [5usize, 8, 10] {
            let c = qft_like(n);
            let coupling = CouplingMap::heavy_hex_for(n);
            assert_qft_routing_preserves_action(&c, &coupling);
        }
    }

    #[test]
    fn route_qft_returns_none_for_non_qft_circuits() {
        let mut c = Circuit::new(4);
        c.push(Gate::H(0)).push(Gate::Cx(0, 1)).push(Gate::Cx(1, 2)).push(Gate::Cx(2, 3));
        let coupling = CouplingMap::linear(4);
        assert!(route_qft(&c, &coupling).is_none());
    }

    #[test]
    fn route_best_uses_qft_fast_path_and_preserves_action() {
        for n in [5usize, 8, 10] {
            let c = qft_like(n);
            let coupling = CouplingMap::linear(n);
            let best = route_best(&c, &coupling);
            let cp_count = c.gates.iter().filter(|g| matches!(g, Gate::Cp(..))).count();
            assert_eq!(
                swap_count(&best),
                cp_count,
                "n={}: route_best should delegate to route_qft's fixed n*(n-1)/2 swap count",
                n
            );
            assert_route_best_preserves_action(&c, &coupling);
        }
    }

    // -----------------------------------------------------------------
    // Commutation-aware SABRE front-layer scheduling
    // (`build_commutation_predecessors`).
    // -----------------------------------------------------------------

    #[test]
    fn commuting_control_side_gate_has_no_dependency_edge_to_the_cx() {
        // Rz(0, t) . Cx(0, 1): Rz sits on Cx's *control* wire, where
        // ir_optimize::commutes's rule 1 says a diagonal gate commutes
        // through Cx unconditionally. The naive per-qubit-order
        // dependency would still force Cx to wait on Rz (same wire,
        // earlier in program order) -- the commutation-aware version
        // must not.
        let gates = vec![Gate::Rz(0, 0.4), Gate::Cx(0, 1)];
        let gate_qubits: Vec<Vec<LogicalQubit>> =
            gates.iter().map(|g| g.qubits().into_iter().map(LogicalQubit).collect()).collect();
        let predecessors = build_commutation_predecessors(&gates, &gate_qubits);
        assert_eq!(
            predecessors[1],
            Vec::<usize>::new(),
            "Cx should have no true predecessor on its control wire when the earlier gate is a \
             commuting diagonal gate, got {:?}",
            predecessors[1]
        );
    }

    #[test]
    fn non_commuting_control_side_gate_still_has_a_dependency_edge_to_the_cx() {
        // X(0) . Cx(0, 1): X is single-qubit but NOT diagonal, and
        // sits on the CONTROL wire where only diagonal gates commute
        // (rule 1) -- X-basis gates only commute through Cx on the
        // TARGET wire (rule 3). This must still be a real dependency,
        // the mirror case of the test above confirming the relaxation
        // doesn't overreach into pairs that were never proven to
        // commute.
        let gates = vec![Gate::X(0), Gate::Cx(0, 1)];
        let gate_qubits: Vec<Vec<LogicalQubit>> =
            gates.iter().map(|g| g.qubits().into_iter().map(LogicalQubit).collect()).collect();
        let predecessors = build_commutation_predecessors(&gates, &gate_qubits);
        assert_eq!(
            predecessors[1],
            vec![0],
            "Cx must still wait on a non-commuting earlier gate on its control wire, got {:?}",
            predecessors[1]
        );
    }

    #[test]
    fn disjoint_gates_have_no_dependency_edge_regardless_of_program_order() {
        let gates = vec![Gate::H(0), Gate::X(1)];
        let gate_qubits: Vec<Vec<LogicalQubit>> =
            gates.iter().map(|g| g.qubits().into_iter().map(LogicalQubit).collect()).collect();
        let predecessors = build_commutation_predecessors(&gates, &gate_qubits);
        assert!(predecessors[1].is_empty());
    }

    #[test]
    fn dependency_edges_are_not_missed_across_an_intervening_commuting_gate() {
        // Rz(0, t) . Cx(0, 1) . X(0), all on wire 0 (Cx also touches
        // wire 1): the middle Cx commutes with the leading Rz (control
        // wire, rule 1) -- no edge 0->1 -- but Rz and X do NOT commute
        // with each other directly (both single-qubit, same wire,
        // neither is one of the two/diagonal-vs-Cx/Cz shapes any of
        // the three rules cover), so there's a real edge 0->2 on top
        // of the real edge 1->2 (X isn't X-basis-through-Cx-target
        // here, it's sitting on Cx's control wire, so rule 3 doesn't
        // apply either -- edge 1->2 is real too).
        //
        // This is exactly the trap this function's own doc comment
        // warns about: gate 0 and gate 1 commute, so a "just check the
        // nearest predecessor on each wire" implementation would find
        // gate 1 as X's nearest blocker, assume transitivity through
        // it covers gate 0 too, and silently drop the real 0->2
        // constraint -- since there's no 0->1 edge to carry it. The
        // full pairwise check must not drop it.
        let gates = vec![Gate::Rz(0, 0.4), Gate::Cx(0, 1), Gate::X(0)];
        let gate_qubits: Vec<Vec<LogicalQubit>> =
            gates.iter().map(|g| g.qubits().into_iter().map(LogicalQubit).collect()).collect();
        let predecessors = build_commutation_predecessors(&gates, &gate_qubits);
        assert_eq!(predecessors[1], Vec::<usize>::new(), "Cx should not depend on the commuting Rz");
        assert_eq!(
            predecessors[2],
            vec![0, 1],
            "X must depend on BOTH the non-commuting Rz (direct, since 0 and 1 commute so there's \
             no transitive path from 0 to carry it) and the non-commuting Cx, got {:?}",
            predecessors[2]
        );
    }

    #[test]
    fn route_sabre_preserves_action_on_a_circuit_with_commuting_front_layer_gates() {
        // A circuit shaped so the commutation-aware relaxation actually
        // fires during routing (several Rz's on Cx control wires,
        // interleaved with genuinely blocked long-range two-qubit
        // gates on a linear coupling map) -- the real correctness bar
        // this change has to clear is that reordering provably-
        // commuting gates during scheduling never changes the circuit
        // this crate actually executes, checked the same way every
        // other routing identity in this module is (fidelity against a
        // reference simulator run from a random initial state), not
        // just by trusting the commutation algebra on its own.
        let mut c = Circuit::new(6);
        c.push(Gate::Rz(0, 0.3))
            .push(Gate::Cx(0, 1))
            .push(Gate::Rz(2, 0.7))
            .push(Gate::Cx(2, 3))
            .push(Gate::Cx(0, 5))
            .push(Gate::Rz(3, 1.1))
            .push(Gate::Cx(3, 4))
            .push(Gate::Cx(1, 4));
        let coupling = CouplingMap::linear(6);
        assert_route_best_preserves_action(&c, &coupling);

        let routed = route_sabre(&c, &coupling);
        let mut direct = randomized_register(c.num_qubits);
        let mut routed_reg = direct.clone();
        for g in &c.gates {
            apply_gate(&mut direct, g);
        }
        for g in &routed.gates {
            apply_gate(&mut routed_reg, g);
        }
        let fidelity = direct.fidelity(&routed_reg).unwrap();
        assert!(
            (fidelity - 1.0).abs() < TOL,
            "route_sabre with commutation-aware scheduling doesn't match original: fidelity {}",
            fidelity
        );
    }
}
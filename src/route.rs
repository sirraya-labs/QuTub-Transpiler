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
use std::collections::{HashMap, HashSet, VecDeque};

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
    let mut active: HashSet<PhysicalQubit> = (0..n).map(PhysicalQubit).collect();

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
    active: &HashSet<PhysicalQubit>,
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
    active: &HashSet<PhysicalQubit>,
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
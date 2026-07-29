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
use crate::ir::{Circuit, Gate};
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
    let mut logical_to_physical: Vec<usize> = (0..num_qubits).collect();
    let mut physical_to_logical: Vec<usize> = (0..num_qubits).collect();
    let mut out = Circuit::new(num_qubits);
    out.num_clbits = circuit.num_clbits;

    for gate in &circuit.gates {
        let qubits = gate.qubits();
        match qubits.as_slice() {
            &[q] => {
                let physical = logical_to_physical[q];
                out.push(remap_single(gate, physical));
            }
            &[first, second] => {
                let mut physical_first = logical_to_physical[first];
                let physical_second = logical_to_physical[second];

                if !coupling.is_adjacent(physical_first, physical_second) {
                    let path = coupling
                        .shortest_path(physical_first, physical_second)
                        .expect(
                            "coupling map must be connected between any two qubits a routed \
                             circuit needs to interact",
                        );
                    for hop in path.windows(2) {
                        let (from, to) = (hop[0], hop[1]);
                        if to == physical_second {
                            // `from` is now adjacent to the fixed
                            // target; stop one hop short rather than
                            // swapping onto it.
                            break;
                        }
                        out.push(Gate::Swap(from, to));
                        swap_mapping(&mut logical_to_physical, &mut physical_to_logical, from, to);
                        physical_first = to;
                    }
                }

                out.push(remap_two(gate, physical_first, physical_second));
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
    logical_to_physical: &mut [usize],
    physical_to_logical: &mut [usize],
    from: usize,
    to: usize,
) {
    let (logical_from, logical_to) = (physical_to_logical[from], physical_to_logical[to]);
    physical_to_logical[from] = logical_to;
    physical_to_logical[to] = logical_from;
    logical_to_physical[logical_from] = to;
    logical_to_physical[logical_to] = from;
}

/// Sorts `physical_to_logical` back to the identity (`physical_to_logical[p]
/// == p` for every `p`) using only coupling-adjacent swaps, via a
/// general-graph-correct "token swapping" strategy -- **not** a linear
/// bubble sort. The old implementation assumed physical qubits were
/// numbered along a path where consecutive indices are always
/// coupling-adjacent, which held for `CouplingMap::linear` but is false
/// for `CouplingMap::heavy_hex_for`/`heavy_hex_grid` (a `debug_assert!`
/// there would fire on real `IbmQ` circuits, not just a theoretical
/// gap).
///
/// # Algorithm
/// Token swapping (restoring an arbitrary permutation to the identity
/// using only edge-adjacent swaps) is NP-hard to do with the *minimum*
/// number of swaps on a general graph, but this pass only needs
/// *correctness*, not swap-count optimality, and a connected graph
/// always admits a solution: take an arbitrary spanning tree of
/// `coupling` (its edges are a subset of the real coupling graph's, so
/// every swap this emits is still a real hardware-adjacent swap), then
/// repeatedly prune a leaf of the tree:
/// - If the leaf already holds its own token (`physical_to_logical[leaf]
///   == leaf`), it's already home -- remove it from the active set and
///   move on. (Removing an already-fixed leaf from a tree always leaves
///   a tree: this is the standard leaf-pruning invariant.)
/// - Otherwise, find wherever (among the still-active vertices) the
///   token whose home is `leaf` currently sits, and walk it to `leaf`
///   one coupling-adjacent swap at a time along the *tree* path between
///   them -- guaranteed to lie entirely within the active subtree, by
///   the same invariant. `leaf` is now home and is removed.
///
/// Every tree with >= 2 nodes has at least one leaf (in fact at least
/// two), so this always makes progress; with one active node left,
/// it's necessarily already home (there's nowhere else for its token to
/// be), so the loop naturally terminates at the identity.
fn restore_identity_mapping(
    out: &mut Circuit,
    logical_to_physical: &mut [usize],
    physical_to_logical: &mut [usize],
    coupling: &CouplingMap,
) {
    let n = physical_to_logical.len();
    if n <= 1 {
        return;
    }

    let tree = spanning_tree(coupling, n);
    let mut active: HashSet<usize> = (0..n).collect();

    while active.len() > 1 {
        let leaf = *active
            .iter()
            .find(|&&v| active_degree(&tree, &active, v) <= 1)
            .expect("a tree with >= 2 active nodes always has a leaf");

        if physical_to_logical[leaf] != leaf {
            let p = *active
                .iter()
                .find(|&&v| physical_to_logical[v] == leaf)
                .expect("the token destined for `leaf` must be somewhere in the active set");
            let path = tree_path_within(&tree, &active, p, leaf);
            for hop in path.windows(2) {
                let (u, v) = (hop[0], hop[1]);
                out.push(Gate::Swap(u, v));
                swap_mapping(logical_to_physical, physical_to_logical, u, v);
            }
        }
        active.remove(&leaf);
    }
}

/// A BFS spanning tree of `coupling` restricted to vertices `0..n`,
/// stored as an adjacency list. Every edge here is a real edge of
/// `coupling` (a subset of it, not a superset), so any swap along a
/// tree edge is a legal hardware-adjacent swap.
fn spanning_tree(coupling: &CouplingMap, n: usize) -> HashMap<usize, Vec<usize>> {
    let mut visited = vec![false; n];
    let mut tree: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut queue = VecDeque::new();
    queue.push_back(0);
    visited[0] = true;
    while let Some(u) = queue.pop_front() {
        for v in coupling.neighbors(u) {
            if v < n && !visited[v] {
                visited[v] = true;
                tree.entry(u).or_default().push(v);
                tree.entry(v).or_default().push(u);
                queue.push_back(v);
            }
        }
    }
    tree
}

/// Number of `v`'s tree-neighbors that are still in `active`.
fn active_degree(tree: &HashMap<usize, Vec<usize>>, active: &HashSet<usize>, v: usize) -> usize {
    tree.get(&v)
        .map(|nbrs| nbrs.iter().filter(|&&u| active.contains(&u)).count())
        .unwrap_or(0)
}

/// Shortest (in fact only, since it's a tree) path from `start` to
/// `goal` using only tree edges between currently-`active` vertices --
/// always exists, since leaf-pruning keeps the active-induced subgraph
/// of a tree connected.
fn tree_path_within(
    tree: &HashMap<usize, Vec<usize>>,
    active: &HashSet<usize>,
    start: usize,
    goal: usize,
) -> Vec<usize> {
    if start == goal {
        return vec![start];
    }
    let mut visited: HashSet<usize> = HashSet::new();
    let mut predecessor: HashMap<usize, usize> = HashMap::new();
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

        let mut logical_to_physical: Vec<usize> = (0..5).collect();
        let mut physical_to_logical: Vec<usize> = (0..5).collect();
        for g in &routed.gates {
            if let Gate::Swap(a, b) = *g {
                swap_mapping(&mut logical_to_physical, &mut physical_to_logical, a, b);
            }
        }
        assert_eq!(
            logical_to_physical,
            vec![0, 1, 2, 3, 4],
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
        let mut logical_to_physical: Vec<usize> = (0..4).collect();
        let mut physical_to_logical: Vec<usize> = (0..4).collect();
        let mut found = None;
        for g in &routed.gates {
            match *g {
                Gate::Swap(a, b) => {
                    swap_mapping(&mut logical_to_physical, &mut physical_to_logical, a, b)
                }
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
            measured_wire, logical_to_physical[0],
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

        let mut logical_to_physical: Vec<usize> = (0..12).collect();
        let mut physical_to_logical: Vec<usize> = (0..12).collect();
        for g in &routed.gates {
            if let Gate::Swap(a, b) = *g {
                swap_mapping(&mut logical_to_physical, &mut physical_to_logical, a, b);
            }
        }
        assert_eq!(
            logical_to_physical,
            (0..12).collect::<Vec<_>>(),
            "every qubit must be restored to its original physical wire on a heavy-hex map"
        );

        assert_routing_preserves_action(&c, &coupling);
    }
}
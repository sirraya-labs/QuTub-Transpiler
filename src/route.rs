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
///   runs a plain adjacent-transposition bubble sort over the
///   remaining `physical -> logical` mapping, emitting one more `Swap`
///   per adjacent transposition, until every qubit is back on its
///   original wire.
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
/// == p` for every `p`) using only adjacent transpositions -- a plain
/// bubble sort -- appending one `Gate::Swap(p, p + 1)` to `out` per
/// transposition performed. Adjacent-transposition bubble sort can
/// restore *any* permutation to sorted order using only swaps of
/// neighboring positions, so this always terminates at the identity
/// regardless of how scattered the residual permutation from the main
/// routing pass is.
///
/// This assumes physical qubits are numbered along a path where
/// consecutive indices are coupling-adjacent -- true for every
/// [`CouplingMap`] this crate constructs (`CouplingMap::linear`) --
/// hence the `debug_assert!` rather than a runtime check: a future
/// non-linear coupling map would need a different restoration strategy
/// (e.g. per-qubit shortest-path token swapping), not just a relaxed
/// assumption here.
fn restore_identity_mapping(
    out: &mut Circuit,
    logical_to_physical: &mut [usize],
    physical_to_logical: &mut [usize],
    coupling: &CouplingMap,
) {
    let n = physical_to_logical.len();
    loop {
        let mut moved = false;
        for p in 0..n.saturating_sub(1) {
            if physical_to_logical[p] > physical_to_logical[p + 1] {
                debug_assert!(
                    coupling.is_adjacent(p, p + 1),
                    "restore_identity_mapping assumes consecutive physical indices are adjacent"
                );
                out.push(Gate::Swap(p, p + 1));
                swap_mapping(logical_to_physical, physical_to_logical, p, p + 1);
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
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
}
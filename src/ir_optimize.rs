//! Source-level (pre-decomposition) optimization passes over
//! [`Circuit`], distinct from [`crate::optimize`]'s peephole pass over
//! already-decomposed native gates. Two kinds of gate pair cancel here:
//! literal self-inverses (`H;H`, `Cx(a,b);Cx(a,b)`, `Swap(a,b);Swap(a,b)`,
//! ...) and explicit inverse pairs (`S;Sdg`, `T;Tdg`). Both only fire on
//! *adjacent* occurrences with identical qubit arguments, so the useful
//! part of this module is the commuting-reorder pass that slides gates
//! past each other to make non-adjacent cancellable pairs adjacent.
//!
//! The only commutation rule used is the universally true one: **two
//! gates with disjoint qubit sets commute**, unconditionally, because
//! they act on different tensor factors. No gate-type-specific
//! commutation table (e.g. "Rz commutes through a CNOT control") is
//! implemented -- that class of rule is real and used by production
//! transpilers, but each such rule needs its own derivation and test the
//! same way the two-qubit identities in `native.rs` did, and getting one
//! wrong silently produces a wrong circuit rather than a missed
//! optimization. Disjoint-support commutativity needs no such proof.
//!
//! The `tests` submodule below checks every pass end to end against the
//! real simulator (same methodology as `tests/decompositions.rs`): run
//! the original circuit and the optimized one from the same random
//! state, compare via `QuantumRegister::fidelity`.

use crate::ir::{Circuit, Gate};
use std::collections::HashSet;

/// Runs the cancel/reorder passes to a fixed point.
pub fn optimize(circuit: &Circuit) -> Circuit {
    let mut gates = circuit.gates.clone();
    loop {
        let after_cancel = cancel_pass(&gates);
        let after_reorder = commute_forward_pass(&after_cancel);
        let after_cancel2 = cancel_pass(&after_reorder);
        if after_cancel2.len() == gates.len() {
            gates = after_cancel2;
            break;
        }
        gates = after_cancel2;
    }
    Circuit {
        num_qubits: circuit.num_qubits,
        num_clbits: circuit.num_clbits,
        gates,
    }
}

fn qubits_of(g: &Gate) -> Vec<usize> {
    g.qubits()
}

fn disjoint(a: &Gate, b: &Gate) -> bool {
    // `Measure(q, c)` is only disjoint-by-qubit-set from a gate on a
    // different qubit -- but two Measures writing different qubits
    // into the *same* classical bit `c` would be silently reordered by
    // that check alone, changing which one "wins" the bit. Treat any
    // gate touching a Measure as never disjoint, so it's never a
    // candidate for the commuting-reorder pass to slide past anything,
    // in either direction. Conservative on purpose: this crate does
    // not yet track classical-bit dependencies precisely enough to
    // reorder Measures safely (see `ir::Gate::Measure`'s doc comment).
    if matches!(a, Gate::Measure(..)) || matches!(b, Gate::Measure(..)) {
        return false;
    }
    let qa: HashSet<usize> = qubits_of(a).into_iter().collect();
    let qb: HashSet<usize> = qubits_of(b).into_iter().collect();
    qa.is_disjoint(&qb)
}

/// `true` if `a` immediately followed by `b` is the identity (same
/// qubits, mutually inverse operation).
fn is_inverse_pair(a: &Gate, b: &Gate) -> bool {
    use Gate::*;
    match (a, b) {
        // Self-inverse gates: two identical applications cancel.
        (H(q1), H(q2)) => q1 == q2,
        (X(q1), X(q2)) => q1 == q2,
        (Y(q1), Y(q2)) => q1 == q2,
        (Z(q1), Z(q2)) => q1 == q2,
        (Cx(a1, b1), Cx(a2, b2)) => a1 == a2 && b1 == b2,
        (Cz(a1, b1), Cz(a2, b2)) => (a1 == a2 && b1 == b2) || (a1 == b2 && b1 == a2),
        (Swap(a1, b1), Swap(a2, b2)) => (a1 == a2 && b1 == b2) || (a1 == b2 && b1 == a2),
        // Explicit inverse pairs.
        (S(q1), Sdg(q2)) | (Sdg(q1), S(q2)) => q1 == q2,
        (T(q1), Tdg(q2)) | (Tdg(q1), T(q2)) => q1 == q2,
        // Zero-angle rotations and angle-negating pairs on the same
        // qubit(s) also cancel; angle equality (not just presence)
        // matters here so this stays exact, not approximate.
        (Rx(q1, a1), Rx(q2, a2)) => q1 == q2 && (a1 + a2).abs() < 1e-12,
        (Ry(q1, a1), Ry(q2, a2)) => q1 == q2 && (a1 + a2).abs() < 1e-12,
        (Rz(q1, a1), Rz(q2, a2)) => q1 == q2 && (a1 + a2).abs() < 1e-12,
        (Rxx(a1, b1, t1), Rxx(a2, b2, t2))
        | (Ryy(a1, b1, t1), Ryy(a2, b2, t2))
        | (Rzz(a1, b1, t1), Rzz(a2, b2, t2)) => {
            ((a1 == a2 && b1 == b2) || (a1 == b2 && b1 == a2)) && (t1 + t2).abs() < 1e-12
        }
        (Cp(a1, b1, l1), Cp(a2, b2, l2)) => a1 == a2 && b1 == b2 && (l1 + l2).abs() < 1e-12,
        _ => false,
    }
}

/// Removes every adjacent inverse pair, in one linear pass (a stack:
/// push a gate unless it cancels the top of the stack).
fn cancel_pass(gates: &[Gate]) -> Vec<Gate> {
    let mut stack: Vec<Gate> = Vec::with_capacity(gates.len());
    for g in gates {
        let cancels = stack
            .last()
            .map(|top| is_inverse_pair(top, g))
            .unwrap_or(false);
        if cancels {
            stack.pop();
        } else {
            stack.push(g.clone());
        }
    }
    stack
}

/// For each gate, tries to slide it backward (earlier in the circuit)
/// past a run of gates it's disjoint from, stopping as soon as it would
/// land next to a gate it can cancel with, or hits a non-disjoint gate
/// it can't pass. This is a single forward scan that greedily pulls
/// cancellable pairs together; running it to a fixed point with
/// `cancel_pass` (see `optimize`) handles chains of reorders.
fn commute_forward_pass(gates: &[Gate]) -> Vec<Gate> {
    let mut out: Vec<Gate> = Vec::with_capacity(gates.len());
    for g in gates {
        // Find the furthest-back position we can slide g to: walk
        // backward from the end of `out` while each gate we pass is
        // disjoint from g. Stop early if we find a cancel partner.
        let mut insert_at = out.len();
        let mut cancel_at: Option<usize> = None;
        while insert_at > 0 {
            let candidate = &out[insert_at - 1];
            if is_inverse_pair(candidate, g) {
                cancel_at = Some(insert_at - 1);
                break;
            }
            if disjoint(candidate, g) {
                insert_at -= 1;
                continue;
            }
            break;
        }
        match cancel_at {
            Some(pos) => {
                out.remove(pos);
            }
            None => out.insert(insert_at, g.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    // Validates `ir_optimize` two ways: exact-count checks for specific
    // cancellation patterns, and (for anything less trivial) a real
    // before/after fidelity comparison against
    // `sirraya_qutub::core::QuantumRegister`, same methodology as
    // `tests/decompositions.rs`.

    use rand::Rng;
    use sirraya_qutub::core::QuantumRegister;
    use crate::ir::{Circuit, Gate};
    use crate::ir_optimize::optimize as optimize_ir;

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
                 shot-based statistical methodology called for in the P0.1 roadmap item. \
                 No test in this file exercises Measure; this arm exists only to satisfy \
                 exhaustiveness."
            ),
        }
    }

    fn assert_same_action(circuit: &Circuit, optimized: &Circuit) {
        let mut direct = randomized_register(circuit.num_qubits);
        let mut opt_reg = direct.clone();
        for g in &circuit.gates {
            apply_gate(&mut direct, g);
        }
        for g in &optimized.gates {
            apply_gate(&mut opt_reg, g);
        }
        let fidelity = direct.fidelity(&opt_reg).unwrap();
        assert!(
            (fidelity - 1.0).abs() < TOL,
            "optimized circuit doesn't match: fidelity {} (optimized: {:?})",
            fidelity,
            optimized.gates
        );
    }

    #[test]
    fn cancels_adjacent_cnot_pair() {
        let mut c = Circuit::new(2);
        c.push(Gate::H(0)).push(Gate::Cx(0, 1)).push(Gate::Cx(0, 1)).push(Gate::X(1));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::H(0), Gate::X(1)]);
    }

    #[test]
    fn cancels_adjacent_swap_pair() {
        let mut c = Circuit::new(2);
        c.push(Gate::H(0)).push(Gate::Swap(0, 1)).push(Gate::Swap(1, 0));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::H(0)]);
    }

    #[test]
    fn cancels_s_sdg_pair() {
        let mut c = Circuit::new(1);
        c.push(Gate::S(0)).push(Gate::Sdg(0));
        let opt = optimize_ir(&c);
        assert!(opt.gates.is_empty());
    }

    #[test]
    fn commutes_past_disjoint_gate_to_cancel() {
        let mut c = Circuit::new(3);
        c.push(Gate::Cx(0, 1)).push(Gate::X(2)).push(Gate::Cx(0, 1));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::X(2)]);
        assert_same_action(&c, &opt);
    }

    #[test]
    fn does_not_commute_past_overlapping_gate() {
        let mut c = Circuit::new(3);
        c.push(Gate::Cx(0, 1)).push(Gate::Cx(1, 2)).push(Gate::Cx(0, 1));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates.len(), 3, "no valid cancellation should have been found");
        assert_same_action(&c, &opt);
    }

    #[test]
    fn chain_of_commutes_finds_distant_cancellation() {
        let mut c = Circuit::new(4);
        c.push(Gate::Cx(0, 1))
            .push(Gate::H(2))
            .push(Gate::X(3))
            .push(Gate::Rz(2, 0.4))
            .push(Gate::Cx(0, 1));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates.len(), 3, "the two CNOTs should have cancelled");
        assert_same_action(&c, &opt);
    }

    #[test]
    fn never_reorders_measure_past_a_qubit_disjoint_gate() {
        // X(1) is on a different qubit than Measure(0, 0), so it would
        // look "disjoint" by qubit alone -- but Measure must never be
        // sled past anything, so this must come out unchanged (not
        // reordered, and obviously not merged with anything).
        let mut c = Circuit::new(2);
        c.push(Gate::Measure(0, 0)).push(Gate::X(1));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::Measure(0, 0), Gate::X(1)]);
    }

    #[test]
    fn never_reorders_two_measures_writing_the_same_clbit() {
        // Measure(0, 0) then Measure(1, 0): different qubits, same
        // classical bit. Qubit-disjointness alone would let the
        // commuting pass treat these as swappable, silently changing
        // which measurement's outcome ends up in classical bit 0.
        let mut c = Circuit::new(2);
        c.push(Gate::Measure(0, 0)).push(Gate::Measure(1, 0));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::Measure(0, 0), Gate::Measure(1, 0)]);
    }

    #[test]
    fn preserves_action_on_a_denser_mixed_circuit() {
        let mut c = Circuit::new(3);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 1))
            .push(Gate::T(2))
            .push(Gate::Cx(0, 1))
            .push(Gate::Swap(1, 2))
            .push(Gate::Swap(2, 1))
            .push(Gate::Rz(0, 0.3))
            .push(Gate::Rz(0, -0.3))
            .push(Gate::Cz(0, 2))
            .push(Gate::Y(1));
        let opt = optimize_ir(&c);
        assert!(
            opt.gates.len() < c.gates.len(),
            "expected some cancellation, got {} -> {} gates",
            c.gates.len(),
            opt.gates.len()
        );
        assert_same_action(&c, &opt);
    }
}
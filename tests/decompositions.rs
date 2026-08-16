//! Validates every decomposition identity in `src/native.rs` against
//! `sirraya_qutub::core::QuantumRegister` directly, rather than trusting
//! the ZYZ / RZZ-conjugation algebra on its own. For each gate: build a
//! circuit that applies the gate directly, and a second circuit that
//! applies its native decomposition, both starting from the *same*
//! randomized initial state (a random product of single-qubit rotations
//! so every amplitude is nonzero and phase-sensitive), then compare via
//! `QuantumRegister::fidelity`. A wrong sign anywhere in the algebra
//! reliably shows up as fidelity << 1, not a subtle discrepancy -- these
//! gates are either exactly right or clearly wrong.

use rand::Rng;
use sirraya_qutub::core::QuantumRegister;
use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::native::decompose;
use sirraya_qutub_transpiler::optimize::optimize;

const TOL: f64 = 1e-9;

fn randomized_register(num_qubits: usize, seed_offset: u64) -> QuantumRegister {
    let mut reg = QuantumRegister::new(num_qubits).unwrap();
    let mut rng = rand::thread_rng();
    let _ = seed_offset;
    for q in 0..num_qubits {
        let a: f64 = rng.gen_range(0.0..std::f64::consts::TAU);
        let b: f64 = rng.gen_range(0.0..std::f64::consts::TAU);
        let c: f64 = rng.gen_range(0.0..std::f64::consts::TAU);
        reg.apply_rz(q, a).unwrap();
        reg.apply_ry(q, b).unwrap();
        reg.apply_rz(q, c).unwrap();
    }
    reg
}

/// Runs `gate` two ways from the same random initial state -- directly
/// on a `QuantumRegister`, and via `decompose` + `optimize` executed as
/// native `{Rz, Ry, Rzz}` gates -- and asserts the resulting states
/// match up to global phase.
fn check_gate_matches_decomposition(num_qubits: usize, gate: Gate, trials: usize) {
    for trial in 0..trials {
        let mut direct = randomized_register(num_qubits, trial as u64);
        let mut native_reg = direct.clone();

        apply_ir_gate_directly(&mut direct, &gate);

        let mut circuit = Circuit::new(num_qubits);
        circuit.push(gate.clone());
        let native = optimize(&decompose(&circuit));
        sirraya_qutub_transpiler::emit::apply_to(&native, &mut native_reg).unwrap();

        let fidelity = direct.fidelity(&native_reg).unwrap();
        assert!(
            (fidelity - 1.0).abs() < TOL,
            "gate {:?} trial {}: fidelity {} (native circuit: {:?})",
            gate,
            trial,
            fidelity,
            native.gates
        );
    }
}

/// Applies an `ir::Gate` straight to a `QuantumRegister` using qutub's
/// own gate methods (the "ground truth" side of the comparison).
fn apply_ir_gate_directly(reg: &mut QuantumRegister, gate: &Gate) {
    match *gate {
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
            "apply_ir_gate_directly: Measure needs the shot-based statistical test \
             methodology from the P0.1 roadmap item (QuantumRegister::fidelity doesn't apply \
             to a measured bit), not this direct fidelity-comparison harness. No test in this \
             file exercises Measure; this arm exists only to satisfy exhaustiveness."
        ),
        Gate::If(..) => panic!(
            "apply_ir_gate_directly: If has no fidelity-based test yet, for the same reason \
             as Measure above -- it reads a classical bit this direct fidelity-comparison \
             harness has nowhere to produce. No test in this file exercises If; this arm \
             exists only to satisfy exhaustiveness. (See emit.rs's \
             apply_to_with_measurement/apply_native_gate_with_measurement for the real, \
             tested execution path, and quantum_teleportation.rs for it in real use.)"
        ),
    }
}

#[test]
fn single_qubit_gates_match() {
    for gate in [
        Gate::H(0),
        Gate::X(0),
        Gate::Y(0),
        Gate::Z(0),
        Gate::S(0),
        Gate::Sdg(0),
        Gate::T(0),
        Gate::Tdg(0),
    ] {
        check_gate_matches_decomposition(2, gate.clone(), 5);
    }
}

#[test]
fn rotation_gates_match() {
    let mut rng = rand::thread_rng();
    for _ in 0..8 {
        let angle: f64 = rng.gen_range(-std::f64::consts::TAU..std::f64::consts::TAU);
        check_gate_matches_decomposition(2, Gate::Rx(0, angle), 3);
    }
}

#[test]
fn cnot_matches() {
    check_gate_matches_decomposition(2, Gate::Cx(0, 1), 8);
    check_gate_matches_decomposition(2, Gate::Cx(1, 0), 8);
}

#[test]
fn cz_matches() {
    check_gate_matches_decomposition(2, Gate::Cz(0, 1), 8);
}

#[test]
fn controlled_phase_matches() {
    let mut rng = rand::thread_rng();
    for _ in 0..8 {
        let lambda: f64 = rng.gen_range(-std::f64::consts::TAU..std::f64::consts::TAU);
        check_gate_matches_decomposition(2, Gate::Cp(0, 1, lambda), 3);
    }
}

#[test]
fn swap_matches() {
    check_gate_matches_decomposition(2, Gate::Swap(0, 1), 8);
}

#[test]
fn rxx_matches() {
    let mut rng = rand::thread_rng();
    for _ in 0..8 {
        let angle: f64 = rng.gen_range(-std::f64::consts::TAU..std::f64::consts::TAU);
        check_gate_matches_decomposition(2, Gate::Rxx(0, 1, angle), 3);
    }
}

#[test]
fn ryy_matches() {
    let mut rng = rand::thread_rng();
    for _ in 0..8 {
        let angle: f64 = rng.gen_range(-std::f64::consts::TAU..std::f64::consts::TAU);
        check_gate_matches_decomposition(2, Gate::Ryy(0, 1, angle), 3);
    }
}

#[test]
fn three_qubit_circuit_matches() {
    // A denser check: several gates in sequence across 3 qubits,
    // decomposed and optimized as a whole circuit, not gate-by-gate.
    let mut circuit = Circuit::new(3);
    circuit
        .push(Gate::H(0))
        .push(Gate::Cx(0, 1))
        .push(Gate::Rz(2, 0.37))
        .push(Gate::Cx(1, 2))
        .push(Gate::T(0))
        .push(Gate::Ryy(0, 2, 0.91))
        .push(Gate::Swap(1, 2))
        .push(Gate::Cp(0, 1, 1.2));

    let mut direct = randomized_register(3, 0);
    let mut native_reg = direct.clone();
    for g in &circuit.gates {
        apply_ir_gate_directly(&mut direct, g);
    }
    let native = optimize(&decompose(&circuit));
    sirraya_qutub_transpiler::emit::apply_to(&native, &mut native_reg).unwrap();

    let fidelity = direct.fidelity(&native_reg).unwrap();
    assert!(
        (fidelity - 1.0).abs() < TOL,
        "combined circuit fidelity {} (native gate count: {})",
        fidelity,
        native.gates.len()
    );
}
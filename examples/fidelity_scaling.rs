//! Builds GHZ-state-preparation circuits of increasing width, compiles
//! each to the native gate set, and prints the resulting gate counts
//! and estimated fidelity on Quantinuum Helios -- a quick illustration
//! of why gate count (not just qubit count) is what actually drives the
//! fidelity budget down, and why a compiler that avoids unnecessary
//! gates matters as circuits grow.
//!
//! Run with: `cargo run --example fidelity_scaling`

use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::{decompose, estimate_circuit_fidelity, optimize, PublishedCalibration};

/// H on qubit 0, then a CNOT ladder out to every other qubit -- the
/// standard GHZ-state preparation circuit.
fn ghz_circuit(num_qubits: usize) -> Circuit {
    let mut circuit = Circuit::new(num_qubits);
    circuit.push(Gate::H(0));
    for q in 1..num_qubits {
        circuit.push(Gate::Cx(0, q));
    }
    circuit
}

fn main() {
    let cal = PublishedCalibration::quantinuum_helios_2026();
    println!("Calibration: {}", cal.name);
    println!(
        "  single-qubit error/gate: {:.2e}   two-qubit error/gate: {:.2e}\n",
        cal.single_qubit_error_probability(),
        cal.two_qubit_error_probability()
    );

    println!(
        "{:>6}  {:>10}  {:>10}  {:>12}  {:>12}",
        "qubits", "src gates", "native (1q,2q)", "opt gates", "est. fidelity"
    );
    for num_qubits in [2usize, 4, 8, 16, 32, 64, 98] {
        let circuit = ghz_circuit(num_qubits);
        let raw = decompose(&circuit);
        let native = optimize(&raw);
        let (single, two) = native.gate_counts();
        let fidelity = estimate_circuit_fidelity(&native, &cal);

        println!(
            "{:>6}  {:>10}  {:>5},{:<5}  {:>12}  {:>11.4}%",
            num_qubits,
            circuit.gates.len(),
            single,
            two,
            native.gates.len(),
            fidelity * 100.0
        );
    }

    println!(
        "\n(98 qubits matches Quantinuum Helios's own qubit count -- a full-width\nGHZ state is close to a worst case for this device: n-1 sequential\ntwo-qubit gates on the critical path.)"
    );
}

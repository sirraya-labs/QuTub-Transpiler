//! Prints, for each source-level gate this crate knows how to compile,
//! the native `{Rz, Ry, Rzz}` sequence it decomposes to (before and
//! after the peephole optimizer). Useful as a quick reference for "what
//! does this actually cost on native hardware" -- e.g. CNOT is 1
//! two-qubit gate either way, but a bare Ry rotation is free (it's
//! already native) while H costs 2 single-qubit gates.
//!
//! Run with: `cargo run --example gate_cheatsheet`

use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::{decompose, optimize};

fn show(label: &str, gate: Gate, num_qubits: usize) {
    let mut circuit = Circuit::new(num_qubits);
    circuit.push(gate);

    let raw = decompose(&circuit);
    let opt = optimize(&raw);
    let (single, two) = opt.gate_counts();

    println!("{label:<18} raw={:>2} gates  optimized={:>2} gates  ({single} single-qubit, {two} two-qubit)", raw.gates.len(), opt.gates.len());
    for g in &opt.gates {
        println!("    {:?}", g);
    }
}

fn main() {
    println!("=== Single-qubit gates -> {{Rz, Ry}} ===\n");
    show("H(0)", Gate::H(0), 1);
    show("X(0)", Gate::X(0), 1);
    show("Y(0)", Gate::Y(0), 1);
    show("Z(0)", Gate::Z(0), 1);
    show("S(0)", Gate::S(0), 1);
    show("T(0)", Gate::T(0), 1);
    show("Rx(0, 0.42)", Gate::Rx(0, 0.42), 1);
    show("Ry(0, 0.42)  (already native)", Gate::Ry(0, 0.42), 1);
    show("Rz(0, 0.42)  (already native)", Gate::Rz(0, 0.42), 1);

    println!("\n=== Two-qubit gates -> {{Rz, Ry, Rzz}} ===\n");
    show("Cx(0, 1)", Gate::Cx(0, 1), 2);
    show("Cz(0, 1)", Gate::Cz(0, 1), 2);
    show("Swap(0, 1)", Gate::Swap(0, 1), 2);
    show("Rxx(0, 1, 0.9)", Gate::Rxx(0, 1, 0.9), 2);
    show("Ryy(0, 1, 0.9)", Gate::Ryy(0, 1, 0.9), 2);
    show("Rzz(0, 1, 0.9)  (already native)", Gate::Rzz(0, 1, 0.9), 2);
    show("Cp(0, 1, 1.2)", Gate::Cp(0, 1, 1.2), 2);
}

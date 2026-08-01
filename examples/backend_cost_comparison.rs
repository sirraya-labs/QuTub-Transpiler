//! Compare the compilation cost of the same quantum circuit across all
//! supported hardware backends.
//!
//! This example constructs circuits programmatically using the
//! [`ir::Circuit`] builder API, lowers each circuit to every backend,
//! compares the resulting native gate counts, estimates execution
//! fidelity using each backend's published calibration, and recommends
//! the backend expected to produce the highest fidelity.
//!
//! It demonstrates:
//! - Programmatic circuit construction (no OpenQASM input).
//! - Backend-aware lowering.
//! - Hardware-specific native gate counts.
//! - Fidelity estimation.
//! - Backend selection based on estimated execution fidelity.
//!
//! Run with:
//!
//! cargo run --example backend_cost_comparison

use sirraya_qutub_transpiler::backend::{lower, Backend, BackendCircuit};
use sirraya_qutub_transpiler::fidelity::estimate_backend_circuit_fidelity;
use sirraya_qutub_transpiler::ir::{Circuit, Gate};

/// Every backend currently supported by the crate.
const BACKENDS: [Backend; 3] = [
    Backend::TrappedIon,
    Backend::IbmQ,
    Backend::Rigetti,
];

/// Construct a Bell pair.
///
/// Circuit:
///
/// q0 ──H────■──
///           │
/// q1 ───────X──
fn bell_pair() -> Circuit {
    let mut c = Circuit::new(2);
    c.push(Gate::H(0))
        .push(Gate::Cx(0, 1));
    c
}

/// Construct an n-qubit GHZ state.
///
/// Applies:
/// - H on qubit 0
/// - CX(0, i) for every remaining qubit
fn ghz(n: usize) -> Circuit {
    let mut c = Circuit::new(n);

    c.push(Gate::H(0));

    for i in 1..n {
        c.push(Gate::Cx(0, i));
    }

    c
}

/// Construct an n-qubit Quantum Fourier Transform.
///
/// For each qubit:
/// - Apply H
/// - Apply controlled phase rotations
/// - Reverse qubit order using SWAP gates
fn qft(n: usize) -> Circuit {
    let mut c = Circuit::new(n);

    for i in 0..n {
        c.push(Gate::H(i));

        for j in (i + 1)..n {
            let lambda =
                std::f64::consts::PI / (1u32 << (j - i)) as f64;

            c.push(Gate::Cp(j, i, lambda));
        }
    }

    for i in 0..(n / 2) {
        c.push(Gate::Swap(i, n - 1 - i));
    }

    c
}

fn print_header(name: &str, circuit: &Circuit) {
    println!();
    println!("{}", "=".repeat(78));
    println!("Circuit : {}", name);
    println!("Qubits  : {}", circuit.num_qubits);
    println!("Source  : {:?}", circuit.gate_counts());
    println!("{}", "-".repeat(78));

    println!(
        "{:<14} {:>12} {:>12} {:>20}",
        "Backend",
        "1Q Gates",
        "2Q Gates",
        "Estimated Fidelity"
    );

    println!("{}", "-".repeat(78));
}

fn main() {
    let circuits = vec![
        ("bell_pair", bell_pair()),
        ("ghz_4", ghz(4)),
        ("qft_3", qft(3)),
    ];

    for (name, circuit) in &circuits {
        print_header(name, circuit);

        let mut best: Option<(Backend, f64)> = None;

        for &backend in &BACKENDS {
            let backend_circuit: BackendCircuit = lower(circuit, backend);

            let (single, two) = backend_circuit.gate_counts();

            let calibration = backend.calibration();

            let fidelity =
                estimate_backend_circuit_fidelity(&backend_circuit, &calibration);

            println!(
                "{:<14} {:>12} {:>12} {:>20.6}",
                format!("{:?}", backend),
                single,
                two,
                fidelity
            );

            if best.map_or(true, |(_, best_fidelity)| fidelity > best_fidelity) {
                best = Some((backend, fidelity));
            }
        }

        println!("{}", "-".repeat(78));

        if let Some((backend, fidelity)) = best {
            println!(
                "Recommended backend : {:?}",
                backend
            );

            println!(
                "Estimated fidelity  : {:.6}",
                fidelity
            );

            println!(
                "Reason              : Highest estimated fidelity under the \
                 bundled backend calibration model."
            );
        }

        println!();
    }

    println!("{}", "=".repeat(78));
    println!("Note");
    println!("{}", "-".repeat(78));
    println!(
        "Estimated fidelities are derived from the calibration models \
bundled with this crate. They are intended for comparative analysis \
between supported backends rather than guarantees of hardware execution."
    );
}

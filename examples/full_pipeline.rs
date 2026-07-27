//! Full pipeline demo: QASM text in, source-level optimization,
//! multi-backend native-gate lowering, a fidelity budget against
//! published Quantinuum Helios figures, and an actual run against
//! `sirraya_qutub::core::QuantumRegister` on every backend.
//!
//! Pipeline shown end to end:
//! `qasm::parse` -> `optimize_ir` (source-level cancellation/reorder)
//! -> `backend::lower` (per-backend native gate set) -> execution
//! (`emit::run` for `TrappedIon`, `emit::run_backend` for all three) ->
//! `estimate_circuit_fidelity` (TrappedIon only, since that's the
//! backend the published Quantinuum Helios numbers describe).
//!
//! Run with: `cargo run --example full_pipeline`

use sirraya_qutub_transpiler::backend::{lower, Backend, BackendCircuit};
use sirraya_qutub_transpiler::{
    decompose, emit, estimate_circuit_fidelity, optimize, optimize_ir, qasm, PublishedCalibration,
};

const SOURCE: &str = r#"
    OPENQASM 2.0;
    include "qelib1.inc";
    qreg q[3];
    creg c[3];
    h q[0];
    cx q[0], q[1];
    cx q[1], q[2];
    t q[0];
    ryy(0.9) q[0], q[2];
    swap q[1], q[2];
    cp(0.5) q[0], q[1];
"#;

fn main() -> Result<(), String> {
    // --- Parse ---------------------------------------------------------
    let circuit = qasm::parse(SOURCE)?;
    println!(
        "Parsed {} qubits, {} source gates: {:?}",
        circuit.num_qubits,
        circuit.gates.len(),
        circuit.gate_counts()
    );

    // --- Source-level optimization --------------------------------------
    // Adjacent self-inverse / explicit-inverse pairs cancel, with a
    // commuting-reorder pass to pull non-adjacent cancellable pairs
    // together first. This circuit has no such pairs, but it's the
    // correct step to run before decomposition on real-world circuits.
    let circuit = optimize_ir(&circuit);
    println!(
        "After source-level optimization: {} gates: {:?}",
        circuit.gates.len(),
        circuit.gate_counts()
    );

    // --- Native (TrappedIon) path: decompose + peephole optimize --------
    let raw_native = decompose(&circuit);
    let native = optimize(&raw_native);
    println!(
        "\nNative gate set: {} gates before optimization -> {} after",
        raw_native.gates.len(),
        native.gates.len()
    );
    let (single, two) = native.gate_counts();
    println!("  {} single-qubit (Rz/Ry), {} two-qubit (Rzz)", single, two);

    // Fidelity estimate against published Quantinuum Helios figures --
    // this is specifically a TrappedIon-target number, since that's the
    // gate set (Rz/Ry/Rzz) those published figures describe.
    let cal = PublishedCalibration::quantinuum_helios_2026();
    let fidelity_estimate = estimate_circuit_fidelity(&native, &cal);
    println!(
        "Estimated circuit fidelity on {}: {:.6}",
        cal.name, fidelity_estimate
    );

    println!("\nNative QASM:\n{}", emit::to_qasm(&native, "full_pipeline_demo"));

    let reg = emit::run(&native)?;
    println!("Final state probability distribution (TrappedIon native path):");
    print_distribution(&reg);

    // --- Multi-backend lowering ------------------------------------------
    // Same source circuit, lowered to each backend's actual native gate
    // set and executed for real -- not just gate-counted.
    for backend in [Backend::TrappedIon, Backend::IbmQ, Backend::Rigetti] {
        let bc: BackendCircuit = lower(&circuit, backend);
        let (b_single, b_two) = bc.gate_counts();
        println!(
            "\n[{:?}] lowered: {} single-qubit, {} two-qubit gates",
            backend, b_single, b_two
        );

        let backend_reg = emit::run_backend(&bc)?;
        println!("[{:?}] final state probability distribution:", backend);
        print_distribution(&backend_reg);
    }

    Ok(())
}

fn print_distribution(reg: &sirraya_qutub::core::QuantumRegister) {
    let mut dist: Vec<(String, f64)> = reg.get_probability_distribution().into_iter().collect();
    dist.sort_by(|a, b| a.0.cmp(&b.0));
    for (bitstring, prob) in dist {
        if prob > 1e-9 {
            println!("  |{}>: {:.6}", bitstring, prob);
        }
    }
}
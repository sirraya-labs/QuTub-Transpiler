//! End-to-end pipeline demo: QASM text in, source-level optimization,
//! per-backend native-gate lowering (with routing), a per-backend
//! fidelity budget against each backend's *own* published calibration,
//! real execution (including real measurement outcomes) against
//! `sirraya_qutub::core::QuantumRegister`, circuit diagrams at every
//! level, and a real IBM-hardware-basis QASM export.
//!
//! Pipeline shown end to end:
//! `qasm::parse` -> `optimize_ir` (source-level cancellation/reorder)
//! -> `backend::lower` (routes, then re-expresses for each backend's
//! native gate set) -> `fidelity::estimate_backend_circuit_fidelity`
//! (each backend judged against its own published calibration via
//! `Backend::calibration` -- TrappedIon against Quantinuum Helios,
//! IbmQ against IBM Heron r2, Rigetti against Rigetti Ankaa-3) ->
//! `emit::run_backend_with_measurement` (real execution, real
//! Born-rule-sampled measurement outcomes) -> `diagram::Diagram`
//! (rendered at both the source and lowered level) -> `ibm_export`
//! (IbmQ only: real Rz/SX/X/Cx basis QASM, the text `submit_ibm.py`
//! hands to Qiskit / IBM Quantum Platform).
//!
//! Run with: `cargo run --example pipeline_end_to_end`

use sirraya_qutub_transpiler::backend::{lower, Backend, BackendCircuit};
use sirraya_qutub_transpiler::diagram::Diagram;
use sirraya_qutub_transpiler::fidelity::estimate_backend_circuit_fidelity;
use sirraya_qutub_transpiler::{
    decompose, emit, estimate_circuit_fidelity, ibm_export, optimize, optimize_ir, qasm,
    PublishedCalibration,
};

/// A Bell pair on qubits 0-1, a real `Rz`-in-the-middle-of-a-`Cx`-
/// sandwich (the exact `Rzz(a,b,theta) == Cx(a,b).Rz(b,theta).Cx(a,b)`
/// identity `backend::IbmQSpec::push_two_qubit_zz` re-expresses in the
/// other direction), qubit 2 rotated and entangled in, then every
/// qubit measured -- small enough to read the diagrams/QASM below in
/// full, large enough to exercise routing and real classical output.
const SOURCE: &str = r#"
    OPENQASM 2.0;
    include "qelib1.inc";
    qreg q[3];
    creg c[3];
    h q[0];
    cx q[0], q[1];
    rz(0.7) q[1];
    cx q[0], q[1];
    ry(1.2) q[2];
    cx q[1], q[2];
    measure q[0] -> c[0];
    measure q[1] -> c[1];
    measure q[2] -> c[2];
"#;

fn main() -> Result<(), String> {
    // --- Parse -----------------------------------------------------
    let circuit = qasm::parse(SOURCE)?;
    println!(
        "Parsed {} qubits, {} clbits, {} source gates: {:?}",
        circuit.num_qubits,
        circuit.num_clbits,
        circuit.gates.len(),
        circuit.gate_counts()
    );
    println!("\n--- Source circuit ---\n{}", Diagram::from_circuit(&circuit).to_ascii());

    // --- Source-level optimization ------------------------------------
    // Adjacent self-inverse / explicit-inverse pairs cancel, with a
    // commuting-reorder pass to pull non-adjacent cancellable pairs
    // together first.
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

    // TrappedIon-specific fidelity estimate against published
    // Quantinuum Helios figures -- the gate set (Rz/Ry/Rzz) those
    // figures actually describe.
    let cal = PublishedCalibration::quantinuum_helios_2026();
    let fidelity_estimate = estimate_circuit_fidelity(&native, &cal);
    println!("Estimated circuit fidelity on {}: {:.6}", cal.name, fidelity_estimate);

    println!("\nNative QASM:\n{}", emit::to_qasm(&native, "pipeline_end_to_end"));

    // Real execution with real, Born-rule-sampled measurement outcomes.
    let (_reg, clbits) = emit::run_with_measurement(&native)?;
    println!("TrappedIon native measurement outcomes (by clbit): {:?}", clbits);

    // --- Multi-backend lowering -----------------------------------------
    // Same source circuit, routed and re-expressed for each backend's
    // actual native gate set, fidelity-estimated against that backend's
    // own published calibration, and actually executed -- not just
    // gate-counted.
    for backend in [Backend::TrappedIon, Backend::IbmQ, Backend::Rigetti] {
        let bc: BackendCircuit = lower(&circuit, backend);
        let (b_single, b_two) = bc.gate_counts();
        println!(
            "\n[{:?}] lowered: {} single-qubit, {} two-qubit gates",
            backend, b_single, b_two
        );

        let backend_cal = backend.calibration();
        let backend_fidelity = estimate_backend_circuit_fidelity(&bc, &backend_cal);
        println!(
            "[{:?}] estimated circuit fidelity on {}: {:.6}",
            backend, backend_cal.name, backend_fidelity
        );

        let (_backend_reg, backend_clbits) = emit::run_backend_with_measurement(&bc)?;
        println!("[{:?}] measurement outcomes (by clbit): {:?}", backend, backend_clbits);

        // IbmQ only: real IBM-basis (Rz/SX/X/Cx) QASM -- see
        // ibm_export.rs's own doc comment for why this only accepts an
        // IbmQ-lowered circuit, and submit_ibm.py for what actually
        // consumes this text.
        if backend == Backend::IbmQ {
            println!(
                "[{:?}] lowered circuit diagram:\n{}",
                backend,
                Diagram::from_backend(&bc).to_ascii()
            );
            let ibm_qasm = ibm_export::to_ibm_qasm(&bc, "pipeline_end_to_end")?;
            println!("[{:?}] real IBM-basis QASM:\n{}", backend, ibm_qasm);
        }
    }

    Ok(())
}